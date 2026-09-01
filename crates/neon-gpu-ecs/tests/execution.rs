//! M5 acceptance tests: `run_frame` execution chain, resource upload, and
//! Changed/Added change filtering, headless.

use neon_gpu_ecs::generator::bind_layout;
use neon_gpu_ecs::ir::*;
use neon_gpu_ecs::tests_support::physics_world;
use neon_gpu_ecs::GpuEcsCtx;

const MAX_ENTITIES: u32 = 16;

fn ecs_headless_device(max_storage_buffers: u32) -> (wgpu::Device, wgpu::Queue) {
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
    .expect("M5 headless execution requires a compute adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("neon-gpu-ecs M5 test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits {
            max_storage_buffers_per_shader_stage: max_storage_buffers,
            ..wgpu::Limits::default()
        },
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .expect("the M5 test adapter must create a device and queue")
}

fn read_vec3_f32s(bytes: &[u8]) -> Vec<[f32; 3]> {
    bytes
        .chunks_exact(16)
        .map(|c| {
            [
                f32::from_le_bytes(c[0..4].try_into().unwrap()),
                f32::from_le_bytes(c[4..8].try_into().unwrap()),
                f32::from_le_bytes(c[8..12].try_into().unwrap()),
            ]
        })
        .collect()
}

// ------------------------------------------------------- integration --------

/// Physics world with known seeds: pos = (0,0,0), vel = (1,0,0). With dt = 1
/// the integrator adds exactly (1,0,0) per frame.
fn seeded_physics_world() -> EcsIr {
    let mut ir = physics_world();
    let vel: Vec<u8> = {
        let mut v = Vec::new();
        v.extend_from_slice(&1.0f32.to_le_bytes());
        v.extend_from_slice(&0f32.to_le_bytes());
        v.extend_from_slice(&0f32.to_le_bytes());
        v
    };
    ir.initial_entities[0].initial_values = Some(vec![vec![0u8; 12], vel, vec![0u8; 4]]);
    ir
}

#[test]
fn physics_integration_matches_cpu_reference() {
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, seeded_physics_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();
    ctx.set_resource(0, &1.0f32.to_le_bytes());

    // CPU reference: vel *= dt; pos += vel; with dt = 1 and vel = (1,0,0).
    for frame in 1..=3u32 {
        ctx.run_frame();
        let positions = read_vec3_f32s(&ctx.read_component_data(0));
        for (e, pos) in positions.iter().enumerate().take(10) {
            assert_eq!(pos, &[frame as f32, 0.0, 0.0], "entity {e} after frame {frame}");
        }
    }
    // Velocities stay (1,0,0).
    let velocities = read_vec3_f32s(&ctx.read_component_data(1));
    for (e, vel) in velocities.iter().enumerate().take(10) {
        assert_eq!(vel, &[1.0, 0.0, 0.0], "velocity entity {e}");
    }
}

#[test]
fn resource_upload_changes_integration_rate() {
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, seeded_physics_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();
    ctx.set_resource(0, &0.5f32.to_le_bytes());
    ctx.run_frame();
    ctx.run_frame();
    let positions = read_vec3_f32s(&ctx.read_component_data(0));
    // The integrator rescales velocity each frame: vel *= dt, pos += vel.
    // dt = 0.5, vel0 = (1,0,0): frame 1 -> vel = 0.5, pos = 0.5;
    // frame 2 -> vel = 0.25, pos = 0.75.
    assert_eq!(positions[0], [0.75, 0.0, 0.0]);
}

// --------------------------------------------- changed filtering ------------

fn comp(id: u32, name: &str, ty: ComponentType) -> ComponentDef {
    ComponentDef {
        id,
        name: name.into(),
        ty,
        default_value: vec![0u8; ty.byte_size()],
    }
}

/// Heater/detector world:
/// - Health starts at 0; `heater` adds 1 while health < 3 (uses If control
///   flow, exercising fallthrough at runtime).
/// - `detector` runs on Changed(Health) and bumps a Marker counter.
/// - Marker must increment on frames 2..4 (writes detected) and freeze from
///   frame 5 on (heater condition false -> no writes -> Changed empty).
fn heater_detector_world() -> EcsIr {
    EcsIr {
        version: 1,
        components: vec![comp(0, "Health", ComponentType::F32), comp(1, "Marker", ComponentType::F32)],
        resources: vec![],
        initial_entities: vec![EntityPrototype {
            component_ids: vec![0, 1],
            count: 4,
            initial_values: None,
        }],
        queries: vec![
            QueryDef {
                id: 0,
                with: vec![ComponentAccess { component_id: 0, access_type: AccessType::ReadWrite }],
                without: vec![],
                filters: vec![],
            },
            QueryDef {
                id: 1,
                with: vec![
                    ComponentAccess { component_id: 0, access_type: AccessType::Read },
                    ComponentAccess { component_id: 1, access_type: AccessType::ReadWrite },
                ],
                without: vec![],
                filters: vec![QueryFilter::Changed(0)],
            },
        ],
        systems: vec![
            SystemDef {
                id: 0,
                name: "heater".into(),
                query_id: 0,
                resource_refs: vec![],
                local_var_count: 3,
                instructions: vec![
                    Instr::Load { dest: 0, component_id: 0, access: AccessType::ReadWrite },
                    Instr::Const { dest: 1, ty: ComponentType::F32, bytes: 3.0f32.to_le_bytes().to_vec() },
                    Instr::Compare { dest: 2, lhs: 0, rhs: 1, cond: CompareOp::Less },
                    Instr::If { cond: 2, true_block: 4, false_block: 7 },
                    Instr::Const { dest: 1, ty: ComponentType::F32, bytes: 1.0f32.to_le_bytes().to_vec() },
                    Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },
                    Instr::Store { src: 0, component_id: 0 },
                    Instr::Return,
                ],
            },
            SystemDef {
                id: 1,
                name: "detector".into(),
                query_id: 1,
                resource_refs: vec![],
                local_var_count: 2,
                instructions: vec![
                    Instr::Load { dest: 0, component_id: 1, access: AccessType::ReadWrite },
                    Instr::Const { dest: 1, ty: ComponentType::F32, bytes: 1.0f32.to_le_bytes().to_vec() },
                    Instr::BinaryOp { dest: 0, lhs: 0, rhs: 1, op: BinaryOpCode::Add },
                    Instr::Store { src: 0, component_id: 1 },
                    Instr::Return,
                ],
            },
        ],
        schedule: ScheduleDef {
            stages: vec![
                Stage { id: 0, name: "Heat".into(), system_ids: vec![0] },
                Stage { id: 1, name: "Detect".into(), system_ids: vec![1] },
            ],
        },
    }
}

fn read_f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

#[test]
fn changed_filter_detects_writes_and_freezes_when_idle() {
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, heater_detector_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();

    // Frame 1: baseline == current at seed -> Changed matches nothing;
    // detector is idle; heater writes (0 < 3).
    ctx.run_frame();
    let prep = ctx.read_frame_prep();
    assert_eq!(prep[1].count, 0, "frame 1 Changed must match nothing");
    assert_eq!(read_f32s(&ctx.read_component_data(1))[0], 0.0);
    assert_eq!(read_f32s(&ctx.read_component_data(0))[0], 1.0);

    // Frames 2..4: heater wrote on the previous frame -> Changed matches all
    // 4 entities; detector bumps Marker each frame.
    for frame in 2..=4u32 {
        ctx.run_frame();
        let prep = ctx.read_frame_prep();
        assert_eq!(
            prep[1].count, 4,
            "Changed must match all entities while heater writes (frame {frame})"
        );
        let markers = read_f32s(&ctx.read_component_data(1));
        assert_eq!(markers[0], (frame - 1) as f32);
    }

    // Frame 5+: health reached 3 -> heater stops writing -> Changed empty ->
    // Marker freezes.
    ctx.run_frame();
    let prep = ctx.read_frame_prep();
    assert_eq!(prep[1].count, 0, "Changed must freeze once writes stop");
    ctx.run_frame();
    let markers = read_f32s(&ctx.read_component_data(1));
    assert_eq!(markers[0], 3.0, "Marker must stay frozen at 3");
    // Health stays at 3 as well.
    assert_eq!(read_f32s(&ctx.read_component_data(0))[0], 3.0);
}

// -------------------------------------------------- added filtering ---------

#[test]
fn added_filter_matches_zero_baseline_only() {
    // Added(c) requires baseline == 0 and current != 0. Seed normally writes
    // baseline == current == 1, so we overwrite one component's version
    // buffers directly to simulate an AddComponent that happened after the
    // previous frame's sorting point (full structural replay lands in M6).
    let mut ir = heater_detector_world();
    ir.queries[1].filters = vec![QueryFilter::Added(0)];
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, ir, MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();

    // Give entity 2 a fresh Health: current version 1, baseline version 0.
    let mut current = vec![0u32; MAX_ENTITIES as usize];
    let mut baseline = vec![0u32; MAX_ENTITIES as usize];
    for e in 0..4usize {
        current[e] = 1;
        baseline[e] = 1;
    }
    baseline[2] = 0;
    let to_bytes = |words: &[u32]| {
        let mut out = Vec::with_capacity(words.len() * 4);
        for w in words {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    };
    ctx.queue.write_buffer(&ctx.component_buffers[0].1, 0, &to_bytes(&current));
    ctx.queue.write_buffer(&ctx.component_buffers[0].2, 0, &to_bytes(&baseline));

    // Run sorting only and check the Added query (query 1) matches exactly
    // entity 2: its baseline version is zero and current is non-zero.
    ctx.run_sort();
    let prep = ctx.read_frame_prep();
    assert_eq!(prep[1].count, 1);
    let ids = ctx.read_compacted_ids();
    let start = prep[1].start as usize;
    assert_eq!(ids[start], 2);
}

// ------------------------------------------- empty dispatch is safe -----------

#[test]
fn zero_count_dispatch_is_a_noop() {
    // Detector-only run: seed Marker queries can never match because the
    // heater world's Changed query is empty on frame 1; the indirect dispatch
    // with 0 workgroups must be harmless.
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, heater_detector_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();
    ctx.run_frame();
    let args = ctx.read_indirect_args();
    assert_eq!(args[1], [0, 1, 1], "empty query must dispatch zero workgroups");
}

// ------------------------------------------- version bump bookkeeping --------

#[test]
fn stores_bump_component_versions() {
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, heater_detector_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();

    assert_eq!(ctx.read_component_versions(0)[0], 1, "seeded version is 1");
    ctx.run_frame();
    // Frame 1: sort saw baseline==current; snapshot copied 1 to baseline;
    // heater stored once -> current == 2.
    assert_eq!(ctx.read_component_versions(0)[0], 2);
    ctx.run_frame();
    assert_eq!(ctx.read_component_versions(0)[0], 3);
}

// ------------------------------------------- render data stays untouched ------

#[test]
fn render_instance_buffer_defaults_before_any_render_system() {
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, seeded_physics_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();
    ctx.run_frame();
    // No RenderData query/system in this world: buffer must stay zero.
    let bytes = ctx.read_buffer_blocking(&ctx.render_instances, 32);
    let instance = bytemuck::from_bytes::<bind_layout::RenderInstance>(&bytes[0..32]);
    assert_eq!(instance.transform, [0.0; 4]);
    assert_eq!(instance.color, [0.0; 4]);
}
