//! Bounded composition-surface probe.
//! Emits JSONL for handle creation, producer submission, consumer readback and
//! pixel diagnosis. It intentionally does not use an HWND swapchain.

#[cfg(windows)]
use std::time::{Duration, Instant};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
use windows::core::Interface;
#[cfg(windows)]
use windows::UI::Composition::{Compositor, ContainerVisual, SpriteVisual};
#[cfg(windows)]
use windows::Win32::Foundation::{GENERIC_ALL, HWND};
#[cfg(windows)]
use windows::Win32::Graphics::DirectComposition::DCompositionCreateSurfaceHandle;
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ, SRCCOPY};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
#[cfg(windows)]
use windows::Win32::System::WinRT::Composition::{ICompositorDesktopInterop, ICompositorInterop};
#[cfg(windows)]
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
#[cfg(windows)]
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT};
#[cfg(windows)]
use windows::Win32::System::Threading::ExitProcess;
#[cfg(windows)]
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Window, WindowId},
};

#[cfg(windows)]
struct Probe {
    window: Option<Window>,
    deadline: Option<Instant>,
    result: Option<Result<(), String>>,
    _dispatcher: windows::System::DispatcherQueueController,
}

#[cfg(windows)]
fn emit(stage: &str, pass: bool, extra: serde_json::Value) {
    println!("{}", serde_json::json!({"probe":"composition-surface","stage":stage,"pass":pass,"data":extra}));
}

#[cfg(windows)]
fn hwnd(window: &Window) -> Result<HWND, String> {
    let handle = window.window_handle().map_err(|e| e.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else { return Err("not Win32".into()) };
    Ok(HWND(handle.hwnd.get() as *mut _))
}

#[cfg(windows)]
fn capture_screen_pixel(hwnd: HWND) -> Result<[u8; 4], String> {
    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|e| format!("GetWindowRect: {e:?}"))?;
    let x = (rect.left + rect.right) / 2;
    let y = (rect.top + rect.bottom) / 2;
    let screen = unsafe { GetDC(None) };
    if screen.is_invalid() { return Err("GetDC(screen) returned null".into()); }
    let memory = unsafe { CreateCompatibleDC(Some(screen)) };
    let bitmap = unsafe { CreateCompatibleBitmap(screen, 1, 1) };
    if memory.is_invalid() || bitmap.is_invalid() { let _ = unsafe { ReleaseDC(None, screen) }; return Err("CreateCompatibleDC/bitmap failed".into()); }
    unsafe { SelectObject(memory, HGDIOBJ(bitmap.0)); }
    unsafe { BitBlt(memory, 0, 0, 1, 1, Some(screen), x, y, SRCCOPY) }.map_err(|e| format!("BitBlt: {e:?}"))?;
    let mut info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: 1, biHeight: -1, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() }, ..Default::default() };
    let mut pixel = [0u8; 4];
    let copied = unsafe { GetDIBits(memory, bitmap, 0, 1, Some(pixel.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS) };
    unsafe { DeleteObject(bitmap.into()); DeleteDC(memory); ReleaseDC(None, screen); }
    if copied == 0 { return Err("GetDIBits returned zero".into()); }
    Ok(pixel)
}

#[cfg(windows)]
fn run_probe(window: &Window, event_loop: &ActiveEventLoop) -> Result<(), String> {
    let hwnd = hwnd(window)?;
    let handle = unsafe { DCompositionCreateSurfaceHandle(GENERIC_ALL.0, None) }
        .map_err(|e| format!("DCompositionCreateSurfaceHandle: {e:?}"))?;
    emit("surface-handle", true, serde_json::json!({"surface_handle": format!("{:p}", handle.0)}));

    let compositor = Compositor::new().map_err(|e| format!("Compositor: {e:?}"))?;
    let desktop: ICompositorDesktopInterop = compositor.cast().map_err(|e| format!("desktop interop: {e:?}"))?;
    let target = unsafe { desktop.CreateDesktopWindowTarget(hwnd, false) }.map_err(|e| format!("desktop target: {e:?}"))?;
    let interop: ICompositorInterop = compositor.cast().map_err(|e| format!("compositor interop: {e:?}"))?;
    let surface = unsafe { interop.CreateCompositionSurfaceForHandle(handle) }.map_err(|e| format!("composition surface: {e:?}"))?;
    let brush = compositor.CreateSurfaceBrushWithSurface(&surface).map_err(|e| format!("surface brush: {e:?}"))?;
    let visual: SpriteVisual = compositor.CreateSpriteVisual().map_err(|e| format!("sprite: {e:?}"))?;
    visual.SetBrush(&brush).map_err(|e| format!("sprite brush: {e:?}"))?;
    let size = window.inner_size();
    visual.SetSize(windows_numerics::Vector2::new(size.width as f32, size.height as f32)).map_err(|e| format!("sprite size: {e:?}"))?;
    let root: ContainerVisual = compositor.CreateContainerVisual().map_err(|e| format!("root: {e:?}"))?;
    root.Children().map_err(|e| format!("children: {e:?}"))?.InsertAtTop(&visual).map_err(|e| format!("insert: {e:?}"))?;
    target.SetRoot(&root).map_err(|e| format!("root: {e:?}"))?;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(event_loop.owned_display_handle())));
    let surface_wgpu = unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::SurfaceHandle(handle.0 as *mut _)) }.map_err(|e| format!("wgpu SurfaceHandle: {e}"))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, compatible_surface: Some(&surface_wgpu), force_fallback_adapter: false, apply_limit_buckets: false })).map_err(|e| format!("adapter: {e}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("composition-surface-probe"), required_features: wgpu::Features::empty(), required_limits: wgpu::Limits::default(), experimental_features: Default::default(), memory_hints: wgpu::MemoryHints::Performance, trace: wgpu::Trace::Off })).map_err(|e| format!("device: {e}"))?;
    let caps = surface_wgpu.get_capabilities(&adapter);
    let format = caps.formats.iter().copied().find(|f| f == &wgpu::TextureFormat::Rgba8Unorm || f == &wgpu::TextureFormat::Bgra8Unorm).ok_or("no RGBA/BGRA surface format")?;
    let alpha = caps.alpha_modes.iter().copied().find(|m| *m == wgpu::CompositeAlphaMode::PreMultiplied).unwrap_or(wgpu::CompositeAlphaMode::Opaque);
    let config = wgpu::SurfaceConfiguration { usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, format, color_space: wgpu::SurfaceColorSpace::Auto, width: size.width.max(1), height: size.height.max(1), present_mode: wgpu::PresentMode::Fifo, alpha_mode: alpha, view_formats: vec![], desired_maximum_frame_latency: 2 };
    surface_wgpu.configure(&device, &config);
    let patterns = [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
    let mut screen_pixels = Vec::new();
    for (producer_frame, color) in patterns.into_iter().enumerate() {
        let frame = match surface_wgpu.get_current_texture() { wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f, other => return Err(format!("acquire frame {producer_frame}: {other:?}")) };
        let mut encoder = device.create_command_encoder(&Default::default());
        let view = frame.texture.create_view(&Default::default());
        { let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("probe-producer"), color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: color[0], g: color[1], b: color[2], a: color[3] }), store: wgpu::StoreOp::Store } })], depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None }); }
        queue.submit(Some(encoder.finish()));
        queue.present(frame);
        device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_millis(1000)) }).map_err(|e| format!("producer wait: {e}"))?;
        std::thread::sleep(Duration::from_millis(120));
        let pixel = capture_screen_pixel(hwnd)?;
        emit("producer-submit", true, serde_json::json!({"producer_frame":producer_frame + 1,"format":format!("{format:?}"),"alpha_mode":format!("{alpha:?}")}));
        emit("screen-capture", true, serde_json::json!({"screen_frame":producer_frame + 1,"pixel_bgra":pixel,"coordinate":"window_center"}));
        screen_pixels.push(pixel);
    }
    let distinct = screen_pixels.windows(2).all(|pair| pair[0] != pair[1]);
    emit("screen-consumer", distinct, serde_json::json!({"screen_frame_count":screen_pixels.len(),"pixels_bgra":screen_pixels,"reason":if distinct { "pass_screen_changed_per_producer_frame" } else { "stale_or_coordinate_or_occluded" }}));
    if !distinct { return Err("screen_consumer_stale_coordinate_or_occluded".into()); }
    Ok(())
}

#[cfg(windows)]
impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = match event_loop.create_window(Window::default_attributes().with_title("Neon3 composition surface probe").with_inner_size(winit::dpi::PhysicalSize::new(320, 180)).with_decorations(false).with_transparent(true)) { Ok(w) => w, Err(e) => { self.result = Some(Err(e.to_string())); event_loop.exit(); return; } };
        self.result = Some(run_probe(&window, event_loop));
        self.window = Some(window);
        self.deadline = Some(Instant::now() + Duration::from_secs(4));
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(self.deadline.unwrap()));
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) { if self.deadline.is_some_and(|d| Instant::now() >= d) { event_loop.exit(); } }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) { if matches!(event, WindowEvent::CloseRequested) { event_loop.exit(); } }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let watchdog_finished = finished.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(15));
        if !watchdog_finished.load(Ordering::Acquire) {
            eprintln!("composition_surface_probe watchdog timeout: 15000ms");
            unsafe { ExitProcess(124); }
        }
    });
    let com_result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if com_result.is_err() { return Err(format!("CoInitializeEx: {com_result:?}").into()); }
    let options = DispatcherQueueOptions { dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32, threadType: DQTYPE_THREAD_CURRENT, apartmentType: DQTAT_COM_STA };
    let dispatcher = unsafe { CreateDispatcherQueueController(options)? };
    let mut probe = Probe { window: None, deadline: None, result: None, _dispatcher: dispatcher };
    EventLoop::new()?.run_app(&mut probe)?;
    finished.store(true, Ordering::Release);
    match probe.result.unwrap_or_else(|| Err("probe_timeout".into())) { Ok(()) => { emit("result", true, serde_json::json!({"bounded_process_timeout_ms":15000})); Ok(()) }, Err(e) => { emit("result", false, serde_json::json!({"error":e,"bounded_process_timeout_ms":15000})); Err(e.into()) } }
}

#[cfg(not(windows))]
fn main() { eprintln!("composition_surface_probe is Windows-only"); std::process::exit(2); }
