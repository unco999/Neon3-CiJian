//! Composition backdrop + wgpu content overlay via proxy copy.
//!
//! Chain:
//!   1. WinRT composition tree: GaussianBlur(backdrop) base layer.
//!   2. wgpu (DX12) renders UI into a shared D3D12 texture
//!      (D3D12_HEAP_FLAG_SHARED, via HAL interop).
//!   3. A D3D11 device opens the shared texture
//!      (ID3D11Device1::OpenSharedResource1) and copies it into a
//!      CompositionDrawingSurface (ICompositionDrawingSurfaceInterop
//!      BeginDraw/EndDraw -> ID3D11Texture2D), one GPU copy per frame.
//!      `ICompositorInterop::CreateGraphicsDevice` requires an IDXGIDevice,
//!      which D3D12 does not implement, hence the D3D11 device.
//!   4. CreateSurfaceBrushWithSurface + SpriteVisual above the blur layer.
//!
//! Emits JSONL stage records.

use std::time::Duration;

use windows::core::{implement, Interface, PCWSTR};
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectSource_Impl, IGraphicsEffect_Impl,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    Compositor, CompositionBackdropBrush, CompositionDrawingSurface, CompositionEffectBrush,
    CompositionEffectSourceParameter, CompositionGraphicsDevice, ContainerVisual, SpriteVisual,
};
use windows::Win32::Foundation::{GENERIC_ALL, POINT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, ID3D11Device,
    ID3D11DeviceContext, ID3D11Device1, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::ID3D12Resource;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::{
    ICompositorDesktopInterop, ICompositorInterop, ICompositionDrawingSurfaceInterop,
};
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
    IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl,
};
use windows_numerics::Vector2;

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};
#[cfg(windows)]
use winit::platform::windows::WindowAttributesExtWindows;
#[cfg(windows)]
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

// ---------------------------------------------------------------------------
// GaussianBlurEffect (hand-rolled; same as the backdrop probe)
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

use windows::Foundation::{IPropertyValue, PropertyValue};

// ---------------------------------------------------------------------------
// Standalone D3D11 device (owns the composition graphics device)
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn create_d3d11_device() -> Result<ID3D11Device, String> {
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE(std::ptr::null_mut()),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .map_err(|e| format!("D3D11CreateDevice: {e:?}"))?;
    device.ok_or_else(|| "null D3D11 device".to_string())
}

// ---------------------------------------------------------------------------
// wgpu offscreen renderer: renders into a shared D3D12 texture
// ---------------------------------------------------------------------------

struct WgpuRenderer {
    shared_texture: wgpu::Texture,
    shared_resource: ID3D12Resource,
    shared_handle: windows::Win32::Foundation::HANDLE,
    width: u32,
    height: u32,
    pipeline: wgpu::RenderPipeline,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

#[cfg(windows)]
fn create_wgpu_renderer(width: u32, height: u32) -> Result<WgpuRenderer, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        backend_options: Default::default(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .map_err(|e| format!("request adapter: {e:?}"))?;
    let (device, queue) = pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-proxy-wgpu"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }),
    )
    .map_err(|e| format!("request device: {e:?}"))?;

    // Shared D3D12 texture (D3D12_HEAP_FLAG_SHARED) that wgpu renders into.
    let hal_device = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
        .ok_or_else(|| "device is not DX12".to_string())?;
    let raw_device: &windows::Win32::Graphics::Direct3D12::ID3D12Device =
        hal_device.raw_device();
    let resource_desc = windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_DESC {
        Dimension: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: u64::from(width.max(1)),
        Height: height.max(1),
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: windows::Win32::Graphics::Direct3D12::D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
    };
    let heap = windows::Win32::Graphics::Direct3D12::D3D12_HEAP_PROPERTIES {
        Type: windows::Win32::Graphics::Direct3D12::D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: Default::default(),
        MemoryPoolPreference: Default::default(),
        CreationNodeMask: 0,
        VisibleNodeMask: 0,
    };
    let shared_resource: ID3D12Resource = unsafe {
        let mut resource = None;
        raw_device
            .CreateCommittedResource(
                &heap,
                windows::Win32::Graphics::Direct3D12::D3D12_HEAP_FLAG_SHARED,
                &resource_desc,
                windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
            .map_err(|e| format!("CreateCommittedResource: {e:?}"))?;
        resource.ok_or_else(|| "null shared resource".to_string())?
    };
    let shared_handle = unsafe {
        raw_device
            .CreateSharedHandle(&shared_resource, None, GENERIC_ALL.0, PCWSTR::null())
            .map_err(|e| format!("CreateSharedHandle: {e:?}"))?
    };
    println!(
        "{}",
        serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"shared-texture","width":width,"height":height,"pass":true})
    );

    // Wrap as wgpu texture.
    let hal_texture = unsafe {
        wgpu::hal::dx12::Device::texture_from_raw(
            shared_resource.clone(),
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
    let shared_texture = unsafe {
        device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("neon3-proxy-shared"),
                size: wgpu::Extent3d {
                    width: width.max(1),
                    height: height.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::COLOR_TARGET,
        )
    };

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("proxy-shader"),
        source: wgpu::ShaderSource::Wgsl(
            "@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32>{\
             var p=array<vec2<f32>,6>(vec2<f32>(-0.6,-0.6),vec2<f32>(0.6,-0.6),vec2<f32>(-0.6,0.6),vec2<f32>(0.6,-0.6),vec2<f32>(0.6,0.6),vec2<f32>(-0.6,0.6));\
             return vec4<f32>(p[i],0.0,1.0);}\
             @fragment fn fs() -> @location(0) vec4<f32>{return vec4<f32>(0.95,0.45,0.1,0.9);}"
                .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("proxy-pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8Unorm,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });

    Ok(WgpuRenderer {
        device,
        queue,
        pipeline,
        shared_texture,
        shared_resource,
        shared_handle,
        width: width.max(1),
        height: height.max(1),
    })
}

#[cfg(windows)]
fn wgpu_render_frame(renderer: &WgpuRenderer) -> Result<(), String> {
    let view = renderer.shared_texture.create_view(&Default::default());
    let mut encoder = renderer.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("proxy-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&renderer.pipeline);
        pass.draw(0..6, 0..1);
    }
    renderer.queue.submit(Some(encoder.finish()));
    Ok(())
}

// ---------------------------------------------------------------------------
// Proxy copy: shared D3D12 texture -> D3D11 texture in the drawing surface
// ---------------------------------------------------------------------------

struct ProxyCopier {
    d3d11_device: ID3D11Device,
    d3d11_context: ID3D11DeviceContext,
}

#[cfg(windows)]
fn create_proxy_copier() -> Result<ProxyCopier, String> {
    let d3d11_device = create_d3d11_device()?;
    let d3d11_context = unsafe { d3d11_device.GetImmediateContext() }
        .map_err(|e| format!("GetImmediateContext: {e:?}"))?;
    // Required only to verify the device implements IDXGIDevice (as expected).
    unsafe { d3d11_device.cast::<IDXGIDevice>() }
        .map_err(|e| format!("cast IDXGIDevice: {e:?}"))?;
    Ok(ProxyCopier {
        d3d11_device,
        d3d11_context,
    })
}

#[cfg(windows)]
fn proxy_copy_frame(
    copier: &ProxyCopier,
    rendering_surface: &CompositionDrawingSurface,
    shared_handle: windows::Win32::Foundation::HANDLE,
) -> Result<(), String> {
    // Open the shared D3D12 texture on the D3D11 device (ID3D11Device1).
    let d3d11_1: windows::Win32::Graphics::Direct3D11::ID3D11Device1 = copier
        .d3d11_device
        .cast()
        .map_err(|e| format!("cast ID3D11Device1: {e:?}"))?;
    let shared_d3d11: ID3D11Texture2D = unsafe {
        d3d11_1
            .OpenSharedResource1::<ID3D11Texture2D>(shared_handle)
    }
    .map_err(|e| format!("OpenSharedResource1: {e:?}"))?;

    // BeginDraw -> the drawing surface's D3D11 texture.
    let surface_interop: ICompositionDrawingSurfaceInterop = rendering_surface
        .cast()
        .map_err(|e| format!("cast ICompositionDrawingSurfaceInterop: {e:?}"))?;
    let mut offset = POINT::default();
    let dst: ID3D11Texture2D = unsafe { surface_interop.BeginDraw::<ID3D11Texture2D>(None, &mut offset) }
        .map_err(|e| format!("BeginDraw: {e:?}"))?;

    // One GPU copy: shared(D3D12-viewed-as-D3D11) -> drawing surface.
    unsafe {
        copier
            .d3d11_context
            .CopyResource(&dst, &shared_d3d11)
    };
    unsafe { surface_interop.EndDraw() }.map_err(|e| format!("EndDraw: {e:?}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Composition chain
// ---------------------------------------------------------------------------

struct OverlayChain {
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _effect_brush: CompositionEffectBrush,
    _backdrop_brush: CompositionBackdropBrush,
    _graphics_device: CompositionGraphicsDevice,
    _drawing_surface: CompositionDrawingSurface,
    _content_sprite: SpriteVisual,
    _root: ContainerVisual,
}

#[cfg(windows)]
fn window_hwnd(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return None;
    };
    Some(windows::Win32::Foundation::HWND(win32.hwnd.get() as *mut _))
}

#[cfg(windows)]
fn build_overlay_chain(window: &Window) -> Result<OverlayChain, String> {
    let hwnd = window_hwnd(window).ok_or_else(|| "not a Win32 window".to_string())?;
    let compositor = Compositor::new().map_err(|e| format!("Compositor::new: {e:?}"))?;
    let desktop_interop: ICompositorDesktopInterop = compositor
        .cast()
        .map_err(|e| format!("cast ICompositorDesktopInterop: {e:?}"))?;
    let target = unsafe { desktop_interop.CreateDesktopWindowTarget(hwnd, false) }
        .map_err(|e| format!("CreateDesktopWindowTarget: {e:?}"))?;

    // Backdrop blur base layer.
    let source_param: IGraphicsEffectSource =
        CompositionEffectSourceParameter::Create(&windows::core::HSTRING::from("backdrop"))
            .map_err(|e| format!("Create source parameter: {e:?}"))?
            .cast()
            .map_err(|e| format!("cast source parameter: {e:?}"))?;
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
        .map_err(|e| format!("CreateEffectFactory: {e:?}"))?;
    let load_status = factory.LoadStatus().map(|s| s.0).unwrap_or(-1);
    let effect_brush = factory
        .CreateBrush()
        .map_err(|e| format!("factory.CreateBrush: {e:?}"))?;
    let backdrop_brush = compositor
        .CreateBackdropBrush()
        .map_err(|e| format!("CreateBackdropBrush: {e:?}"))?;
    effect_brush
        .SetSourceParameter(&windows::core::HSTRING::from("backdrop"), &backdrop_brush)
        .map_err(|e| format!("SetSourceParameter: {e:?}"))?;

    let size = window.inner_size();
    let blur_sprite = compositor
        .CreateSpriteVisual()
        .map_err(|e| format!("CreateSpriteVisual: {e:?}"))?;
    blur_sprite
        .SetBrush(&effect_brush)
        .map_err(|e| format!("blur sprite.SetBrush: {e:?}"))?;
    blur_sprite
        .SetSize(Vector2::new(size.width.max(1) as f32, size.height.max(1) as f32))
        .map_err(|e| format!("blur sprite.SetSize: {e:?}"))?;

    let root = compositor
        .CreateContainerVisual()
        .map_err(|e| format!("CreateContainerVisual: {e:?}"))?;
    root.Children()
        .map_err(|e| format!("root.Children: {e:?}"))?
        .InsertAtTop(&blur_sprite)
        .map_err(|e| format!("InsertAtTop blur: {e:?}"))?;

    // wgpu content layer: D3D11 graphics device -> drawing surface -> brush.
    let d3d11 = create_d3d11_device()?;
    let compositor_interop: ICompositorInterop = compositor
        .cast()
        .map_err(|e| format!("cast ICompositorInterop: {e:?}"))?;
    let graphics_device = unsafe { compositor_interop.CreateGraphicsDevice(&d3d11) }
        .map_err(|e| format!("CreateGraphicsDevice: {e:?}"))?;
    let drawing_surface = graphics_device
        .CreateDrawingSurface2(
            windows::Graphics::SizeInt32 {
                Width: size.width.max(1) as i32,
                Height: size.height.max(1) as i32,
            },
            DirectXPixelFormat::R8G8B8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )
        .map_err(|e| format!("CreateDrawingSurface2: {e:?}"))?;
    let surface_brush = compositor
        .CreateSurfaceBrushWithSurface(&drawing_surface)
        .map_err(|e| format!("CreateSurfaceBrushWithSurface: {e:?}"))?;
    let content_sprite = compositor
        .CreateSpriteVisual()
        .map_err(|e| format!("content CreateSpriteVisual: {e:?}"))?;
    content_sprite
        .SetBrush(&surface_brush)
        .map_err(|e| format!("content sprite.SetBrush: {e:?}"))?;
    content_sprite
        .SetSize(Vector2::new(size.width.max(1) as f32, size.height.max(1) as f32))
        .map_err(|e| format!("content sprite.SetSize: {e:?}"))?;
    root.Children()
        .map_err(|e| format!("root.Children content: {e:?}"))?
        .InsertAtTop(&content_sprite)
        .map_err(|e| format!("InsertAtTop content: {e:?}"))?;
    target
        .SetRoot(&root)
        .map_err(|e| format!("target.SetRoot: {e:?}"))?;
    println!(
        "{}",
        serde_json::json!({
            "probe": "backdrop-wgpu-overlay",
            "stage": "chain",
            "size": [size.width, size.height],
            "factory_load_status": load_status,
            "pass": true
        })
    );

    Ok(OverlayChain {
        _compositor: compositor,
        _target: target,
        _effect_brush: effect_brush,
        _backdrop_brush: backdrop_brush,
        _graphics_device: graphics_device,
        _drawing_surface: drawing_surface,
        _content_sprite: content_sprite,
        _root: root,
    })
}

#[cfg(not(windows))]
fn build_overlay_chain(_window: &Window) -> Result<OverlayChain, String> {
    Err("Windows-only probe".into())
}

// ---------------------------------------------------------------------------
// winit app
// ---------------------------------------------------------------------------

struct App {
    _chain: Option<OverlayChain>,
    _renderer: Option<WgpuRenderer>,
    _copier: Option<ProxyCopier>,
    window: Option<Window>,
    deadline: Option<std::time::Instant>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("Neon3 Backdrop + WGPU Overlay Probe")
                    .with_inner_size(winit::dpi::PhysicalSize::new(640, 360))
                    .with_decorations(false)
                    .with_transparent(true)
                    .with_no_redirection_bitmap(true),
            )
            .expect("create probe window");

        let chain = match build_overlay_chain(&window) {
            Ok(chain) => chain,
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"error","error":error,"pass":false})
                );
                self.window = Some(window);
                self.deadline = Some(std::time::Instant::now() + Duration::from_secs(2));
                return;
            }
        };

        // wgpu renderer + proxy copier + one full frame.
        let size = window.inner_size();
        let renderer = match create_wgpu_renderer(size.width, size.height) {
            Ok(renderer) => renderer,
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"renderer-error","error":error,"pass":false})
                );
                self._chain = Some(chain);
                self.window = Some(window);
                self.deadline = Some(std::time::Instant::now() + Duration::from_secs(2));
                return;
            }
        };
        let copier = match create_proxy_copier() {
            Ok(copier) => copier,
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"copier-error","error":error,"pass":false})
                );
                self._chain = Some(chain);
                self._renderer = Some(renderer);
                self.window = Some(window);
                self.deadline = Some(std::time::Instant::now() + Duration::from_secs(2));
                return;
            }
        };

        // Render + copy one frame.
        let mut frame_ok = true;
        if let Err(error) = wgpu_render_frame(&renderer) {
            println!(
                "{}",
                serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"render","error":error,"pass":false})
            );
            frame_ok = false;
        }
        if frame_ok {
            match proxy_copy_frame(&copier, &chain._drawing_surface, renderer.shared_handle) {
                Ok(()) => println!(
                    "{}",
                    serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"proxy-copy","pass":true})
                ),
                Err(error) => println!(
                    "{}",
                    serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"proxy-copy","error":error,"pass":false})
                ),
            }
        }

        self._chain = Some(chain);
        self._renderer = Some(renderer);
        self._copier = Some(copier);
        self.window = Some(window);
        println!(
            "{}",
            serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"visible","message":"blur backdrop + orange wgpu quad visible for 14 seconds","pass":true})
        );
        self.deadline = Some(std::time::Instant::now() + Duration::from_secs(14));
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.deadline.unwrap()));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            event_loop.exit();
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let co_result = unsafe {
        windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        )
    };
    let dq_options = windows::Win32::System::WinRT::DispatcherQueueOptions {
        dwSize: std::mem::size_of::<windows::Win32::System::WinRT::DispatcherQueueOptions>() as u32,
        threadType: windows::Win32::System::WinRT::DQTYPE_THREAD_CURRENT,
        apartmentType: windows::Win32::System::WinRT::DQTAT_COM_STA,
    };
    let dq_controller = unsafe {
        windows::Win32::System::WinRT::CreateDispatcherQueueController(dq_options)
    };
    let current_queue_present = windows::System::DispatcherQueue::GetForCurrentThread().is_ok();
    println!(
        "{}",
        serde_json::json!({
            "probe": "backdrop-wgpu-overlay",
            "stage": "com-init",
            "co_result": format!("{co_result:?}"),
            "dq_controller": dq_controller.is_ok(),
            "current_queue_present": current_queue_present,
            "pass": dq_controller.is_ok() && current_queue_present
        })
    );
    let _dq = dq_controller?;

    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut App {
        window: None,
        _chain: None,
        _renderer: None,
        _copier: None,
        deadline: None,
    })?;
    println!(
        "{}",
        serde_json::json!({"probe":"backdrop-wgpu-overlay","stage":"result","pass":true})
    );
    Ok(())
}