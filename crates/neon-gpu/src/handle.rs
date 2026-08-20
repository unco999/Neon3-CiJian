//! Handles and GPU pointers for pool elements.
//!
//! A [`Handle`] is the stable, CPU-side identity of a pool element. It is
//! deliberately *not* a raw address: it carries a `generation` so that a
//! stale handle (element freed, slot reused by someone else) can be detected
//! and rejected. A [`GpuPtr`] is the translation of a live handle into a byte
//! offset inside the pool's storage buffer; shaders receive these offsets and
//! resolve nested fields by adding compile-time offsets from the layout.

/// Stable identity of a pool element.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Handle {
    pub slot: u32,
    pub generation: u32,
}

impl Handle {
    pub const fn new(slot: u32, generation: u32) -> Self {
        Self { slot, generation }
    }
}

/// Byte offset of a live element inside a pool's storage buffer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GpuPtr {
    pub offset: u32,
    pub size: u32,
}

impl GpuPtr {
    pub const fn new(offset: u32, size: u32) -> Self {
        Self { offset, size }
    }
}

/// A pool-local index that refers to a specific pool.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PoolId(pub u32);

impl PoolId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }
}