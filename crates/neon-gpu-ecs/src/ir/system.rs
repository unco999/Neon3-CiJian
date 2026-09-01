//! System definitions and the TAC (three-address code) instruction set.

use super::query::{AccessType, QueryFilter};
use serde::{Deserialize, Serialize};

/// One GPU-side system: a query plus a TAC instruction body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemDef {
    /// Must equal the system's index inside `EcsIr::systems`.
    pub id: u32,
    pub name: String,
    /// Which entities this system iterates.
    pub query_id: u32,
    /// Fixed resources read/written by this system.
    pub resource_refs: Vec<super::resource::ResourceRef>,
    /// Number of scratch locals the body needs (v0..vN-1).
    pub local_var_count: u32,
    pub instructions: Vec<Instr>,
}

impl SystemDef {
    /// Components written by this system, derived from instruction effects.
    pub fn written_components(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for instr in &self.instructions {
            let component_id = match instr {
                Instr::Store { component_id, .. } | Instr::AtomicOp { component_id, .. } => {
                    *component_id
                }
                _ => continue,
            };
            if !out.contains(&component_id) {
                out.push(component_id);
            }
        }
        out
    }

    /// True when this system writes the shared RenderData instance buffer.
    pub fn writes_render(&self) -> bool {
        self.instructions
            .iter()
            .any(|instr| matches!(instr, Instr::StoreRender { .. }))
    }

    /// True when this system emits structural-change commands.
    pub fn is_structural(&self) -> bool {
        self.instructions.iter().any(|instr| {
            matches!(
                instr,
                Instr::CallBuiltin { func, .. } if func.is_structural()
            )
        })
    }

    /// Query filters of this system's query, resolved against the IR query list.
    pub fn filters<'a>(&self, queries: &'a [super::query::QueryDef]) -> &'a [QueryFilter] {
        queries
            .get(self.query_id as usize)
            .map(|q| q.filters.as_slice())
            .unwrap_or(&[])
    }
}

/// Three-address-code instruction. `dest`/`lhs`/`rhs`/`src` refer to local
/// slots `v0..v{local_var_count}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Instr {
    /// Load a component into a local: `v_dest = comp[entity]`.
    Load { dest: u32, component_id: u32, access: AccessType },
    /// Load an immediate constant: `v_dest = <typed literal>`.
    Const { dest: u32, ty: super::types::ComponentType, bytes: Vec<u8> },
    /// Load the current entity's own ID: `v_dest = ecs_entity`.
    LoadEntityId { dest: u32 },
    /// Store a local back: `comp[entity] = v_src`.
    Store { src: u32, component_id: u32 },
    /// Load a fixed resource: `v_dest = res`.
    LoadResource { dest: u32, resource_id: u32 },
    /// `v_dest = v_lhs op v_rhs`.
    BinaryOp { dest: u32, lhs: u32, rhs: u32, op: BinaryOpCode },
    /// `v_dest = op v_src`.
    UnaryOp { dest: u32, src: u32, op: UnaryOpCode },
    /// `v_dest = (v_lhs cond v_rhs)`; result slot is `Bool`.
    Compare { dest: u32, lhs: u32, rhs: u32, cond: CompareOp },
    /// Conditional branch over the flat instruction list.
    If { cond: u32, true_block: u32, false_block: u32 },
    /// Unconditional branch over the flat instruction list.
    Jump { target: u32 },
    /// End this entity's thread.
    Return,
    /// Built-in call; result goes to `v_dest` unless the function is a
    /// structural-change call (`SpawnEntity` etc.), whose result is discarded
    /// or stored in an `U32` local.
    CallBuiltin { dest: u32, func: BuiltinFunc, args: Vec<u32> },
    /// Atomic RMW on a component slot: `atomicAdd(&comp[entity], v_value)`.
    AtomicOp { component_id: u32, op: AtomicOpCode, value: u32 },
    /// Write a `vec4f` local into the RenderData instance buffer of the
    /// current entity slot: `renderInstances[idx].field = v_src`.
    StoreRender { src: u32, field: RenderField },
}

/// Fields of the fixed RenderData instance layout
/// (`struct RenderInstance { transform: vec4f, color: vec4f }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderField {
    /// `vec4f`: world position in xyz, w free (e.g. scale or health).
    Transform,
    /// `vec4f`: RGBA color.
    Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOpCode {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOpCode {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareOp {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuiltinFunc {
    // Math builtins.
    Sin,
    Cos,
    Normalize,
    Length,
    Dot,
    Cross,
    // Structural-change builtins: append a command to the GPU command buffer.
    SpawnEntity,
    DeleteEntity,
    AddComponent,
    RemoveComponent,
}

impl BuiltinFunc {
    /// True for structural-change calls routed to the command buffer.
    pub fn is_structural(self) -> bool {
        matches!(
            self,
            BuiltinFunc::SpawnEntity
                | BuiltinFunc::DeleteEntity
                | BuiltinFunc::AddComponent
                | BuiltinFunc::RemoveComponent
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtomicOpCode {
    Add,
    Sub,
    Exchange,
    CompareExchange,
}
