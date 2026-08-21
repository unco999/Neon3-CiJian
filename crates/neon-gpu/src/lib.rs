//! # neon-gpu
//!
//! The GPU compute system library of Neon3: data pool addressing, struct
//! layout, and compact bindings.
//!
//! This is a leaf library: it never owns a window or an instance. The caller
//! (`neon-wgpu-runtime`) owns the `wgpu::Device`; `neon-gpu` creates and maps
//! the buffers it needs on that device.
//!
//! ## Modules
//!
//! - [`layout`] — single source of truth for nested struct layout (offsets,
//!   sizes, strides) and WGSL type emission.
//! - [`handle`] — stable [`Handle`]s and GPU-side byte pointers ([`GpuPtr`]).
//! - [`pool`] — the authoritative CPU-side allocator: free list, generations,
//!   deferred deletion.
//! - [`gpu`] — the GPU half: storage buffers with persistent wgpu-hal mapping,
//!   plus [`PoolHeap`](gpu::PoolHeap) for compact multi-pool bind groups.
//!
//! ## Design notes
//!
//! - Elements are fixed-size (no variable-length arrays). Each pool is a slab
//!   of `layout.array_stride`-spaced slots in one storage buffer.
//! - A [`Handle`] is `(slot, generation)`; it is never a raw address. The
//!   only way to obtain a [`GpuPtr`] is to resolve a live handle, so stale
//!   handles cannot silently become pointers.
//! - Deletion is deferred by a frame window: freed slots are only reused once
//!   the GPU is guaranteed to have stopped reading them, and each reuse bumps
//!   the generation so old handles are detected as stale.
//! - CPU writes go through a persistent wgpu-hal mapping of the storage
//!   buffer (see [`gpu`]), with explicit flush/invalidate of dirty ranges.

pub mod gpu;
pub mod hal_map;
pub mod handle;
pub mod layout;
pub mod pool;

pub use gpu::{GpuError, GpuPool, PoolHeap};
pub use handle::{GpuPtr, Handle, PoolId};
pub use layout::{LayoutBuilder, StructLayout, Type, ty};
pub use pool::{DataPool, PoolError};
