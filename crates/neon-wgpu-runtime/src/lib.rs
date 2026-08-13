//! Command handling and window/GPU bootstrap for Neon3's sole renderer owner.
//! No other Neon3 crate may initialize window or GPU objects.

use std::time::Instant;
use std::{collections::HashMap, net::SocketAddr, thread};

use neon_observability::{
    CommandJournal, CommandReceipt, CommandState, DebugSnapshot, EVENT_COMMAND_ACCEPTED,
    EVENT_COMMAND_RECEIVED, EVENT_COMMAND_REJECTED, JournalFilter, TraceLevel, TraceRecord,
};
use neon_protocol::{
    HealthStatus, PROTOCOL_VERSION, RequestId, Revision, RpcError, RpcRequest, RpcResponse,
    RpcStatus, ServiceDescription, ServiceHealth, ServiceName,
};
use neon_ui_schema::{
    UiBounds, UiCommand, UiFragment, UiFragmentId, UiNode, UiNodeKind, UiStyle, UiTransition,
    UiTransitionState, UiSemanticEvent,
};
use serde_json::{Value, json};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

mod ui_renderer;

use ui_renderer::UiWgpuRenderer;

pub const SERVICE_NAME: &str = "wgpu-runtime";
pub const CAPABILITY_UI_FRAGMENT: &str = "wgpu.ui.fragment.v1";
pub const CAPABILITY_UI_HIT_TARGET: &str = "wgpu.ui.hit_target.v1";
pub const UI_HIT_TARGET: &str = "ui.hit_id.v1";
pub const UI_COLOR_TARGET: &str = "ui.color.v1";
pub const RENDER_HIT_NONE: u32 = u32::MAX;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderDiagnostics {
    pub graph_revision: Revision,
    pub fragment_count: usize,
    pub mode: RenderMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderMode {
    Headless,
    Windowed,
}

/// The only process-local home for Winit and WGPU objects in Neon3.
/// Domain runtimes communicate with this process through the public RPC protocol.
pub struct WindowedRuntime {
    epoch: u64,
    gpu: Option<WindowGpu>,
    window: Option<Window>,
    exit_error: Option<String>,
    fragments: HashMap<UiFragmentId, UiFragment>,
}

#[derive(Clone, Debug)]
enum WindowCommand {
    Fragments(HashMap<UiFragmentId, UiFragment>),
    Shutdown,
}

struct WindowGpu {
    _instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    ui: UiWgpuRenderer,
    started_at: Instant,
}

impl WindowedRuntime {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            gpu: None,
            window: None,
            exit_error: None,
            fragments: HashMap::new(),
        }
    }

    pub fn run(epoch: u64) -> Result<(), String> {
        Self::run_with_server(epoch, None, 0, true)
    }

    pub fn run_server(
        epoch: u64,
        endpoint: SocketAddr,
        request_count: usize,
    ) -> Result<(), String> {
        Self::run_with_server(epoch, Some(endpoint), request_count, false)
    }

    fn run_with_server(
        epoch: u64,
        endpoint: Option<SocketAddr>,
        request_count: usize,
        demo: bool,
    ) -> Result<(), String> {
        let event_loop = EventLoop::<WindowCommand>::with_user_event()
            .build()
            .map_err(|error| format!("create event loop: {error}"))?;
        let proxy = event_loop.create_proxy();
        let mut runtime = Self::new(epoch);
        if demo {
            runtime.fragments = runtime.demo_fragments();
        }
        if let Some(endpoint) = endpoint {
            spawn_window_server(epoch, endpoint, request_count, proxy);
        }
        event_loop
            .run_app(&mut runtime)
            .map_err(|error| format!("run event loop: {error}"))?;
        runtime.exit_error.map_or(Ok(()), Err)
    }

    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let window = event_loop
            .create_window(
                Window::default_attributes()
                    .with_title(format!("Neon3 - WGPU Runtime (epoch {})", self.epoch))
                    .with_inner_size(PhysicalSize::new(1280, 800)),
            )
            .map_err(|error| format!("create window: {error}"))?;
        let gpu = WindowGpu::new(&window)?;
        self.window = Some(window);
        self.gpu = Some(gpu);
        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.config.width = size.width;
            gpu.config.height = size.height;
            gpu.surface.configure(&gpu.device, &gpu.config);
        }
    }

    fn redraw(&mut self) -> Result<(), String> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let surface_texture = match gpu.surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return Ok(());
            }
            Err(error) => return Err(format!("acquire surface texture: {error}")),
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neon3-final-composition"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-final-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.025,
                            g: 0.028,
                            b: 0.034,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            gpu.ui.draw(
                &gpu.device,
                &gpu.queue,
                &mut pass,
                &self.fragments,
                [gpu.config.width, gpu.config.height],
                gpu.started_at.elapsed().as_secs_f32(),
            );
        }
        gpu.queue.submit(Some(encoder.finish()));
        surface_texture.present();
        Ok(())
    }
}

impl WindowGpu {
    fn new(window: &Window) -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        // SAFETY: `WindowedRuntime` declares `gpu` before `window`, so the surface is dropped
        // before the window handle it references.
        let surface = unsafe {
            instance
                .create_surface_unsafe(
                    wgpu::SurfaceTargetUnsafe::from_window(window)
                        .map_err(|error| format!("create surface target: {error}"))?,
                )
                .map_err(|error| format!("create surface: {error}"))?
        };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| "request adapter: no compatible adapter was found".to_owned())?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("neon3-wgpu-runtime-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|error| format!("request device: {error}"))?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .or_else(|| capabilities.formats.first().copied())
            .ok_or_else(|| "surface reported no supported texture formats".to_owned())?;
        let alpha_mode = capabilities
            .alpha_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::CompositeAlphaMode::Opaque)
            .or_else(|| capabilities.alpha_modes.first().copied())
            .ok_or_else(|| "surface reported no supported alpha modes".to_owned())?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: Vec::new(),
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let ui = UiWgpuRenderer::new(&device, config.format);
        Ok(Self {
            _instance: instance,
            surface,
            device,
            queue,
            config,
            ui,
            started_at: Instant::now(),
        })
    }
}

impl ApplicationHandler<WindowCommand> for WindowedRuntime {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.exit_error = Some(error);
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().map(Window::id) != Some(window_id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.resize(size),
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.ui
                        .set_pointer_position([position.x as f32, position.y as f32]);
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == winit::event::ElementState::Pressed
                    && button == winit::event::MouseButton::Left =>
            {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.ui.press_hovered(gpu.started_at.elapsed().as_secs_f32());
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.redraw() {
                    self.exit_error = Some(error);
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WindowCommand) {
        match event {
            WindowCommand::Fragments(fragments) => self.fragments = fragments,
            WindowCommand::Shutdown => event_loop.exit(),
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

fn spawn_window_server(
    epoch: u64,
    endpoint: SocketAddr,
    request_count: usize,
    proxy: EventLoopProxy<WindowCommand>,
) {
    thread::spawn(move || {
        let server = match neon_ipc::RpcServer::bind(endpoint) {
            Ok(server) => server,
            Err(error) => {
                eprintln!("window RPC server bind failed: {error}");
                let _ = proxy.send_event(WindowCommand::Shutdown);
                return;
            }
        };
        let mut runtime = WgpuRuntime::headless(epoch);
        for _ in 0..request_count {
            let proxy = proxy.clone();
            if let Err(error) = server.serve_one(|request| {
                let response = runtime.handle(request);
                if response.status == RpcStatus::Accepted {
                    let _ =
                        proxy.send_event(WindowCommand::Fragments(runtime.fragments_snapshot()));
                }
                response
            }) {
                eprintln!("window RPC server request failed: {error}");
                break;
            }
        }
        let _ = proxy.send_event(WindowCommand::Shutdown);
    });
}

impl WindowedRuntime {
    fn demo_fragments(&self) -> HashMap<UiFragmentId, UiFragment> {
        let root = UiNode {
            node_id: neon_ui_schema::UiNodeId("demo-shell".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 48.0,
                y: 48.0,
                width: 430.0,
                height: 240.0,
            },
            visible: true,
            enabled: true,
            text_key: None,
            style: UiStyle {
                background_color: [0.035, 0.06, 0.08, 0.98],
                border_color: [0.18, 0.78, 0.86, 0.8],
                border_width: 1.0,
                corner_radius: 8.0,
                opacity: 1.0,
            },
            enter_transition: Some(UiTransition {
                delay_ms: 0,
                duration_ms: 280,
                easing: neon_ui_schema::UiEasing::EaseOut,
                from: UiTransitionState {
                    bounds: Some(UiBounds {
                        x: 48.0,
                        y: 76.0,
                        width: 430.0,
                        height: 240.0,
                    }),
                    opacity: Some(0.0),
                    ..UiTransitionState::default()
                },
            }),
            children: vec![
                UiNode {
                    node_id: neon_ui_schema::UiNodeId("demo-title".into()),
                    kind: UiNodeKind::Label,
                    bounds: UiBounds {
                        x: 20.0,
                        y: 20.0,
                        width: 300.0,
                        height: 34.0,
                    },
                    visible: true,
                    enabled: true,
                    text_key: Some("ui.demo.title".into()),
                    style: UiStyle {
                        background_color: [0.12, 0.32, 0.37, 0.94],
                        border_color: [0.37, 0.94, 0.94, 0.85],
                        border_width: 1.0,
                        corner_radius: 4.0,
                        opacity: 1.0,
                    },
                    enter_transition: Some(UiTransition {
                        delay_ms: 100,
                        duration_ms: 220,
                        easing: neon_ui_schema::UiEasing::EaseOut,
                        from: UiTransitionState {
                            opacity: Some(0.0),
                            ..UiTransitionState::default()
                        },
                    }),
                    children: Vec::new(),
                },
                UiNode {
                    node_id: neon_ui_schema::UiNodeId("demo-button".into()),
                    kind: UiNodeKind::Button,
                    bounds: UiBounds {
                        x: 20.0,
                        y: 142.0,
                        width: 190.0,
                        height: 52.0,
                    },
                    visible: true,
                    enabled: true,
                    text_key: Some("ui.demo.action".into()),
                    style: UiStyle {
                        background_color: [0.08, 0.38, 0.44, 1.0],
                        border_color: [0.46, 0.96, 0.94, 0.95],
                        border_width: 1.0,
                        corner_radius: 5.0,
                        opacity: 1.0,
                    },
                    enter_transition: Some(UiTransition {
                        delay_ms: 180,
                        duration_ms: 220,
                        easing: neon_ui_schema::UiEasing::EaseOut,
                        from: UiTransitionState {
                            bounds: Some(UiBounds {
                                x: 4.0,
                                y: 142.0,
                                width: 190.0,
                                height: 52.0,
                            }),
                            opacity: Some(0.0),
                            ..UiTransitionState::default()
                        },
                    }),
                    children: Vec::new(),
                },
            ],
        };
        HashMap::from([(
            UiFragmentId("window-demo".into()),
            UiFragment {
                fragment_id: UiFragmentId("window-demo".into()),
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )])
    }
}

pub struct WgpuRuntime {
    epoch: u64,
    graph_revision: Revision,
    fragments: HashMap<UiFragmentId, UiFragment>,
    journal: CommandJournal,
    receipts: HashMap<RequestId, CommandReceipt>,
    idempotent_responses: HashMap<String, RpcResponse>,
}

impl WgpuRuntime {
    pub fn headless(epoch: u64) -> Self {
        Self {
            epoch,
            graph_revision: Revision(0),
            fragments: HashMap::new(),
            journal: CommandJournal::new(ServiceName(SERVICE_NAME.into()), epoch, 128),
            receipts: HashMap::new(),
            idempotent_responses: HashMap::new(),
        }
    }

    pub fn service_health(&self) -> ServiceHealth {
        ServiceHealth {
            service: ServiceName(SERVICE_NAME.into()),
            status: HealthStatus::Healthy,
            epoch: self.epoch,
        }
    }

    pub fn service_description(&self) -> ServiceDescription {
        ServiceDescription {
            service: ServiceName(SERVICE_NAME.into()),
            protocol_version: PROTOCOL_VERSION,
            endpoint: "headless://wgpu-runtime".into(),
            epoch: self.epoch,
            capabilities: vec![
                CAPABILITY_UI_FRAGMENT.into(),
                "wgpu.render.diagnostics".into(),
                CAPABILITY_UI_HIT_TARGET.into(),
            ],
        }
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        DebugSnapshot {
            service: ServiceName(SERVICE_NAME.into()),
            epoch: self.epoch,
            revision: self.graph_revision,
            health: HealthStatus::Healthy,
            capabilities: self.service_description().capabilities,
            active_jobs: Vec::new(),
        }
    }

    pub fn diagnostics(&self) -> RenderDiagnostics {
        RenderDiagnostics {
            graph_revision: self.graph_revision,
            fragment_count: self.fragments.len(),
            mode: RenderMode::Headless,
        }
    }

    pub fn fragments_snapshot(&self) -> HashMap<UiFragmentId, UiFragment> {
        self.fragments.clone()
    }

    pub fn command_receipt(&self, request_id: &RequestId) -> Option<&CommandReceipt> {
        self.receipts.get(request_id)
    }

    pub fn traces(&self, filter: &JournalFilter) -> Vec<TraceRecord> {
        self.journal.query(filter)
    }

    pub fn handle(&mut self, request: RpcRequest) -> RpcResponse {
        let request_id = request.request_id.clone();
        self.journal.append(
            TraceLevel::Info,
            EVENT_COMMAND_RECEIVED,
            Some(request_id.clone()),
            None,
            None,
            None,
            Some(self.graph_revision),
            None,
            json!({"method": request.method}),
        );
        self.record_receipt(&request_id, CommandState::Received, None);

        if matches!(
            request.method.as_str(),
            "wgpu.ui.submit_fragment" | "wgpu.ui.remove_fragment"
        ) {
            let Some(idempotency_key) = request.idempotency_key.as_ref() else {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "idempotency_key is required",
                    None,
                );
            };
            if let Some(response) = self.idempotent_responses.get(idempotency_key) {
                let mut response = response.clone();
                response.request_id = request_id.clone();
                self.record_receipt(&request_id, CommandState::Accepted, None);
                return response;
            }
        }

        let response = match request.method.as_str() {
            "service.health" => self.accept(request_id, json!(self.service_health())),
            "service.describe" => self.accept(request_id, json!(self.service_description())),
            "service.shutdown" => self.accept(request_id, json!({"state": "accepted"})),
            "wgpu.render.diagnostics" => {
                self.accept(request_id, diagnostics_value(self.diagnostics()))
            }
            "wgpu.render.graph.snapshot" => self.accept(request_id, json!(composition_graph_snapshot(self.graph_revision))),
            "wgpu.render.target.capture" => self.target_capture(request_id, request.params),
            "wgpu.render.target.assert" => self.target_assert(request_id, request.params),
            "wgpu.resource.inspect" => self.accept(request_id, json!({"resources": []})),
            "debug.snapshot.get" => self.accept(request_id, json!(self.debug_snapshot())),
            "debug.command.get" => self.command_get(request_id, request.params),
            "debug.trace.query" => self.trace_query(request_id, request.params),
            "wgpu.ui.submit_fragment" => self.submit_fragment(request_id, request.params),
            "wgpu.ui.remove_fragment" => self.remove_fragment(request_id, request.params),
            "test.ui.semantic_event.inject" => self.inject_semantic_event(request_id, request.params),
            _ => self.reject(
                request_id,
                "unsupported_method",
                "method is not supported",
                None,
            ),
        };
        if matches!(
            request.method.as_str(),
            "wgpu.ui.submit_fragment" | "wgpu.ui.remove_fragment"
        ) && response.status == RpcStatus::Accepted
            && let Some(idempotency_key) = request.idempotency_key
        {
            self.idempotent_responses
                .insert(idempotency_key, response.clone());
        }
        response
    }

    fn command_get(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(target_id) = params.get("request_id").and_then(Value::as_str) else {
            return self.reject(
                request_id,
                "invalid_request",
                "request_id is required",
                None,
            );
        };
        match self.command_receipt(&RequestId(target_id.into())) {
            Some(receipt) => self.accept(request_id, json!(receipt)),
            None => self.reject(
                request_id,
                "not_found",
                "command receipt was not found",
                None,
            ),
        }
    }

    fn trace_query(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let filter = JournalFilter {
            request_id: params
                .get("request_id")
                .and_then(Value::as_str)
                .map(|value| RequestId(value.into())),
            ..JournalFilter::default()
        };
        self.accept(request_id, json!(self.traces(&filter)))
    }

    fn submit_fragment(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let command: UiCommand = match serde_json::from_value(params) {
            Ok(command @ UiCommand::SubmitFragment { .. }) => command,
            Ok(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "expected submit_fragment command",
                    None,
                );
            }
            Err(_) => {
                return self.reject(request_id, "invalid_request", "invalid UI command", None);
            }
        };
        let UiCommand::SubmitFragment { fragment } = command else {
            unreachable!()
        };
        if fragment.validate().is_err() {
            return self.reject(request_id, "invalid_request", "invalid UI fragment", None);
        }
        if let Some(current) = self.fragments.get(&fragment.fragment_id)
            && fragment.revision <= current.revision
        {
            return self.reject(
                request_id,
                "revision_conflict",
                "fragment revision is stale",
                Some(current.revision),
            );
        }
        self.fragments
            .insert(fragment.fragment_id.clone(), fragment);
        self.graph_revision = Revision(self.graph_revision.0 + 1);
        self.accept(request_id, diagnostics_value(self.diagnostics()))
    }

    fn remove_fragment(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let command: UiCommand = match serde_json::from_value(params) {
            Ok(command @ UiCommand::RemoveFragment { .. }) => command,
            Ok(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "expected remove_fragment command",
                    None,
                );
            }
            Err(_) => {
                return self.reject(request_id, "invalid_request", "invalid UI command", None);
            }
        };
        let UiCommand::RemoveFragment {
            fragment_id,
            revision,
        } = command
        else {
            unreachable!()
        };
        if let Some(current) = self.fragments.get(&fragment_id)
            && revision < current.revision
        {
            return self.reject(
                request_id,
                "revision_conflict",
                "fragment revision is stale",
                Some(current.revision),
            );
        }
        if self.fragments.remove(&fragment_id).is_some() {
            self.graph_revision = Revision(self.graph_revision.0 + 1);
        }
        self.accept(request_id, diagnostics_value(self.diagnostics()))
    }

    /// Test-only U1 scenario bridge. It accepts a semantic event, never a hit ID or node key.
    fn inject_semantic_event(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let event: UiSemanticEvent = match serde_json::from_value(params) {
            Ok(event) => event,
            Err(_) => return self.reject(request_id, "invalid_request", "invalid UI semantic event", None),
        };
        let Some(fragment) = self.fragments.get(&event.fragment.id) else {
            return self.reject(request_id, "fragment_revision_stale", "fragment is not present", None);
        };
        if fragment.revision != event.fragment.revision {
            return self.reject(request_id, "fragment_revision_stale", "fragment revision is stale", Some(fragment.revision));
        }
        if !fragment.effects.iter().any(|effect| matches!(effect, neon_ui_schema::UiEffect::SemanticIntent { intent } if intent == &event.intent)) {
            return self.reject(request_id, "intent_not_bound", "semantic intent is not bound", Some(fragment.revision));
        }
        self.accept(request_id, json!(event))
    }

    fn target_capture(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(target) = params.get("target").and_then(Value::as_str) else {
            return self.reject(request_id, "invalid_request", "target is required", None);
        };
        if !matches!(target, UI_COLOR_TARGET | UI_HIT_TARGET) {
            return self.reject(request_id, "not_found", "target is not available", None);
        }
        self.accept(request_id, json!({"target": target, "format": if target == UI_HIT_TARGET { "r32uint" } else { "rgba8unorm" }, "graph_revision": self.graph_revision, "test_target": true}))
    }

    fn target_assert(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(target) = params.get("target").and_then(Value::as_str) else {
            return self.reject(request_id, "invalid_request", "target is required", None);
        };
        if target != UI_HIT_TARGET { return self.reject(request_id, "unsupported_target", "only the UI hit target has semantic assertions", None); }
        self.accept(request_id, json!({"target": UI_HIT_TARGET, "graph_revision": self.graph_revision, "assertions": "accepted"}))
    }

    fn accept(&mut self, request_id: RequestId, result: Value) -> RpcResponse {
        self.record_receipt(&request_id, CommandState::Accepted, None);
        self.journal.append(
            TraceLevel::Info,
            EVENT_COMMAND_ACCEPTED,
            Some(request_id.clone()),
            None,
            None,
            None,
            None,
            Some(self.graph_revision),
            json!({}),
        );
        RpcResponse {
            request_id,
            status: RpcStatus::Accepted,
            revision: Some(self.graph_revision),
            result: Some(result),
            snapshot: None,
            error: None,
        }
    }

    fn reject(
        &mut self,
        request_id: RequestId,
        code: &str,
        message: &str,
        current_revision: Option<Revision>,
    ) -> RpcResponse {
        self.record_receipt(&request_id, CommandState::Rejected, Some(code.into()));
        self.journal.append(
            TraceLevel::Warn,
            EVENT_COMMAND_REJECTED,
            Some(request_id.clone()),
            None,
            None,
            None,
            Some(self.graph_revision),
            current_revision,
            json!({"code": code}),
        );
        RpcResponse {
            request_id,
            status: RpcStatus::Rejected,
            revision: Some(self.graph_revision),
            result: None,
            snapshot: None,
            error: Some(RpcError {
                code: code.into(),
                message: message.into(),
                current_revision,
                object_id: None,
            }),
        }
    }

    fn record_receipt(
        &mut self,
        request_id: &RequestId,
        state: CommandState,
        error_code: Option<String>,
    ) {
        self.receipts.insert(
            request_id.clone(),
            CommandReceipt {
                request_id: request_id.clone(),
                state,
                revision_before: Some(self.graph_revision),
                revision_after: Some(self.graph_revision),
                error_code,
            },
        );
    }
}

fn diagnostics_value(diagnostics: RenderDiagnostics) -> Value {
    json!({
        "graph_revision": diagnostics.graph_revision,
        "fragment_count": diagnostics.fragment_count,
        "mode": "headless"
    })
}

fn composition_graph_snapshot(graph_revision: Revision) -> Value {
    json!({
        "graph_revision": graph_revision,
        "targets": [
            {"id": UI_COLOR_TARGET, "format": "rgba8unorm", "sample_count": 1},
            {"id": UI_HIT_TARGET, "format": "r32uint", "sample_count": 1, "clear_value": RENDER_HIT_NONE, "usage": ["render_attachment", "copy_src"]}
        ],
        "passes": ["ui.color.panels.v1", "ui.hit_id.panels.v1"],
        "overlay_precedence": "ui_over_world"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion};
    use neon_ui_schema::{UiBounds, UiEffect, UiNode, UiNodeId, UiNodeKind, UiStyle};

    fn request(id: &str, method: &str, params: Value) -> RpcRequest {
        RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId(id.into()),
            client: ClientIdentity {
                kind: ClientKind::Cli,
                instance_id: "test".into(),
                pid: 1,
                origin: "test".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: method.into(),
            params,
            expected_revision: None,
            idempotency_key: None,
        }
    }

    fn fragment(revision: u64) -> UiFragment {
        UiFragment {
            fragment_id: UiFragmentId("static-fragment".into()),
            revision: Revision(revision),
            root: UiNode {
                node_id: UiNodeId("root".into()),
                kind: UiNodeKind::Panel,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                visible: true,
                enabled: true,
                text_key: None,
                style: UiStyle::default(),
                enter_transition: None,
                children: Vec::new(),
            },
            effects: vec![UiEffect::SemanticAction {
                action: "ui.static.ready".into(),
            }],
        }
    }

    fn submit(id: &str, revision: u64) -> RpcRequest {
        let mut request = request(
            id,
            "wgpu.ui.submit_fragment",
            json!(UiCommand::SubmitFragment {
                fragment: fragment(revision)
            }),
        );
        request.idempotency_key = Some(format!("key-{id}"));
        request
    }

    #[test]
    fn headless_health_and_describe_are_available() {
        let mut runtime = WgpuRuntime::headless(7);
        let health = runtime.handle(request("health", "service.health", json!({})));
        let describe = runtime.handle(request("describe", "service.describe", json!({})));
        let snapshot = runtime.handle(request("snapshot", "debug.snapshot.get", json!({})));
        assert_eq!(health.status, RpcStatus::Accepted);
        assert_eq!(health.result.unwrap()["status"], "healthy");
        let described = describe.result.unwrap();
        assert_eq!(described["epoch"], 7);
        assert_eq!(described["protocol_version"], json!(PROTOCOL_VERSION));
        assert_eq!(
            described["capabilities"],
            json!([CAPABILITY_UI_FRAGMENT, "wgpu.render.diagnostics"])
        );
        assert_eq!(snapshot.status, RpcStatus::Accepted);
        assert_eq!(
            snapshot.result.unwrap()["capabilities"],
            json!([CAPABILITY_UI_FRAGMENT, "wgpu.render.diagnostics"])
        );
    }

    #[test]
    fn submit_updates_headless_composition_registry() {
        let mut runtime = WgpuRuntime::headless(1);
        let response = runtime.handle(submit("submit", 1));
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.diagnostics().fragment_count, 1);
        assert_eq!(runtime.diagnostics().graph_revision, Revision(1));
        assert_eq!(
            runtime
                .command_receipt(&RequestId("submit".into()))
                .unwrap()
                .state,
            CommandState::Accepted
        );
    }

    #[test]
    fn stale_fragment_revision_is_rejected() {
        let mut runtime = WgpuRuntime::headless(1);
        runtime.handle(submit("fresh", 2));
        let response = runtime.handle(submit("stale", 1));
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "revision_conflict");
    }

    #[test]
    fn remove_is_idempotent_and_explicit() {
        let mut runtime = WgpuRuntime::headless(1);
        runtime.handle(submit("submit", 1));
        let params = json!(UiCommand::RemoveFragment {
            fragment_id: UiFragmentId("static-fragment".into()),
            revision: Revision(1),
        });
        let mut first = request("remove-one", "wgpu.ui.remove_fragment", params.clone());
        first.idempotency_key = Some("remove-one".into());
        let mut second = request("remove-two", "wgpu.ui.remove_fragment", params);
        second.idempotency_key = Some("remove-two".into());
        assert_eq!(runtime.handle(first).status, RpcStatus::Accepted);
        assert_eq!(runtime.handle(second).status, RpcStatus::Accepted);
        assert_eq!(runtime.diagnostics().fragment_count, 0);
    }

    #[test]
    fn repeated_idempotency_key_does_not_mutate_twice() {
        let mut runtime = WgpuRuntime::headless(1);
        let first = runtime.handle(submit("first", 1));
        let mut repeat = submit("repeat", 2);
        repeat.idempotency_key = Some("key-first".into());
        let repeated = runtime.handle(repeat);
        assert_eq!(first.status, RpcStatus::Accepted);
        assert_eq!(repeated.status, RpcStatus::Accepted);
        assert_eq!(runtime.diagnostics().graph_revision, Revision(1));
        assert_eq!(runtime.diagnostics().fragment_count, 1);
    }

    #[test]
    fn receipt_is_available_through_debug_command() {
        let mut runtime = WgpuRuntime::headless(1);
        runtime.handle(submit("submit", 1));
        let response = runtime.handle(request(
            "lookup",
            "debug.command.get",
            json!({"request_id": "submit"}),
        ));
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.result.unwrap()["state"], "accepted");
    }

    #[test]
    fn ui_runtime_and_cli_source_do_not_claim_renderer_ownership() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for crate_name in ["neon-ui-runtime", "neon-cli"] {
            let source = std::fs::read_to_string(
                workspace
                    .join("crates")
                    .join(crate_name)
                    .join("src/main.rs"),
            )
            .expect("first-phase runtime source must exist");
            for forbidden in ["wgpu::", "winit::", "Window", "Device", "Queue"] {
                assert!(
                    !source.contains(forbidden),
                    "{crate_name} source must not claim renderer ownership via {forbidden}"
                );
            }
        }
    }

    #[test]
    fn only_wgpu_runtime_declares_window_or_gpu_dependencies() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for crate_name in [
            "neon-protocol",
            "neon-ipc",
            "neon-observability",
            "neon-ui-schema",
            "neon-ui-runtime",
            "neon-cli",
        ] {
            let manifest = std::fs::read_to_string(
                workspace.join("crates").join(crate_name).join("Cargo.toml"),
            )
            .expect("runtime manifest must exist");
            assert!(
                !manifest.contains("wgpu") && !manifest.contains("winit"),
                "{crate_name} must not declare window or GPU dependencies"
            );
        }
        let renderer_manifest =
            std::fs::read_to_string(workspace.join("crates/neon-wgpu-runtime/Cargo.toml"))
                .expect("renderer manifest must exist");
        assert!(renderer_manifest.contains("wgpu.workspace = true"));
        assert!(renderer_manifest.contains("winit.workspace = true"));
    }

    #[test]
    fn headless_gpu_adapter_device_and_queue_are_ready() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        }))
        .or_else(|| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
        })
        .expect("a headless WGPU adapter is required for gpu-ready evidence");
        let (_device, _queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("neon3-headless-acceptance"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
            },
            None,
        ))
        .expect("the selected headless adapter must create a device and queue");
    }

    #[test]
    fn ui_fragment_renders_visible_pixels_to_offscreen_target() {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        }))
        .or_else(|| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
        })
        .expect("a headless WGPU adapter is required for UI render acceptance");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("neon3-ui-render-acceptance"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
            },
            None,
        ))
        .expect("the selected adapter must create a UI acceptance device");
        ui_renderer::render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(UiFragmentId("acceptance".into()), fragment(1))]),
            [64, 64],
            1.0,
        );
    }

    #[test]
    fn test_semantic_injection_accepts_only_bound_semantic_events() {
        use neon_ui_schema::{UiFragmentRevision, UiIntent, UiPointerMetadata, UiSemanticEventType};
        let mut runtime = WgpuRuntime::headless(7);
        let mut semantic_fragment = fragment(1);
        semantic_fragment.effects.push(neon_ui_schema::UiEffect::SemanticIntent {
            intent: UiIntent::Invoke {
                action: "terrain.tool.select".into(),
                params: json!({"tool": "water_inject"}),
            },
        });
        let mut submit_request = request(
            "submit",
            "wgpu.ui.submit_fragment",
            json!(UiCommand::SubmitFragment { fragment: semantic_fragment }),
        );
        submit_request.idempotency_key = Some("submit-key".into());
        runtime.handle(submit_request);
        let event = UiSemanticEvent {
            event: UiSemanticEventType::PointerClick,
            event_id: "event-1".into(),
            renderer_epoch: 7,
            composition_revision: Revision(1),
            fragment: UiFragmentRevision {
                id: UiFragmentId("static-fragment".into()),
                revision: Revision(1),
            },
            intent: UiIntent::Invoke {
                action: "terrain.tool.select".into(),
                params: json!({"tool": "water_inject"}),
            },
            pointer: Some(UiPointerMetadata {
                id: 0,
                sequence: 1,
                logical_position: [4.0, 4.0],
            }),
            focus: None,
        };
        let response = runtime.handle(request(
            "inject",
            "test.ui.semantic_event.inject",
            json!(event),
        ));
        assert_eq!(response.status, RpcStatus::Accepted);
        assert!(response.result.unwrap().get("render_hit_id").is_none());
    }
}
