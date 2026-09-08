//! Real-window Composition geometric-clip probe.
//!
//! The probe owns a visible HWND, creates the production AcrylicHost, builds
//! an asymmetric eight-vertex clip, exercises resize with the cached clip,
//! then leaves the window visible until the watchdog timeout.

#![cfg(windows)]

use std::time::{Duration, Instant};

use neon_wgpu_runtime::acrylic_backdrop::AcrylicHost;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA,
    DQTYPE_THREAD_CURRENT,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId};

const BOUNDS: [f32; 4] = [24.0, 18.0, 320.0, 220.0];
const CUT: [f32; 4] = [7.0, 13.0, 19.0, 11.0];

fn polygon(bounds: [f32; 4], cut: [f32; 4]) -> [[f32; 2]; 8] {
    let [x, y, width, height] = bounds;
    let [bl, br, tr, tl] = cut;
    [
        [x + tl, y],
        [x + width - tr, y],
        [x + width, y + tr],
        [x + width, y + height - br],
        [x + width - br, y + height],
        [x + bl, y + height],
        [x, y + height - bl],
        [x, y + tl],
    ]
}

fn polygon_area(points: [[f32; 2]; 8]) -> f32 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
        .sum::<f32>()
        .abs()
        * 0.5
}

fn init_dispatcher_queue() -> Result<windows::System::DispatcherQueueController, String> {
    let co_result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if co_result.is_err() {
        return Err(format!("CoInitializeEx: {co_result:?}"));
    }
    let options = DispatcherQueueOptions {
        dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_STA,
    };
    unsafe { CreateDispatcherQueueController(options) }
        .map_err(|error| format!("CreateDispatcherQueueController: {error:?}"))
}

struct Probe {
    dispatcher_queue: windows::System::DispatcherQueueController,
    window: Option<Window>,
    host: Option<AcrylicHost>,
    started: Option<Instant>,
    timeout: Duration,
    emitted: bool,
}

impl Probe {
    fn emit(&mut self, event_loop: &ActiveEventLoop, host: Result<AcrylicHost, String>) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        let points = polygon(BOUNDS, CUT);
        let area = polygon_area(points);
        let mut resize_ok = false;
        let mut create_error = None;
        match host {
            Ok(host) => {
                match host.resize(640, 480) {
                    Ok(()) => resize_ok = true,
                    Err(error) => create_error = Some(format!("resize: {error}")),
                }
                self.host = Some(host);
            }
            Err(error) => create_error = Some(error),
        }
        let pass = create_error.is_none() && resize_ok && area > 0.0 && points.len() == 8;
        println!(
            "{}",
            serde_json::json!({
                "probe": "acrylic-clip",
                "stage": "composition",
                "pass": pass,
                "window_visible": self.window.is_some(),
                "geometry": {
                    "bounds_physical": BOUNDS,
                    "cut_physical": CUT,
                    "vertices": points,
                    "vertex_count": points.len(),
                    "area": area,
                },
                "resize": { "called": true, "ok": resize_ok },
                "error": create_error,
            })
        );
        if !pass {
            event_loop.exit();
        }
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        self.started = Some(Instant::now());
        let window = match event_loop.create_window(
            Window::default_attributes()
                .with_title("Neon3 Acrylic Clip Probe")
                .with_inner_size(LogicalSize::new(640.0, 480.0)),
        ) {
            Ok(window) => window,
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({ "probe": "acrylic-clip", "stage": "window", "pass": false, "error": error.to_string() })
                );
                event_loop.exit();
                return;
            }
        };
        let hwnd = match window.window_handle().map_err(|error| error.to_string()).and_then(|handle| {
            let raw = handle.as_raw();
            match raw {
                RawWindowHandle::Win32(handle) => Ok(windows::Win32::Foundation::HWND(
                    handle.hwnd.get() as *mut _,
                )),
                _ => Err("expected Win32 HWND".to_owned()),
            }
        }) {
            Ok(hwnd) => hwnd,
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({ "probe": "acrylic-clip", "stage": "hwnd", "pass": false, "error": error.to_string() })
                );
                event_loop.exit();
                return;
            }
        };
        self.window = Some(window);
        let host: Result<AcrylicHost, String> = AcrylicHost::new(hwnd, 640, 480)
            .map_err(|error| error.to_string())
            .and_then(|host| {
            host.set_backdrop_shell_bounds(
                BOUNDS[0],
                BOUNDS[1],
                BOUNDS[2],
                BOUNDS[3],
                CUT,
            )
            .map(|_| host)
            .map_err(|error| error.to_string())
        });
        self.emit(event_loop, host);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self
            .started
            .is_some_and(|started| started.elapsed() >= self.timeout)
        {
            event_loop.exit();
        }
    }
}

fn main() -> Result<(), String> {
    let timeout = std::env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .windows(2)
        .find(|args| args[0] == "--timeout-ms")
        .and_then(|args| args[1].parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(12));
    let dispatcher_queue = init_dispatcher_queue()?;
    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut probe = Probe {
        dispatcher_queue,
        window: None,
        host: None,
        started: None,
        timeout,
        emitted: false,
    };
    event_loop.run_app(&mut probe).map_err(|error| error.to_string())
}
