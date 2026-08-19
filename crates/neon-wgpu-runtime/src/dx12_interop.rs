//! Windows D3D12 interop owned by `neon-wgpu-runtime`.
//!
//! This module never exports raw handles through the public protocol. The caller
//! must pass the returned handles to the local broker, which duplicates them into
//! the external consumer process and then applies the protocol broker token.

use std::fmt;

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{
            CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, GENERIC_ALL, HANDLE, LUID,
        },
        Graphics::{
            Direct3D12::{
                D3D12_FENCE_FLAG_SHARED, D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES,
                D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_STATE_COMMON,
                D3D12_TEXTURE_LAYOUT_UNKNOWN,
                ID3D12Device, ID3D12Fence, ID3D12Resource,
            },
            Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R32_UINT, DXGI_SAMPLE_DESC},
        },
        System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
    },
};

#[derive(Debug)]
pub enum Error {
    WrongBackend,
    MissingHalDevice,
    MissingHalAdapter,
    AdapterDescription(String),
    CreateResource(String),
    CreateFence(String),
    CreateSharedHandle(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongBackend => write!(f, "WGPU device is not using DX12"),
            Self::MissingHalDevice => write!(f, "DX12 HAL device is unavailable"),
            Self::MissingHalAdapter => write!(f, "DX12 HAL adapter is unavailable"),
            Self::AdapterDescription(error) => write!(f, "read DX12 adapter description: {error}"),
            Self::CreateResource(error) => write!(f, "create shared DX12 resource: {error}"),
            Self::CreateFence(error) => write!(f, "create shared DX12 fence: {error}"),
            Self::CreateSharedHandle(error) => write!(f, "create shared DX12 handle: {error}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInfo {
    pub luid: String,
    pub name: String,
}

#[derive(Debug)]
pub struct SharedSurface {
    pub texture: wgpu::Texture,
    pub texture_handle: HANDLE,
    pub fence: ID3D12Fence,
    pub fence_handle: HANDLE,
    pub consumer_fence: ID3D12Fence,
    pub consumer_fence_handle: HANDLE,
    pub adapter: AdapterInfo,
    pub width: u32,
    pub height: u32,
    pub frame_sequence: u64,
}

// `HANDLE` wraps a raw pointer and is therefore not auto-`Send`/`Sync`, but it
// is just a process-scoped kernel handle value (safe to move between threads in
// the same process). The owning `HeadlessExternalGpu` serializes access through
// a `Mutex`, so this is sound.
unsafe impl Send for SharedSurface {}
unsafe impl Sync for SharedSurface {}

pub fn duplicate_handle_to_process(handle: HANDLE, target_pid: u32) -> Result<usize, Error> {
    let target = unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, target_pid) }
        .map_err(|error| Error::CreateSharedHandle(format!("open target process: {error}")))?;
    let mut duplicated = HANDLE::default();
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle,
            target,
            &mut duplicated,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
        .map_err(|error| Error::CreateSharedHandle(format!("duplicate handle: {error}")))?;
        CloseHandle(target).ok();
    }
    Ok(duplicated.0 as usize)
}

pub fn adapter_info(adapter: &wgpu::Adapter) -> Result<AdapterInfo, Error> {
    let hal_adapter = unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() }
        .ok_or(Error::MissingHalAdapter)?;
    let desc = unsafe { hal_adapter.raw_adapter().GetDesc2() }
        .map_err(|error| Error::AdapterDescription(error.to_string()))?;
    Ok(AdapterInfo {
        luid: format_luid(desc.AdapterLuid),
        name: String::from_utf16_lossy(
            &desc
                .Description
                .iter()
                .copied()
                .take_while(|value| *value != 0)
                .collect::<Vec<_>>(),
        ),
    })
}

pub fn create_shared_surface(
    device: &wgpu::Device,
    adapter: &wgpu::Adapter,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> Result<SharedSurface, Error> {
    let adapter_info = adapter_info(adapter)?;
    let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
        .ok_or(Error::MissingHalDevice)?;
    let raw_device: &ID3D12Device = hal_device.raw_device();
    let dxgi_format = match format {
        wgpu::TextureFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        wgpu::TextureFormat::R32Uint => DXGI_FORMAT_R32_UINT,
        _ => return Err(Error::CreateResource("unsupported shared surface format".into())),
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: u64::from(width),
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: dxgi_format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        // Both color and ID shared surfaces are cleared and rendered by the
        // Neon owner before Bevy samples them. D3D12 requires this capability
        // to be declared when the resource is created.
        Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
    };
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: Default::default(),
        MemoryPoolPreference: Default::default(),
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };
    let resource: ID3D12Resource = unsafe {
        let mut resource = None;
        raw_device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
            .map_err(|error| Error::CreateResource(error.to_string()))?;
        resource.ok_or_else(|| Error::CreateResource("D3D12 returned a null resource".into()))?
    };
    let texture_handle = unsafe {
        raw_device
            .CreateSharedHandle(&resource, None, GENERIC_ALL.0, PCWSTR::null())
            .map_err(|error| Error::CreateSharedHandle(error.to_string()))?
    };
    let fence: ID3D12Fence = unsafe {
        raw_device
            .CreateFence(0, D3D12_FENCE_FLAG_SHARED)
            .map_err(|error| Error::CreateFence(error.to_string()))?
    };
    let fence_handle = unsafe {
        raw_device
            .CreateSharedHandle(&fence, None, GENERIC_ALL.0, PCWSTR::null())
            .map_err(|error| Error::CreateSharedHandle(error.to_string()))?
    };
    let consumer_fence: ID3D12Fence = unsafe {
        raw_device
            .CreateFence(0, D3D12_FENCE_FLAG_SHARED)
            .map_err(|error| Error::CreateFence(error.to_string()))?
    };
    let consumer_fence_handle = unsafe {
        raw_device
            .CreateSharedHandle(&consumer_fence, None, GENERIC_ALL.0, PCWSTR::null())
            .map_err(|error| Error::CreateSharedHandle(error.to_string()))?
    };
    let hal_texture = unsafe {
        wgpu::hal::dx12::Device::texture_from_raw(
            resource.clone(),
            format,
            wgpu::TextureDimension::D2,
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            1,
            1,
        )
    };
    let texture = unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("neon3-external-shared-surface"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::COLOR_TARGET,
        )
    };
    Ok(SharedSurface {
        texture,
        texture_handle,
        fence,
        fence_handle,
        consumer_fence,
        consumer_fence_handle,
        adapter: adapter_info,
        width,
        height,
        frame_sequence: 0,
    })
}

fn format_luid(luid: LUID) -> String {
    format!("{:08x}{:08x}", luid.HighPart as u32, luid.LowPart)
}
