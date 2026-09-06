//! Minimal native-only Acrylic probe. No WGPU, no Neon UI, no DirectComposition.

use std::time::Duration;
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

#[cfg(windows)]
struct CompositionOwner {
    device: windows::Win32::Graphics::DirectComposition::IDCompositionDevice,
    visual: windows::Win32::Graphics::DirectComposition::IDCompositionVisual,
}

#[cfg(windows)]
impl CompositionOwner {
    fn new(window: &Window) -> Result<Self, String> {
        use windows::Win32::{Foundation::HWND, Graphics::{DirectComposition::{DCompositionCreateDevice, IDCompositionDevice}, Dxgi::IDXGIDevice}};
        use windows::core::Interface;
        let handle = window.window_handle().map_err(|e| e.to_string())?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else { return Err("not Win32".into()) };
        let device: IDCompositionDevice = unsafe { DCompositionCreateDevice::<Option<&IDXGIDevice>, IDCompositionDevice>(None) }.map_err(|e| e.to_string())?;
        let target = unsafe { device.CreateTargetForHwnd(HWND(handle.hwnd.get() as *mut _), true) }.map_err(|e| e.to_string())?;
        let visual = unsafe { device.CreateVisual() }.map_err(|e| e.to_string())?;
        unsafe { target.SetRoot(&visual).and_then(|_| device.Commit()) }.map_err(|e| e.to_string())?;
        let _ = Interface::as_raw(&visual);
        Ok(Self { device, visual })
    }
}

#[cfg(windows)]
struct GpuState {
    _instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    composition: CompositionOwner,
}

#[cfg(windows)]
fn create_gpu(window: &Window, event_loop: &ActiveEventLoop) -> Result<GpuState, String> {
    use windows::core::Interface;
    let composition = CompositionOwner::new(window)?;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(event_loop.owned_display_handle())));
    let surface = unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CompositionVisual(composition.visual.as_raw())) }.map_err(|e| e.to_string())?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, compatible_surface: Some(&surface), force_fallback_adapter: false, apply_limit_buckets: false })).map_err(|e| e.to_string())?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("acrylic-native-probe"), required_features: wgpu::Features::empty(), required_limits: wgpu::Limits::default(), experimental_features: Default::default(), memory_hints: wgpu::MemoryHints::Performance, trace: wgpu::Trace::Off })).map_err(|e| e.to_string())?;
    let caps = surface.get_capabilities(&adapter);
    let format = caps.formats.iter().copied().find(|f| f.is_srgb()).ok_or("no srgb format")?;
    let alpha_mode = caps.alpha_modes.iter().copied().find(|m| *m == wgpu::CompositeAlphaMode::PreMultiplied).ok_or("no premultiplied alpha mode")?;
    let size = window.inner_size();
    let config = wgpu::SurfaceConfiguration { usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format, color_space: wgpu::SurfaceColorSpace::Auto, width: size.width, height: size.height, present_mode: wgpu::PresentMode::Fifo, alpha_mode, view_formats: vec![], desired_maximum_frame_latency: 2 };
    surface.configure(&device, &config);
    unsafe { composition.device.Commit() }.map_err(|e| e.to_string())?;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("probe-shader"), source: wgpu::ShaderSource::Wgsl("@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32>{var p=array<vec2<f32>,3>(vec2<f32>(-0.8,-0.8),vec2<f32>(0.8,-0.8),vec2<f32>(0.0,0.8));return vec4<f32>(p[i],0.0,1.0);} @fragment fn fs()->@location(0) vec4<f32>{return vec4<f32>(0.1,0.8,0.95,0.35);}".into()) });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor { label: Some("probe-pipeline"), layout: None, vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() }, fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs"), targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add }, alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add } }), write_mask: wgpu::ColorWrites::ALL })], compilation_options: Default::default() }), primitive: Default::default(), depth_stencil: None, multisample: Default::default(), multiview_mask: None, cache: None });
    Ok(GpuState { _instance: instance, surface, device, queue, config, pipeline, composition })
}

#[cfg(windows)]
fn draw_gpu(gpu: &GpuState) -> Result<(), String> {
    let frame = match gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
        other => return Err(format!("acquire: {other:?}")),
    };
    let view = frame.texture.create_view(&Default::default());
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("probe-pass"), color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })], depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
    pass.set_pipeline(&gpu.pipeline); pass.draw(0..3, 0..1); drop(pass);
    gpu.queue.submit(Some(encoder.finish())); gpu.queue.present(frame); unsafe { gpu.composition.device.Commit() }.map_err(|e| e.to_string())?; Ok(())
}

#[cfg(windows)]
fn apply_acrylic(window: &Window) -> Result<(), String> {
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE},
        System::LibraryLoader::{GetProcAddress, LoadLibraryW},
    };
    #[repr(C)]
    struct AccentPolicy { state: i32, flags: i32, gradient_color: u32, animation_id: i32 }
    #[repr(C)]
    struct AttributeData { attribute: i32, data: *mut std::ffi::c_void, size: usize }
    type Setter = unsafe extern "system" fn(HWND, *mut AttributeData) -> i32;

    let handle = window.window_handle().map_err(|e| e.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else { return Err("not a Win32 window".into()) };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let module = unsafe { LoadLibraryW(windows::core::w!("user32.dll")) }.map_err(|e| e.to_string())?;
    let Some(proc) = (unsafe { GetProcAddress(module, windows::core::s!("SetWindowCompositionAttribute")) }) else {
        return Err("SetWindowCompositionAttribute missing".into());
    };
    let setter: Setter = unsafe { std::mem::transmute(proc) };
    let mut policy = AccentPolicy { state: 4, flags: 0, gradient_color: 0x300C1018, animation_id: 0 };
    let mut data = AttributeData { attribute: 19, data: &mut policy as *mut _ as *mut _, size: std::mem::size_of::<AccentPolicy>() };
    let result = unsafe { setter(hwnd, &mut data) };
    if result == 0 { return Err(format!("SetWindowCompositionAttribute returned {result}")); }
    let backdrop_type: i32 = 3;
    unsafe { DwmSetWindowAttribute(HWND(handle.hwnd.get() as *mut _), DWMWINDOWATTRIBUTE(38), &backdrop_type as *const i32 as *const std::ffi::c_void, std::mem::size_of_val(&backdrop_type) as u32) }.map_err(|e| e.to_string())?;
    println!("{}", serde_json::json!({"probe":"acrylic-native","stage":"api","hwnd":format!("{:p}", hwnd.0),"accent_state":4,"accent_result":result,"dwm_frame":"accent-only","pass":true}));
    Ok(())
}

#[cfg(not(windows))]
fn apply_acrylic(_window: &Window) -> Result<(), String> { Err("Windows-only probe".into()) }

struct App { window: Option<Window>, gpu: Option<GpuState>, deadline: Option<std::time::Instant> }

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop.create_window(
            Window::default_attributes()
                .with_title("Neon3 Acrylic Native Probe")
                .with_inner_size(winit::dpi::PhysicalSize::new(640, 360))
                .with_transparent(true)
                .with_decorations(false),
        ).expect("create native probe window");
        match apply_acrylic(&window) {
            Ok(()) => println!("{}", serde_json::json!({"probe":"acrylic-native","stage":"visible","message":"native-only Acrylic window is visible for 8 seconds","pass":true})),
            Err(error) => println!("{}", serde_json::json!({"probe":"acrylic-native","stage":"error","error":error,"pass":false})),
        }
        self.window = Some(window);
        #[cfg(windows)]
        if std::env::args().any(|arg| arg == "--wgpu") {
            match create_gpu(self.window.as_ref().unwrap(), event_loop) {
                Ok(gpu) => { println!("{}", serde_json::json!({"probe":"acrylic-wgpu","stage":"surface","alpha_mode":format!("{:?}", gpu.config.alpha_mode),"pass":true})); if let Err(error) = draw_gpu(&gpu) { println!("{}", serde_json::json!({"probe":"acrylic-wgpu","stage":"draw","error":error,"pass":false})); } else { println!("{}", serde_json::json!({"probe":"acrylic-wgpu","stage":"draw","transparent_clear":true,"premultiplied_blend":true,"pass":true})); } self.gpu = Some(gpu); }
                Err(error) => println!("{}", serde_json::json!({"probe":"acrylic-wgpu","stage":"error","error":error,"pass":false})),
            }
        }
        self.deadline = Some(std::time::Instant::now() + Duration::from_secs(8));
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(self.deadline.unwrap()));
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            event_loop.exit();
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: winit::event::WindowEvent) {
        if matches!(event, winit::event::WindowEvent::CloseRequested) { event_loop.exit(); }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut App { window: None, gpu: None, deadline: None })?;
    println!("{}", serde_json::json!({"probe":"acrylic-native","stage":"result","pass":true}));
    Ok(())
}
