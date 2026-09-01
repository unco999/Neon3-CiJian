//! Shared test fixtures: small valid worlds used by unit and integration tests.
//!
//! Exposed publicly (not behind a feature) so both `#[cfg(test)]` unit tests
//! and `tests/*.rs` integration tests can build the same worlds.

use crate::ir::*;

/// A minimal physics world: Transform(Vec3F) + Velocity(Vec3F) + Health(F32),
/// one DeltaTime resource, one query, one `physics_update` system in one stage.
pub fn physics_world() -> EcsIr {
    EcsIr {
        version: 1,
        components: vec![
            comp(0, "Transform", ComponentType::Vec3F),
            comp(1, "Velocity", ComponentType::Vec3F),
            comp(2, "Health", ComponentType::F32),
        ],
        resources: vec![ResourceDef {
            id: 0,
            name: "DeltaTime".into(),
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
            name: "physics_update".into(),
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
            stages: vec![Stage { id: 0, name: "Logic".into(), system_ids: vec![0] }],
        },
    }
}

fn comp(id: u32, name: &str, ty: ComponentType) -> ComponentDef {
    ComponentDef {
        id,
        name: name.into(),
        ty,
        default_value: vec![0u8; ty.byte_size()],
    }
}
