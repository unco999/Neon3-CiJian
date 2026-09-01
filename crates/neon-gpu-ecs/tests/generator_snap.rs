//! M2 acceptance tests: straight-line TAC→WGSL generation, bind layout text
//! and sorting kernel emission. WGSL legality is verified by compiling the
//! generated module with `create_shader_module` on a headless device.

use neon_gpu_ecs::generator::{check_limits, check_schedule_conflicts, generate_wgsl};
use neon_gpu_ecs::ir::*;
use neon_gpu_ecs::tests_support::physics_world;
use neon_gpu_ecs::EcsError;

/// Headless compute device following the `ai_test_device` pattern of
/// neon-wgpu-runtime. Default limits suffice here because
/// `create_shader_module` does not enforce per-stage binding counts.
fn ecs_test_device() -> (wgpu::Device, wgpu::Queue) {
    let backends = if cfg!(target_os = "windows") {
        wgpu::Backends::VULKAN
    } else {
        wgpu::Backends::PRIMARY
    };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("M2 WGSL compilation requires a compute adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("neon-gpu-ecs M2 test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .expect("the M2 test adapter must create a device and queue")
}

#[test]
fn physics_world_generates_wgsl() {
    let wgsl = generate_wgsl(&physics_world()).expect("physics world must generate WGSL");
    assert!(wgsl.contains("struct QueryRange"));
    assert!(wgsl.contains("struct StructuralCommand"));
    assert!(wgsl.contains("struct RenderInstance"));
}

#[test]
fn group0_bindings_follow_the_layout_contract() {
    let wgsl = generate_wgsl(&physics_world()).unwrap();
    // Fixed group 0 slots.
    assert!(wgsl.contains("@group(0) @binding(0) var<storage, read_write> entityActive"));
    assert!(wgsl.contains("@group(0) @binding(1) var<storage, read_write> queryCounts : array<atomic<u32>>"));
    assert!(wgsl.contains("@group(0) @binding(3) var<storage, read_write> framePrepBuffer : array<QueryRange>"));
    assert!(wgsl.contains("@group(0) @binding(5) var<storage, read_write> indirectArgs : array<vec3u>"));
    assert!(wgsl.contains("@group(0) @binding(7) var<storage, read_write> commandCount : atomic<u32>"));
    // Three bindings per component: data / version / baseline, slots 8..16.
    assert!(wgsl.contains("@group(0) @binding(8) var<storage, read_write> ecs_c0 : array<vec3f>"));
    assert!(wgsl.contains("@group(0) @binding(9) var<storage, read_write> ecs_cv0 : array<atomic<u32>>"));
    assert!(wgsl.contains("@group(0) @binding(10) var<storage, read_write> ecs_cb0 : array<atomic<u32>>"));
    assert!(wgsl.contains("@group(0) @binding(11) var<storage, read_write> ecs_c1 : array<vec3f>"));
    assert!(wgsl.contains("@group(0) @binding(14) var<storage, read_write> ecs_c2 : array<f32>"));
    // Group 1: DeltaTime uniform + render instance buffer.
    assert!(wgsl.contains("@group(1) @binding(0) var<uniform> ecs_r0 : f32"));
    assert!(wgsl.contains("@group(1) @binding(30) var<storage, read_write> renderInstances"));
}

#[test]
fn sorting_entry_points_are_emitted() {
    let wgsl = generate_wgsl(&physics_world()).unwrap();
    assert!(wgsl.contains("fn system_prep_count("));
    assert!(wgsl.contains("fn system_prep_scan()"));
    assert!(wgsl.contains("fn system_prep_fill("));
    assert!(wgsl.contains("fn ecs_pass_0("));
    // Count bumps per-query atomics.
    assert!(wgsl.contains("atomicAdd(&queryCounts[0u], 1u)"));
    // Scan publishes ranges and derives indirect args.
    assert!(wgsl.contains("framePrepBuffer[ecs_q] = QueryRange(ecs_total, ecs_c)"));
    assert!(wgsl.contains("indirectArgs[ecs_q] = vec3u((ecs_c + 63u) / 64u, 1u, 1u)"));
    // Fill scatters through cursors.
    assert!(wgsl.contains("atomicAdd(&queryCursors[0u], 1u)"));
    assert!(wgsl.contains("compactedEntityIds[ecs_slot] = ecs_e"));
}

#[test]
fn system_body_follows_the_compaction_contract() {
    let wgsl = generate_wgsl(&physics_world()).unwrap();
    assert!(wgsl.contains("fn system_physics_update("));
    assert!(wgsl.contains("let ecs_cmd = framePrepBuffer[0u]"));
    assert!(wgsl.contains("if (ecs_index >= ecs_cmd.count) { return; }"));
    assert!(wgsl.contains("let ecs_entity = compactedEntityIds[ecs_cmd.start + ecs_index]"));
    // Straight-line body: loads, dt multiply, add, stores with version bumps.
    assert!(wgsl.contains("v0 = ecs_c0[ecs_entity]"));
    assert!(wgsl.contains("v2 = ecs_r0"));
    assert!(wgsl.contains("v1 = v1 * v2"));
    assert!(wgsl.contains("v0 = v0 + v1"));
    assert!(wgsl.contains("ecs_c0[ecs_entity] = v0"));
    assert!(wgsl.contains("atomicAdd(&ecs_cv0[ecs_entity], 1u)"));
}

#[test]
fn atomic_components_use_atomic_accessors() {
    let mut ir = physics_world();
    ir.components[2] = ComponentDef {
        id: 2,
        name: "Health".into(),
        ty: ComponentType::U32,
        default_value: vec![0; 4],
    };
    ir.queries[0].with.push(ComponentAccess { component_id: 2, access_type: AccessType::ReadWrite });
    // health += 1 via Load/BinaryOp/Store on the U32 component.
    ir.systems[0].instructions.splice(5..5, [
        Instr::Load { dest: 4, component_id: 2, access: AccessType::ReadWrite },
        Instr::Const { dest: 5, ty: ComponentType::U32, bytes: 1u32.to_le_bytes().to_vec() },
        Instr::BinaryOp { dest: 4, lhs: 4, rhs: 5, op: BinaryOpCode::Add },
        Instr::Store { src: 4, component_id: 2 },
    ]);
    ir.systems[0].local_var_count = 6;
    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(wgsl.contains("ecs_c2 : array<atomic<u32>>"));
    assert!(wgsl.contains("v4 = atomicLoad(&ecs_c2[ecs_entity])"));
    assert!(wgsl.contains("atomicStore(&ecs_c2[ecs_entity], v4)"));
}

#[test]
fn bool_components_use_select_for_stores() {
    let mut ir = physics_world();
    ir.components.push(ComponentDef {
        id: 3,
        name: "Alive".into(),
        ty: ComponentType::Bool,
        default_value: vec![1, 0, 0, 0],
    });
    ir.queries[0].with.push(ComponentAccess { component_id: 3, access_type: AccessType::ReadWrite });
    ir.systems.push(SystemDef {
        id: 1,
        name: "kill".into(),
        query_id: 0,
        resource_refs: vec![],
        local_var_count: 2,
        instructions: vec![
            Instr::Load { dest: 0, component_id: 3, access: AccessType::ReadWrite },
            Instr::UnaryOp { dest: 1, src: 0, op: UnaryOpCode::Not },
            Instr::Store { src: 1, component_id: 3 },
            Instr::Return,
        ],
    });
    ir.schedule.stages[0].system_ids.push(1);
    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(wgsl.contains("ecs_c3 : array<u32>"));
    assert!(wgsl.contains("v0 = ecs_c3[ecs_entity] != 0u"));
    assert!(wgsl.contains("ecs_c3[ecs_entity] = select(0u, 1u, v1)"));
}

#[test]
fn generated_wgsl_compiles_on_a_headless_device() {
    let (device, _queue) = ecs_test_device();
    let wgsl = generate_wgsl(&physics_world()).unwrap();
    // Naga validation happens inside create_shader_module.
    let _module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("neon-gpu-ecs M2 module"),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
}

#[test]
fn schedule_conflict_is_rejected_before_generation() {
    let mut ir = physics_world();
    ir.systems.push(SystemDef {
        id: 1,
        name: "drag".into(),
        query_id: 0,
        resource_refs: vec![],
        local_var_count: 1,
        instructions: vec![
            Instr::Load { dest: 0, component_id: 1, access: AccessType::ReadWrite },
            Instr::Store { src: 0, component_id: 1 },
            Instr::Return,
        ],
    });
    ir.schedule.stages[0].system_ids.push(1);
    let err = check_schedule_conflicts(&ir).unwrap_err();
    match err {
        EcsError::ScheduleConflict(message) => {
            assert!(message.contains("both write component 1"), "{message}");
        }
        other => panic!("expected ScheduleConflict, got {other:?}"),
    }
    // generate_wgsl must fail too (it re-runs IR validation + conflicts).
    assert!(matches!(
        generate_wgsl(&ir),
        Err(EcsError::IrInvalid(_)) | Err(EcsError::ScheduleConflict(_))
    ));
}

#[test]
fn limits_check_reports_required_storage_bindings() {
    let ir = physics_world();
    // 8 fixed slots + 3 components * 3 buffers = 17.
    let err = check_limits(&ir, 8).unwrap_err();
    match err {
        EcsError::Limits(message) => {
            assert!(message.contains("17"), "{message}");
        }
        other => panic!("expected Limits, got {other:?}"),
    }
    check_limits(&ir, 17).expect("17 storage bindings must fit a 17-slot device");
}

#[test]
fn mixed_type_binary_op_is_rejected() {
    let mut ir = physics_world();
    // v0 = Transform(vec3f), v2 = DeltaTime(f32); adding them must fail.
    ir.systems[0].instructions = vec![
        Instr::Load { dest: 0, component_id: 0, access: AccessType::ReadWrite },
        Instr::Load { dest: 1, component_id: 1, access: AccessType::ReadWrite },
        Instr::LoadResource { dest: 2, resource_id: 0 },
        Instr::BinaryOp { dest: 0, lhs: 0, rhs: 2, op: BinaryOpCode::Add },
        Instr::Return,
    ];
    match generate_wgsl(&ir) {
        Err(EcsError::WgslInvalid(message)) => {
            assert!(message.contains("mixes vec3f and f32"), "{message}");
        }
        other => panic!("expected WgslInvalid, got {other:?}"),
    }
}

#[test]
fn unassigned_local_read_is_rejected() {
    let mut ir = physics_world();
    ir.systems[0].instructions = vec![
        Instr::BinaryOp { dest: 0, lhs: 1, rhs: 2, op: BinaryOpCode::Add },
        Instr::Return,
    ];
    match generate_wgsl(&ir) {
        Err(EcsError::WgslInvalid(message)) => {
            assert!(message.contains("reads v1 before any assignment"), "{message}");
        }
        other => panic!("expected WgslInvalid, got {other:?}"),
    }
}
