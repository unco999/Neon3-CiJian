//! Headless GPU integration tests for `neon-gpu`.
//!
//! These prove the whole path end to end on a real device:
//!
//! 1. CPU writes elements into a pool through the persistent wgpu-hal mapping;
//! 2. a compute shader reads the pool through the compact `PoolHeap` bind
//!    group (group 0) and writes results to a storage buffer (group 1);
//! 3. results are copied into a MAP_READ buffer that is *also* mapped through
//!    wgpu-hal, invalidated, and read back on the CPU.

use std::ptr::NonNull;

use bytemuck::{Pod, Zeroable};
use neon_gpu::gpu::{GpuError, GpuPool, PoolHeap};
use neon_gpu::hal_map::{self, HalBackend, MappedBuffer};
use neon_gpu::layout::{LayoutBuilder, Type};
use neon_gpu::ty;
use neon_gpu::{GpuPtr, Handle, PoolId};

const POOL_CAPACITY: u32 = 16;
const RESULT_WORDS: u32 = 8;

/// CPU view of one pool element; must match the WGSL `Item` struct exactly.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct ItemBytes {
    a: f32,
    b: f32,
    weight: f32,
    flags: u32,
}

impl ItemBytes {
    fn new(a: f32, b: f32, weight: f32, flags: u32) -> Self {
        Self {
            a,
            b,
            weight,
            flags,
        }
    }
}

fn item_layout() -> neon_gpu::StructLayout {
    let inner = LayoutBuilder::new("Inner")
        .field("a", ty::F32)
        .field("b", ty::F32)
        .build()
        .unwrap();
    LayoutBuilder::new("Item")
        .field("inner", Type::Struct(inner))
        .field("weight", ty::F32)
        .field("flags", ty::U32)
        .build()
        .unwrap()
}

/// A headless wgpu device on the default backend.
fn headless_device() -> (wgpu::Device, wgpu::Queue, HalBackend) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    }))
    .expect("no adapter available");

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("neon-gpu test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: Default::default(),
        memory_hints: wgpu::MemoryHints::default(),
        trace: Default::default(),
    }))
    .expect("device request failed");

    let backend = HalBackend::from_backend(device.adapter_info().backend)
        .expect("unsupported backend for hal mapping");
    (device, queue, backend)
}

fn map_buffer(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    backend: HalBackend,
    size: u64,
) -> MappedBuffer {
    // SAFETY: the caller-created buffer has MAP_READ usage and is not mapped
    // through any other path; we unmap it before it is dropped.
    let mapping = unsafe { hal_map::map(device, buffer, backend, 0..size) }.expect("hal map");
    MappedBuffer {
        ptr: mapping.ptr,
        size,
        is_coherent: mapping.is_coherent,
        backend,
    }
}

fn shader_source(layout: &neon_gpu::StructLayout, pool_count: u32) -> String {
    format!(
        r#"
{structs}
@group(0) @binding(0) var<storage, read> pool: array<Item, {pool_count}>;
@group(1) @binding(0) var<storage, read_write> result: array<f32, {result_words}>;

@compute @workgroup_size(1, 1, 1)
fn main() {{
    var total: f32 = 0.0;
    for (var i: u32 = 0u; i < 3u; i = i + 1u) {{
        total = total + pool[i].inner.a * pool[i].weight + f32(pool[i].flags);
    }}
    result[0] = total;
    result[1] = pool[1].inner.b;
    result[2] = f32(pool[0].flags);
    result[3] = pool[1].inner.a;
    result[4] = pool[2].inner.a * pool[2].weight + f32(pool[2].flags);
}}
"#,
        structs = layout.wgsl_source().unwrap(),
        pool_count = pool_count,
        result_words = RESULT_WORDS,
    )
}

#[test]
fn cpu_write_gpu_read_roundtrip() {
    let (device, queue, backend) = headless_device();
    let layout = item_layout();
    assert_eq!(layout.size, 16);
    assert_eq!(layout.array_stride, 16);

    // --- pool + heap ------------------------------------------------------
    let mut heap = PoolHeap::new(device.clone());
    let pool_id: PoolId = heap
        .add_pool(GpuPool::new(&device, layout.clone(), POOL_CAPACITY, true, "test_pool").unwrap());

    // Write three live elements through the persistent mapping.
    let items = [
        ItemBytes::new(2.0, 3.0, 4.0, 5),
        ItemBytes::new(1.5, 1.5, 2.0, 1),
        ItemBytes::new(10.0, 0.5, 0.5, 0),
    ];
    let mut handles = Vec::new();
    for item in &items {
        let h = heap.pool_mut(pool_id).alloc().unwrap();
        heap.pool_mut(pool_id)
            .write_bytes(h, bytemuck::bytes_of(item))
            .unwrap();
        handles.push(h);
    }
    assert_eq!(heap.pool(pool_id).version(), 6); // 3 allocs + 3 writes
    assert_eq!(heap.pool(pool_id).live_count(), 3);

    // CPU round trip without GPU involvement.
    let bytes = heap.pool(pool_id).read_bytes(handles[0]).unwrap();
    let back: &ItemBytes = bytemuck::from_bytes(&bytes);
    assert_eq!(back.a, 2.0);
    assert_eq!(back.flags, 5);

    // Make the writes visible to the GPU.
    heap.pool_mut(pool_id).flush().unwrap();

    // --- result buffers ---------------------------------------------------
    let result = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test result"),
        size: RESULT_WORDS as u64 * 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test readback"),
        size: RESULT_WORDS as u64 * 4,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let readback_map = map_buffer(&device, &readback, backend, RESULT_WORDS as u64 * 4);

    let result_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("result bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let result_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("result bg"),
        layout: &result_bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: result.as_entire_binding(),
        }],
    });

    // --- pipeline (compact heap bind group as group 0) --------------------
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("pool reader"),
        source: wgpu::ShaderSource::Wgsl(shader_source(&layout, POOL_CAPACITY).into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("pool reader pipeline"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pool reader layout"),
                bind_group_layouts: &[Some(heap.bind_group_layout()), Some(&result_bgl)],
                immediate_size: 0,
            }),
        ),
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });

    // --- execute ----------------------------------------------------------
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("pool reader encoder"),
    });
    {
        // Copy the flushed CPU bytes into the storage view first.
        heap.pool_mut(pool_id).sync_to_gpu(&mut encoder);
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, heap.bind_group(), &[]);
        pass.set_bind_group(1, &result_bg, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&result, 0, &readback, 0, RESULT_WORDS as u64 * 4);
    queue.submit([encoder.finish()]);
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(30)),
        })
        .unwrap();

    // --- read back through the hal mapping --------------------------------
    // SAFETY: the readback buffer is still mapped and the ranges are in bounds.
    unsafe {
        hal_map::invalidate(
            &device,
            &readback,
            backend,
            [0..RESULT_WORDS as u64 * 4].into_iter(),
        )
    }
    .expect("invalidate");

    let words: &[f32] =
        bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(readback_map.ptr.as_ptr(), 32) });
    assert_eq!(words[0], 22.0); // total: 13 + 4 + 5
    assert_eq!(words[1], 1.5); // pool[1].inner.b
    assert_eq!(words[2], 5.0); // pool[0].flags
    assert_eq!(words[3], 1.5); // pool[1].inner.a
    assert_eq!(words[4], 5.0); // pool[2]: 10 * 0.5 + 0

    // SAFETY: the readback buffer is still alive and mapped.
    unsafe {
        hal_map::unmap(&device, &readback, backend).expect("unmap readback");
    }
}

#[test]
fn deferred_free_tombstones_slot() {
    let (device, queue_owned, _backend) = headless_device();
    let layout = item_layout();
    let mut pool =
        GpuPool::with_deferred_frames(&device, layout, 8, true, "tombstone pool", 2).unwrap();

    let mut handles = Vec::new();
    for i in 0..4u32 {
        let h = pool.alloc().unwrap();
        pool.write_bytes(
            h,
            bytemuck::bytes_of(&ItemBytes::new(i as f32, 0.0, 1.0, 2)),
        )
        .unwrap();
        handles.push(h);
    }
    pool.free(handles[1]).unwrap();
    assert_eq!(pool.live_count(), 3);

    // Wait out the deferred window; reclaimed slots are zeroed.
    pool.advance_frame(1);
    pool.advance_frame(2);

    // Flush + sync so the GPU-side storage view also gets the tombstones.
    pool.flush().unwrap();
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("tombstone encoder"),
    });
    pool.sync_to_gpu(&mut encoder);
    queue_owned.submit([encoder.finish()]);

    // The freed slot is now free again and must be reused by the next alloc.
    let reused = pool.alloc().unwrap();
    assert_eq!(reused.slot, handles[1].slot);
    let bytes = pool.read_bytes(reused).unwrap();
    let item: &ItemBytes = bytemuck::from_bytes(&bytes);
    assert_eq!(item.a, 0.0, "reclaimed slot must be tombstoned to zeros");
    assert_eq!(item.flags, 0);

    // The old handle is stale: resolving it must fail with a generation
    // mismatch (slot was reused with a newer generation).
    let stale = pool.resolve(handles[1]).unwrap_err();
    assert!(matches!(stale, neon_gpu::PoolError::StaleGeneration { .. }));

    // Resolve a live handle to an explicit byte pointer, as a shader would.
    let ptr: GpuPtr = pool.resolve(handles[0]).unwrap();
    assert_eq!(ptr.offset, 0);
    assert_eq!(ptr.size, 16);
}

#[test]
fn oversize_write_rejected() {
    let (device, _queue, _backend) = headless_device();
    let layout = item_layout();
    let mut pool = GpuPool::new(&device, layout, 8, true, "strict pool").unwrap();
    let h = pool.alloc().unwrap();
    let err = pool.write_bytes(h, &[0u8; 17]).unwrap_err();
    assert!(matches!(
        err,
        GpuError::SizeMismatch {
            given: 17,
            expected: 16
        }
    ));
}

#[test]
fn empty_buffer_rejected() {
    let (device, _queue, _backend) = headless_device();
    let layout = item_layout();
    let err = GpuPool::new(&device, layout, 0, true, "empty")
        .err()
        .unwrap();
    assert!(matches!(err, GpuError::EmptyBuffer));
}

#[test]
fn resolve_rejects_stale_and_out_of_bounds() {
    let (device, _queue, _backend) = headless_device();
    let layout = item_layout();
    let mut pool = GpuPool::new(&device, layout, 4, true, "resolve pool").unwrap();
    let _h = pool.alloc().unwrap();
    let _ = pool.alloc().unwrap();
    // Slot 99 never existed.
    let err = pool.resolve(Handle::new(99, 0)).unwrap_err();
    assert!(matches!(err, neon_gpu::PoolError::OutOfBounds { .. }));
    // Generation mismatch after slot 0's generation was bumped by reuse.
    let err = pool.resolve(Handle::new(0, 7)).unwrap_err();
    assert!(matches!(err, neon_gpu::PoolError::StaleGeneration { .. }));
}

#[test]
fn handle_resolution_never_leaks_raw_addresses() {
    let (device, _queue, _backend) = headless_device();
    let layout = item_layout();
    let mut pool = GpuPool::new(&device, layout, 4, true, "addr pool").unwrap();
    let h = pool.alloc().unwrap();
    let ptr = pool.resolve(h).unwrap();
    assert!(ptr.offset % 16 == 0, "slots are stride-aligned");
    assert!(ptr.offset < 4 * 16);
    assert_eq!(ptr.size, 16);
    // A raw slot guess cannot be resolved without a matching generation.
    let err = pool.resolve(Handle::new(0, u32::MAX)).unwrap_err();
    assert!(matches!(err, neon_gpu::PoolError::StaleGeneration { .. }));
}

/// `NonNull` and mapping plumbing are Send/Sync so the pool can be owned by
/// the render thread while reads happen on a worker (compile-time check).
#[test]
fn mapped_buffer_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MappedBuffer>();
    assert_send_sync::<GpuPool>();
    assert_send_sync::<PoolHeap>();
    let _ = NonNull::<u8>::dangling();
}
