//! M1 acceptance tests: IR serialization roundtrip, type/byte validation and
//! rejection of invalid references. Pure CPU, no GPU required.

use neon_gpu_ecs::ir::*;
use neon_gpu_ecs::EcsError;

fn comp(id: u32, name: &str, ty: ComponentType) -> ComponentDef {
    ComponentDef {
        id,
        name: name.to_string(),
        ty,
        default_value: vec![0u8; ty.byte_size()],
    }
}

/// Base world: Transform(Vec3F) + Velocity(Vec3F) + Health(F32), one DeltaTime
/// resource, one physics system in one "logic" stage, 10 prototype entities.
fn base_ir() -> EcsIr {
    EcsIr {
        version: 1,
        components: vec![
            comp(0, "Transform", ComponentType::Vec3F),
            comp(1, "Velocity", ComponentType::Vec3F),
            comp(2, "Health", ComponentType::F32),
        ],
        resources: vec![ResourceDef {
            id: 0,
            name: "DeltaTime".to_string(),
            ty: ComponentType::F32,
            binding_slot: 0,
            default_value: (1.0f32 / 60.0).to_le_bytes().to_vec(),
        }],
        initial_entities: vec![EntityPrototype {
            component_ids: vec![0, 1, 2],
            count: 10,
            initial_values: None,
        }],
        queries: vec![QueryDef {
            id: 0,
            with: vec![
                ComponentAccess { component_id: 0, access_type: AccessType::ReadWrite },
                ComponentAccess { component_id: 1, access_type: AccessType::ReadWrite },
            ],
            without: vec![],
            filters: vec![],
        }],
        systems: vec![SystemDef {
            id: 0,
            name: "physics_update".to_string(),
            query_id: 0,
            resource_refs: vec![ResourceRef { resource_id: 0, access_type: AccessType::Read }],
            local_var_count: 3,
            instructions: vec![
                Instr::Load { dest: 0, component_id: 0, access: AccessType::ReadWrite },
                Instr::Load { dest: 1, component_id: 1, access: AccessType::ReadWrite },
                Instr::LoadResource { dest: 2, resource_id: 0 },
                Instr::BinaryOp { dest: 1, lhs: 1, rhs: 2, op: BinaryOpCode::Mul },
                Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },
                Instr::Store { src: 0, component_id: 0 },
                Instr::Store { src: 1, component_id: 1 },
                Instr::Return,
            ],
        }],
        schedule: ScheduleDef {
            stages: vec![Stage { id: 0, name: "Logic".to_string(), system_ids: vec![0] }],
        },
    }
}

fn problems(err: &EcsError) -> &str {
    match err {
        EcsError::IrInvalid(message) => message,
        other => panic!("expected IrInvalid, got {other:?}"),
    }
}

#[test]
fn base_world_is_valid() {
    base_ir().validate().expect("base world must validate");
}

#[test]
fn serde_roundtrip_preserves_world() {
    let ir = base_ir();
    let json = serde_json::to_string(&ir).expect("serialize");
    let back: EcsIr = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(ir, back);
    back.validate().expect("roundtripped world must validate");
}

#[test]
fn wrong_version_is_rejected() {
    let mut ir = base_ir();
    ir.version = 7;
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("version"), "{err}");
}

#[test]
fn component_default_value_byte_count_must_match_type() {
    let mut ir = base_ir();
    ir.components[2].default_value = vec![1, 2, 3]; // F32 needs 4 bytes
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("default_value is 3 bytes, expected 4"), "{err}");
}

#[test]
fn component_id_must_match_index() {
    let mut ir = base_ir();
    ir.components[1].id = 9;
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("does not match its index"), "{err}");
}

#[test]
fn query_referencing_unknown_component_is_rejected() {
    let mut ir = base_ir();
    ir.queries[0].with.push(ComponentAccess { component_id: 42, access_type: AccessType::Read });
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("unknown component 42"), "{err}");
}

#[test]
fn query_filter_referencing_unknown_component_is_rejected() {
    let mut ir = base_ir();
    ir.queries[0].filters.push(QueryFilter::Changed(99));
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("unknown component 99"), "{err}");
}

#[test]
fn query_with_and_without_conflict_is_rejected() {
    let mut ir = base_ir();
    ir.queries[0].without.push(0);
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("both with and without"), "{err}");
}

#[test]
fn stage_referencing_unknown_system_is_rejected() {
    let mut ir = base_ir();
    ir.schedule.stages[0].system_ids.push(5);
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("unknown system 5"), "{err}");
}

#[test]
fn unscheduled_system_is_rejected() {
    let mut ir = base_ir();
    // Second system exists but no stage lists it.
    ir.systems.push(SystemDef {
        id: 1,
        name: "orphan".to_string(),
        query_id: 0,
        resource_refs: vec![],
        local_var_count: 1,
        instructions: vec![
            Instr::Load { dest: 0, component_id: 2, access: AccessType::Read },
            Instr::Return,
        ],
    });
    // The orphan system must also be covered by the query for this test to
    // isolate the scheduling rule.
    ir.queries[0].with.push(ComponentAccess { component_id: 2, access_type: AccessType::Read });
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("not referenced by any stage"), "{err}");
}

#[test]
fn same_stage_write_conflict_is_rejected() {
    let mut ir = base_ir();
    // physics_update writes Transform (0) and Velocity (1). Add a second
    // system that also writes Velocity, then place both in the same stage.
    ir.systems.push(SystemDef {
        id: 1,
        name: "drag".to_string(),
        query_id: 0,
        resource_refs: vec![],
        local_var_count: 1,
        instructions: vec![
            Instr::Load { dest: 0, component_id: 1, access: AccessType::ReadWrite },
            Instr::Store { src: 0, component_id: 1 },
            Instr::Return,
        ],
    });
    ir.schedule.stages[0].system_ids = vec![0, 1];
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("both write component 1"), "{err}");
}

#[test]
fn local_slot_out_of_range_is_rejected() {
    let mut ir = base_ir();
    ir.systems[0].instructions[0] =
        Instr::Load { dest: 9, component_id: 0, access: AccessType::ReadWrite };
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("local v9 out of range"), "{err}");
}

#[test]
fn jump_target_out_of_range_is_rejected() {
    let mut ir = base_ir();
    ir.systems[0].instructions.push(Instr::Jump { target: 999 });
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("out of range"), "{err}");
}

#[test]
fn load_resource_must_be_declared_in_resource_refs() {
    let mut ir = base_ir();
    ir.resources.push(ResourceDef {
        id: 1,
        name: "CameraMatrix".to_string(),
        ty: ComponentType::Mat4F,
        binding_slot: 1,
        default_value: vec![0u8; 64],
    });
    // Use resource 1 in the body but forget to declare it in resource_refs.
    ir.systems[0]
        .instructions
        .insert(3, Instr::LoadResource { dest: 2, resource_id: 1 });
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("not declared in resource_refs"), "{err}");
}

#[test]
fn builtin_arity_is_checked() {
    let mut ir = base_ir();
    ir.systems[0].instructions.push(Instr::CallBuiltin {
        dest: 0,
        func: BuiltinFunc::SpawnEntity,
        args: vec![0, 1], // SpawnEntity takes exactly 1 arg
    });
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("expects 1 args, got 2"), "{err}");
}

#[test]
fn prototype_value_lengths_are_checked() {
    let mut ir = base_ir();
    ir.initial_entities[0].initial_values = Some(vec![vec![0u8; 16], vec![0u8; 12], vec![0u8; 4]]);
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("is 16 bytes, expected 12"), "{err}");
}

#[test]
fn duplicate_binding_slot_is_rejected() {
    let mut ir = base_ir();
    ir.resources.push(ResourceDef {
        id: 1,
        name: "CameraMatrix".to_string(),
        ty: ComponentType::Mat4F,
        binding_slot: 0, // collides with DeltaTime
        default_value: vec![0u8; 64],
    });
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("duplicates binding_slot 0"), "{err}");
}

#[test]
fn component_names_must_be_wgsl_idents() {
    let mut ir = base_ir();
    ir.components[0].name = "my transform".to_string();
    let err = ir.validate().unwrap_err();
    assert!(problems(&err).contains("not a valid WGSL identifier"), "{err}");
}
