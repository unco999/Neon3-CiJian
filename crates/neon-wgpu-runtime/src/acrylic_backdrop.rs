//! Windows-only acrylic backdrop host for the windowed runtime.
//!
//! Wraps the verified probe chain (see `src/bin/backdrop_wgpu_proxy_probe.rs`):
//!
//!   1. WinRT composition tree: GaussianBlur(backdrop brush) as the base layer,
//!      attached to the window via `DesktopWindowTarget`.
//!   2. A `CompositionDrawingSurface` content layer driven by wgpu through a
//!      shared D3D12 texture. `ICompositorInterop::CreateGraphicsDevice` requires
//!      an `IDXGIDevice` (D3D11), so a D3D11 device opens the shared texture and
//!      `CopyResource`s it into the drawing surface each frame (proxy copy).
//!   3. `CreateSurfaceBrushWithSurface` + SpriteVisual above the blur layer.
//!
//! The composition tree replaces the wgpu swapchain as the final presentation
//! target; wgpu renders into the shared texture instead of a window surface.

use windows::core::{implement, Interface, PCWSTR};
use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectSource_Impl, IGraphicsEffect_Impl,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    Compositor, CompositionBackdropBrush, CompositionDrawingSurface, CompositionEffectBrush,
    CompositionEffectSourceParameter, CompositionGraphicsDevice, CompositionStretch,
    ContainerVisual, SpriteVisual,
};
use windows::Win32::Foundation::{GENERIC_ALL, HANDLE, POINT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_BOX, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT,
    D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
    D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_STATE_COMMON, ID3D12Device,
    ID3D12Resource, D3D12_TEXTURE_LAYOUT_UNKNOWN,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::System::WinRT::Composition::{
    ICompositorDesktopInterop, ICompositorInterop, ICompositionDrawingSurfaceInterop,
};
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
    IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl,
};
use windows_numerics::Vector2;

// ---------------------------------------------------------------------------
// GaussianBlur effect (hand-rolled; CLSID_D2D1GaussianBlur from d2d1effects.h)
// ---------------------------------------------------------------------------

const CLSID_D2D1_GAUSSIAN_BLUR: windows::core::GUID = windows::core::GUID::from_values(
    0x1feb6d69,
    0x2fe6,
    0x4ac9,
    [0x8c, 0x58, 0x1d, 0x7f, 0x93, 0xe7, 0xa6, 0xa5],
);

fn invalid_param() -> windows::core::Error {
    windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057u32 as i32))
}

fn prop_str(name: &PCWSTR) -> String {
    let prop = unsafe { name.as_wide() };
    prop.iter()
        .map(|&c| char::from_u32(c as u32).unwrap_or('?'))
        .collect()
}

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct GaussianBlurEffect {
    name: std::cell::RefCell<windows::core::HSTRING>,
    blur_amount: f32,
    optimization: u32,
    border_mode: u32,
    source: IGraphicsEffectSource,
}

impl IGraphicsEffectSource_Impl for GaussianBlurEffect_Impl {}

impl IGraphicsEffect_Impl for GaussianBlurEffect_Impl {
    fn Name(&self) -> windows::core::Result<windows::core::HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &windows::core::HSTRING) -> windows::core::Result<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlurEffect_Impl {
    fn GetEffectId(&self) -> windows::core::Result<windows::core::GUID> {
        Ok(CLSID_D2D1_GAUSSIAN_BLUR)
    }
    fn GetNamedPropertyMapping(
        &self,
        name: &PCWSTR,
        index: *mut u32,
        mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> windows::core::Result<()> {
        let prop = prop_str(name);
        let mapped = if prop == "BlurAmount" {
            Some(0)
        } else if prop == "Optimization" {
            Some(1)
        } else if prop == "BorderMode" {
            Some(2)
        } else {
            None
        };
        match mapped {
            Some(i) => unsafe {
                *index = i;
                *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
            },
            None => return Err(invalid_param()),
        }
        Ok(())
    }
    fn GetPropertyCount(&self) -> windows::core::Result<u32> {
        Ok(3)
    }
    fn GetProperty(&self, index: u32) -> windows::core::Result<IPropertyValue> {
        let inspectable = match index {
            0 => PropertyValue::CreateSingle(self.blur_amount)?,
            1 => PropertyValue::CreateUInt32(self.optimization)?,
            2 => PropertyValue::CreateUInt32(self.border_mode)?,
            _ => return Err(invalid_param()),
        };
        inspectable.cast()
    }
    fn GetSource(&self, index: u32) -> windows::core::Result<IGraphicsEffectSource> {
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(invalid_param())
        }
    }
    fn GetSourceCount(&self) -> windows::core::Result<u32> {
        Ok(1)
    }
}

// ---------------------------------------------------------------------------
// Acrylic host: owns the composition tree and the proxy-copy bridge
// ---------------------------------------------------------------------------

pub struct AcrylicHost {
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _effect_brush: CompositionEffectBrush,
    _backdrop_brush: CompositionBackdropBrush,
    _blur_sprite: SpriteVisual,
    _graphics_device: CompositionGraphicsDevice,
    _drawing_surface: CompositionDrawingSurface,
    _content_sprite: SpriteVisual,
    _swapchain_sprite: Option<SpriteVisual>,
    _root: ContainerVisual,
    d3d11_device: ID3D11Device,
    d3d11_context: ID3D11DeviceContext,
    #[allow(dead_code)]
    width: u32,
    #[allow(dead_code)]
    height: u32,
    /// Shared D3D12 texture that wgpu renders into; opened by the D3D11 device
    /// every frame via `OpenSharedResource1` and copied into the drawing
    /// surface. Not a kernel-handle ownership issue: the handle is closed once
    /// the D3D11 texture has been opened.
    shared_handle: HANDLE,
}

#[derive(Debug)]
pub enum AcrylicError {
    Message(String),
}

impl std::fmt::Display for AcrylicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for AcrylicError {}

impl AcrylicHost {
    /// Build the full acrylic composition tree for the given HWND + size.
    /// `wgpu` device is rendered into a shared texture that is copied into the
    /// drawing surface on every `present_frame`.
    pub fn new(
        hwnd: windows::Win32::Foundation::HWND,
        width: u32,
        height: u32,
    ) -> Result<Self, AcrylicError> {
        let compositor = Compositor::new()
            .map_err(|e| AcrylicError::Message(format!("Compositor::new: {e:?}")))?;
        let desktop_interop: ICompositorDesktopInterop = compositor
            .cast()
            .map_err(|e| AcrylicError::Message(format!("cast ICompositorDesktopInterop: {e:?}")))?;
        let target = unsafe { desktop_interop.CreateDesktopWindowTarget(hwnd, false) }
            .map_err(|e| AcrylicError::Message(format!("CreateDesktopWindowTarget: {e:?}")))?;

        // -- Backdrop blur base layer --------------------------------------
        let source_param: IGraphicsEffectSource =
            CompositionEffectSourceParameter::Create(&windows::core::HSTRING::from("backdrop"))
                .map_err(|e| AcrylicError::Message(format!("create source param: {e:?}")))?
                .cast()
                .map_err(|e| AcrylicError::Message(format!("cast source param: {e:?}")))?;
        let blur_amount: f32 = std::env::var("NEON_BLUR_AMOUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(16.0);
        let effect = GaussianBlurEffect {
            name: std::cell::RefCell::new(windows::core::HSTRING::from("GaussianBlurEffect")),
            blur_amount,
            optimization: 1u32,
            border_mode: 0u32,
            source: source_param,
        };
        let effect_interface: IGraphicsEffect = effect.into();
        let factory = compositor
            .CreateEffectFactory(&effect_interface)
            .map_err(|e| AcrylicError::Message(format!("CreateEffectFactory: {e:?}")))?;
        let effect_brush = factory
            .CreateBrush()
            .map_err(|e| AcrylicError::Message(format!("factory.CreateBrush: {e:?}")))?;
        let backdrop_brush = compositor
            .CreateBackdropBrush()
            .map_err(|e| AcrylicError::Message(format!("CreateBackdropBrush: {e:?}")))?;
        effect_brush
            .SetSourceParameter(&windows::core::HSTRING::from("backdrop"), &backdrop_brush)
            .map_err(|e| AcrylicError::Message(format!("SetSourceParameter: {e:?}")))?;

        let blur_sprite = compositor
            .CreateSpriteVisual()
            .map_err(|e| AcrylicError::Message(format!("CreateSpriteVisual: {e:?}")))?;
        blur_sprite
            .SetBrush(&effect_brush)
            .map_err(|e| AcrylicError::Message(format!("blur sprite.SetBrush: {e:?}")))?;
        blur_sprite
            .SetSize(Vector2::new(width.max(1) as f32, height.max(1) as f32))
            .map_err(|e| AcrylicError::Message(format!("blur sprite.SetSize: {e:?}")))?;

        let root = compositor
            .CreateContainerVisual()
            .map_err(|e| AcrylicError::Message(format!("CreateContainerVisual: {e:?}")))?;
        root.Children()
            .map_err(|e| AcrylicError::Message(format!("root.Children: {e:?}")))?
            .InsertAtTop(&blur_sprite)
            .map_err(|e| AcrylicError::Message(format!("InsertAtTop blur: {e:?}")))?;

        // -- D3D11 graphics device + drawing surface ------------------------
        let mut d3d11_device: Option<ID3D11Device> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE(std::ptr::null_mut()),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d11_device),
                None,
                None,
            )
        }
        .map_err(|e| AcrylicError::Message(format!("D3D11CreateDevice: {e:?}")))?;
        let d3d11_device = d3d11_device
            .ok_or_else(|| AcrylicError::Message("null D3D11 device".into()))?;
        let d3d11_context = unsafe { d3d11_device.GetImmediateContext() }
            .map_err(|e| AcrylicError::Message(format!("GetImmediateContext: {e:?}")))?;

        let compositor_interop: ICompositorInterop = compositor
            .cast()
            .map_err(|e| AcrylicError::Message(format!("cast ICompositorInterop: {e:?}")))?;
        let graphics_device = unsafe { compositor_interop.CreateGraphicsDevice(&d3d11_device) }
            .map_err(|e| AcrylicError::Message(format!("CreateGraphicsDevice: {e:?}")))?;
        let drawing_surface = graphics_device
            .CreateDrawingSurface2(
                windows::Graphics::SizeInt32 {
                    Width: width.max(1) as i32,
                    Height: height.max(1) as i32,
                },
                DirectXPixelFormat::R8G8B8A8UIntNormalized,
                DirectXAlphaMode::Premultiplied,
            )
            .map_err(|e| AcrylicError::Message(format!("CreateDrawingSurface2: {e:?}")))?;

        // -- Content sprite above the blur layer ----------------------------
        let surface_brush = compositor
            .CreateSurfaceBrushWithSurface(&drawing_surface)
            .map_err(|e| AcrylicError::Message(format!("CreateSurfaceBrushWithSurface: {e:?}")))?;
        surface_brush
            .SetStretch(CompositionStretch::Fill)
            .map_err(|e| AcrylicError::Message(format!("content brush.SetStretch: {e:?}")))?;
        let content_sprite = compositor
            .CreateSpriteVisual()
            .map_err(|e| AcrylicError::Message(format!("content CreateSpriteVisual: {e:?}")))?;
        content_sprite
            .SetBrush(&surface_brush)
            .map_err(|e| AcrylicError::Message(format!("content sprite.SetBrush: {e:?}")))?;
        content_sprite
            .SetSize(Vector2::new(width.max(1) as f32, height.max(1) as f32))
            .map_err(|e| AcrylicError::Message(format!("content sprite.SetSize: {e:?}")))?;
        root.Children()
            .map_err(|e| AcrylicError::Message(format!("root.Children content: {e:?}")))?
            .InsertAtTop(&content_sprite)
            .map_err(|e| AcrylicError::Message(format!("InsertAtTop content: {e:?}")))?;
        target
            .SetRoot(&root)
            .map_err(|e| AcrylicError::Message(format!("target.SetRoot: {e:?}")))?;

        // -- Shared D3D12 texture placeholder handle (created by the wgpu
        //    renderer and passed to `set_shared_handle`). We still open one
        //    here so `present_frame` has a startable path; the runtime updates
        //    it after creating the wgpu shared texture.
        let shared_handle = HANDLE(0 as *mut _);

        Ok(Self {
            _compositor: compositor,
            _target: target,
            _effect_brush: effect_brush,
            _backdrop_brush: backdrop_brush,
            _blur_sprite: blur_sprite,
            _graphics_device: graphics_device,
            _drawing_surface: drawing_surface,
            _content_sprite: content_sprite,
            _swapchain_sprite: None,
            _root: root,
            d3d11_device,
            d3d11_context,
            width: width.max(1),
            height: height.max(1),
            shared_handle,
        })
    }

    /// Replace the diagnostic drawing-surface content with the native wgpu
    /// DXGI swapchain. This is the official WinRT interop path and avoids a
    /// D3D12 -> D3D11 texture copy.
    pub fn attach_swapchain(
        &mut self,
        swapchain: &windows::Win32::Graphics::Dxgi::IDXGISwapChain3,
    ) -> Result<(), AcrylicError> {
        self._content_sprite
            .SetOpacity(0.0)
            .map_err(|e| AcrylicError::Message(format!("hide drawing-surface sprite: {e:?}")))?;
        let compositor_interop: ICompositorInterop = self
            ._compositor
            .cast()
            .map_err(|e| AcrylicError::Message(format!("cast compositor interop: {e:?}")))?;
        let composition_surface = unsafe {
            compositor_interop.CreateCompositionSurfaceForSwapChain(swapchain)
        }
        .map_err(|e| AcrylicError::Message(format!("CreateCompositionSurfaceForSwapChain: {e:?}")))?;
        let brush = self
            ._compositor
            .CreateSurfaceBrushWithSurface(&composition_surface)
            .map_err(|e| AcrylicError::Message(format!("swapchain surface brush: {e:?}")))?;
        brush
            .SetStretch(CompositionStretch::Fill)
            .map_err(|e| AcrylicError::Message(format!("swapchain brush stretch: {e:?}")))?;
        let sprite = self
            ._compositor
            .CreateSpriteVisual()
            .map_err(|e| AcrylicError::Message(format!("swapchain sprite: {e:?}")))?;
        sprite
            .SetBrush(&brush)
            .map_err(|e| AcrylicError::Message(format!("swapchain sprite brush: {e:?}")))?;
        sprite
            .SetSize(Vector2::new(self.width as f32, self.height as f32))
            .map_err(|e| AcrylicError::Message(format!("swapchain sprite size: {e:?}")))?;
        self._root
            .Children()
            .map_err(|e| AcrylicError::Message(format!("root children: {e:?}")))?
            .InsertAtTop(&sprite)
            .map_err(|e| AcrylicError::Message(format!("insert swapchain sprite: {e:?}")))?;
        self._swapchain_sprite = Some(sprite);
        Ok(())
    }

    /// Update the shared texture handle wgpu renders into (set after the wgpu
    /// shared surface is created).
    pub fn set_shared_handle(&mut self, handle: HANDLE) {
        self.shared_handle = handle;
    }

    /// Update drawing surface + sprite sizes on window resize.
    pub fn resize(&self, width: u32, height: u32) -> Result<(), AcrylicError> {
        if let Some(sprite) = self._swapchain_sprite.as_ref() {
            sprite
                .SetSize(Vector2::new(width.max(1) as f32, height.max(1) as f32))
                .map_err(|e| AcrylicError::Message(format!("resize swapchain sprite: {e:?}")))?;
        }
        self._content_sprite
            .SetSize(Vector2::new(width.max(1) as f32, height.max(1) as f32))
            .map_err(|e| AcrylicError::Message(format!("resize content sprite: {e:?}")))?;
        self._blur_sprite
            .SetSize(Vector2::new(width.max(1) as f32, height.max(1) as f32))
            .map_err(|e| AcrylicError::Message(format!("resize blur sprite: {e:?}")))?;
        let surface_interop: ICompositionDrawingSurfaceInterop = self
            ._drawing_surface
            .cast()
            .map_err(|e| AcrylicError::Message(format!("cast surface interop: {e:?}")))?;
        unsafe {
            surface_interop.Resize(windows::Win32::Foundation::SIZE {
                cx: width.max(1) as i32,
                cy: height.max(1) as i32,
            })
        }
        .map_err(|e| AcrylicError::Message(format!("Resize drawing surface: {e:?}")))?;
        Ok(())
    }

    /// Copy the shared wgpu texture into the drawing surface and end the draw.
    pub fn present_frame(&self, frame_sequence: u64) -> Result<(), AcrylicError> {
        if self.shared_handle.0.is_null() {
            return Err(AcrylicError::Message("shared texture handle not set".into()));
        }
        // Open the shared D3D12 texture on the D3D11 device (ID3D11Device1).
        let d3d11_1: ID3D11Device1 = self
            .d3d11_device
            .cast()
            .map_err(|e| AcrylicError::Message(format!("cast ID3D11Device1: {e:?}")))?;
        let shared_d3d11: ID3D11Texture2D = unsafe {
            d3d11_1
                .OpenSharedResource1::<ID3D11Texture2D>(self.shared_handle)
        }
        .map_err(|e| AcrylicError::Message(format!("OpenSharedResource1: {e:?}")))?;

        let surface_interop: ICompositionDrawingSurfaceInterop = self
            ._drawing_surface
            .cast()
            .map_err(|e| AcrylicError::Message(format!("cast ICompositionDrawingSurfaceInterop: {e:?}")))?;
        let mut offset = POINT::default();
        let dst: ID3D11Texture2D = unsafe {
            surface_interop.BeginDraw::<ID3D11Texture2D>(None, &mut offset)
        }
        .map_err(|e| AcrylicError::Message(format!("BeginDraw: {e:?}")))?;
        let mut source_desc = Default::default();
        let mut destination_desc = Default::default();
        unsafe {
            shared_d3d11.GetDesc(&mut source_desc);
            dst.GetDesc(&mut destination_desc);
        }
        let copy_width = source_desc.Width.min(
            destination_desc
                .Width
                .saturating_sub(offset.x.max(0) as u32),
        );
        let copy_height = source_desc.Height.min(
            destination_desc
                .Height
                .saturating_sub(offset.y.max(0) as u32),
        );
        if copy_width == 0 || copy_height == 0 {
            unsafe { surface_interop.EndDraw() }
                .map_err(|e| AcrylicError::Message(format!("EndDraw after empty region: {e:?}")))?;
            return Err(AcrylicError::Message(format!(
                "empty copy region: source={}x{}, destination={}x{}, offset=[{},{}]",
                source_desc.Width,
                source_desc.Height,
                destination_desc.Width,
                destination_desc.Height,
                offset.x,
                offset.y
            )));
        }
        let source_box = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: copy_width,
            bottom: copy_height,
            back: 1,
        };
        if std::env::var("NEON_DIAG") == Ok("opaque".into()) {
            eprintln!(
                "{}",
                serde_json::json!({
                    "event": "neon.acrylic.present",
                    "producer_frame": frame_sequence,
                    "consumer_draw_sequence": frame_sequence,
                    "offset": [offset.x, offset.y],
                    "source_size": [source_desc.Width, source_desc.Height],
                    "destination_size": [destination_desc.Width, destination_desc.Height],
                    "copy_size": [copy_width, copy_height],
                    "shared_handle_set": !self.shared_handle.0.is_null(),
                    "frame": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
                })
            );
        }
        unsafe {
            self.d3d11_context.CopySubresourceRegion(
                &dst,
                0,
                offset.x.max(0) as u32,
                offset.y.max(0) as u32,
                0,
                &shared_d3d11,
                0,
                Some(&source_box),
            );
        }
        unsafe { surface_interop.EndDraw() }
            .map_err(|e| AcrylicError::Message(format!("EndDraw: {e:?}")))?;
        Ok(())
    }
}

/// Create a shared D3D12 texture that wgpu can render into, and return the
/// wgpu view of it plus the shared NT handle for the D3D11 copier.
#[cfg(windows)]
pub struct WgpuSharedTarget {
    pub texture: wgpu::Texture,
    pub shared_handle: HANDLE,
    pub _resource: ID3D12Resource,
}

#[cfg(windows)]
pub fn create_wgpu_shared_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> Result<WgpuSharedTarget, AcrylicError> {
    let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
        .ok_or_else(|| AcrylicError::Message("device is not DX12".into()))?;
    let raw_device: &ID3D12Device = hal_device.raw_device();
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: u64::from(width.max(1)),
        Height: height.max(1),
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
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
            .map_err(|e| AcrylicError::Message(format!("CreateCommittedResource: {e:?}")))?;
        resource.ok_or_else(|| AcrylicError::Message("null shared resource".into()))?
    };
    let shared_handle = unsafe {
        raw_device
            .CreateSharedHandle(&resource, None, GENERIC_ALL.0, PCWSTR::null())
            .map_err(|e| AcrylicError::Message(format!("CreateSharedHandle: {e:?}")))? 
    };
    let hal_texture = unsafe {
        wgpu::hal::dx12::Device::texture_from_raw(
            resource.clone(),
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureDimension::D2,
            wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
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
                label: Some("neon3-acrylic-shared-target"),
                size: wgpu::Extent3d {
                    width: width.max(1),
                    height: height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::COLOR_TARGET,
        )
    };
    Ok(WgpuSharedTarget {
        texture,
        shared_handle,
        _resource: resource,
    })
}
