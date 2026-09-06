//! Neon2-style transparent-window probe.
//!
//! Replicates the proven Neon2 loading-window path exactly:
//!   WindowBuilder::with_transparent(true) + wgpu Instance::create_surface(window),
//! and relies on wgpu's own `WGPU_DX12_PRESENTATION_SYSTEM=DxgiFromVisual`
//! DirectComposition integration for the premultiplied-alpha swapchain.
//! No manual DirectComposition code lives here.

use std::{sync::Arc, time::Duration};

use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, EventLoop},
    event::WindowEvent,
    window::{Window, WindowId},
};
#[cfg(windows)]
use winit::platform::windows::WindowAttributesExtWindows;

struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
}

fn create_gpu(window: &Arc<Window>) -> Result<GpuState, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        backend_options: wgpu::BackendOptions {
            dx12: wgpu::Dx12BackendOptions {
                presentation_system: wgpu::Dx12SwapchainKind::DxgiFromVisual,
                ..Default::default()
            },
            ..Default::default()
        },
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let surface = instance
        .create_surface(window.clone())
        .map_err(|error| format!("create surface: {error:?}"))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .map_err(|error| format!("request adapter: {error:?}"))?;
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-transparent-window-probe"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|error| format!("request device: {error:?}"))?;
    let caps = surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::is_srgb)
        .or_else(|| caps.formats.first().copied())
        .ok_or_else(|| "no surface format".to_owned())?;
    let alpha_mode = caps
        .alpha_modes
        .iter()
        .copied()
        .find(|mode| *mode == wgpu::CompositeAlphaMode::PreMultiplied)
        .or_else(|| caps.alpha_modes.iter().copied().find(|mode| *mode == wgpu::CompositeAlphaMode::PostMultiplied))
        .or_else(|| caps.alpha_modes.first().copied())
        .ok_or_else(|| "no alpha mode".to_owned())?;
    let size = window.inner_size();
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode,
        view_formats: Vec::new(),
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);
    println!(
        "{}",
        serde_json::json!({
            "probe": "transparent-window",
            "stage": "surface",
            "backend": format!("{:?}", adapter.get_info().backend),
            "adapter": adapter.get_info().name,
            "format": format!("{format:?}"),
            "alpha_mode": format!("{alpha_mode:?}"),
            "available_alpha_modes": caps.alpha_modes.iter().map(|mode| format!("{mode:?}")).collect::<Vec<_>>(),
        })
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("probe-shader"),
        source: wgpu::ShaderSource::Wgsl(
            "@vertex fn vs(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32>{\
             var p=array<vec2<f32>,3>(vec2<f32>(-0.8,-0.8),vec2<f32>(0.8,-0.8),vec2<f32>(0.0,0.8));\
             return vec4<f32>(p[i],0.0,1.0);}\
             @fragment fn fs()->@location(0) vec4<f32>{return vec4<f32>(0.2,0.9,0.4,1.0);}"
                .into(),
        ),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("probe-pipeline"),
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
                format,
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
    Ok(GpuState {
        surface,
        device,
        queue,
        config,
        pipeline,
    })
}

fn draw(gpu: &GpuState) -> Result<(), String> {
    let frame = match gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(frame)
        | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
        other => return Err(format!("acquire: {other:?}")),
    };
    let view = frame.texture.create_view(&Default::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&Default::default());
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("probe-pass"),
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
    pass.set_pipeline(&gpu.pipeline);
    pass.draw(0..3, 0..1);
    drop(pass);
    gpu.queue.submit(Some(encoder.finish()));
    gpu.queue.present(frame);
    Ok(())
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    deadline: Option<std::time::Instant>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Neon3 Transparent Window Probe")
                        .with_inner_size(winit::dpi::PhysicalSize::new(640, 360))
                        .with_decorations(false)
                        .with_transparent(true)
                        .with_no_redirection_bitmap(true),
                )
                .expect("create probe window"),
        );
        match create_gpu(&window) {
            Ok(gpu) => {
                println!(
                    "{}",
                    serde_json::json!({"probe": "transparent-window", "stage": "created", "pass": true})
                );
                if let Err(error) = draw(&gpu) {
                    println!(
                        "{}",
                        serde_json::json!({"probe": "transparent-window", "stage": "draw", "error": error, "pass": false})
                    );
                } else {
                    println!(
                        "{}",
                        serde_json::json!({"probe": "transparent-window", "stage": "draw", "pass": true, "transparent_clear": true, "alpha_blend": true})
                    );
                }
                self.gpu = Some(gpu);
            }
            Err(error) => println!(
                "{}",
                serde_json::json!({"probe": "transparent-window", "stage": "error", "error": error, "pass": false})
            ),
        }
        self.window = Some(window);
        self.deadline = Some(std::time::Instant::now() + Duration::from_secs(20));
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(self.deadline.unwrap()));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
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
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut App {
        window: None,
        gpu: None,
        deadline: None,
    })?;
    println!("{}", serde_json::json!({"probe": "transparent-window", "stage": "result", "pass": true}));
    Ok(())
}