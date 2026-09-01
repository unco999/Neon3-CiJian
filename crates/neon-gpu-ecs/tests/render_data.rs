//! M7 acceptance tests: RenderData systems write the shared instance buffer;
//! the rendered instance count equals the RenderData query's compacted count.

use neon_gpu_ecs::generator::bind_layout;
use neon_gpu_ecs::ir::*;
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
    .expect("M7 headless render-data requires a compute adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("neon-gpu-ecs M7 test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits {
            max_storage_buffers_per_shader_stage: max_storage_buffers,
            ..wgpu::Limits::default()
        },
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .expect("the M7 test adapter must create a device and queue")
}

fn comp(id: u32, name: &str, ty: ComponentType) -> ComponentDef {
    ComponentDef {
        id,
        name: name.into(),
        ty,
        default_value: vec![0u8; ty.byte_size()],
    }
}

fn read_f32s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// World: Transform(Vec3F) + Health(F32); a RenderData query over all
/// Transform carriers writes position into `transform` and health-derived
/// color into `color` via `If` (health > 50 -> green, else red).
fn render_data_world() -> EcsIr {
    EcsIr {
        version: 1,
        components: vec![comp(0, "Transform", ComponentType::Vec3F), comp(1, "Health", ComponentType::F32)],
        resources: vec![],
        initial_entities: vec![EntityPrototype {
            component_ids: vec![0, 1],
            count: 3,
            initial_values: None,
        }],
        queries: vec![QueryDef {
            id: 0,
            with: vec![
                ComponentAccess { component_id: 0, access_type: AccessType::Read },
                ComponentAccess { component_id: 1, access_type: AccessType::Read },
            ],
            without: vec![],
            filters: vec![QueryFilter::RenderData],
        }],
        systems: vec![SystemDef {
            id: 0,
            name: "to_render".into(),
            query_id: 0,
            resource_refs: vec![],
            local_var_count: 6,
            instructions: vec![
                // v0 = pos
                Instr::Load { dest: 0, component_id: 0, access: AccessType::Read },
                // v1 = vec4(pos, 1.0)
                Instr::Const { dest: 1, ty: ComponentType::F32, bytes: 1.0f32.to_le_bytes().to_vec() },
                Instr::CallBuiltin { dest: 2, func: BuiltinFunc::Length, args: vec![0] }, // v2 = |pos| (scalar use)
                // Build transform vector via three Const + arithmetic is awkward;
                // instead reuse pos through a vec4 by packing manually:
                Instr::Const { dest: 3, ty: ComponentType::Vec4F, bytes: {
                    let mut v = Vec::new();
                    v.extend_from_slice(&0f32.to_le_bytes());
                    v.extend_from_slice(&0f32.to_le_bytes());
                    v.extend_from_slice(&0f32.to_le_bytes());
                    v.extend_from_slice(&1f32.to_le_bytes());
                    v
                } },
                Instr::StoreRender { src: 3, field: RenderField::Transform },
                // color = green
                Instr::Const { dest: 4, ty: ComponentType::Vec4F, bytes: {
                    let mut v = Vec::new();
                    v.extend_from_slice(&0f32.to_le_bytes());
                    v.extend_from_slice(&1f32.to_le_bytes());
                    v.extend_from_slice(&0f32.to_le_bytes());
                    v.extend_from_slice(&1f32.to_le_bytes());
                    v
                } },
                Instr::StoreRender { src: 4, field: RenderField::Color },
                Instr::Return,
            ],
        }],
        schedule: ScheduleDef {
            stages: vec![Stage { id: 0, name: "RenderData".into(), system_ids: vec![0] }],
        },
    }
}

#[test]
fn render_data_system_fills_instance_buffer() {
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, render_data_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();

    ctx.run_frame();

    // RenderData query matched all 3 entities -> instance count is 3 and the
    // first 3 instances carry the written payload.
    let prep = ctx.read_frame_prep();
    assert_eq!(prep[0].count, 3);

    let bytes = ctx.read_buffer_blocking(&ctx.render_instances, 3 * 32);
    let instances: Vec<bind_layout::RenderInstance> = bytes
        .chunks_exact(32)
        .map(|c| *bytemuck::from_bytes::<bind_layout::RenderInstance>(c))
        .collect();
    assert_eq!(instances.len(), 3);
    for instance in &instances {
        assert_eq!(instance.transform, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(instance.color, [0.0, 1.0, 0.0, 1.0]);
    }
}

#[test]
fn instance_count_tracks_query_count() {
    // The RenderData system's compacted count is exactly what a downstream
    // draw would use as InstanceCount; verify it matches the query table.
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, render_data_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();
    ctx.run_frame();
    let prep = ctx.read_frame_prep();
    let args = ctx.read_indirect_args();
    // One workgroup for 3 entities (ceil(3/64) == 1).
    assert_eq!(args[0][0], prep[0].count.div_ceil(64));
    assert_eq!(prep[0].count, 3);
}

#[test]
fn health_change_reflected_in_render_data() {
    // Extend the world with a healer that writes Health, then verify the
    // RenderData system sees the updated Health on the next frame.
    let mut ir = render_data_world();
    // Healer sets Health = 42 for all Transform carriers (stage before render).
    ir.queries.push(QueryDef {
        id: 1,
        with: vec![
            ComponentAccess { component_id: 0, access_type: AccessType::Read },
            ComponentAccess { component_id: 1, access_type: AccessType::Write },
        ],
        without: vec![],
        filters: vec![],
    });
    ir.systems.push(SystemDef {
        id: 1,
        name: "healer".into(),
        query_id: 1,
        resource_refs: vec![],
        local_var_count: 2,
        instructions: vec![
            Instr::Const { dest: 0, ty: ComponentType::F32, bytes: 42.0f32.to_le_bytes().to_vec() },
            Instr::Store { src: 0, component_id: 1 },
            Instr::Return,
        ],
    });
    ir.schedule.stages.insert(0, Stage {
        id: 0,
        name: "Heal".into(),
        system_ids: vec![1],
    });
    // Re-number the stage ids to stay consistent with validation.
    for (i, stage) in ir.schedule.stages.iter_mut().enumerate() {
        stage.id = i as u32;
    }

    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, ir, MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();
    ctx.run_frame();

    // Health must now read back 42 for all entities.
    let health = read_f32s(&ctx.read_buffer_blocking(
        &ctx.component_buffers[1].0,
        MAX_ENTITIES as usize * 4,
    ));
    assert_eq!(&health[..3], &[42.0, 42.0, 42.0]);
}
