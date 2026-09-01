//! M4 acceptance tests: runtime skeleton + sorting pipeline, headless.
//!
//! Builds a multi-query world, seeds the prototype population, runs the
//! count → scan → fill pass and compares `framePrepBuffer`,
//! `compactedEntityIds` and `indirectArgs` against the pure CPU reference
//! implementation in `runtime::init`.

use neon_gpu_ecs::ir::*;
use neon_gpu_ecs::runtime::init;
use neon_gpu_ecs::GpuEcsCtx;

const MAX_ENTITIES: u32 = 16;

fn comp(id: u32, name: &str, ty: ComponentType) -> ComponentDef {
    ComponentDef {
        id,
        name: name.into(),
        ty,
        default_value: vec![0u8; ty.byte_size()],
    }
}

/// World with two populations and four queries exercising with/without and
/// the RenderData filter:
///
/// - population A: 6 entities with Transform+Velocity (ids 0..6)
/// - population B: 4 entities with Transform+Velocity+Health (ids 6..10)
/// - q0: with=[T, V]            -> 10 entities
/// - q1: with=[T], without=[H]  -> 6 entities (population A)
/// - q2: with=[H]               -> 4 entities (population B)
/// - q3: RenderData (with=[T])  -> 10 entities
fn sorting_world() -> EcsIr {
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
        initial_entities: vec![
            EntityPrototype { component_ids: vec![0, 1], count: 6, initial_values: None },
            EntityPrototype { component_ids: vec![0, 1, 2], count: 4, initial_values: None },
        ],
        queries: vec![
            QueryDef {
                id: 0,
                with: vec![
                    ComponentAccess { component_id: 0, access_type: AccessType::Read },
                    ComponentAccess { component_id: 1, access_type: AccessType::Read },
                ],
                without: vec![],
                filters: vec![],
            },
            QueryDef {
                id: 1,
                with: vec![ComponentAccess { component_id: 0, access_type: AccessType::Read }],
                without: vec![2],
                filters: vec![],
            },
            QueryDef {
                id: 2,
                with: vec![ComponentAccess { component_id: 2, access_type: AccessType::Read }],
                without: vec![],
                filters: vec![],
            },
            QueryDef {
                id: 3,
                with: vec![ComponentAccess { component_id: 0, access_type: AccessType::Read }],
                without: vec![],
                filters: vec![QueryFilter::RenderData],
            },
        ],
        // A trivial system keeps the schedule valid (every system scheduled).
        systems: vec![SystemDef {
            id: 0,
            name: "noop".into(),
            query_id: 0,
            resource_refs: vec![],
            local_var_count: 1,
            instructions: vec![Instr::Return],
        }],
        schedule: ScheduleDef {
            stages: vec![Stage { id: 0, name: "Logic".into(), system_ids: vec![0] }],
        },
    }
}

/// Headless device with room for all group 0 storage bindings
/// (8 fixed + 3 per component).
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
    .expect("M4 headless sorting requires a compute adapter");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("neon-gpu-ecs M4 test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits {
            max_storage_buffers_per_shader_stage: max_storage_buffers,
            ..wgpu::Limits::default()
        },
        experimental_features: wgpu::ExperimentalFeatures::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .expect("the M4 test adapter must create a device and queue")
}

/// The CPU reference for the sorting pass: per-query `{start, count}` as an
/// exclusive prefix sum over the reference match counts, and the compacted
/// id lists (order-insensitive, since fill scatters via atomics).
fn cpu_reference(ir: &EcsIr) -> (Vec<neon_gpu_ecs::generator::QueryRange>, Vec<Vec<u32>>) {
    let mut ranges = Vec::new();
    let mut lists = Vec::new();
    let mut total = 0u32;
    for q in 0..ir.queries.len() {
        let matched = init::initial_query_match(ir, q);
        let count = matched.len() as u32;
        ranges.push(neon_gpu_ecs::generator::QueryRange { start: total, count });
        lists.push(matched);
        total += count;
    }
    (ranges, lists)
}

#[test]
fn sorting_matches_cpu_reference() {
    let ir = sorting_world();
    let (expected_ranges, expected_lists) = cpu_reference(&ir);

    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, ir.clone(), MAX_ENTITIES, 8)
        .expect("world must fit the high-limit test device");
    ctx.seed_initial();
    ctx.run_sort();

    let ranges = ctx.read_frame_prep();
    assert_eq!(ranges, expected_ranges, "framePrepBuffer mismatch");

    let ids = ctx.read_compacted_ids();
    for (q, range) in ranges.iter().enumerate() {
        let start = range.start as usize;
        let end = start + range.count as usize;
        let mut got: Vec<u32> = ids[start..end].to_vec();
        got.sort_unstable();
        let mut want = expected_lists[q].clone();
        want.sort_unstable();
        assert_eq!(got, want, "compacted ids mismatch for query {q}");
    }

    let args = ctx.read_indirect_args();
    for (q, range) in ranges.iter().enumerate() {
        let want_x = range.count.div_ceil(64);
        assert_eq!(args[q], [want_x, 1, 1], "indirect args mismatch for query {q}");
    }
}

#[test]
fn second_sort_is_stable_after_counter_reset() {
    // The scan kernel resets queryCounts; running sorting twice must yield
    // identical ranges instead of doubled counts.
    let ir = sorting_world();
    let (expected_ranges, _) = cpu_reference(&ir);

    let (device, queue) = ecs_headless_device(32);
    let ctx = GpuEcsCtx::new(device, queue, ir, MAX_ENTITIES, 8).unwrap();
    ctx.seed_initial();
    ctx.run_sort();
    ctx.run_sort();
    assert_eq!(ctx.read_frame_prep(), expected_ranges, "second sort must match");
}

#[test]
fn insufficient_storage_buffer_limit_is_rejected() {
    // Default limits allow 8 storage buffers per stage; the sorting world
    // needs 8 + 3*3 = 17 group 0 bindings.
    let (device, queue) = ecs_headless_device(8);
    let result = GpuEcsCtx::new(device, queue, sorting_world(), MAX_ENTITIES, 8);
    match result {
        Err(neon_gpu_ecs::EcsError::Limits(message)) => {
            assert!(message.contains("17"), "{message}");
            assert!(message.contains("max_storage_buffers_per_shader_stage"), "{message}");
        }
        Err(other) => panic!("expected Limits, got {other:?}"),
        Ok(_) => panic!("8-slot device must reject a 17-binding world"),
    }
}

#[test]
fn cpu_reference_matches_hand_computed_expectations() {
    // Guard the reference itself before trusting it as ground truth.
    let ir = sorting_world();
    let (ranges, lists) = cpu_reference(&ir);
    assert_eq!(ranges[0].start, 0);
    assert_eq!(ranges[0].count, 10);
    assert_eq!(ranges[1].start, 10);
    assert_eq!(ranges[1].count, 6);
    assert_eq!(ranges[2].start, 16);
    assert_eq!(ranges[2].count, 4);
    assert_eq!(ranges[3].start, 20);
    assert_eq!(ranges[3].count, 10);
    assert_eq!(lists[1], (0..6).collect::<Vec<_>>());
    assert_eq!(lists[2], (6..10).collect::<Vec<_>>());
}
