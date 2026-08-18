//! Bevy-side D3D12 external texture import.
//!
//! This uses the wgpu version selected by Bevy 0.19.1 (29.0.4), independently
//! from Neon3's wgpu version. Only native D3D12 COM resources cross the process
//! boundary.

use std::fmt;

use windows::{
    Win32::{
        Foundation::HANDLE,
        Graphics::Direct3D12::{ID3D12Device, ID3D12Resource},
    },
};

#[derive(Debug)]
pub enum ImportError {
    NotDx12,
    HalDeviceUnavailable,
    OpenSharedHandle(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDx12 => write!(f, "Bevy RenderDevice is not using DX12"),
            Self::HalDeviceUnavailable => write!(f, "Bevy DX12 HAL device is unavailable"),
            Self::OpenSharedHandle(error) => write!(f, "open shared D3D12 texture: {error}"),
        }
    }
}

impl std::error::Error for ImportError {}

#[derive(Debug)]
pub struct ImportedTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

pub fn import_texture(
    device: &wgpu::Device,
    handle: usize,
    size: [u32; 2],
    format: wgpu::TextureFormat,
) -> Result<ImportedTexture, ImportError> {
    let hal_device = unsafe { device.as_hal::<wgpu_hal::api::Dx12>() }
        .ok_or(ImportError::HalDeviceUnavailable)?;
    let raw_device: &ID3D12Device = hal_device.raw_device();
    let raw_resource: ID3D12Resource = unsafe {
        let mut resource = None;
        raw_device
            .OpenSharedHandle(
                HANDLE(handle as *mut std::ffi::c_void),
                &mut resource,
            )
            .map_err(|error| ImportError::OpenSharedHandle(error.to_string()))?;
        resource.ok_or_else(|| ImportError::OpenSharedHandle("null resource".into()))?
    };
    let hal_texture = unsafe {
        wgpu_hal::dx12::Device::texture_from_raw(
            raw_resource,
            format,
            wgpu::TextureDimension::D2,
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            1,
            1,
        )
    };
    let texture = unsafe {
        device.create_texture_from_hal::<wgpu_hal::api::Dx12>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("neon3-bevy-external-surface"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        )
    };
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(ImportedTexture { texture, view })
}
