//! M6 acceptance tests: structural changes (Spawn/Delete) through the GPU
//! command ring, ping-pong readback and CPU replay, headless.

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
    .expect("M6 headless structural replay requires a compute adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("neon-gpu-ecs M6 test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits {
            max_storage_buffers_per_shader_stage: max_storage_buffers,
            ..wgpu::Limits::default()
        },
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .expect("the M6 test adapter must create a device and queue")
}

fn comp(id: u32, name: &str, ty: ComponentType) -> ComponentDef {
    ComponentDef {
        id,
        name: name.into(),
        ty,
        default_value: vec![0u8; ty.byte_size()],
    }
}

fn read_u32s(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// World: prototype 0 seeds 2 entities with Health+Seed (ids 0,1); prototype
/// 1 seeds 1 entity with Health only (id 2) and doubles as the spawn target.
/// The spawner system appends one SpawnEntity(1) command per seeded entity
/// that carries Seed.
fn spawn_world() -> EcsIr {
    EcsIr {
        version: 1,
        components: vec![comp(0, "Health", ComponentType::F32), comp(1, "Seed", ComponentType::U32)],
        resources: vec![],
        initial_entities: vec![
            EntityPrototype { component_ids: vec![0, 1], count: 2, initial_values: None },
            EntityPrototype { component_ids: vec![0], count: 1, initial_values: None },
        ],
        queries: vec![
            QueryDef {
                id: 0,
                with: vec![ComponentAccess { component_id: 1, access_type: AccessType::Read }],
                without: vec![],
                filters: vec![],
            },
            QueryDef {
                id: 1,
                with: vec![ComponentAccess { component_id: 0, access_type: AccessType::Read }],
                without: vec![],
                filters: vec![],
            },
        ],
        systems: vec![SystemDef {
            id: 0,
            name: "spawner".into(),
            query_id: 0,
            resource_refs: vec![],
            local_var_count: 2,
            instructions: vec![
                Instr::Const { dest: 0, ty: ComponentType::U32, bytes: 1u32.to_le_bytes().to_vec() },
                Instr::CallBuiltin { dest: 1, func: BuiltinFunc::SpawnEntity, args: vec![0] },
                Instr::Return,
            ],
        }],
        schedule: ScheduleDef {
            stages: vec![Stage { id: 0, name: "Spawn".into(), system_ids: vec![0] }],
        },
    }
}

#[test]
fn spawn_commands_activate_entities_on_replay() {
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, spawn_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();

    // Seeding activates entities 0,1 (proto 0) and 2 (proto 1).
    let active = read_u32s(&ctx.read_buffer_blocking(&ctx.entity_active, MAX_ENTITIES as usize * 4));
    assert_eq!(&active[..5], &[1, 1, 1, 0, 0]);

    // Frame 1: spawner runs on the 2 Seed entities and appends 2 spawn
    // commands to ring 0. No replay yet (first frame).
    ctx.run_frame();
    let active = read_u32s(&ctx.read_buffer_blocking(&ctx.entity_active, MAX_ENTITIES as usize * 4));
    assert_eq!(&active[..5], &[1, 1, 1, 0, 0]);

    // Frame 2 start replays ring 0: two new entities take slots 3 and 4
    // (lowest free slots), activate, and get Health at version 1.
    ctx.run_frame();
    let active = read_u32s(&ctx.read_buffer_blocking(&ctx.entity_active, MAX_ENTITIES as usize * 4));
    assert_eq!(&active[..6], &[1, 1, 1, 1, 1, 0]);

    let health_versions = ctx.read_component_versions(0);
    assert_eq!(&health_versions[..5], &[1, 1, 1, 1, 1]);
    let seed_versions = ctx.read_component_versions(1);
    // Spawned entities have no Seed component.
    assert_eq!(&seed_versions[3..5], &[0, 0]);

    // Sorting now matches 5 entities for the Health query.
    let prep = ctx.read_frame_prep();
    assert_eq!(prep[1].count, 5, "Health query must see the spawned entities");
}

#[test]
fn rings_ping_pong_across_frames() {
    // Proto 0 seeds entities 0,1 (Health+Seed); proto 1 seeds entity 2.
    // Only the Seed carriers (0,1) keep spawning 2 commands per frame.
    // Frame 1 writes ring 0, frame 2 replays ring 0 and writes ring 1,
    // frame 3 replays ring 1.
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, spawn_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();

    ctx.run_frame(); // entities 0,1 write 2 spawns (ring 0)
    ctx.run_frame(); // replay ring 0 -> slots 3,4; entities 0,1 write 2 more (ring 1)
    ctx.run_frame(); // replay ring 1 -> slots 5,6

    let active = read_u32s(&ctx.read_buffer_blocking(&ctx.entity_active, MAX_ENTITIES as usize * 4));
    assert_eq!(&active[..8], &[1, 1, 1, 1, 1, 1, 1, 0]);
    let prep = ctx.read_frame_prep();
    assert_eq!(prep[1].count, 7, "Health query must see all 7 active entities");
}

#[test]
fn ring_replay_does_not_double_apply() {
    // If a ring were replayed twice (or never reset), the third frame would
    // show 8 active entities instead of 6. Covered behaviorally here.
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, spawn_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();
    ctx.run_frame();
    ctx.run_frame();
    ctx.run_frame();
    ctx.run_frame(); // replay ring 0 again: seeds wrote 2 more during frame 3
    let active = read_u32s(&ctx.read_buffer_blocking(&ctx.entity_active, MAX_ENTITIES as usize * 4));
    let total: u32 = active[..8].iter().sum();
    assert_eq!(total, 8, "each ring replay applies its commands exactly once");
}

// ------------------------------------------------------------- delete -------

/// World: 4 entities with Life; each deletes itself on the first frame.
fn delete_world() -> EcsIr {
    EcsIr {
        version: 1,
        components: vec![comp(0, "Life", ComponentType::F32)],
        resources: vec![],
        initial_entities: vec![EntityPrototype {
            component_ids: vec![0],
            count: 4,
            initial_values: None,
        }],
        queries: vec![QueryDef {
            id: 0,
            with: vec![ComponentAccess { component_id: 0, access_type: AccessType::Read }],
            without: vec![],
            filters: vec![],
        }],
        systems: vec![SystemDef {
            id: 0,
            name: "die".into(),
            query_id: 0,
            resource_refs: vec![],
            local_var_count: 2,
            instructions: vec![
                Instr::LoadEntityId { dest: 0 },
                Instr::CallBuiltin { dest: 1, func: BuiltinFunc::DeleteEntity, args: vec![0] },
                Instr::Return,
            ],
        }],
        schedule: ScheduleDef {
            stages: vec![Stage { id: 0, name: "Die".into(), system_ids: vec![0] }],
        },
    }
}

#[test]
fn delete_commands_deactivate_entities_and_free_slots() {
    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, delete_world(), MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();

    // Frame 1: all 4 entities dispatch delete-self commands.
    ctx.run_frame();
    // Frame 2 start replays: all 4 deactivated, versions zeroed.
    ctx.run_frame();
    let active = read_u32s(&ctx.read_buffer_blocking(&ctx.entity_active, MAX_ENTITIES as usize * 4));
    assert_eq!(&active[..5], &[0, 0, 0, 0, 0]);
    let life_versions = ctx.read_component_versions(0);
    assert_eq!(&life_versions[..4], &[0, 0, 0, 0]);

    // Sorting matches nothing afterwards.
    let prep = ctx.read_frame_prep();
    assert_eq!(prep[0].count, 0);
}
