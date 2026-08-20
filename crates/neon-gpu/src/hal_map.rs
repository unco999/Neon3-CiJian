//! wgpu-hal persistent-mapping bridge ("black magic" layer).
//!
//! A `wgpu::Buffer` created with `MAP_WRITE` / `MAP_READ` usage is downcast to
//! its wgpu-hal buffer and mapped **once, persistently**. CPU writes then go
//! straight into GPU-visible memory, avoiding per-frame staging + copy for
//! pool updates. When the mapping is not coherent we flush / invalidate
//! explicit ranges so the backend can move data between CPU and GPU caches.
//!
//! This is safe only when the caller follows the contract documented on
//! `wgpu_hal::Device::map_buffer`:
//!
//! - the buffer was created with the matching `MAP_*` usage;
//! - CPU writes are followed by `flush` before any GPU read of the same bytes;
//! - GPU writes are followed by `invalidate` before any CPU read of the same
//!   bytes;
//! - the buffer is unmapped before it is destroyed (we do this on `Drop`).

use std::ops::Range;
use std::ptr::NonNull;

use wgpu_hal::{BufferMapping, MemoryRange};

use crate::gpu::GpuError;

/// The active wgpu-hal backend, used to pick the right `Api` for downcasting.
///
/// The enum mirrors `wgpu_hal::api`: `Metal` only exists on Apple targets and
/// `Gles` requires the `gles` feature (enabled by wgpu defaults). The `Api`
/// type is cfg-gated the same way in `wgpu-hal`, so every variant here maps to
/// a type that actually exists on the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HalBackend {
    #[cfg(target_os = "windows")]
    Dx12,
    Vulkan,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    Metal,
    Gles,
}

impl HalBackend {
    pub fn from_backend(backend: wgpu::Backend) -> Result<Self, GpuError> {
        let mapped = match backend {
            #[cfg(target_os = "windows")]
            wgpu::Backend::Dx12 => HalBackend::Dx12,
            wgpu::Backend::Vulkan => HalBackend::Vulkan,
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            wgpu::Backend::Metal => HalBackend::Metal,
            wgpu::Backend::Gl => HalBackend::Gles,
            other => return Err(GpuError::UnsupportedBackend(other)),
        };
        Ok(mapped)
    }

    fn name(&self) -> &'static str {
        match self {
            #[cfg(target_os = "windows")]
            HalBackend::Dx12 => "dx12",
            HalBackend::Vulkan => "vulkan",
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            HalBackend::Metal => "metal",
            HalBackend::Gles => "gles",
        }
    }
}

/// Map `range` of `buffer` persistently. Returns the CPU view of the memory.
///
/// # Safety
/// See the module docs. The returned pointer stays valid until [`unmap`] is
/// called for the same buffer.
pub unsafe fn map(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    backend: HalBackend,
    range: Range<u64>,
) -> Result<BufferMapping, GpuError> {
    let name = backend.name();
    // SAFETY: see function docs; the caller guarantees the mapping contract.
    unsafe {
        match backend {
            #[cfg(target_os = "windows")]
            HalBackend::Dx12 => map_impl::<wgpu_hal::api::Dx12>(device, buffer, range, name),
            HalBackend::Vulkan => map_impl::<wgpu_hal::api::Vulkan>(device, buffer, range, name),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            HalBackend::Metal => map_impl::<wgpu_hal::api::Metal>(device, buffer, range, name),
            HalBackend::Gles => map_impl::<wgpu_hal::api::Gles>(device, buffer, range, name),
        }
    }
}

/// Make CPU writes visible to the GPU for the given ranges.
///
/// # Safety
/// `ranges` must be inside the mapped region and the buffer must be mapped.
pub unsafe fn flush(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    backend: HalBackend,
    ranges: impl Iterator<Item = MemoryRange>,
) -> Result<(), GpuError> {
    let name = backend.name();
    // SAFETY: see function docs; the caller guarantees the mapping contract.
    unsafe {
        match backend {
            #[cfg(target_os = "windows")]
            HalBackend::Dx12 => flush_impl::<wgpu_hal::api::Dx12>(device, buffer, ranges, name),
            HalBackend::Vulkan => flush_impl::<wgpu_hal::api::Vulkan>(device, buffer, ranges, name),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            HalBackend::Metal => flush_impl::<wgpu_hal::api::Metal>(device, buffer, ranges, name),
            HalBackend::Gles => flush_impl::<wgpu_hal::api::Gles>(device, buffer, ranges, name),
        }
    }
}

/// Make GPU writes visible to the CPU for the given ranges.
///
/// # Safety
/// `ranges` must be inside the mapped region and the buffer must be mapped.
pub unsafe fn invalidate(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    backend: HalBackend,
    ranges: impl Iterator<Item = MemoryRange>,
) -> Result<(), GpuError> {
    let name = backend.name();
    // SAFETY: see function docs; the caller guarantees the mapping contract.
    unsafe {
        match backend {
            #[cfg(target_os = "windows")]
            HalBackend::Dx12 => invalidate_impl::<wgpu_hal::api::Dx12>(device, buffer, ranges, name),
            HalBackend::Vulkan => invalidate_impl::<wgpu_hal::api::Vulkan>(device, buffer, ranges, name),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            HalBackend::Metal => {
                invalidate_impl::<wgpu_hal::api::Metal>(device, buffer, ranges, name)
            }
            HalBackend::Gles => invalidate_impl::<wgpu_hal::api::Gles>(device, buffer, ranges, name),
        }
    }
}

/// End a persistent mapping.
///
/// # Safety
/// The buffer must be mapped and must not be destroyed while the mapping
/// guard's pointer is in use.
pub unsafe fn unmap(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    backend: HalBackend,
) -> Result<(), GpuError> {
    let name = backend.name();
    // SAFETY: see function docs; the caller guarantees the mapping contract.
    unsafe {
        match backend {
            #[cfg(target_os = "windows")]
            HalBackend::Dx12 => unmap_impl::<wgpu_hal::api::Dx12>(device, buffer, name),
            HalBackend::Vulkan => unmap_impl::<wgpu_hal::api::Vulkan>(device, buffer, name),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            HalBackend::Metal => unmap_impl::<wgpu_hal::api::Metal>(device, buffer, name),
            HalBackend::Gles => unmap_impl::<wgpu_hal::api::Gles>(device, buffer, name),
        }
    }
}

unsafe fn map_impl<A: wgpu_hal::Api>(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    range: Range<u64>,
    backend_name: &'static str,
) -> Result<BufferMapping, GpuError> {
    // SAFETY: `as_hal` is only sound while the wgpu handle stays alive, which
    // is guaranteed by the caller's borrow of `device`/`buffer`.
    let dev = unsafe {
        device
            .as_hal::<A>()
            .ok_or_else(|| GpuError::BackendMismatch(format!("{backend_name}:device")))?
    };
    let buf = unsafe {
        buffer
            .as_hal::<A>()
            .ok_or_else(|| GpuError::BackendMismatch(format!("{backend_name}:buffer")))?
    };
    // SAFETY: caller guarantees the buffer has MAP_* usage and is mapped once.
    unsafe { wgpu_hal::Device::map_buffer(&*dev, &*buf, range) }
        .map_err(|e| GpuError::Mapping(e.to_string()))
}

unsafe fn flush_impl<A: wgpu_hal::Api>(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    ranges: impl Iterator<Item = MemoryRange>,
    backend_name: &'static str,
) -> Result<(), GpuError> {
    // SAFETY: `as_hal` is only sound while the wgpu handle stays alive, which
    // is guaranteed by the caller's borrow of `device`/`buffer`.
    let dev = unsafe {
        device
            .as_hal::<A>()
            .ok_or_else(|| GpuError::BackendMismatch(format!("{backend_name}:device")))?
    };
    let buf = unsafe {
        buffer
            .as_hal::<A>()
            .ok_or_else(|| GpuError::BackendMismatch(format!("{backend_name}:buffer")))?
    };
    // SAFETY: caller guarantees the buffer is mapped and ranges are in bounds.
    unsafe { wgpu_hal::Device::flush_mapped_ranges(&*dev, &*buf, ranges) };
    Ok(())
}

unsafe fn invalidate_impl<A: wgpu_hal::Api>(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    ranges: impl Iterator<Item = MemoryRange>,
    backend_name: &'static str,
) -> Result<(), GpuError> {
    // SAFETY: `as_hal` is only sound while the wgpu handle stays alive, which
    // is guaranteed by the caller's borrow of `device`/`buffer`.
    let dev = unsafe {
        device
            .as_hal::<A>()
            .ok_or_else(|| GpuError::BackendMismatch(format!("{backend_name}:device")))?
    };
    let buf = unsafe {
        buffer
            .as_hal::<A>()
            .ok_or_else(|| GpuError::BackendMismatch(format!("{backend_name}:buffer")))?
    };
    // SAFETY: caller guarantees the buffer is mapped and ranges are in bounds.
    unsafe { wgpu_hal::Device::invalidate_mapped_ranges(&*dev, &*buf, ranges) };
    Ok(())
}

unsafe fn unmap_impl<A: wgpu_hal::Api>(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    backend_name: &'static str,
) -> Result<(), GpuError> {
    // SAFETY: `as_hal` is only sound while the wgpu handle stays alive, which
    // is guaranteed by the caller's borrow of `device`/`buffer`.
    let dev = unsafe {
        device
            .as_hal::<A>()
            .ok_or_else(|| GpuError::BackendMismatch(format!("{backend_name}:device")))?
    };
    let buf = unsafe {
        buffer
            .as_hal::<A>()
            .ok_or_else(|| GpuError::BackendMismatch(format!("{backend_name}:buffer")))?
    };
    // SAFETY: caller guarantees the buffer is currently mapped.
    unsafe { wgpu_hal::Device::unmap_buffer(&*dev, &*buf) };
    Ok(())
}

/// CPU view of a persistently mapped buffer.
pub struct MappedBuffer {
    pub ptr: NonNull<u8>,
    pub size: u64,
    pub is_coherent: bool,
    pub backend: HalBackend,
}

// The raw pointer inside is only dereferenced through `&mut self` methods on
// the owning pool, which is `Send`. See `GpuPool`.
// SAFETY: exclusive access is enforced by `&mut self` on `GpuPool`.
unsafe impl Send for MappedBuffer {}

// SAFETY: all construction/use of MappedBuffer happens behind `&mut`.
unsafe impl Sync for MappedBuffer {}