//! CompositionBackdropBrush route probe — Windows.UI.Composition.
//!
//! Verifies the full WinRT composition backdrop chain on a real winit window:
//!   Compositor -> CreateBackdropBrush + hand-rolled GaussianBlur IGraphicsEffect
//!              -> CreateEffectFactory -> CompositionEffectBrush
//!              -> SetSourceParameter("backdrop", backdropBrush)
//!              -> SpriteVisual(brush) -> DesktopWindowTarget.Root
//!
//! The window shows the blurred desktop/background behind it for ~14 seconds.
//! Emits JSONL stage records like the other probes.

use std::time::Duration;

use windows::core::{implement, Interface, PCWSTR};
use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectSource_Impl, IGraphicsEffect_Impl,
};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    Compositor, CompositionBackdropBrush, CompositionEffectBrush, CompositionEffectSourceParameter,
    ContainerVisual, SpriteVisual,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
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
// Hand-rolled GaussianBlurEffect : needs neither Win2D nor any extra runtime.
// Implements IGraphicsEffect + IGraphicsEffectD2D1Interop, pointing at the
// system-internal D2D gaussian blur effect CLSID.
// ---------------------------------------------------------------------------

// CLSID_D2D1GaussianBlur from d2d1effects.h
// DEFINE_GUID(CLSID_D2D1GaussianBlur, 0x1feb6d69, 0x2fe6, 0x4ac9, 0x8c, 0x58, 0x1d, 0x7f, 0x93, 0xe7, 0xa6, 0xa5);
const CLSID_D2D1_GAUSSIAN_BLUR: windows::core::GUID = windows::core::GUID::from_values(
    0x1feb6d69,
    0x2fe6,
    0x4ac9,
    [0x8c, 0x58, 0x1d, 0x7f, 0x93, 0xe7, 0xa6, 0xa5],
);

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
        let name = self.name.borrow().clone();
        eprintln!("[effect] IGraphicsEffect::Name -> {name}");
        Ok(name)
    }
    fn SetName(&self, name: &windows::core::HSTRING) -> windows::core::Result<()> {
        eprintln!("[effect] IGraphicsEffect::SetName -> {name}");
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlurEffect_Impl {
    fn GetEffectId(&self) -> windows::core::Result<windows::core::GUID> {
        eprintln!("[effect] GetEffectId -> {CLSID_D2D1_GAUSSIAN_BLUR:?}");
        Ok(CLSID_D2D1_GAUSSIAN_BLUR)
    }

    fn GetNamedPropertyMapping(
        &self,
        name: &PCWSTR,
        index: *mut u32,
        mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> windows::core::Result<()> {
        let prop = unsafe { name.as_wide() };
        let prop_str: String = prop.iter().map(|&c| char::from_u32(c as u32).unwrap_or('?')).collect();
        eprintln!("[effect] GetNamedPropertyMapping -> {prop_str}");
        if prop == widestring("BlurAmount") {
            unsafe {
                *index = 0;
                *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
            }
            Ok(())
        } else if prop == widestring("Optimization") {
            unsafe {
                *index = 1;
                *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
            }
            Ok(())
        } else if prop == widestring("BorderMode") {
            unsafe {
                *index = 2;
                *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
            }
            Ok(())
        } else {
            Err(invalid_param())
        }
    }

    fn GetPropertyCount(&self) -> windows::core::Result<u32> {
        eprintln!("[effect] GetPropertyCount -> 3");
        Ok(3)
    }

    fn GetProperty(&self, index: u32) -> windows::core::Result<IPropertyValue> {
        eprintln!("[effect] GetProperty({index})");
        let inspectable = match index {
            0 => PropertyValue::CreateSingle(self.blur_amount)?,
            1 => PropertyValue::CreateUInt32(self.optimization)?,
            2 => PropertyValue::CreateUInt32(self.border_mode)?,
            _ => return Err(invalid_param()),
        };
        inspectable.cast()
    }

    fn GetSource(&self, index: u32) -> windows::core::Result<IGraphicsEffectSource> {
        eprintln!("[effect] GetSource({index})");
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(invalid_param())
        }
    }

    fn GetSourceCount(&self) -> windows::core::Result<u32> {
        if std::env::var("NEON_NO_SOURCE").is_ok() {
            eprintln!("[effect] GetSourceCount -> 0 (NEON_NO_SOURCE)");
            Ok(0)
        } else {
            eprintln!("[effect] GetSourceCount -> 1");
            Ok(1)
        }
    }
}

fn widestring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn invalid_param() -> windows::core::Error {
    windows::core::Error::from_hresult(windows::core::HRESULT(0x80070057u32 as i32)) // E_INVALIDARG
}

// ---------------------------------------------------------------------------
// Composition chain
// ---------------------------------------------------------------------------

struct BackdropChain {
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _effect_brush: Option<CompositionEffectBrush>,
    _backdrop_brush: CompositionBackdropBrush,
    _sprite: SpriteVisual,
    _root: ContainerVisual,
}

#[cfg(windows)]
fn build_backdrop_chain(window: &Window) -> Result<BackdropChain, String> {
    // COM (STA) and the DispatcherQueue were initialised in main() before the
    // winit event loop started, so the current thread is ready for
    // Windows.UI.Composition.

    let hwnd = window_hwnd(window).ok_or_else(|| "not a Win32 window".to_string())?;

    // 1. Compositor (Windows.UI.Composition, activatable on desktop)
    let compositor = Compositor::new().map_err(|e| format!("Compositor::new: {e:?}"))?;
    println!(
        "{}",
        serde_json::json!({"probe":"composition-backdrop","stage":"compositor","hwnd":format!("{hwnd:?}"),"pass":true})
    );

    // 2. DesktopWindowTarget via CompositorInterop
    let interop: ICompositorDesktopInterop = compositor.cast().map_err(|e| format!("cast ICompositorDesktopInterop: {e:?}"))?;
    let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, false) }
        .map_err(|e| format!("CreateDesktopWindowTarget: {e:?}"))?;
    println!(
        "{}",
        serde_json::json!({"probe":"composition-backdrop","stage":"desktop-target","pass":true})
    );

    // 3. Effect: hand-rolled gaussian blur, large blur amount
    let no_blur = std::env::var("NEON_NO_BLUR").is_ok();
    let blur_amount: f32 = std::env::var("NEON_BLUR_AMOUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16.0);
    let source_param: IGraphicsEffectSource = CompositionEffectSourceParameter::Create(&windows::core::HSTRING::from("backdrop"))
        .map_err(|e| format!("Create source parameter: {e:?}"))?
        .cast()
        .map_err(|e| format!("cast source parameter: {e:?}"))?;
    let effect = GaussianBlurEffect {
        name: std::cell::RefCell::new(windows::core::HSTRING::from("GaussianBlurEffect")),
        blur_amount,
        optimization: 1u32, // Balanced
        border_mode: 0u32,  // Soft
        source: source_param,
    };
    let effect_interface: IGraphicsEffect = effect.into();
    println!(
        "{}",
        serde_json::json!({"probe":"composition-backdrop","stage":"effect-object","no_blur":no_blur,"pass":true})
    );

    // 4. Compositor.CreateEffectFactory + CreateBrush (skip when verifying the
    //    bare DesktopWindowTarget + backdrop chain without a custom effect).
    let effect_brush: CompositionEffectBrush = if no_blur {
        let backdrop = compositor
            .CreateBackdropBrush()
            .map_err(|e| format!("bootstrap CreateBackdropBrush: {e:?}"))?;
        // No effect brush available without the factory, so fall through to the
        // bootstrap path below that uses the backdrop brush directly.
        println!(
            "{}",
            serde_json::json!({"probe":"composition-backdrop","stage":"bootstrap-backdrop","pass":true})
        );
        return build_bare_backdrop_chain(window, compositor, target, backdrop, hwnd);
    } else {
        let factory = compositor
            .CreateEffectFactory(&effect_interface)
            .map_err(|e| format!("CreateEffectFactory: {e:?}"))?;
        println!(
            "{}",
            serde_json::json!({"probe":"composition-backdrop","stage":"factory","pass":true})
        );
        let brush = factory.CreateBrush().map_err(|e| format!("factory.CreateBrush: {e:?}"))?;
        let load_status = factory.LoadStatus().map(|s| s.0).unwrap_or(-1);
        println!(
            "{}",
            serde_json::json!({"probe":"composition-backdrop","stage":"effect","blur_amount":blur_amount,"factory_load_status":load_status,"pass":true})
        );
        brush
    };

    // 5. Backdrop brush bound to the "backdrop" source parameter
    let backdrop_brush = compositor
        .CreateBackdropBrush()
        .map_err(|e| format!("CreateBackdropBrush: {e:?}"))?;
    effect_brush
        .SetSourceParameter(&windows::core::HSTRING::from("backdrop"), &backdrop_brush)
        .map_err(|e| format!("SetSourceParameter: {e:?}"))?;
    println!(
        "{}",
        serde_json::json!({"probe":"composition-backdrop","stage":"brush","pass":true})
    );

    // 6. SpriteVisual with the effect brush, sized to the window
    let size = window.inner_size();
    let sprite = compositor
        .CreateSpriteVisual()
        .map_err(|e| format!("CreateSpriteVisual: {e:?}"))?;
    sprite
        .SetBrush(&effect_brush)
        .map_err(|e| format!("sprite.SetBrush: {e:?}"))?;
    sprite
        .SetSize(Vector2::new(size.width.max(1) as f32, size.height.max(1) as f32))
        .map_err(|e| format!("sprite.SetSize: {e:?}"))?;

    // 7. Root container + SetRoot on the desktop target
    let root = compositor
        .CreateContainerVisual()
        .map_err(|e| format!("CreateContainerVisual: {e:?}"))?;
    root.Children()
        .map_err(|e| format!("root.Children: {e:?}"))?
        .InsertAtTop(&sprite)
        .map_err(|e| format!("InsertAtTop: {e:?}"))?;
    target.SetRoot(&root).map_err(|e| format!("target.SetRoot: {e:?}"))?;
    println!(
        "{}",
        serde_json::json!({"probe":"composition-backdrop","stage":"root","size":[size.width,size.height],"pass":true})
    );

    Ok(BackdropChain {
        _compositor: compositor,
        _target: target,
        _effect_brush: Some(effect_brush),
        _backdrop_brush: backdrop_brush,
        _sprite: sprite,
        _root: root,
    })
}

// Bootstrap path: bare DesktopWindowTarget + SpriteVisual + backdrop brush,
// no effect factory at all. Proves the composition target chain works before
// layering the hand-rolled GaussianBlur effect on top.
#[cfg(windows)]
fn build_bare_backdrop_chain(
    window: &Window,
    compositor: Compositor,
    target: DesktopWindowTarget,
    backdrop_brush: CompositionBackdropBrush,
    _hwnd: windows::Win32::Foundation::HWND,
) -> Result<BackdropChain, String> {
    let size = window.inner_size();
    let sprite = compositor
        .CreateSpriteVisual()
        .map_err(|e| format!("bootstrap CreateSpriteVisual: {e:?}"))?;
    sprite
        .SetBrush(&backdrop_brush)
        .map_err(|e| format!("bootstrap sprite.SetBrush: {e:?}"))?;
    sprite
        .SetSize(Vector2::new(size.width.max(1) as f32, size.height.max(1) as f32))
        .map_err(|e| format!("bootstrap sprite.SetSize: {e:?}"))?;
    let root = compositor
        .CreateContainerVisual()
        .map_err(|e| format!("bootstrap CreateContainerVisual: {e:?}"))?;
    root.Children()
        .map_err(|e| format!("bootstrap root.Children: {e:?}"))?
        .InsertAtTop(&sprite)
        .map_err(|e| format!("bootstrap InsertAtTop: {e:?}"))?;
    target
        .SetRoot(&root)
        .map_err(|e| format!("bootstrap target.SetRoot: {e:?}"))?;
    println!(
        "{}",
        serde_json::json!({"probe":"composition-backdrop","stage":"bootstrap-root","size":[size.width,size.height],"pass":true})
    );
    Ok(BackdropChain {
        _compositor: compositor,
        _target: target,
        _effect_brush: None,
        _backdrop_brush: backdrop_brush,
        _sprite: sprite,
        _root: root,
    })
}

#[cfg(not(windows))]
fn build_backdrop_chain(_window: &Window) -> Result<BackdropChain, String> {
    Err("Windows-only probe".into())
}

// ---------------------------------------------------------------------------
// Screen capture: BitBlt the window region out of the screen DC so we can
// visually verify whether the composition backdrop blur actually rendered.
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn capture_window_png(
    hwnd: windows::Win32::Foundation::HWND,
    out_path: &std::path::Path,
) -> Result<(), String> {
    use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
    GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    SRCCOPY,
};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|e| format!("GetWindowRect: {e:?}"))?;
    let width = (rect.right - rect.left) as i32;
    let height = (rect.bottom - rect.top) as i32;
    if width <= 0 || height <= 0 {
        return Err(format!("window rect not valid: {width}x{height}"));
    }

    let screen_dc = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        return Err("GetDC failed".into());
    }
    let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    let bitmap = unsafe { CreateCompatibleBitmap(screen_dc, width, height) };
    if bitmap.0.is_null() {
        unsafe { ReleaseDC(None, screen_dc) };
        return Err("CreateCompatibleBitmap failed".into());
    }
    let _old = unsafe {
        SelectObject(mem_dc, windows::Win32::Graphics::Gdi::HGDIOBJ(bitmap.0))
    };
    unsafe {
        BitBlt(
            mem_dc,
            0,
            0,
            width,
            height,
            Some(screen_dc),
            rect.left,
            rect.top,
            SRCCOPY,
        )
    }
    .map_err(|e| format!("BitBlt: {e:?}"))?;

    // DIB section: bottom-up BGRA32
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let lines = unsafe {
        GetDIBits(
            mem_dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr() as *mut core::ffi::c_void),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    unsafe {
        DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(bitmap.0));
        DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
    }
    if lines == 0 {
        return Err("GetDIBits returned 0 lines".into());
    }

    // BGRA -> RGBA
    let mut rgba = Vec::with_capacity(pixels.len());
    for px in pixels.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }

    let file = std::fs::File::create(out_path).map_err(|e| format!("create {out_path:?}: {e}"))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| format!("png header: {e}"))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| format!("png write: {e}"))?;
    Ok(())
}

#[cfg(not(windows))]
fn capture_window_png(
    _hwnd: windows::Win32::Foundation::HWND,
    _out_path: &std::path::Path,
) -> Result<(), String> {
    Err("Windows-only capture".into())
}

// ---------------------------------------------------------------------------
// winit app
// ---------------------------------------------------------------------------

struct App {
    window: Option<Window>,
    _chain: Option<BackdropChain>,
    deadline: Option<std::time::Instant>,
    captures_done: u32,
    start: Option<std::time::Instant>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title("Neon3 Composition Backdrop Probe")
                    .with_inner_size(winit::dpi::PhysicalSize::new(640, 360))
                    .with_decorations(false)
                    .with_transparent(true)
                    .with_no_redirection_bitmap(true),
            )
            .expect("create probe window");
        match build_backdrop_chain(&window) {
            Ok(chain) => {
                self._chain = Some(chain);
                println!(
                    "{}",
                    serde_json::json!({"probe":"composition-backdrop","stage":"visible","message":"backdrop blur window is visible for 14 seconds; check whether the desktop behind the window is blurred","pass":true})
                );
            }
            Err(error) => println!(
                "{}",
                serde_json::json!({"probe":"composition-backdrop","stage":"error","error":error,"pass":false})
            ),
        }
        self.window = Some(window);
        self.start = Some(std::time::Instant::now());
        self.deadline = Some(std::time::Instant::now() + Duration::from_secs(14));
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.deadline.unwrap()));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(start) = self.start else { return };
        let elapsed = start.elapsed();
        if self.captures_done < 2
            && elapsed >= Duration::from_millis(2500 + 4000 * self.captures_done as u64)
        {
            if let Some(window) = &self.window {
                #[cfg(windows)]
                {
                    let hwnd = window_hwnd(window);
                    if let Some(hwnd) = hwnd {
                        let out_dir = std::env::temp_dir().join("neon3-backdrop-probe");
                        let _ = std::fs::create_dir_all(&out_dir);
                        let path = out_dir.join(format!("frame-{}.png", self.captures_done + 1));
                        match capture_window_png(hwnd, &path) {
                            Ok(()) => println!(
                                "{}",
                                serde_json::json!({"probe":"composition-backdrop","stage":"capture","n":self.captures_done+1,"path":path.to_string_lossy(),"pass":true})
                            ),
                            Err(error) => println!(
                                "{}",
                                serde_json::json!({"probe":"composition-backdrop","stage":"capture","n":self.captures_done+1,"error":error,"pass":false})
                            ),
                        }
                    }
                }
                self.captures_done += 1;
            }
        }
        if self.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            event_loop.exit();
        }
    }
}

#[cfg(windows)]
fn window_hwnd(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return None;
    };
    Some(windows::Win32::Foundation::HWND(win32.hwnd.get() as *mut _))
}

#[cfg(not(windows))]
fn window_hwnd(_window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    None
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Windows.UI.Composition on the desktop needs an STA thread with a
    // DispatcherQueue bound to the current thread BEFORE any window/COM work.
    // winit's event loop initialises COM itself, so we must win the race and
    // initialise STA + the queue here, on the main thread, first.
    let co_result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let dq_options = DispatcherQueueOptions {
        dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_STA,
    };
    let dq_controller = unsafe { CreateDispatcherQueueController(dq_options) };
    let current_queue_present = windows::System::DispatcherQueue::GetForCurrentThread().is_ok();
    println!(
        "{}",
        serde_json::json!({
            "probe": "composition-backdrop",
            "stage": "com-init",
            "co_result": format!("{co_result:?}"),
            "dq_controller": dq_controller.is_ok(),
            "current_queue_present": current_queue_present,
            "pass": dq_controller.is_ok() && current_queue_present
        })
    );
    // Keep the queue controller alive for the whole session.
    let _dq = dq_controller?;

    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut App {
        window: None,
        _chain: None,
        deadline: None,
        captures_done: 0,
        start: None,
    })?;
    println!(
        "{}",
        serde_json::json!({"probe":"composition-backdrop","stage":"result","pass":true})
    );
    Ok(())
}