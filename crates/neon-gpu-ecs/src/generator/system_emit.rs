//! TAC → WGSL emission for system entry points.
//!
//! The emitter first infers a WGSL type for every local slot from first
//! assignment, then translates the instruction list. All systems — including
//! the RenderData population — iterate the compacted entity list produced by
//! the sorting pass; `StoreRender` writes into the contiguous instance slot
//! (the compaction index), so the instance buffer stays dense and its count
//! equals the query count in `framePrepBuffer`.

use crate::generator::bind_layout;
use crate::ir::{
    AtomicOpCode, BinaryOpCode, BuiltinFunc, CompareOp, ComponentDef, ComponentType, EcsIr, Instr,
    ResourceDef, SystemDef, UnaryOpCode,
};
use crate::EcsError;

/// Everything needed to emit one entry point.
pub(super) struct EmitCtx<'a> {
    pub ir: &'a EcsIr,
    /// Slot of this system's query inside `framePrepBuffer`.
    pub slot: u32,
    /// Entry point suffix (system id).
    pub system_id: u32,
}

/// Infer the WGSL type of every local slot from first assignment. Slots that
/// are never assigned stay `None` and are declared as `f32` (zero-initialised
/// scratch); reading an unassigned slot is rejected by [`read_local`].
pub(super) fn infer_local_types(
    system: &SystemDef,
    ir: &EcsIr,
) -> Result<Vec<Option<&'static str>>, EcsError> {
    let n = system.local_var_count as usize;
    let mut types: Vec<Option<&'static str>> = vec![None; n];

    let name = system.name.as_str();

    for (pc, instr) in system.instructions.iter().enumerate() {
        let at = format!("instruction {pc}");
        match instr {
            Instr::Load { dest, component_id, .. } => {
                let comp = component(ir, *component_id, name, &at)?;
                assign_local(&mut types, *dest, comp.ty.wgsl_local_type(), name, &at)?;
            }
            Instr::Const { dest, ty, .. } => {
                assign_local(&mut types, *dest, ty.wgsl_local_type(), name, &at)?;
            }
            Instr::LoadEntityId { dest } => {
                assign_local(&mut types, *dest, "u32", name, &at)?;
            }
            Instr::LoadResource { dest, resource_id } => {
                let res = resource(ir, *resource_id, name, &at)?;
                assign_local(&mut types, *dest, res.ty.wgsl_local_type(), name, &at)?;
            }
            Instr::BinaryOp { dest, lhs, rhs, op } => {
                let ty = read_local(&types, *lhs, name, &at)?;
                let rhs_ty = read_local(&types, *rhs, name, &at)?;
                let result = binary_result(*op, ty, rhs_ty, name, &at)?;
                assign_local(&mut types, *dest, result, name, &at)?;
            }
            Instr::UnaryOp { dest, src, op } => {
                let ty = read_local(&types, *src, name, &at)?;
                if matches!(op, UnaryOpCode::Not) && ty != "bool" {
                    return Err(EcsError::WgslInvalid(format!(
                        "system '{name}': {at} Not requires a bool operand, got {ty}"
                    )));
                }
                assign_local(&mut types, *dest, ty, name, &at)?;
            }
            Instr::Compare { dest, lhs, rhs, .. } => {
                let ty = read_local(&types, *lhs, name, &at)?;
                let rhs_ty = read_local(&types, *rhs, name, &at)?;
                if ty != rhs_ty {
                    return Err(EcsError::WgslInvalid(format!(
                        "system '{name}': {at} Compare mixes {ty} and {rhs_ty}"
                    )));
                }
                assign_local(&mut types, *dest, "bool", name, &at)?;
            }
            Instr::If { cond, .. } => {
                let ty = read_local(&types, *cond, name, &at)?;
                if ty != "bool" {
                    return Err(EcsError::WgslInvalid(format!(
                        "system '{name}': {at} If condition must be bool, got {ty}"
                    )));
                }
            }
            Instr::CallBuiltin { dest, func, args } => {
                for (i, arg) in args.iter().enumerate() {
                    read_local(&types, *arg, name, &format!("{at} arg {i}"))?;
                }
                if !func.is_structural() {
                    let result = builtin_result(*func, &types, args, name, &at)?;
                    assign_local(&mut types, *dest, result, name, &at)?;
                }
                // Structural results are discarded (CPU replay assigns ids).
            }
            Instr::AtomicOp { value, .. } => {
                let ty = read_local(&types, *value, name, &at)?;
                if ty != "u32" {
                    return Err(EcsError::WgslInvalid(format!(
                        "system '{name}': {at} AtomicOp value must be u32, got {ty}"
                    )));
                }
            }
            Instr::Store { src, component_id } => {
                let comp = component(ir, *component_id, name, &at)?;
                let ty = read_local(&types, *src, name, &at)?;
                if ty != comp.ty.wgsl_local_type() {
                    return Err(EcsError::WgslInvalid(format!(
                        "system '{name}': {at} Store of {ty} into component '{}' typed {}",
                        comp.name,
                        comp.ty.wgsl_local_type()
                    )));
                }
            }
            Instr::StoreRender { src, .. } => {
                let ty = read_local(&types, *src, name, &at)?;
                if ty != "vec4f" {
                    return Err(EcsError::WgslInvalid(format!(
                        "system '{name}': {at} StoreRender requires vec4f, got {ty}"
                    )));
                }
            }
            Instr::Jump { .. } | Instr::Return => {}
        }
    }
    Ok(types)
}

/// Result type of a binary operation, enforcing WGSL typing rules:
/// operands must share a type, except `Mul`, which allows scalar promotion
/// (`vecNf * f32` / `f32 * vecNf`), matching the design notes' physics
/// pattern `vel = vel * dt`.
fn binary_result(
    op: BinaryOpCode,
    lhs_ty: &'static str,
    rhs_ty: &'static str,
    name: &str,
    at: &str,
) -> Result<&'static str, EcsError> {
    if lhs_ty == rhs_ty {
        return Ok(lhs_ty);
    }
    match op {
        BinaryOpCode::Mul => {
            let promoted = match (lhs_ty, rhs_ty) {
                ("f32", "vec2f") | ("vec2f", "f32") => "vec2f",
                ("f32", "vec3f") | ("vec3f", "f32") => "vec3f",
                ("f32", "vec4f") | ("vec4f", "f32") => "vec4f",
                _ => {
                    return Err(EcsError::WgslInvalid(format!(
                        "system '{name}': {at} Mul mixes {lhs_ty} and {rhs_ty}"
                    )))
                }
            };
            Ok(promoted)
        }
        _ => Err(EcsError::WgslInvalid(format!(
            "system '{name}': {at} binary op mixes {lhs_ty} and {rhs_ty}"
        ))),
    }
}

fn assign_local(
    types: &mut [Option<&'static str>],
    slot: u32,
    ty: &'static str,
    name: &str,
    at: &str,
) -> Result<(), EcsError> {
    let len = types.len();
    let cell = types.get_mut(slot as usize).ok_or_else(|| {
        EcsError::WgslInvalid(format!(
            "system '{name}': {at} references local v{slot} beyond local_var_count {len}"
        ))
    })?;
    match *cell {
        Some(existing) if existing != ty => Err(EcsError::WgslInvalid(format!(
            "system '{name}': local v{slot} typed as {existing} but {at} needs {ty}"
        ))),
        Some(_) => Ok(()),
        None => {
            *cell = Some(ty);
            Ok(())
        }
    }
}

/// Non-structural builtin result type.
fn builtin_result(
    func: BuiltinFunc,
    types: &[Option<&'static str>],
    args: &[u32],
    name: &str,
    at: &str,
) -> Result<&'static str, EcsError> {
    let arg_ty = read_local(types, args[0], name, at)?;
    Ok(match func {
        BuiltinFunc::Sin | BuiltinFunc::Cos | BuiltinFunc::Normalize => arg_ty,
        BuiltinFunc::Length | BuiltinFunc::Dot => "f32",
        BuiltinFunc::Cross => "vec3f",
        _ => unreachable!("structural builtins have no result"),
    })
}

fn read_local(
    types: &[Option<&'static str>],
    slot: u32,
    name: &str,
    at: &str,
) -> Result<&'static str, EcsError> {
    types.get(slot as usize).and_then(|t| *t).ok_or_else(|| {
        EcsError::WgslInvalid(format!(
            "system '{name}': {at} reads v{slot} before any assignment"
        ))
    })
}

fn component<'a>(
    ir: &'a EcsIr,
    id: u32,
    system: &str,
    at: &str,
) -> Result<&'a ComponentDef, EcsError> {
    ir.components.get(id as usize).ok_or_else(|| {
        EcsError::WgslInvalid(format!(
            "system '{system}': {at} references unknown component {id}"
        ))
    })
}

/// True when the component's data array is declared as `array<atomic<...>>`
/// and therefore needs `atomicLoad`/`atomicStore` accessors.
fn is_atomic_storage(ty: ComponentType) -> bool {
    matches!(ty, ComponentType::U32 | ComponentType::I32)
}

fn resource<'a>(
    ir: &'a EcsIr,
    id: u32,
    system: &str,
    at: &str,
) -> Result<&'a ResourceDef, EcsError> {
    ir.resources.get(id as usize).ok_or_else(|| {
        EcsError::WgslInvalid(format!(
            "system '{system}': {at} references unknown resource {id}"
        ))
    })
}

/// Emit one system entry point iterating the compacted entity list.
/// Bodies containing `If`/`Jump` are lowered to a `loop + switch` program
/// counter state machine; straight-line bodies keep the flat form.
pub(super) fn emit_system(system: &SystemDef, ctx: &EmitCtx<'_>) -> Result<String, EcsError> {
    let local_types = infer_local_types(system, ctx.ir)?;
    let mut out = String::new();
    let name = &system.name;

    out.push_str(&format!(
        "@compute @workgroup_size(64)\nfn system_{name}(@builtin(global_invocation_id) ecs_gid : vec3u) {{\n"
    ));
    out.push_str(&format!("    let ecs_cmd = framePrepBuffer[{}u];\n", ctx.slot));
    out.push_str("    let ecs_index = ecs_gid.x;\n");
    out.push_str("    if (ecs_index >= ecs_cmd.count) { return; }\n");
    out.push_str("    let ecs_entity = compactedEntityIds[ecs_cmd.start + ecs_index];\n");
    out.push_str("    let ecs_slot = ecs_index;\n");

    // Local declarations; never-assigned slots default to f32 scratch.
    for (slot, ty) in local_types.iter().enumerate() {
        out.push_str(&format!("    var v{slot} : {};\n", ty.unwrap_or("f32")));
    }

    let has_control_flow = system
        .instructions
        .iter()
        .any(|instr| matches!(instr, Instr::If { .. } | Instr::Jump { .. }));

    if has_control_flow {
        emit_state_machine(system, ctx, &mut out)?;
    } else {
        for instr in &system.instructions {
            out.push_str(&emit_instr(instr, ctx)?);
        }
    }

    out.push_str("}\n");
    Ok(out)
}

/// A basic block: instructions `[first, last_excl)` plus how control leaves it.
struct Block {
    first: usize,
    last_excl: usize,
    term: Term,
}

/// How a block transfers control.
enum Term {
    /// `Return`: leave the entry point.
    Return,
    /// `Jump`: go to the block starting at the target instruction.
    Jump(u32),
    /// `If`: branch on a bool local.
    If { cond: u32, t: u32, f: u32 },
    /// No terminator: fall through to the next block in program order.
    Fall,
}

/// Lower a control-flow body to `var ecs_pc` + `loop { switch(ecs_pc) ... }`.
/// WGSL has no `goto`, so flat TAC control flow becomes a program-counter
/// state machine. Blocks without an explicit terminator fall through to the
/// following block; the final block returns implicitly. Branch targets are
/// always block starts because every target and every post-terminator
/// instruction is registered as a block start. Dead (unreachable) blocks are
/// emitted but never execute.
fn emit_state_machine(
    system: &SystemDef,
    ctx: &EmitCtx<'_>,
    out: &mut String,
) -> Result<(), EcsError> {
    let name = &system.name;
    let instrs = &system.instructions;
    let n = instrs.len();
    if n == 0 {
        return Ok(());
    }

    let is_term = |i: usize| {
        matches!(
            instrs[i],
            Instr::If { .. } | Instr::Jump { .. } | Instr::Return
        )
    };

    // Block starts: instruction 0, every branch target, and every instruction
    // following a terminator.
    let mut starts: Vec<usize> = vec![0];
    for (i, instr) in instrs.iter().enumerate() {
        match instr {
            Instr::If { true_block, false_block, .. } => {
                for t in [*true_block as usize, *false_block as usize] {
                    if t < n && !starts.contains(&t) {
                        starts.push(t);
                    }
                }
            }
            Instr::Jump { target } => {
                let t = *target as usize;
                if t < n && !starts.contains(&t) {
                    starts.push(t);
                }
            }
            _ => {}
        }
        if is_term(i) && i + 1 < n && !starts.contains(&(i + 1)) {
            starts.push(i + 1);
        }
    }
    starts.sort_unstable();

    // Split into blocks; the terminator, when present, is always the last
    // instruction of the block range.
    let mut blocks: Vec<Block> = Vec::new();
    for (k, first) in starts.iter().enumerate() {
        let last_excl = if k + 1 < starts.len() { starts[k + 1] } else { n };
        let last_i = last_excl - 1;
        let term = match &instrs[last_i] {
            Instr::Return => Term::Return,
            Instr::Jump { target } => Term::Jump(*target),
            Instr::If { cond, true_block, false_block } => Term::If {
                cond: *cond,
                t: *true_block,
                f: *false_block,
            },
            _ => {
                if k + 1 < starts.len() {
                    Term::Fall
                } else {
                    // Final block without an explicit terminator returns.
                    Term::Return
                }
            }
        };
        blocks.push(Block { first: *first, last_excl, term });
    }

    // Map a branch target instruction index to its case number.
    let case_of = |idx: usize| -> Result<u32, EcsError> {
        blocks
            .iter()
            .position(|b| b.first == idx)
            .map(|p| p as u32)
            .ok_or_else(|| {
                EcsError::WgslInvalid(format!(
                    "system '{name}': branch target instruction {idx} is not a block start"
                ))
            })
    };

    // Emit the state machine.
    out.push_str("    var ecs_pc : u32 = 0u;\n");
    out.push_str("    loop {\n");
    out.push_str("        switch (ecs_pc) {\n");
    for (case, block) in blocks.iter().enumerate() {
        out.push_str(&format!("            case {case}u: {{\n"));
        // Body instructions exclude the terminator itself.
        let body_end = match block.term {
            Term::Fall => block.last_excl,
            _ => block.last_excl - 1,
        };
        for instr in instrs[block.first..body_end].iter() {
            out.push_str(&indent(&emit_instr(instr, ctx)?, 4));
        }
        match &block.term {
            Term::Return => {
                out.push_str("                return;\n");
            }
            Term::Jump(target) => {
                let target_case = case_of(*target as usize)?;
                out.push_str(&format!("                ecs_pc = {target_case}u;\n"));
                out.push_str("                break;\n");
            }
            Term::If { cond, t, f } => {
                let t_case = case_of(*t as usize)?;
                let f_case = case_of(*f as usize)?;
                out.push_str(&format!("                if (v{cond}) {{\n"));
                out.push_str(&format!("                    ecs_pc = {t_case}u;\n"));
                out.push_str("                } else {\n");
                out.push_str(&format!("                    ecs_pc = {f_case}u;\n"));
                out.push_str("                }\n");
                out.push_str("                break;\n");
            }
            Term::Fall => {
                out.push_str(&format!("                ecs_pc = {}u;\n", case + 1));
                out.push_str("                break;\n");
            }
        }
        out.push_str("            }\n");
    }
    // WGSL switch selectors must be exhaustively covered; ecs_pc only ever
    // holds valid case numbers, so the default arm is a defensive return.
    out.push_str("            default: {\n");
    out.push_str("                return;\n");
    out.push_str("            }\n");
    out.push_str("        }\n");
    out.push_str("    }\n");
    Ok(())
}

/// Re-indent emitted instruction lines deeper for switch bodies.
fn indent(text: &str, extra: usize) -> String {
    let pad = " ".repeat(extra);
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{pad}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn emit_instr(instr: &Instr, ctx: &EmitCtx<'_>) -> Result<String, EcsError> {
    let mut out = String::new();
    match instr {
        Instr::Load { dest, component_id, .. } => {
            let comp = ctx.ir.components.get(*component_id as usize).unwrap();
            if comp.ty == ComponentType::Bool {
                // Bool payload is stored as u32 0/1 in the plain data array;
                // presence is already guaranteed by the query.
                out.push_str(&format!(
                    "    v{dest} = ecs_c{component_id}[ecs_entity] != 0u;\n"
                ));
            } else if is_atomic_storage(comp.ty) {
                out.push_str(&format!(
                    "    v{dest} = atomicLoad(&ecs_c{component_id}[ecs_entity]);\n"
                ));
            } else {
                out.push_str(&format!(
                    "    v{dest} = ecs_c{component_id}[ecs_entity];\n"
                ));
            }
        }
        Instr::Const { dest, ty, bytes } => {
            let literal = const_literal(*ty, bytes)?;
            out.push_str(&format!("    v{dest} = {literal};\n"));
        }
        Instr::LoadEntityId { dest } => {
            out.push_str(&format!("    v{dest} = ecs_entity;\n"));
        }
        Instr::LoadResource { dest, resource_id } => {
            out.push_str(&format!("    v{dest} = ecs_r{resource_id};\n"));
        }
        Instr::BinaryOp { dest, lhs, rhs, op } => {
            let op = match op {
                BinaryOpCode::Add => "+",
                BinaryOpCode::Sub => "-",
                BinaryOpCode::Mul => "*",
                BinaryOpCode::Div => "/",
                BinaryOpCode::Mod => "%",
                BinaryOpCode::And => "&",
                BinaryOpCode::Or => "|",
                BinaryOpCode::Xor => "^",
            };
            out.push_str(&format!("    v{dest} = v{lhs} {op} v{rhs};\n"));
        }
        Instr::UnaryOp { dest, src, op } => {
            let op = match op {
                UnaryOpCode::Neg => "-",
                UnaryOpCode::Not => "!",
            };
            out.push_str(&format!("    v{dest} = {op}v{src};\n"));
        }
        Instr::Compare { dest, lhs, rhs, cond } => {
            let op = match cond {
                CompareOp::Equal => "==",
                CompareOp::NotEqual => "!=",
                CompareOp::Less => "<",
                CompareOp::LessEqual => "<=",
                CompareOp::Greater => ">",
                CompareOp::GreaterEqual => ">=",
            };
            out.push_str(&format!("    v{dest} = v{lhs} {op} v{rhs};\n"));
        }
        Instr::Store { src, component_id } => {
            let comp = ctx.ir.components.get(*component_id as usize).unwrap();
            if comp.ty == ComponentType::Bool {
                out.push_str(&format!(
                    "    ecs_c{component_id}[ecs_entity] = select(0u, 1u, v{src});\n"
                ));
            } else if is_atomic_storage(comp.ty) {
                out.push_str(&format!(
                    "    atomicStore(&ecs_c{component_id}[ecs_entity], v{src});\n"
                ));
            } else {
                out.push_str(&format!(
                    "    ecs_c{component_id}[ecs_entity] = v{src};\n"
                ));
            }
            // Bump the version: every Store marks the component changed since
            // the last baseline snapshot.
            out.push_str(&format!(
                "    atomicAdd(&ecs_cv{component_id}[ecs_entity], 1u);\n"
            ));
        }
        Instr::StoreRender { src, field } => {
            let field = match field {
                crate::ir::RenderField::Transform => "transform",
                crate::ir::RenderField::Color => "color",
            };
            out.push_str(&format!(
                "    renderInstances[ecs_slot].{field} = v{src};\n"
            ));
        }
        Instr::CallBuiltin { dest, func, args } => {
            if func.is_structural() {
                let kind = match func {
                    BuiltinFunc::SpawnEntity => bind_layout::COMMAND_KIND_SPAWN,
                    BuiltinFunc::DeleteEntity => bind_layout::COMMAND_KIND_DELETE,
                    BuiltinFunc::AddComponent => bind_layout::COMMAND_KIND_ADD_COMPONENT,
                    BuiltinFunc::RemoveComponent => bind_layout::COMMAND_KIND_REMOVE_COMPONENT,
                    _ => unreachable!(),
                };
                // Command record layout: { kind, a, b, reserved }.
                //   Spawn:  a = prototype index,      b = 0
                //   Delete: a = entity id,            b = 0
                //   Add:    a = entity id,            b = component id
                //   Remove: a = entity id,            b = component id
                let a = args.first().copied().ok_or_else(|| {
                    EcsError::WgslInvalid("structural builtin without operands".into())
                })?;
                let b = match func {
                    BuiltinFunc::SpawnEntity | BuiltinFunc::DeleteEntity => None,
                    BuiltinFunc::AddComponent | BuiltinFunc::RemoveComponent => Some(args[1]),
                    _ => unreachable!(),
                };
                let b_expr = match b {
                    Some(slot) => format!("v{slot}"),
                    None => "0u".to_string(),
                };
                out.push_str("    {\n");
                out.push_str("        let ecs_ci = atomicAdd(&commandCount, 1u);\n");
                out.push_str("        if (ecs_ci < arrayLength(&commandBuffer)) {\n");
                out.push_str(&format!(
                    "            commandBuffer[ecs_ci] = StructuralCommand({kind}u, v{a}, {b_expr}, 0u);\n"
                ));
                out.push_str("        }\n");
                out.push_str("    }\n");
            } else {
                let call = match func {
                    BuiltinFunc::Sin => format!("sin(v{})", args[0]),
                    BuiltinFunc::Cos => format!("cos(v{})", args[0]),
                    BuiltinFunc::Normalize => format!("normalize(v{})", args[0]),
                    BuiltinFunc::Length => format!("length(v{})", args[0]),
                    BuiltinFunc::Dot => format!("dot(v{}, v{})", args[0], args[1]),
                    BuiltinFunc::Cross => format!("cross(v{}, v{})", args[0], args[1]),
                    _ => unreachable!("structural handled above"),
                };
                out.push_str(&format!("    v{dest} = {call};\n"));
            }
        }
        Instr::AtomicOp { component_id, op, value } => {
            let fn_name = match op {
                AtomicOpCode::Add => "atomicAdd",
                AtomicOpCode::Sub => "atomicSub",
                AtomicOpCode::Exchange => "atomicExchange",
                AtomicOpCode::CompareExchange => {
                    return Err(EcsError::WgslInvalid(format!(
                        "AtomicOp CompareExchange needs two value operands and is not supported in v1 (system '{}')",
                        ctx.ir.systems.get(ctx.system_id as usize).map(|s| s.name.as_str()).unwrap_or("?")
                    )));
                }
            };
            out.push_str(&format!(
                "    {fn_name}(&ecs_c{component_id}[ecs_entity], v{value});\n"
            ));
        }
        Instr::If { .. } | Instr::Jump { .. } => {
            return Err(EcsError::WgslInvalid(
                "control flow emission lands in M3; this build only emits straight-line bodies"
                    .into(),
            ));
        }
        Instr::Return => {
            out.push_str("    return;\n");
        }
    }
    Ok(out)
}

/// Translate a little-endian byte literal into a WGSL expression.
pub(super) fn const_literal(ty: ComponentType, bytes: &[u8]) -> Result<String, EcsError> {
    let need = ty.byte_size();
    if bytes.len() != need {
        return Err(EcsError::WgslInvalid(format!(
            "Const literal is {} bytes, expected {need}",
            bytes.len()
        )));
    }
    let read_f32 = |off: usize| f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
    let read_u32 = |off: usize| u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
    let read_i32 = |off: usize| i32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());

    Ok(match ty {
        ComponentType::F32 => format!("{}f", read_f32(0)),
        ComponentType::Vec2F => format!("vec2f({}f, {}f)", read_f32(0), read_f32(4)),
        ComponentType::Vec3F => format!(
            "vec3f({}f, {}f, {}f)",
            read_f32(0),
            read_f32(4),
            read_f32(8)
        ),
        ComponentType::Vec4F => format!(
            "vec4f({}f, {}f, {}f, {}f)",
            read_f32(0),
            read_f32(4),
            read_f32(8),
            read_f32(12)
        ),
        ComponentType::U32 => format!("{}u", read_u32(0)),
        ComponentType::I32 => format!("{}", read_i32(0)),
        ComponentType::Bool => format!("{}", read_u32(0) != 0),
        ComponentType::Mat4F => {
            return Err(EcsError::WgslInvalid(
                "Const of mat4x4f is not supported".into(),
            ))
        }
    })
}
