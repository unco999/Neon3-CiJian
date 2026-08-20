//! GPU half of the data pool: a storage buffer backed by a persistent
//! wgpu-hal mapping, plus the compact bind group that lets any pipeline read
//! all pools from one bind group slot.
//!
//! Ownership model (matches `AGENTS.md`): this crate is a leaf library. The
//! device/queue stay with the caller (`neon-wgpu-runtime`); `GpuPool` only
//! borrows a `wgpu::Device` handle to create resources and perform mapping.

use std::num::NonZeroU64;
use std::ops::Range;
use std::ptr::NonNull;

use crate::hal_map::{self, HalBackend, MappedBuffer};
use crate::handle::{GpuPtr, Handle, PoolId};
use crate::layout::StructLayout;
use crate::pool::{DataPool, PoolError};

/// Errors from the GPU half of a pool.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("unsupported wgpu backend {0:?}")]
    UnsupportedBackend(wgpu::Backend),
    #[error("hal downcast failed for {0}")]
    BackendMismatch(String),
    #[error("pool buffer size {size} exceeds device limit {limit}")]
    BufferTooLarge { size: u64, limit: u64 },
    #[error("pool error: {0}")]
    Pool(#[from] PoolError),
    #[error("write of {given} bytes does not match element size {expected}")]
    SizeMismatch { given: usize, expected: u32 },
    #[error("mapping error: {0}")]
    Mapping(String),
    #[error("pool buffer size must be non-zero")]
    EmptyBuffer,
}

/// A data pool on the GPU.
///
/// Two buffers per pool (wgpu 30 forbids `MAP` + `STORAGE` on one buffer):
///
/// - `mapped`: `MAP_WRITE | COPY_SRC`, CPU writes go through a persistent
///   wgpu-hal mapping and are flushed;
/// - `storage`: `STORAGE | COPY_DST | COPY_SRC`, the buffer shaders bind.
///
/// After `flush`, call [`GpuPool::sync_to_gpu`] with the command encoder used
/// for the submission so the storage view receives the new bytes (one GPU
/// copy, no CPU round trip).
pub struct GpuPool {
    device: wgpu::Device,
    logic: DataPool,
    mapped: wgpu::Buffer,
    mapped_hal: MappedBuffer,
    storage: wgpu::Buffer,
    /// Ranges written since the last flush (hal flush bookkeeping).
    dirty: Vec<Range<u64>>,
    /// Whether the storage view is out of date relative to the mapped buffer.
    /// Kept separate from `dirty`: on coherent backends `flush` clears the
    /// ranges but the storage copy is still required.
    pending_sync: bool,
    /// Whether the GPU only reads this pool (storage read-only binding).
    read_only: bool,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    label: String,
}

impl GpuPool {
    /// Create a pool for `layout` elements with the given capacity.
    pub fn new(
        device: &wgpu::Device,
        layout: StructLayout,
        capacity: u32,
        read_only: bool,
        label: impl Into<String>,
    ) -> Result<Self, GpuError> {
        Self::with_deferred_frames(device, layout, capacity, read_only, label, 2)
    }

    pub fn with_deferred_frames(
        device: &wgpu::Device,
        layout: StructLayout,
        capacity: u32,
        read_only: bool,
        label: impl Into<String>,
        deferred_frames: u64,
    ) -> Result<Self, GpuError> {
        let label = label.into();
        let stride = layout.array_stride as u64;
        let size = stride * capacity as u64;
        if size == 0 {
            return Err(GpuError::EmptyBuffer);
        }
        let limit = device.limits().max_storage_buffer_binding_size as u64;
        if size > limit {
            return Err(GpuError::BufferTooLarge { size, limit });
        }

        let backend = HalBackend::from_backend(device.adapter_info().backend)?;

        let mapped = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label}::mapped")),
            size,
            usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });

        // Zero-fill through the wgpu API *before* the hal mapping. wgpu-core
        // tracks buffer initialization: writes through the hal mapping are
        // invisible to it, and an "uninitialized" source would be zero-filled
        // again by wgpu-core right before our copy, wiping the pool data.
        let mut view = mapped
            .slice(..)
            .get_mapped_range_mut()
            .expect("buffer was created mapped_at_creation");
        view.copy_from_slice(&vec![0u8; size as usize]);
        drop(view);
        mapped.unmap();

        // SAFETY: the mapped buffer has MAP_WRITE usage; we map it exactly once
        // and hold the mapping until `Drop`.
        let mapping = unsafe { hal_map::map(device, &mapped, backend, 0..size)? };

        let mapped_hal = MappedBuffer {
            ptr: mapping.ptr,
            size,
            is_coherent: mapping.is_coherent,
            backend,
        };

        let storage = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label}::storage")),
            size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let logic = DataPool::with_deferred_frames(layout, capacity, deferred_frames);

        let storage_ty = wgpu::BufferBindingType::Storage { read_only };
        let layout_desc = wgpu::BindGroupLayoutDescriptor {
            label: Some(&format!("{label}::bgl")),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE
                    | wgpu::ShaderStages::VERTEX
                    | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: storage_ty,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(size),
                },
                count: None,
            }],
        };
        let bind_group_layout = device.create_bind_group_layout(&layout_desc);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("{label}::bg")),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: storage.as_entire_binding(),
            }],
        });

        Ok(Self {
            device: device.clone(),
            logic,
            mapped,
            mapped_hal,
            storage,
            dirty: Vec::new(),
            pending_sync: false,
            read_only,
            bind_group_layout,
            bind_group,
            label,
        })
    }

    /// Allocate a slot and return a stable handle.
    pub fn alloc(&mut self) -> Result<Handle, PoolError> {
        self.logic.alloc()
    }

    /// Free a slot; it is reclaimed after the deferred-free window passes.
    pub fn free(&mut self, handle: Handle) -> Result<(), PoolError> {
        self.logic.free(handle)
    }

    /// Resolve a live handle to a byte offset in the storage buffer.
    pub fn resolve(&self, handle: Handle) -> Result<GpuPtr, PoolError> {
        self.logic.resolve(handle)
    }

    pub fn is_live(&self, handle: Handle) -> bool {
        self.logic.is_live(handle)
    }

    /// Monotonic mutation counter.
    pub fn version(&self) -> u64 {
        self.logic.version()
    }

    pub fn layout(&self) -> &StructLayout {
        self.logic.layout()
    }

    pub fn capacity(&self) -> u32 {
        self.logic.capacity()
    }

    pub fn live_count(&self) -> u32 {
        self.logic.live_count()
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Overwrite a live element's bytes from CPU memory via the persistent
    /// mapping. Does not flush; call [`GpuPool::flush`] before the GPU reads it.
    pub fn write_bytes(&mut self, handle: Handle, bytes: &[u8]) -> Result<(), GpuError> {
        let ptr = self.resolve(handle)?;
        if bytes.len() as u32 != ptr.size {
            return Err(GpuError::SizeMismatch {
                given: bytes.len(),
                expected: ptr.size,
            });
        }
        // SAFETY: `ptr.offset` was validated by the allocator, `bytes.len()`
        // matches the element size, and the mapping covers the whole buffer.
        unsafe {
            let dst = self.mapped_hal.ptr.as_ptr().add(ptr.offset as usize);
            std::ptr::copy(bytes.as_ptr(), dst, bytes.len());
        }
        self.dirty.push(ptr.offset as u64..(ptr.offset + ptr.size) as u64);
        self.pending_sync = true;
        self.logic.bump_version();
        Ok(())
    }

    /// Read a live element's bytes back to CPU memory (own writes only; the
    /// pool is not expected to be written by the GPU in this milestone).
    pub fn read_bytes(&self, handle: Handle) -> Result<Vec<u8>, GpuError> {
        let ptr = self.resolve(handle)?;
        let mut out = vec![0u8; ptr.size as usize];
        // SAFETY: same reasoning as `write_bytes`.
        unsafe {
            let src = self.mapped_hal.ptr.as_ptr().add(ptr.offset as usize);
            std::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), out.len());
        }
        Ok(out)
    }

    /// Make all pending CPU writes visible to the GPU.
    pub fn flush(&mut self) -> Result<(), GpuError> {
        if self.dirty.is_empty() {
            return Ok(());
        }
        if !self.mapped_hal.is_coherent {
            let ranges = std::mem::take(&mut self.dirty);
            // SAFETY: ranges were recorded from validated writes inside the
            // buffer; the buffer is still mapped.
            unsafe {
                hal_map::flush(
                    &self.device,
                    &self.mapped,
                    self.mapped_hal.backend,
                    ranges.into_iter(),
                )?;
            }
        } else {
            self.dirty.clear();
        }
        Ok(())
    }

    /// Record a copy of the flushed CPU bytes into the storage view on
    /// `encoder`. Must be called on the encoder of the submission that reads
    /// the pool, after [`GpuPool::flush`]. No-op if nothing changed since the
    /// last sync.
    pub fn sync_to_gpu(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if !self.pending_sync {
            return;
        }
        self.pending_sync = false;
        encoder.copy_buffer_to_buffer(&self.mapped, 0, &self.storage, 0, self.storage.size());
    }

    /// Advance the allocator to a new frame and zero any reclaimed slots
    /// (tombstones) so stale shader reads never see another element's data.
    pub fn advance_frame(&mut self, frame: u64) {
        let reclaimed = self.logic.advance_frame(frame);
        for slot in reclaimed {
            let offset = slot as u64 * self.logic.layout().array_stride as u64;
            // SAFETY: reclaimed slots are within bounds and mapped.
            unsafe {
                let dst = self.mapped_hal.ptr.as_ptr().add(offset as usize);
                std::ptr::write_bytes(dst, 0, self.logic.layout().size as usize);
            }
            self.dirty.push(offset..offset + self.logic.layout().array_stride as u64);
        }
        self.pending_sync = true;
    }

    /// The GPU-visible storage buffer (what shaders bind).
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.storage
    }

    /// The CPU-mapped write buffer (what `write_bytes` touches).
    pub fn mapped_buffer(&self) -> &wgpu::Buffer {
        &self.mapped
    }

    /// This pool's single-entry bind group.
    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// This pool's single-entry bind group layout.
    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }
}

impl Drop for GpuPool {
    fn drop(&mut self) {
        // Flush pending writes, then end the persistent mapping before the
        // wgpu mapped buffer is destroyed (field drop order below).
        if self.mapped_hal.ptr != NonNull::dangling() {
            let _ = self.flush();
            // SAFETY: the mapped buffer is still mapped at this point and is
            // not yet destroyed (it is a later field).
            unsafe {
                let _ = hal_map::unmap(&self.device, &self.mapped, self.mapped_hal.backend);
            }
            self.mapped_hal.ptr = NonNull::dangling();
        }
    }
}

/// A collection of pools exposed as one compact bind group.
///
/// All pools share a single bind group (group 0) with one storage entry per
/// pool, so any compute or render pipeline that needs pool data binds exactly
/// one group. Pools must be added before the pipelines that consume them are
/// created.
pub struct PoolHeap {
    device: wgpu::Device,
    pools: Vec<GpuPool>,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
}

impl PoolHeap {
    pub fn new(device: wgpu::Device) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("neon-gpu::PoolHeap (empty)"),
            entries: &[],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon-gpu::PoolHeap (empty)"),
            layout: &bind_group_layout,
            entries: &[],
        });
        Self {
            device,
            pools: Vec::new(),
            bind_group_layout,
            bind_group,
        }
    }

    /// Register a pool and rebuild the compact bind group. Returns its id.
    pub fn add_pool(&mut self, pool: GpuPool) -> PoolId {
        self.pools.push(pool);
        self.rebuild();
        PoolId::new(self.pools.len() as u32 - 1)
    }

    fn rebuild(&mut self) {
        let entries: Vec<wgpu::BindGroupLayoutEntry> = self
            .pools
            .iter()
            .enumerate()
            .map(|(i, p)| {
                wgpu::BindGroupLayoutEntry {
                    binding: i as u32,
                    visibility: wgpu::ShaderStages::COMPUTE
                        | wgpu::ShaderStages::VERTEX
                        | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage {
                            read_only: p.read_only,
                        },
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(p.buffer().size()),
                    },
                    count: None,
                }
            })
            .collect();

        self.bind_group_layout = self.device.create_bind_group_layout(
            &wgpu::BindGroupLayoutDescriptor {
                label: Some("neon-gpu::PoolHeap::bgl"),
                entries: &entries,
            },
        );

        let entries: Vec<wgpu::BindGroupEntry> = self
            .pools
            .iter()
            .enumerate()
            .map(|(i, p)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: p.buffer().as_entire_binding(),
            })
            .collect();

        self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon-gpu::PoolHeap::bg"),
            layout: &self.bind_group_layout,
            entries: &entries,
        });
    }

    pub fn pool(&self, id: PoolId) -> &GpuPool {
        &self.pools[id.0 as usize]
    }

    pub fn pool_mut(&mut self, id: PoolId) -> &mut GpuPool {
        &mut self.pools[id.0 as usize]
    }

    pub fn pools(&self) -> &[GpuPool] {
        &self.pools
    }

    pub fn pools_mut(&mut self) -> &mut [GpuPool] {
        &mut self.pools
    }

    pub fn bind_group_layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }
}