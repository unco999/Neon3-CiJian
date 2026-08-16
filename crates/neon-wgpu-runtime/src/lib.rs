//! Command handling and window/GPU bootstrap for Neon3's sole renderer owner.
//! No other Neon3 crate may initialize window or GPU objects.

use std::sync::{Arc, Mutex};

use std::time::{Duration, Instant};
use std::{collections::{HashMap, VecDeque}, net::SocketAddr, thread};

use neon_ipc::RpcClient;
use neon_observability::{
    CommandJournal, CommandReceipt, CommandState, DebugSnapshot, EVENT_COMMAND_ACCEPTED,
    EVENT_COMMAND_RECEIVED, EVENT_COMMAND_REJECTED, JournalFilter, TraceLevel, TraceRecord,
};
use neon_protocol::{
    AiTerrainGenerateCommand, AiTerrainGenerationResult, AssetBytes, AssetRef, ClientIdentity, ClientKind, HealthStatus, InteractionId, InteractionSemanticTarget, InteractionTraceError, InteractionTraceFilters, InteractionTraceOutcome, InteractionTraceQuery, InteractionTraceRecord, InteractionTraceStage, PROTOCOL_VERSION, RequestId, Revision, RpcError, RpcRequest, RpcResponse,
    RpcStatus, ServiceDescription, ServiceHealth, ServiceName,
};
#[cfg(test)]
use neon_ui_schema::UiFragmentSubmission;
use neon_ui_schema::{
    UiBounds, UiCommand, UiDataGridWindowRequest, UiFragment, UiFragmentId, UiHostInbound, UiNode, UiNodeKind,
    UiSemanticEvent, UiStyle, UiTransition, UiTransitionState, UiWindowRequest,
};
use serde_json::{Value, json};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

mod gpu_preview;
mod ui_program_gpu;
mod ui_renderer;
use gpu_preview::HeightmapPreviewConverter;
pub use ui_program_gpu::GpuUiProgramBackend;
use ui_renderer::{UiHitBinding, UiWgpuRenderer};

pub const SERVICE_NAME: &str = "wgpu-runtime";
pub const CAPABILITY_UI_FRAGMENT: &str = "wgpu.ui.fragment.v1";
pub const CAPABILITY_UI_HIT_TARGET: &str = "wgpu.ui.hit_target.v1";
pub const CAPABILITY_UI_SEMANTIC_EVENT: &str = "wgpu.ui.semantic_event.v1";
pub const CAPABILITY_UI_PROGRAM_SEMANTIC_EVENT: &str = "wgpu.ui.program.semantic_event.v1";
pub const CAPABILITY_UI_RENDER_SURFACE: &str = "wgpu.ui.render_surface.v1";
pub const CAPABILITY_AI_TERRAIN_GENERATION: &str = "wgpu.ai.terrain_generation.v1";
pub const CAPABILITY_DEBUG_INTERACTION: &str = "debug.interaction.v1";
pub const UI_HIT_TARGET: &str = "ui.hit_id.v1";
pub const UI_COLOR_TARGET: &str = "ui.color.v1";
pub const RENDER_HIT_NONE: u32 = u32::MAX;
const DATA_GRID_WINDOW_DEBOUNCE: Duration = Duration::from_millis(24);
const INTERACTION_TRACE_CAPACITY: usize = 256;

/// Bounded process-local retention for interaction diagnostics. The public
/// records intentionally carry no renderer-local hit-test or position data.
struct InteractionTraceStore {
    next_sequence: u64,
    records: VecDeque<InteractionTraceRecord>,
    accepted_waiting_for_composition: VecDeque<InteractionId>,
}

impl InteractionTraceStore {
    fn new() -> Self {
        Self {
            next_sequence: 1,
            records: VecDeque::with_capacity(INTERACTION_TRACE_CAPACITY),
            accepted_waiting_for_composition: VecDeque::new(),
        }
    }

    fn append(
        &mut self,
        interaction_id: InteractionId,
        stage: InteractionTraceStage,
        outcome: InteractionTraceOutcome,
        error: Option<InteractionTraceError>,
        semantic_target: Option<InteractionSemanticTarget>,
        fragment_revision: Option<Revision>,
        composition_revision: Revision,
        downstream_request_id: Option<RequestId>,
    ) {
        let record = InteractionTraceRecord {
            sequence: self.next_sequence,
            interaction_id,
            stage,
            outcome,
            error,
            semantic_target,
            fragment_revision,
            composition_revision,
            downstream_request_id,
        };
        self.next_sequence += 1;
        if self.records.len() == INTERACTION_TRACE_CAPACITY {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    fn delivery_accepted(&mut self, interaction_id: InteractionId) {
        self.accepted_waiting_for_composition.push_back(interaction_id);
    }

    fn composition_applied(&mut self, composition_revision: Revision) {
        let Some(interaction_id) = self.accepted_waiting_for_composition.pop_front() else {
            return;
        };
        let previous = self.records.iter().rev().find(|record| record.interaction_id == interaction_id);
        self.append(
            interaction_id,
            InteractionTraceStage::CompositionRevisionApplied,
            InteractionTraceOutcome::Accepted,
            None,
            previous.and_then(|record| record.semantic_target.clone()),
            previous.and_then(|record| record.fragment_revision),
            composition_revision,
            previous.and_then(|record| record.downstream_request_id.clone()),
        );
    }

    fn get(&self, interaction_id: &InteractionId) -> Vec<InteractionTraceRecord> {
        self.records
            .iter()
            .filter(|record| &record.interaction_id == interaction_id)
            .cloned()
            .collect()
    }

    fn query(&self, query: &InteractionTraceQuery) -> Vec<InteractionTraceRecord> {
        let filters = query.filters.as_ref();
        let limit = query.limit.unwrap_or(100).min(INTERACTION_TRACE_CAPACITY);
        self.records
            .iter()
            .filter(|record| query.after.is_none_or(|after| record.sequence > after))
            .filter(|record| filters.is_none_or(|filters| interaction_matches(record, filters)))
            .take(limit)
            .cloned()
            .collect()
    }
}

fn interaction_matches(record: &InteractionTraceRecord, filters: &InteractionTraceFilters) -> bool {
    filters.interaction_id.as_ref().is_none_or(|id| &record.interaction_id == id)
        && filters.stage.is_none_or(|stage| record.stage == stage)
        && filters.outcome.is_none_or(|outcome| record.outcome == outcome)
        && filters.semantic_node_path.as_ref().is_none_or(|path| {
            record.semantic_target.as_ref().is_some_and(|target| &target.node_path == path)
        })
        && filters.downstream_request_id.as_ref().is_none_or(|id| {
            record.downstream_request_id.as_ref() == Some(id)
        })
}

fn append_interaction_record(
    traces: &Arc<Mutex<InteractionTraceStore>>,
    interaction_id: InteractionId,
    stage: InteractionTraceStage,
    outcome: InteractionTraceOutcome,
    error: Option<InteractionTraceError>,
    binding: Option<&UiHitBinding>,
    composition_revision: Revision,
) {
    if let Ok(mut traces) = traces.lock() {
        traces.append(
            interaction_id,
            stage,
            outcome,
            error,
            binding.map(semantic_target),
            binding.map(|binding| binding.fragment.revision),
            composition_revision,
            None,
        );
    }
}

/// Retains only the newest local viewport demand for each virtual grid. The
/// renderer dispatches these after a short debounce, never per wheel event.
#[derive(Default)]
struct LatestDataGridWindowRequests {
    pending: HashMap<String, (Instant, UiDataGridWindowRequest)>,
}

impl LatestDataGridWindowRequests {
    fn schedule(&mut self, request: UiDataGridWindowRequest, now: Instant) {
        let key = format!("{}/{}", request.fragment.id.0, request.source_key);
        self.pending.insert(key, (now, request));
    }

    fn take_ready(&mut self, now: Instant) -> Vec<UiDataGridWindowRequest> {
        let ready = self.pending.iter()
            .filter(|(_, (scheduled, _))| { now.duration_since(*scheduled) >= DATA_GRID_WINDOW_DEBOUNCE
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        ready.into_iter().filter_map(|key| self.pending.remove(&key).map(|(_, request)| request)).collect()
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|(scheduled, _)| *scheduled + DATA_GRID_WINDOW_DEBOUNCE).min()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UiResourceState { Loading, Ready, Failed }

impl UiResourceState { fn as_str(&self) -> &'static str { match self { Self::Loading => "loading", Self::Ready => "ready", Self::Failed => "failed" } } }

#[derive(Clone, Debug)]
struct UiResourceRecord { asset: AssetRef, job_id: String, state: UiResourceState }

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalInteractionState { Idle, Hovered, Captured, Cancelled }

/// Renderer-owned feedback only. Its semantic keys are diagnostic labels; the
/// actual hit IDs, pointer positions, selection geometry, and capture handles
/// stay private to this process. No field in this type is domain authority.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiLocalPresentationState {
    pub hovered_node_key: Option<String>,
    pub pressed_node_key: Option<String>,
    pub captured_node_key: Option<String>,
    pub scroll_preview_active: bool,
    pub text_edit_active: bool,
    pub renderer_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HitSampleRequest { pointer_id: u64, sequence: u64, composition_revision: Revision, target_generation: u64 }

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalInputState {
    state: LocalInteractionState,
    hover_id: Option<u32>,
    capture_id: Option<u32>,
    last_sequence: std::collections::HashMap<u64, u64>,
    pending: std::collections::HashMap<u64, HitSampleRequest>,
}

impl Default for LocalInputState {
    fn default() -> Self { Self { state: LocalInteractionState::Idle, hover_id: None, capture_id: None, last_sequence: HashMap::new(), pending: HashMap::new() } }
}

impl LocalInputState {
    fn request_sample(&mut self, request: HitSampleRequest) { self.pending.insert(request.pointer_id, request); }

    fn complete_sample(&mut self, pointer_id: u64, current_revision: Revision, current_generation: u64, hit_id: u32) -> Result<(), &'static str> {
        let Some(request) = self.pending.remove(&pointer_id) else { return Err("interaction_cancelled"); };
        if request.target_generation != current_generation { return Err("hit_target_generation_stale"); }
        if request.composition_revision != current_revision { return Err("composition_revision_stale"); }
        if self.last_sequence.get(&request.pointer_id).is_some_and(|last| request.sequence <= *last) { return Err("input_sequence_stale"); }
        self.last_sequence.insert(request.pointer_id, request.sequence);
        self.set_hover_id((hit_id != RENDER_HIT_NONE).then_some(hit_id));
        Ok(())
    }

    fn set_hover_id(&mut self, hit_id: Option<u32>) {
        self.hover_id = hit_id;
        self.state = if self.hover_id.is_some() {
            LocalInteractionState::Hovered
        } else {
            LocalInteractionState::Idle
        };
    }

    fn pointer_down(&mut self) -> Result<(), &'static str> {
        let Some(hit_id) = self.hover_id else { return Err("focus_invalid"); };
        self.capture_id = Some(hit_id); self.state = LocalInteractionState::Captured; Ok(())
    }

    fn pointer_up(&mut self, eligible: bool) -> Result<(), &'static str> {
        let captured = self.capture_id.take().is_some(); self.state = LocalInteractionState::Idle;
        if captured && eligible { Ok(()) } else { Err("interaction_cancelled") }
    }

    fn cancel(&mut self) { self.capture_id = None; self.hover_id = None; self.pending.clear(); self.state = LocalInteractionState::Cancelled; }

    fn state_name(&self) -> &'static str { match self.state { LocalInteractionState::Idle => "idle", LocalInteractionState::Hovered => "hovered", LocalInteractionState::Captured => "captured", LocalInteractionState::Cancelled => "cancelled" } }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderDiagnostics {
    pub graph_revision: Revision,
    pub fragment_count: usize,
    pub mode: RenderMode,
    pub hit_target_generation: u64,
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
    applied_composition_revision: Revision,
    redraw_pending: bool,
    animation_active: bool,
    pending_composition_ack: Option<std::sync::mpsc::Sender<()>>,
    ui_endpoint: Option<SocketAddr>,
    pointer_delivery: Arc<Mutex<Value>>,
    interaction_traces: Arc<Mutex<InteractionTraceStore>>,
    next_interaction_id: u64,
    data_grid_window_requests: LatestDataGridWindowRequests,
    next_data_grid_window_sequence: u64,
}

#[derive(Clone, Debug)]
enum WindowCommand {
    Fragments {
        composition_revision: Revision,
        fragments: HashMap<UiFragmentId, UiFragment>,
        applied: Option<std::sync::mpsc::Sender<()>>,
    },
    GenerateTerrainPreview {
        command: AiTerrainGenerateCommand,
        job_id: String,
        completed: std::sync::mpsc::Sender<Result<AiTerrainGenerationResult, String>>,
    },
    AiModelStatus {
        completed: std::sync::mpsc::Sender<Option<neon_wgpu_ai::ModelInfo>>,
    },
    InputDebugSnapshot {
        completed: std::sync::mpsc::Sender<Value>,
    },
    InputDebugProbe {
        logical_position: Option<[f64; 2]>,
        physical_position: Option<[f64; 2]>,
        completed: std::sync::mpsc::Sender<Value>,
    },
    InputDebugActivate {
        logical_position: [f64; 2],
        completed: std::sync::mpsc::Sender<Result<Value, &'static str>>,
    },
    Shutdown,
}

struct WindowGpu {
    _instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    ui: UiWgpuRenderer,
    ai: neon_wgpu_ai::AiEngine,
    heightmap_preview: HeightmapPreviewConverter,
    hit_target: wgpu::Texture,
    hit_target_view: wgpu::TextureView,
    hit_target_generation: u64,
    input: LocalInputState,
    pending_hit_pixel: Option<[u32; 2]>,
    pending_hit_slot: Option<(usize, u64)>,
    captured_binding: Option<UiHitBinding>,
    pending_control_value: Option<neon_ui_schema::UiSemanticPayloadValue>,
    next_input_sequence: u64,
    next_semantic_sequence: u64,
    ime_active: bool,
    shift_down: bool,
    text_selection_drag: bool,
    started_at: Instant,
    last_draw_instance_count: usize,
    hit_target_dirty: bool,
    last_present: Instant,
    frame_count: u64,
    longest_frame_gap_ms: f32,
    last_pointer_outcome: String,
    last_pointer_node_path: Option<String>,
    active_interaction_id: Option<InteractionId>,
}

impl WindowedRuntime {
    fn begin_os_pointer_interaction(&mut self) -> InteractionId {
        self.next_interaction_id += 1;
        let interaction_id = InteractionId(format!("wgpu-window-{}-{}", self.epoch, self.next_interaction_id));
        if let Ok(mut traces) = self.interaction_traces.lock() {
            traces.append(
                interaction_id.clone(),
                InteractionTraceStage::Prepared,
                InteractionTraceOutcome::Pending,
                None,
                None,
                None,
                self.applied_composition_revision,
                None,
            );
        }
        interaction_id
    }

    fn prepare_pointer_interaction(&mut self) {
        let fragments = &self.fragments;
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.ui.prepare_interaction(
                fragments,
                [gpu.config.width, gpu.config.height],
                gpu.started_at.elapsed().as_secs_f32(),
            );
        }
    }

    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            gpu: None,
            window: None,
            exit_error: None,
            fragments: HashMap::new(),
            applied_composition_revision: Revision(0),
            redraw_pending: true,
            animation_active: false,
            pending_composition_ack: None,
            ui_endpoint: None,
            pointer_delivery: Arc::new(Mutex::new(json!({"state": "none"}))),
            interaction_traces: Arc::new(Mutex::new(InteractionTraceStore::new())),
            next_interaction_id: 0,
            data_grid_window_requests: LatestDataGridWindowRequests::default(),
            next_data_grid_window_sequence: 0,
        }
    }

    pub fn run(epoch: u64) -> Result<(), String> {
        Self::run_with_server(epoch, None, None, true)
    }

    pub fn run_server(
        epoch: u64,
        endpoint: SocketAddr,
        ui_endpoint: Option<SocketAddr>,
    ) -> Result<(), String> {
        Self::run_with_server(epoch, Some(endpoint), ui_endpoint, false)
    }

    fn run_with_server(
        epoch: u64,
        endpoint: Option<SocketAddr>,
        ui_endpoint: Option<SocketAddr>,
        demo: bool,
    ) -> Result<(), String> {
        let event_loop = EventLoop::<WindowCommand>::with_user_event()
            .build()
            .map_err(|error| format!("create event loop: {error}"))?;
        let proxy = event_loop.create_proxy();
        let mut runtime = Self::new(epoch);
        runtime.ui_endpoint = ui_endpoint;
        if demo {
            runtime.fragments = runtime.demo_fragments();
            runtime.applied_composition_revision = Revision(1);
        }
        if let Some(endpoint) = endpoint {
            let interaction_traces = runtime.interaction_traces.clone();
            spawn_window_server(epoch, endpoint, proxy, interaction_traces);
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
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(event_loop.owned_display_handle()),
        ));
        let gpu = WindowGpu::new(&window, instance)?;
        self.window = Some(window);
        self.gpu = Some(gpu);
        self.redraw_pending = true;
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
            let (target, view) = create_hit_target(&gpu.device, size);
            gpu.hit_target = target;
            gpu.hit_target_view = view;
            gpu.hit_target_generation += 1;
            gpu.hit_target_dirty = true;
            self.redraw_pending = true;
        }
    }

    fn redraw(&mut self) -> Result<(), String> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Ok(());
        };
        let was_animation_active = self.animation_active;
        let frame_started = Instant::now();
        if gpu.pending_hit_slot.is_some() {
            let _ = gpu.device.poll(wgpu::PollType::Poll);
        }
        if let Some((slot, sequence)) = gpu.pending_hit_slot
            && let Some(result) = gpu.ui.try_complete_hit_readback(slot)
        {
            gpu.pending_hit_slot = None;
            match result {
                Ok(hit_id) => {
                    let _ = gpu.input.complete_sample(0, self.applied_composition_revision, gpu.hit_target_generation, hit_id);
                    let _ = sequence;
                }
                Err(_) => gpu.input.cancel(),
            }
        }
        let surface_texture = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gpu.surface.configure(&gpu.device, &gpu.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("acquire surface texture: validation error".to_owned());
            }
        };
        let acquired_at = Instant::now();
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
                    depth_slice: None,
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
                multiview_mask: None,
            });
            gpu.ui.draw(
                &gpu.device,
                &gpu.queue,
                &mut pass,
                &self.fragments,
                [gpu.config.width, gpu.config.height],
                gpu.started_at.elapsed().as_secs_f32(),
            );
            drop(pass);
            gpu.last_draw_instance_count = gpu.ui.last_panel_instance_count();
        }
        if gpu.hit_target_dirty || gpu.pending_hit_pixel.is_some() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-final-ui-hit-id-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gpu.hit_target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None,
                multiview_mask: None,
            });
            gpu.ui.draw_hit_id(&gpu.device, &gpu.queue, &mut pass, &self.fragments, [gpu.config.width, gpu.config.height], gpu.started_at.elapsed().as_secs_f32());
            gpu.hit_target_dirty = false;
        }
        let queued_readback = gpu.pending_hit_pixel.take().and_then(|pixel| {
            gpu.ui.enqueue_hit_readback(&mut encoder, &gpu.hit_target, pixel).map(|slot| (slot, pixel))
        });
        gpu.queue.submit(Some(encoder.finish()));
        let submitted_at = Instant::now();
        if let Some((slot, _)) = queued_readback {
            gpu.next_input_sequence += 1;
            gpu.input.request_sample(HitSampleRequest { pointer_id: 0, sequence: gpu.next_input_sequence, composition_revision: self.applied_composition_revision, target_generation: gpu.hit_target_generation });
            if gpu.ui.begin_hit_readback_mapping(slot) { gpu.pending_hit_slot = Some((slot, gpu.next_input_sequence)); }
        }
        gpu.queue.present(surface_texture);
        let now = Instant::now();
        let frame_gap_ms = now.duration_since(gpu.last_present).as_secs_f32() * 1000.0;
        gpu.longest_frame_gap_ms = gpu.longest_frame_gap_ms.max(frame_gap_ms);
        gpu.last_present = now;
        gpu.frame_count += 1;
        if let Some(applied) = self.pending_composition_ack.take() {
            let _ = applied.send(());
        }
        self.redraw_pending = false;
        self.animation_active = gpu.ui.has_active_animation(gpu.started_at.elapsed().as_secs_f32());
        if was_animation_active && frame_gap_ms > 34.0 {
            eprintln!(
                "neon-wgpu animation frame gap: {:.1}ms (frame {}, acquire {:.1}ms, encode {:.1}ms, submit+present {:.1}ms)",
                frame_gap_ms,
                gpu.frame_count,
                acquired_at.duration_since(frame_started).as_secs_f32() * 1000.0,
                submitted_at.duration_since(acquired_at).as_secs_f32() * 1000.0,
                now.duration_since(submitted_at).as_secs_f32() * 1000.0,
            );
        }
        if self.animation_active {
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        Ok(())
    }

    fn apply_fragments(&mut self, composition_revision: Revision, fragments: HashMap<UiFragmentId, UiFragment>) -> bool {
        if composition_revision <= self.applied_composition_revision {
            return false;
        }
        self.applied_composition_revision = composition_revision;
        self.fragments = fragments;
        self.redraw_pending = true;
        if let Ok(mut traces) = self.interaction_traces.lock() {
            traces.composition_applied(composition_revision);
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.hit_target_dirty = true;
        }
        true
    }

    fn schedule_data_grid_window_requests(&mut self) {
        let requests = self.gpu.as_ref().map(|gpu| { gpu.ui.data_grid_window_requests(
            &self.fragments,
            self.epoch,
            self.applied_composition_revision,
            &mut self.next_data_grid_window_sequence,
        )
            }).unwrap_or_default();
        let now = Instant::now();
        for request in requests {
            self.data_grid_window_requests.schedule(request, now);
        }
    }

    fn dispatch_ready_data_grid_window_requests(&mut self) {
        let Some(endpoint) = self.ui_endpoint else { return; };
        for request in self.data_grid_window_requests.take_ready(Instant::now()) {
            forward_data_grid_window_request(endpoint, request);
        }
    }

    fn needs_redraw(&self) -> bool {
        self.redraw_pending
            || self.animation_active
            || self.gpu.as_ref().is_some_and(|gpu| gpu.pending_hit_slot.is_some())
    }

    fn input_debug_snapshot(&self) -> Value {
        let Some(gpu) = self.gpu.as_ref() else {
            return json!({"state": "uninitialized"});
        };
        let node_path = |hit_id: Option<u32>| {
            hit_id
                .and_then(|hit_id| gpu.ui.hit_binding(hit_id))
                .and_then(|binding| diagnostic_node_path(&binding))
        };
        json!({
            "state": gpu.input.state_name(),
            "applied_composition_revision": self.applied_composition_revision.0,
            "hovered_node_path": node_path(gpu.input.hover_id),
            "captured_node_path": node_path(gpu.input.capture_id),
            "last_pointer_outcome": gpu.last_pointer_outcome,
            "last_pointer_node_path": gpu.last_pointer_node_path,
            "pending_hit_readback": gpu.pending_hit_slot.is_some(),
            "pending_hit_pixel": gpu.pending_hit_pixel.is_some(),
            "semantic_hit_nodes": gpu.ui.semantic_hit_nodes(),
            "dropdown": gpu.ui.dropdown_debug_snapshot(),
            "pointer_delivery": self.pointer_delivery.lock().ok().map(|state| state.clone()),
        })
    }

    fn input_debug_probe(&mut self, logical_position: Option<[f64; 2]>, physical_position: Option<[f64; 2]>) -> Value {
        let Some(gpu) = self.gpu.as_mut() else {
            return json!({"state": "uninitialized"});
        };
        let scale_factor = self.window.as_ref().map(Window::scale_factor).unwrap_or(1.0);
        let physical_position = physical_position
            .or_else(|| logical_position.map(|position| [position[0] * scale_factor, position[1] * scale_factor]))
            .expect("probe position is validated before dispatch");
        let logical_position = logical_position.unwrap_or([physical_position[0] / scale_factor, physical_position[1] / scale_factor]);
        let physical_position = [physical_position[0] as f32, physical_position[1] as f32];
        gpu.ui.set_pointer_position(physical_position);
        gpu.ui.prepare_interaction(
            &self.fragments,
            [gpu.config.width, gpu.config.height],
            gpu.started_at.elapsed().as_secs_f32(),
        );
        let mut probe = gpu.ui.pointer_probe_snapshot();
        probe["pointer"] = json!({
            "logical": {"x": logical_position[0], "y": logical_position[1]},
            "physical": {"x": physical_position[0], "y": physical_position[1]},
        });
        let x = physical_position[0].max(0.0).min(gpu.config.width.saturating_sub(1) as f32) as u32;
        let y = physical_position[1].max(0.0).min(gpu.config.height.saturating_sub(1) as f32) as u32;
        gpu.pending_hit_pixel = Some([x, y]);
        probe["gpu_hit_readback"] = json!({
            "status": "queued",
            "available": true,
            "pending": gpu.pending_hit_slot.is_some() || gpu.pending_hit_pixel.is_some(),
        });
        probe["last_pointer_delivery"] = self.pointer_delivery.lock().ok().map(|state| state.clone()).unwrap_or_else(|| json!({"state": "unavailable"}));
        probe["last_pointer"] = json!({
            "outcome": gpu.last_pointer_outcome,
            "semantic_node_path": gpu.last_pointer_node_path,
        });
        self.redraw_pending = true;
        probe
    }

    /// Debug-only automation entry point. The point is resolved through the
    /// same prepared CPU binding and capture state used by pointer release;
    /// renderer hit identifiers never leave this process.
    fn input_debug_activate(&mut self, logical_position: [f64; 2]) -> Result<Value, &'static str> {
        let Some(gpu) = self.gpu.as_mut() else { return Err("window_gpu_unavailable"); };
        let scale_factor = self.window.as_ref().map(Window::scale_factor).unwrap_or(1.0);
        let physical = [
            (logical_position[0] * scale_factor) as f32,
            (logical_position[1] * scale_factor) as f32,
        ];
        gpu.ui.set_pointer_position(physical);
        gpu.ui.prepare_interaction(&self.fragments, [gpu.config.width, gpu.config.height], gpu.started_at.elapsed().as_secs_f32());
        let Some(hit_id) = gpu.ui.hit_id_at_pointer() else { return Err("press_without_semantic_hit"); };
        gpu.input.set_hover_id(Some(hit_id));
        gpu.input.pointer_down().map_err(|_| "pointer_down_rejected")?;
        gpu.captured_binding = gpu.input.capture_id.and_then(|id| gpu.ui.hit_binding(id));
        gpu.pending_control_value = gpu.captured_binding.as_ref().and_then(|binding| binding.control_value.clone());
        let Some(binding) = gpu.captured_binding.as_ref() else { return Err("press_without_semantic_binding"); };
        if binding.text_input.is_some() || (gpu.ui.requires_value_gesture(binding) && !gpu.ui.begin_value_gesture(binding)) {
            gpu.captured_binding = None;
            gpu.input.cancel();
            return Err("debug_activation_not_supported_for_control");
        }
        let (binding, control_value, sequence) = release_captured_binding(gpu).ok_or("interaction_cancelled")?;
        let node_path = diagnostic_node_path(&binding);
        gpu.last_pointer_node_path = node_path.clone();
        gpu.last_pointer_outcome = if binding.intent.is_some() { "semantic_event_forwarded".into() } else { "release_without_semantic_binding".into() };
        let endpoint = self.ui_endpoint.ok_or("ui_host_unavailable")?;
        forward_pointer_click(endpoint, self.epoch, self.applied_composition_revision, sequence, None, binding, control_value, self.pointer_delivery.clone(), self.interaction_traces.clone());
        Ok(json!({"state": "forwarded", "sequence": sequence, "node_path": node_path}))
    }
}

/// Finalizes a captured pointer using the normal release semantics. Both OS
/// release and debug activation use this so tests cannot bypass control state.
fn release_captured_binding(gpu: &mut WindowGpu) -> Option<(UiHitBinding, Option<neon_ui_schema::UiSemanticPayloadValue>, u64)> {
    let initial_value = gpu.pending_control_value.take();
    let control_value = gpu.ui.finish_value_gesture().or(initial_value);
    let binding = gpu.captured_binding.take();
    gpu.input.pointer_up(binding.is_some()).ok()?;
    let binding = binding?;
    if binding.text_input.is_some() { return None; }
    gpu.next_semantic_sequence += 1;
    Some((binding, control_value, gpu.next_semantic_sequence))
}

fn diagnostic_node_path(binding: &UiHitBinding) -> Option<String> {
    binding
        .data_grid_cell
        .is_none()
        .then(|| binding.node_path.clone())
}

fn semantic_target(binding: &UiHitBinding) -> InteractionSemanticTarget {
    InteractionSemanticTarget { node_path: binding.node_path.clone() }
}

impl WindowGpu {
    fn new(window: &Window, instance: wgpu::Instance) -> Result<Self, String> {
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
            apply_limit_buckets: false,
        }))
        .map_err(|error| format!("request adapter: {error}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
            label: Some("neon3-wgpu-runtime-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            },
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
        let present_mode = capabilities
            .present_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::PresentMode::Mailbox)
            .unwrap_or(wgpu::PresentMode::Fifo);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: Vec::new(),
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);
        eprintln!("neon-wgpu surface present mode: {:?}", config.present_mode);
        let ui = UiWgpuRenderer::new(&device, config.format);
        let heightmap_preview = HeightmapPreviewConverter::new(&device);
        let mut ai = neon_wgpu_ai::AiEngine::new(device.clone(), queue.clone());
        let configured_pack = std::env::var_os("NEON_AI_PACK")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                let path = std::path::PathBuf::from("assets/ai/terrain_run1/terrain_run1.pack");
                path.exists().then_some(path)
            });
        if let Some(path) = configured_pack {
            match std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| { ai.load_model(&bytes).map(|_| ()).map_err(|error| error.to_string())
                })
            {
                Ok(()) => eprintln!("neon-wgpu AI model loaded from {}", path.display()),
                Err(error) => eprintln!("neon-wgpu AI model unavailable: {error}"),
            }
        }
        let (hit_target, hit_target_view) = create_hit_target(&device, size);
        Ok(Self {
            _instance: instance,
            surface,
            device,
            queue,
            config,
            ui,
            ai,
            heightmap_preview,
            hit_target,
            hit_target_view,
            hit_target_generation: 1,
            input: LocalInputState::default(),
            pending_hit_pixel: None,
            pending_hit_slot: None,
            captured_binding: None,
            pending_control_value: None,
            next_input_sequence: 0,
            next_semantic_sequence: 0,
            ime_active: false,
            shift_down: false,
            text_selection_drag: false,
            started_at: Instant::now(),
            last_draw_instance_count: 0,
            hit_target_dirty: true,
            last_present: Instant::now(),
            frame_count: 0,
            longest_frame_gap_ms: 0.0,
            last_pointer_outcome: "none".into(),
            last_pointer_node_path: None,
            active_interaction_id: None,
        })
    }

    fn generate_terrain_preview(
        &mut self,
        command: AiTerrainGenerateCommand,
        job_id: String,
    ) -> Result<AiTerrainGenerationResult, String> {
        if command.target_id.trim().is_empty()
            || matches!(command.target_id.as_str(), UI_COLOR_TARGET | UI_HIT_TARGET)
        {
            return Err("invalid_render_surface_target".into());
        }
        if !self.ai.has_model() {
            return Err("ai_model_not_loaded".into());
        }
        let generation = self
            .ai
            .generate_gpu(neon_wgpu_ai::GenerateRequest {
                cond: neon_wgpu_ai::format::TerrainCond {
                    sub: command.condition.sub,
                    parent: command.condition.parent,
                    relief: command.condition.relief,
                    texture: command.condition.texture,
                    water: command.condition.water,
                },
                guidance: command.guidance,
                steps: command.steps,
                seed: command.seed,
                size: command.size,
                preview_every: 0,
            })
            .map_err(|error| error.to_string())?;
        let output = self.ui.ensure_render_surface(
            &self.device,
            &command.target_id,
            [generation.size, generation.size],
        );
        self.heightmap_preview.convert_into(
            &self.device,
            &self.queue,
            &generation.heightmap,
            generation.size,
            &output,
        );
        Ok(AiTerrainGenerationResult {
            job_id,
            target_id: command.target_id,
            state: "ready".into(),
            seed: generation.seed,
            width: generation.size,
            height: generation.size,
            elapsed_ms: generation.elapsed_ms,
        })
    }
}

fn create_hit_target(device: &wgpu::Device, size: PhysicalSize<u32>) -> (wgpu::Texture, wgpu::TextureView) {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(UI_HIT_TARGET), size: wgpu::Extent3d { width: size.width.max(1), height: size.height.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    (target, view)
}

fn forward_pointer_click(
    endpoint: SocketAddr,
    renderer_epoch: u64,
    composition_revision: Revision,
    sequence: u64,
    interaction_id: Option<InteractionId>,
    binding: UiHitBinding,
    control_value: Option<neon_ui_schema::UiSemanticPayloadValue>,
    delivery: Arc<Mutex<Value>>,
    traces: Arc<Mutex<InteractionTraceStore>>,
) {
    let node_path = diagnostic_node_path(&binding);
    let Some(intent) = binding.intent.clone() else {
        if let Ok(mut state) = delivery.lock() {
            *state = json!({"state": "not_sent", "reason": "semantic_binding_missing"});
        }
        return;
    };
    if let Ok(mut state) = delivery.lock() {
        *state = json!({"state": "pending", "sequence": sequence, "node_path": node_path.clone()});
    }
    let semantic_target = semantic_target(&binding);
    let fragment_revision = binding.fragment.revision;
    let request_id = RequestId(format!("wgpu-pointer-click-{sequence}"));
    if let (Some(interaction_id), Ok(mut traces)) = (interaction_id.clone(), traces.lock()) {
        traces.append(
            interaction_id,
            InteractionTraceStage::SemanticEventForwarded,
            InteractionTraceOutcome::Pending,
            None,
            Some(semantic_target.clone()),
            Some(fragment_revision),
            composition_revision,
            Some(request_id.clone()),
        );
    }
    thread::spawn(move || {
        let event = UiSemanticEvent {
            event: if binding.data_grid_cell.is_some() && control_value.is_some() {
                neon_ui_schema::UiSemanticEventType::SelectionChanged
            } else {
                neon_ui_schema::UiSemanticEventType::PointerClick
            },
            event_id: request_id.0.clone(),
            renderer_epoch,
            composition_revision,
            fragment: binding.fragment.clone(),
            intent,
            pointer: Some(neon_ui_schema::UiPointerMetadata { id: 0, sequence }),
            focus: None,
            data_grid_cell: binding.data_grid_cell.clone(),
            text: None,
            control_value,
            drag_drop: None,
        };
        let request = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: request_id.clone(),
            client: ClientIdentity { kind: ClientKind::WgpuRuntime, instance_id: format!("window-{renderer_epoch}"), pid: std::process::id(), origin: "neon-wgpu-runtime".into() },
            target: ServiceName("ui-runtime".into()), method: "ui.host.inbound".into(), params: json!(&event),
            expected_revision: Some(event.fragment.revision), idempotency_key: Some(format!("wgpu-pointer-click:{renderer_epoch}:{sequence}")),
        };
        match RpcClient::connect(endpoint).and_then(|mut client| client.call(&request)) {
            Ok(response) if response.status == RpcStatus::Accepted => {
                if let Ok(mut state) = delivery.lock() {
                    *state = json!({"state": "accepted", "sequence": sequence, "node_path": node_path, "revision": response.revision});
                }
                if let (Some(interaction_id), Ok(mut traces)) = (interaction_id.clone(), traces.lock()) {
                    traces.append(interaction_id.clone(), InteractionTraceStage::DeliveryAccepted, InteractionTraceOutcome::Accepted, None, Some(semantic_target.clone()), Some(fragment_revision), composition_revision, Some(request_id));
                    traces.delivery_accepted(interaction_id);
                }
            }
            Ok(response) => {
                let error = response.error;
                if let Ok(mut state) = delivery.lock() {
                    *state = json!({"state": "rejected", "sequence": sequence, "node_path": node_path, "error": error});
                }
                if let (Some(interaction_id), Ok(mut traces)) = (interaction_id, traces.lock()) {
                    let error = error
                        .map(|error| InteractionTraceError {
                            code: error.code,
                            message: error.message,
                        })
                        .unwrap_or(InteractionTraceError {
                            code: "delivery_rejected".into(),
                            message: "UI host rejected semantic delivery".into(),
                        });
                    traces.append(interaction_id, InteractionTraceStage::DeliveryRejected, InteractionTraceOutcome::Rejected, Some(error), Some(semantic_target.clone()), Some(fragment_revision), composition_revision, Some(request_id));
                }
            }
            Err(error) => {
                if let Ok(mut state) = delivery.lock() {
                    *state = json!({"state": "transport_failed", "sequence": sequence, "node_path": node_path, "error": error.to_string()});
                }
                if let (Some(interaction_id), Ok(mut traces)) = (interaction_id, traces.lock()) {
                    traces.append(interaction_id, InteractionTraceStage::TransportFailed, InteractionTraceOutcome::Failed, Some(InteractionTraceError { code: "transport_failed".into(), message: error.to_string() }), Some(semantic_target), Some(fragment_revision), composition_revision, Some(request_id));
                }
            }
        }
    });
}

fn forward_text_input_commit(
    endpoint: SocketAddr,
    renderer_epoch: u64,
    composition_revision: Revision,
    sequence: u64,
    binding: UiHitBinding,
    value: String,
) {
    let Some(event) = text_input_commit_event(
        renderer_epoch,
        composition_revision,
        sequence, binding,
        value,
    ) else { return; };
    thread::spawn(move || {
        let request_id = RequestId( event.
            event_id.clone());
        let request = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: request_id.clone(),
            client: ClientIdentity { kind: ClientKind::WgpuRuntime, instance_id: format!("window-{renderer_epoch}"), pid: std::process::id(), origin: "neon-wgpu-runtime".into() },
            target: ServiceName("ui-runtime".into()), method: "ui.host.inbound".into(), params: json!(&event),
            expected_revision: Some(event.fragment.revision), idempotency_key: Some(format!("wgpu-text-input:{renderer_epoch}:{sequence}")),
        };
        if let Err(error) = RpcClient::connect(endpoint).and_then(|mut client| client.call(&request)) {
            eprintln!("ui text commit delivery failed: {error}");
        }
    });
}

fn text_input_commit_event(
    renderer_epoch: u64,
    composition_revision: Revision,
    sequence: u64,
    binding: UiHitBinding,
    value: String,
) -> Option<UiSemanticEvent> {
    Some(UiSemanticEvent {
        event: neon_ui_schema::UiSemanticEventType::TextInputCommit,
        event_id: format!("wgpu-text-input-{sequence}"),
        renderer_epoch,
        composition_revision,
        fragment: binding.fragment,
        intent: binding.intent?,
        pointer: None,
        focus: Some(neon_ui_schema::UiFocusMetadata { focused: true }),
        data_grid_cell: binding.data_grid_cell,
        text: Some(neon_ui_schema::UiTextInputCommit { value }),
        control_value: binding.control_value,
        drag_drop: None,
    })
}

fn take_data_grid_text_commit(gpu: &mut WindowGpu) -> Option<(u64, UiHitBinding, String)> {
    let (binding, value) = gpu.ui.finish_data_grid_text_input()?;
    gpu.next_semantic_sequence += 1;
    Some((gpu.next_semantic_sequence, binding, value))
}

fn forward_drag_drop(
    endpoint: SocketAddr,
    renderer_epoch: u64,
    composition_revision: Revision,
    sequence: u64,
    resolved: ui_renderer::UiResolvedDragDrop,
) {
    thread::spawn(move || {
        let request_id = RequestId(format!("wgpu-drag-drop-{sequence}"));
        let event = UiSemanticEvent {
            event: neon_ui_schema::UiSemanticEventType::DragDrop,
            event_id: request_id.0.clone(), renderer_epoch, composition_revision, fragment: resolved.fragment.clone(),
            intent: resolved.intent,
            pointer: Some(neon_ui_schema::UiPointerMetadata { id: 0, sequence }),
            focus: None, data_grid_cell: None, text: None, control_value: None,
            drag_drop: Some(neon_ui_schema::UiDragDropPayload { source_key: resolved.source_key, target_key: resolved.target_key, placement: resolved.placement, presentation_template_key: resolved.presentation_template_key }),
        };
        let request = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: request_id.clone(),
            client: ClientIdentity { kind: ClientKind::WgpuRuntime, instance_id: format!("window-{renderer_epoch}"), pid: std::process::id(), origin: "neon-wgpu-runtime".into() },
            target: ServiceName("ui-runtime".into()), method: "ui.host.inbound".into(), params: json!(&event),
            expected_revision: Some(event.fragment.revision), idempotency_key: Some(format!("wgpu-drag-drop:{renderer_epoch}:{sequence}")),
        };
        if let Err(error) = RpcClient::connect(endpoint).and_then(|mut client| client.call(&request)) { eprintln!("ui drag/drop delivery failed: {error}"); }
    });
}

fn forward_data_grid_window_request(endpoint: SocketAddr, window_request: UiDataGridWindowRequest) {
    thread::spawn(move || {
        let request = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION,
            request_id: RequestId(format!("wgpu-data-grid-window-{}", window_request.sequence)),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: format!("window-{}", window_request.renderer_epoch),
                pid: std::process::id(),
                origin: "neon-wgpu-runtime".into(),
            },
            target: ServiceName("ui-runtime".into()),
            method: "ui.host.inbound".into(),
            expected_revision: Some(window_request.fragment.revision),
            idempotency_key: Some(format!(
                "wgpu-data-grid-window:{}:{}:{}",
                window_request.fragment.id.0, window_request.source_key, window_request.sequence,
            )),
            params: json!(UiHostInbound::WindowRequest { request: UiWindowRequest::DataGrid { request: window_request } }),
        };
        let _ = RpcClient::connect(endpoint).and_then(|mut client| client.call(&request));
    });
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
                let mut scroll_changed = false;
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.ui
                        .set_pointer_position([position.x as f32, position.y as f32]);
                    if gpu.text_selection_drag {
                        gpu.ui.set_text_input_caret_from_pointer([position.x as f32, position.y as f32], true);
                    }
                    if gpu.ui.drag_active() { gpu.ui.update_drag_preview(); }
                    scroll_changed = gpu.ui.update_scroll_drag() || gpu.ui.update_scroll_pan();
                    if scroll_changed { self.redraw_pending = true; }
                    gpu.ui.update_value_gesture();
                    let x = position.x.max(0.0).min(gpu.config.width.saturating_sub(1) as f64) as u32;
                    let y = position.y.max(0.0).min(gpu.config.height.saturating_sub(1) as f64) as u32;
                    gpu.pending_hit_pixel = Some([x, y]);
                    self.redraw_pending = true;
                }
                if scroll_changed { self.schedule_data_grid_window_requests(); }
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == winit::event::ElementState::Pressed
                    && button == winit::event::MouseButton::Middle =>
            {
                self.prepare_pointer_interaction();
                if self.gpu.as_mut().is_some_and(|gpu| gpu.ui.begin_scroll_pan_at_pointer()) {
                    self.redraw_pending = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == winit::event::ElementState::Pressed
                    && button == winit::event::MouseButton::Left =>
            {
                let interaction_id = self.begin_os_pointer_interaction();
                let interaction_traces = self.interaction_traces.clone();
                let composition_revision = self.applied_composition_revision;
                self.prepare_pointer_interaction();
                let modal_consumed = self.gpu.as_mut().is_some_and(|gpu| {
                    if !gpu.ui.dismiss_modal_at_pointer() {
                        return false;
                    }
                    gpu.captured_binding = None;
                    gpu.pending_control_value = None;
                    gpu.input.cancel();
                    true
                });
                if modal_consumed {
                    append_interaction_record(&interaction_traces, interaction_id, InteractionTraceStage::HitCaptureResolved, InteractionTraceOutcome::Rejected, Some(InteractionTraceError { code: "modal_outside_press".into(), message: "pointer press was consumed by the active modal".into() }), None, composition_revision);
                    self.redraw_pending = true;
                    return;
                }
                let blur_commit = self.gpu.as_mut().and_then(|gpu| {
                    let next_input = gpu.ui.text_input_at_pointer();
                    let active_path = gpu.ui.active_text_input_path();
                    if gpu.ui.data_grid_text_input_active()
                        && next_input
                            .as_ref()
                            .is_none_or(|input| Some(input.node_path.as_str()) != active_path)
                    {
                        take_data_grid_text_commit(gpu)
                    } else {
                        None
                    }
                });
                if let (Some(endpoint), Some((sequence, binding, value))) =
                    (self.ui_endpoint, blur_commit)
                {
                    if let Some(window) = self.window.as_ref() {
                        window.set_ime_allowed(false);
                    }
                    forward_text_input_commit(
                        endpoint,
                        self.epoch,
                        self.applied_composition_revision,
                        sequence,
                        binding,
                        value,
                    );
                    self.redraw_pending = true;
                }
                if self.gpu.as_mut().is_some_and(|gpu| gpu.ui.begin_scroll_drag_at_pointer()) {
                    append_interaction_record(&interaction_traces, interaction_id, InteractionTraceStage::HitCaptureResolved, InteractionTraceOutcome::Accepted, None, None, composition_revision);
                    self.redraw_pending = true;
                    return;
                }
                let text_input = self.gpu.as_mut().and_then(|gpu| {
                    let input = gpu.ui.text_input_at_pointer()?;
                    gpu.ui.focus_text_input(input.clone());
                    if let Some(pointer) = gpu.ui.pointer_position() {
                        gpu.ui.set_text_input_caret_from_pointer(pointer, false);
                    }
                    gpu.text_selection_drag = true;
                    self.redraw_pending = true;
                    Some(input)
                });
                if let (Some(window), Some(input)) = (self.window.as_ref(), text_input) {
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.captured_binding = None;
                    }
                    window.set_ime_allowed(true);
                    let rect = self.gpu.as_ref().and_then(|gpu| gpu.ui.text_input_ime_rect()).unwrap_or(input.bounds);
                    window.set_ime_cursor_area(PhysicalPosition::new(rect.x.round() as i32, rect.y.round() as i32), PhysicalSize::new(rect.width.max(1.0).round() as u32, rect.height.max(1.0).round() as u32));
                    append_interaction_record(&interaction_traces, interaction_id, InteractionTraceStage::HitCaptureResolved, InteractionTraceOutcome::Accepted, None, None, composition_revision);
                } else if let Some(gpu) = self.gpu.as_mut() {
                    if let Some((binding, value)) = gpu.ui.dropdown_option_at_pointer().or_else(|| gpu.ui.list_option_at_pointer()) {
                        gpu.input.set_hover_id(Some(0));
                        let _ = gpu.input.pointer_down();
                        gpu.captured_binding = Some(binding);
                        gpu.active_interaction_id = Some(interaction_id.clone());
                        gpu.pending_control_value = Some(value);
                        let binding = gpu.captured_binding.clone().expect("binding was just assigned");
                        append_interaction_record(&interaction_traces, interaction_id, InteractionTraceStage::HitCaptureResolved, InteractionTraceOutcome::Accepted, None, Some(&binding), composition_revision);
                        gpu.ui.close_dropdown();
                        self.redraw_pending = true;
                    } else if gpu.ui.dismiss_dropdown_at_pointer() {
                        gpu.captured_binding = None;
                        gpu.pending_control_value = None;
                        gpu.input.cancel();
                        self.redraw_pending = true;
                    } else if gpu.ui.toggle_dropdown_at_pointer() {
                        gpu.captured_binding = None;
                        gpu.input.cancel();
                        self.redraw_pending = true;
                    } else if gpu.ui.begin_drag_at_pointer(&self.fragments) {
                        self.redraw_pending = true;
                    } else {
                        // Pointer release uses the captured hit binding. Fall back to the
                        // already planned control geometry when asynchronous readback has
                        // not completed before the OS press event.
                        gpu.input.set_hover_id(gpu.ui.hit_id_at_pointer());
                        if gpu.input.pointer_down().is_err() {
                            gpu.last_pointer_outcome = "press_without_semantic_hit".into();
                            gpu.last_pointer_node_path = None;
                            append_interaction_record(&interaction_traces, interaction_id, InteractionTraceStage::HitCaptureResolved, InteractionTraceOutcome::Rejected, Some(InteractionTraceError { code: "press_without_semantic_hit".into(), message: "pointer press did not resolve a semantic target".into() }), None, composition_revision);
                            return;
                        }
                        gpu.captured_binding = gpu
                            .input
                            .capture_id
                            .and_then(|hit_id| gpu.ui.hit_binding(hit_id));
                        gpu.pending_control_value = gpu
                            .captured_binding
                            .as_ref()
                            .and_then(|binding| binding.control_value.clone());
                        if gpu.captured_binding.is_none() {
                            let _ = gpu.input.pointer_up(false);
                            gpu.last_pointer_outcome = "press_without_semantic_binding".into();
                            gpu.last_pointer_node_path = None;
                            append_interaction_record(&interaction_traces, interaction_id, InteractionTraceStage::HitCaptureResolved, InteractionTraceOutcome::Rejected, Some(InteractionTraceError { code: "press_without_semantic_binding".into(), message: "captured hit did not resolve a semantic binding".into() }), None, composition_revision);
                            return;
                        }
                        let binding = gpu.captured_binding.clone().expect("binding checked above");
                        gpu.active_interaction_id = Some(interaction_id.clone());
                        append_interaction_record(&interaction_traces, interaction_id, InteractionTraceStage::HitCaptureResolved, InteractionTraceOutcome::Accepted, None, Some(&binding), composition_revision);
                        gpu.last_pointer_node_path = gpu
                            .input
                            .capture_id
                            .and_then(|hit_id| gpu.ui.hit_binding(hit_id))
                            .and_then(|binding| diagnostic_node_path(&binding));
                        gpu.last_pointer_outcome = "pointer_captured".into();
                        if let Some(binding) = gpu.captured_binding.clone() {
                            let started = gpu.ui.begin_value_gesture(&binding);
                            if gpu.ui.requires_value_gesture(&binding) && !started {
                                gpu.captured_binding = None;
                                gpu.input.cancel();
                                gpu.last_pointer_outcome = "press_outside_value_control".into();
                                gpu.last_pointer_node_path = diagnostic_node_path(&binding);
                                return;
                            }
                        }
                        gpu.ui.focus_control_at_pointer();
                        gpu.ui.press_hovered(gpu.started_at.elapsed().as_secs_f32());
                        self.redraw_pending = true;
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == winit::event::ElementState::Released
                    && button == winit::event::MouseButton::Middle =>
            {
                if let Some(gpu) = self.gpu.as_mut() {
                    if gpu.ui.scroll_pan_active() {
                        gpu.ui.end_scroll_pan();
                        self.redraw_pending = true;
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == winit::event::ElementState::Released
                    && button == winit::event::MouseButton::Left =>
            {
                if let Some(gpu) = self.gpu.as_mut()
                    && gpu.ui.scroll_drag_active()
                {
                    gpu.ui.end_scroll_drag();
                    self.redraw_pending = true;
                    return;
                }
                if let Some(gpu) = self.gpu.as_mut() { gpu.text_selection_drag = false; }
                let drag = self.gpu.as_mut().and_then(|gpu| {
                    if !gpu.ui.drag_active() { return None; }
                    let resolved = gpu.ui.finish_drag_at_pointer(&self.fragments);
                    gpu.next_semantic_sequence += 1;
                    Some((gpu.next_semantic_sequence, resolved))
                });
                if let (Some(endpoint), Some((sequence, Some(resolved)))) = (self.ui_endpoint, drag.clone()) {
                    forward_drag_drop(endpoint, self.epoch, self.applied_composition_revision, sequence, resolved);
                    self.redraw_pending = true;
                    return;
                }
                if drag.is_some() { self.redraw_pending = true; return; }
                let binding = self.gpu.as_mut().and_then(|gpu| {
                    let text_binding = gpu.captured_binding.as_ref().is_some_and(|binding| binding.text_input.is_some());
                    if text_binding {
                        let binding = gpu.captured_binding.take()?;
                        gpu.input.pointer_up(true).ok()?;
                        return Some((None, binding, None));
                    }
                    let (binding, control_value, sequence) = release_captured_binding(gpu)?;
                    Some((Some(sequence), binding, control_value))
                });
                if let Some((None, binding, _)) = &binding
                    && let (Some(window), Some(input)) = (self.window.as_ref(), binding.text_input.as_ref())
                {
                    window.set_ime_allowed(true);
                    let rect = self.gpu.as_ref().and_then(|gpu| gpu.ui.text_input_ime_rect()).unwrap_or(input.bounds);
                    window.set_ime_cursor_area(PhysicalPosition::new(rect.x.round() as i32, rect.y.round() as i32), PhysicalSize::new(rect.width.max(1.0).round() as u32, rect.height.max(1.0).round() as u32));
                    }
                if let (Some(endpoint), Some((Some(sequence), binding, control_value))) = (self.ui_endpoint, binding) {
                    if let Some(window) = self.window.as_ref() { window.set_ime_allowed(false); }
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.last_pointer_node_path = diagnostic_node_path(&binding);
                        gpu.last_pointer_outcome = if binding.intent.is_some() {
                            "semantic_event_forwarded".into()
                        } else {
                            "release_without_semantic_binding".into()
                        };
                    }
                    let interaction_id = self.gpu.as_mut().and_then(|gpu| gpu.active_interaction_id.take());
                    forward_pointer_click(
                        endpoint,
                        self.epoch,
                        self.applied_composition_revision,
                        sequence,
                        interaction_id,
                        binding,
                        control_value,
                        self.pointer_delivery.clone(),
                        self.interaction_traces.clone(),
                    );
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = match delta {
                    MouseScrollDelta::LineDelta(x, y) => [x * 24.0, y * 24.0],
                    MouseScrollDelta::PixelDelta(position) => { [position.x as f32, position.y as f32]
                    }
                };
                let scrolled = self.gpu.as_mut().is_some_and(|gpu| {
                    let delta = if gpu.shift_down {
                        [delta[0] + delta[1], 0.0]
                    } else {
                        delta
                    };
                    gpu.ui.scroll_wheel_at_pointer(delta)
                });
                if scrolled {
                    self.redraw_pending = true;
                    self.schedule_data_grid_window_requests();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Enabled) => {
                if let Some(gpu) = self.gpu.as_mut() { gpu.ime_active = true; }
            }
            WindowEvent::Ime(winit::event::Ime::Preedit(value, _)) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.ime_active = true;
                    gpu.ui.set_ime_preedit(value);
                    self.redraw_pending = true;
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(value)) => {
                let committed = self.gpu.as_mut().and_then(|gpu| {
                    let result = gpu.ui.commit_ime_text(&value);
                    if result.is_some() { gpu.next_semantic_sequence += 1; }
                    result.map(|(binding, text)| (gpu.next_semantic_sequence, binding, text))
                });
                if let (Some(endpoint), Some((sequence, binding, value))) = (self.ui_endpoint, committed) {
                    forward_text_input_commit(endpoint, self.epoch, self.applied_composition_revision, sequence, binding, value);
                }
                if let (Some(window), Some(rect)) = (self.window.as_ref(), self.gpu.as_ref().and_then(|gpu| gpu.ui.text_input_ime_rect())) {
                    window.set_ime_cursor_area(PhysicalPosition::new(rect.x.round() as i32, rect.y.round() as i32), PhysicalSize::new(rect.width.max(1.0).round() as u32, rect.height.max(1.0).round() as u32));
                }
                self.redraw_pending = true;
            }
            WindowEvent::Ime(winit::event::Ime::Disabled) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.ime_active = false;
                    gpu.ui.set_ime_preedit(String::new());
                    self.redraw_pending = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let ending_data_grid_edit = matches!(
                    &event.logical_key,
                    Key::Named(NamedKey::Enter | NamedKey::Escape)
                ) && self
                    .gpu
                    .as_ref()
                    .is_some_and(|gpu| gpu.ui.data_grid_text_input_active());
                let committed = self.gpu.as_mut().and_then(|gpu| {
                    if matches!(&event.logical_key, Key::Named(NamedKey::Enter)) {
                        return take_data_grid_text_commit(gpu);
                    }
                    if matches!(&event.logical_key, Key::Named(NamedKey::Escape)) {
                        gpu.ui.cancel_data_grid_text_input();
                        return None;
                    }
                    let result = match &event.logical_key {
                        Key::Named(NamedKey::Backspace) => gpu.ui.backspace_text_input(),
                        Key::Named(NamedKey::Delete) => gpu.ui.delete_text_input(),
                        Key::Named(NamedKey::ArrowLeft) => { gpu.ui.move_text_input_cursor(-1, gpu.shift_down); None }
                        Key::Named(NamedKey::ArrowRight) => { gpu.ui.move_text_input_cursor(1, gpu.shift_down); None }
                        Key::Named(NamedKey::Home) => { gpu.ui.move_text_input_to_edge(false, gpu.shift_down); None }
                        Key::Named(NamedKey::End) => { gpu.ui.move_text_input_to_edge(true, gpu.shift_down); None }
                        Key::Character(value) if !gpu.ime_active && event.text.is_some() => { gpu.ui.commit_ime_text(value)
                        }
                        _ => None,
                    };
                    if result.is_some() { gpu.next_semantic_sequence += 1; }
                    result.map(|(binding, text)| (gpu.next_semantic_sequence, binding, text))
                });
                if let (Some(endpoint), Some((sequence, binding, value))) = (self.ui_endpoint, committed) {
                    forward_text_input_commit(endpoint, self.epoch, self.applied_composition_revision, sequence, binding, value
                    );
                }
                if ending_data_grid_edit && let Some(window) = self.window.as_ref() {
                    window.set_ime_allowed(false);
                }
                if let (Some(window), Some(rect)) = (self.window.as_ref(), self.gpu.as_ref().and_then(|gpu| gpu.ui.text_input_ime_rect())) {
                    window.set_ime_cursor_area(PhysicalPosition::new(rect.x.round() as i32, rect.y.round() as i32), PhysicalSize::new(rect.width.max(1.0).round() as u32, rect.height.max(1.0).round() as u32));
                }
                self.redraw_pending = true;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                if let Some(gpu) = self.gpu.as_mut() { gpu.shift_down = modifiers.state().shift_key(); }
            }
            WindowEvent::Focused(false) => {
                let commit = self.gpu.as_mut().and_then(take_data_grid_text_commit);
                if let (Some(endpoint), Some((sequence, binding, value))) =
                    (self.ui_endpoint, commit)
                {
                    forward_text_input_commit(
                        endpoint,
                        self.epoch,
                        self.applied_composition_revision,
                        sequence,
                        binding,
                        value,
                    );
            }
                if let Some(window) = self.window.as_ref() {
                    window.set_ime_allowed(false);
                }
                if let Some(gpu) = self.gpu.as_mut() { gpu.ui.clear_text_focus(); gpu.ui.cancel_drag(); gpu.ui.cancel_value_gesture(); gpu.ui.end_scroll_pan(); gpu.captured_binding = None; gpu.pending_control_value = None; gpu.input.cancel(); self.redraw_pending = true; }
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
            WindowCommand::Fragments { composition_revision, fragments, applied } => {
                let accepted = self.apply_fragments(composition_revision, fragments);
                if accepted {
                    self.pending_composition_ack = applied;
                } else if let Some(applied) = applied {
                    // A duplicate/idempotent composition can already be current in
                    // the window mailbox. That is successful convergence, not a
                    // renderer rejection.
                    let _ = applied.send(());
                }
            }
            WindowCommand::GenerateTerrainPreview { command, job_id, completed } => {
                let result = self
                    .gpu
                    .as_mut()
                    .ok_or_else(|| "window_gpu_unavailable".to_owned())
                    .and_then(|gpu| gpu.generate_terrain_preview(command, job_id));
                if result.is_ok() {
                    self.redraw_pending = true;
                }
                let _ = completed.send(result);
            }
            WindowCommand::AiModelStatus { completed } => {
                let status = self.gpu.as_ref().and_then(|gpu| gpu.ai.model_info());
                let _ = completed.send(status);
            }
            WindowCommand::InputDebugSnapshot { completed } => {
                let _ = completed.send(self.input_debug_snapshot());
            }
            WindowCommand::InputDebugProbe { logical_position, physical_position, completed } => {
                let _ = completed.send(self.input_debug_probe(logical_position, physical_position));
            }
            WindowCommand::InputDebugActivate { logical_position, completed } => {
                let _ = completed.send(self.input_debug_activate(logical_position));
            }
            WindowCommand::Shutdown => event_loop.exit(),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.dispatch_ready_data_grid_window_requests();
        if let Some(deadline) = self.data_grid_window_requests.next_deadline() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        }
        if let Some(window) = self.window.as_ref()
            && self.needs_redraw()
        {
            window.request_redraw();
        }
    }
}

fn handle_window_ai_generate(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request: RpcRequest,
) -> RpcResponse {
    let request_id = request.request_id.clone();
    runtime.journal.append(
        TraceLevel::Info,
        EVENT_COMMAND_RECEIVED,
        Some(request_id.clone()),
        None,
        None,
        None,
        request.expected_revision,
        None,
        json!({"method": request.method}),
    );
    runtime.record_receipt(&request_id, CommandState::Received, None);
    let Some(idempotency_key) = request.idempotency_key.clone() else {
        return runtime.reject(request_id, "invalid_request", "idempotency_key is required", None);
    };
    if request.expected_revision.is_none() {
        return runtime.reject(request_id, "invalid_request", "expected_revision is required", None);
    }
    if let Some(response) = runtime.idempotent_responses.get(&idempotency_key) {
        let mut response = response.clone();
        response.request_id = request_id;
        return response;
    }
    let command: AiTerrainGenerateCommand = match serde_json::from_value(request.params) {
        Ok(command) => command,
        Err(_) => {
            return runtime.reject(
                request_id,
                "invalid_request",
                "a typed AI terrain generation command is required",
                None,
            );
        }
    };
    let job_id = format!("ai-terrain-{}", request_id.0);
    runtime.journal.append(
        TraceLevel::Info,
        "ai.terrain.generation.started",
        Some(request_id.clone()),
        None,
        Some(job_id.clone()),
        Some(command.target_id.clone()),
        request.expected_revision,
        None,
        json!({"steps": command.steps, "size": command.size, "seed": command.seed}),
    );
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::GenerateTerrainPreview {
            command,
            job_id: job_id.clone(),
            completed: completed_tx,
        })
        .is_err()
    {
        return runtime.reject(
            request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(std::time::Duration::from_secs(300)) {
        Ok(Ok(result)) => {
            runtime.graph_revision = Revision(runtime.graph_revision.0 + 1);
            runtime.journal.append(
                TraceLevel::Info,
                "ai.terrain.generation.ready",
                Some(request_id.clone()),
                None,
                Some(job_id),
                Some(result.target_id.clone()),
                request.expected_revision,
                Some(runtime.graph_revision),
                json!({"width": result.width, "height": result.height, "elapsed_ms": result.elapsed_ms}),
            );
            let response = runtime.accept(request_id, json!(result));
            runtime.idempotent_responses.insert(idempotency_key, response.clone());
            response
        }
        Ok(Err(error)) => {
            runtime.journal.append(
                TraceLevel::Error,
                "ai.terrain.generation.failed",
                Some(request_id.clone()),
                None,
                Some(job_id),
                None,
                request.expected_revision,
                Some(runtime.graph_revision),
                json!({"code": error}),
            );
            runtime.reject(request_id, "ai_generation_failed", &error, Some(runtime.graph_revision))
        }
        Err(_) => runtime.reject(
            request_id,
            "ai_generation_timeout",
            "AI terrain generation did not complete before the deadline",
            Some(runtime.graph_revision),
        ),
    }
}

fn handle_window_ai_model_status(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
) -> RpcResponse {
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::AiModelStatus { completed: completed_tx })
        .is_err()
    {
        return runtime.reject(
            request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(model) => runtime.accept(
            request_id,
            json!({"loaded": model.is_some(), "model": model}),
        ),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not report AI model status",
            None,
        ),
    }
}

fn handle_window_input_debug_snapshot(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
) -> RpcResponse {
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::InputDebugSnapshot {
            completed: completed_tx,
        })
        .is_err()
    {
        return runtime.reject(
            request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(snapshot) => runtime.accept(request_id, snapshot),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not report input state",
            None,
        ),
    }
}

fn probe_position(params: &Value) -> Result<(Option<[f64; 2]>, Option<[f64; 2]>), &'static str> {
    let point = |name: &str| -> Result<Option<[f64; 2]>, &'static str> {
        let Some(value) = params.get(name) else { return Ok(None); };
        let x = value.get("x").and_then(Value::as_f64).ok_or("position x must be a number")?;
        let y = value.get("y").and_then(Value::as_f64).ok_or("position y must be a number")?;
        if !x.is_finite() || !y.is_finite() { return Err("position coordinates must be finite"); }
        Ok(Some([x, y]))
    };
    let logical = point("logical_position")?;
    let physical = point("physical_position")?;
    if logical.is_none() && physical.is_none() { return Err("logical_position or physical_position is required"); }
    Ok((logical, physical))
}

fn handle_window_input_debug_probe(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(request_id, "debug_endpoint_unavailable", "window input probing is only available in debug builds", None);
    }
    let (logical_position, physical_position) = match probe_position(&params) {
        Ok(position) => position,
        Err(message) => return runtime.reject(request_id, "invalid_request", message, None),
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy.send_event(WindowCommand::InputDebugProbe {
        logical_position, physical_position, completed: completed_tx,
    }).is_err() {
        return runtime.reject(request_id, "window_compositor_unavailable", "window compositor is unavailable", None);
    }
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(probe) => runtime.accept(request_id, probe),
        Err(_) => runtime.reject(request_id, "window_compositor_timeout", "window compositor did not report input probe", None),
    }
}

fn handle_window_input_debug_activate(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(request_id, "debug_endpoint_unavailable", "window input activation is only available in debug builds", None);
    }
    let logical_position = match probe_position(&params) {
        Ok((Some(position), None)) => position,
        Ok(_) => return runtime.reject(request_id, "invalid_request", "logical_position is required for debug activation", None),
        Err(message) => return runtime.reject(request_id, "invalid_request", message, None),
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy.send_event(WindowCommand::InputDebugActivate { logical_position, completed: completed_tx }).is_err() {
        return runtime.reject(request_id, "window_compositor_unavailable", "window compositor is unavailable", None);
    }
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request_id, result),
        Ok(Err(code)) => runtime.reject(request_id, code, "debug activation did not resolve an eligible semantic control", None),
        Err(_) => runtime.reject(request_id, "window_compositor_timeout", "window compositor did not activate the prepared binding", None),
    }
}

fn spawn_window_server(
    epoch: u64,
    endpoint: SocketAddr,
    proxy: EventLoopProxy<WindowCommand>,
    interaction_traces: Arc<Mutex<InteractionTraceStore>>,
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
        let mut runtime = WgpuRuntime::window_control(epoch, interaction_traces);
        if let Err(error) = server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            let mutates_composition = matches!(request.method.as_str(), "wgpu.ui.submit_fragment" | "wgpu.ui.remove_fragment");
            let response = if request.method == "wgpu.ai.terrain.generate" {
                handle_window_ai_generate(&mut runtime, &proxy, request)
            } else if request.method == "wgpu.ai.model.status" {
                handle_window_ai_model_status(&mut runtime, &proxy, request.request_id)
            } else if request.method == "debug.window.input.snapshot" {
                handle_window_input_debug_snapshot(&mut runtime, &proxy, request.request_id)
            } else if request.method == "debug.window.input.probe" {
                handle_window_input_debug_probe(&mut runtime, &proxy, request.request_id, request.params)
            } else if request.method == "debug.window.input.activate" {
                handle_window_input_debug_activate(&mut runtime, &proxy, request.request_id, request.params)
            } else {
                runtime.handle(request)
            };
            if mutates_composition && response.status == RpcStatus::Accepted {
                let (applied_tx, applied_rx) = std::sync::mpsc::channel();
                let send = proxy.send_event(WindowCommand::Fragments {
                    composition_revision: runtime.diagnostics().graph_revision,
                    fragments: runtime.fragments_snapshot(),
                    applied: Some(applied_tx),
                });
                if send.is_err() {
                    return (runtime.reject(response.request_id, "window_compositor_unavailable", "window compositor is unavailable", None), !shutdown);
                }
                match applied_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                    Ok(()) => {}
                    Err(_) => { return (runtime.reject(response.request_id, "window_compositor_timeout", "window compositor did not apply the fragment", Some(runtime.diagnostics().graph_revision)), !shutdown);
                    }
                }
            }
            (response, !shutdown)
        }) {
            eprintln!("window RPC server request failed: {error}");
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
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
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
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: Some("ui.demo.title".into()),
                    text: None,
                    image: None,
                    surface: None,
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
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: Some("ui.demo.action".into()),
                    text: None,
                    image: None,
                    surface: None,
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
    window_gpu_available: bool,
    graph_revision: Revision,
    hit_target_generation: u64,
    input: LocalInputState,
    fragments: HashMap<UiFragmentId, UiFragment>,
    journal: CommandJournal,
    receipts: HashMap<RequestId, CommandReceipt>,
    idempotent_responses: HashMap<String, RpcResponse>,
    resources: HashMap<u64, UiResourceRecord>,
    interaction_traces: Arc<Mutex<InteractionTraceStore>>,
}

impl WgpuRuntime {
    pub fn headless(epoch: u64) -> Self {
        Self {
            epoch,
            window_gpu_available: false,
            graph_revision: Revision(0),
            hit_target_generation: 1,
            input: LocalInputState::default(),
            fragments: HashMap::new(),
            journal: CommandJournal::new(ServiceName(SERVICE_NAME.into()), epoch, 128),
            receipts: HashMap::new(),
            idempotent_responses: HashMap::new(),
            resources: HashMap::new(),
            interaction_traces: Arc::new(Mutex::new(InteractionTraceStore::new())),
        }
    }

    fn window_control(epoch: u64, interaction_traces: Arc<Mutex<InteractionTraceStore>>) -> Self {
        let mut runtime = Self::headless(epoch);
        runtime.window_gpu_available = true;
        runtime.interaction_traces = interaction_traces;
        runtime
    }

    pub fn service_health(&self) -> ServiceHealth {
        ServiceHealth {
            service: ServiceName(SERVICE_NAME.into()),
            status: HealthStatus::Healthy,
            epoch: self.epoch,
        }
    }

    pub fn service_description(&self) -> ServiceDescription {
        let mut capabilities = vec![
            CAPABILITY_UI_FRAGMENT.into(),
            "wgpu.render.diagnostics".into(),
            CAPABILITY_UI_HIT_TARGET.into(),
            CAPABILITY_UI_SEMANTIC_EVENT.into(),
            CAPABILITY_UI_PROGRAM_SEMANTIC_EVENT.into(),
            CAPABILITY_UI_RENDER_SURFACE.into(),
        ];
        if self.window_gpu_available {
            capabilities.push(CAPABILITY_AI_TERRAIN_GENERATION.into());
            capabilities.push(CAPABILITY_DEBUG_INTERACTION.into());
        }
        ServiceDescription {
            service: ServiceName(SERVICE_NAME.into()),
            protocol_version: PROTOCOL_VERSION,
            endpoint: "headless://wgpu-runtime".into(),
            epoch: self.epoch,
            capabilities,
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
            hit_target_generation: self.hit_target_generation,
        }
    }

    #[cfg(test)]
    fn resize_hit_target_for_test(&mut self) {
        self.hit_target_generation += 1;
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

    /// Accepts an owner response that was obtained by querying project/resource through the public protocol.
    /// The raw bytes stay in the renderer process and are never exposed by a WGPU RPC response.
    pub fn preload_resource_from_owner(&mut self, request: RpcRequest, content: AssetBytes) -> RpcResponse {
        let request_id = request.request_id.clone();
        let asset: AssetRef = match serde_json::from_value(request.params) {
            Ok(asset) => asset,
            Err(_) => { return self.reject(request_id, "invalid_request", "a stable AssetRef is required", None);
            }
        };
        let job_id = format!("ui-resource-{}-{}", asset.asset_id, asset.revision.0);
        self.resources.insert(asset.asset_id, UiResourceRecord { asset: asset.clone(), job_id: job_id.clone(), state: UiResourceState::Loading });
        self.journal.append(TraceLevel::Info, "ui.resource.loading", Some(request_id.clone()), None, Some(job_id.clone()), None, Some(asset.revision), None, json!({"asset_id": asset.asset_id, "kind": asset.kind}));
        if asset != content.asset {
            return self.fail_resource(request_id, asset, job_id, "asset_revision_mismatch", "owner content does not match the requested AssetRef");
        }
        if !matches!(asset.kind.as_str(), "font" | "image") || content.bytes.is_empty() {
            return self.fail_resource(request_id, asset, job_id, "invalid_resource_content", "owner returned unusable UI resource content");
        }
        self.resources.insert(asset.asset_id, UiResourceRecord { asset: asset.clone(), job_id: job_id.clone(), state: UiResourceState::Ready });
        self.journal.append(TraceLevel::Info, "ui.resource.ready", Some(request_id.clone()), None, Some(job_id.clone()), None, Some(asset.revision), Some(asset.revision), json!({"asset_id": asset.asset_id, "kind": asset.kind, "media_type": content.media_type}));
        self.accept(request_id, json!({"job_id": job_id, "state": "ready"}))
    }

    fn fail_resource(&mut self, request_id: RequestId, asset: AssetRef, job_id: String, code: &'static str, message: &'static str) -> RpcResponse {
        self.resources.insert(asset.asset_id, UiResourceRecord { asset: asset.clone(), job_id: job_id.clone(), state: UiResourceState::Failed });
        self.journal.append(TraceLevel::Error, "ui.resource.failed", Some(request_id.clone()), None, Some(job_id), None, Some(asset.revision), Some(asset.revision), json!({"asset_id": asset.asset_id, "kind": asset.kind, "code": code}));
        self.reject(request_id, code, message, Some(asset.revision))
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
            if request.client.kind == ClientKind::UiReactClient {
                return self.reject(
                    request_id,
                    "renderer_submission_requires_ui_runtime",
                    "React declarations must be submitted through ui-runtime",
                    None,
                );
            }
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
            "wgpu.render.graph.snapshot" => self.accept(request_id, json!(composition_graph_snapshot(self.graph_revision, self.hit_target_generation))),
            "wgpu.ui.fragment.snapshot" => self.fragment_snapshot(request_id, request.params),
            "wgpu.render.target.capture" => self.target_capture(request_id, request.params),
            "wgpu.render.target.assert" => self.target_assert(request_id, request.params),
            "wgpu.resource.inspect" => self.resource_inspect(request_id),
            "wgpu.ui.resource.preload" => self.resource_preload(request_id, request.params),
            "wgpu.resource.wait_ready" => self.resource_wait_ready(request_id, request.params),
            "debug.snapshot.get" => self.accept(request_id, json!(self.debug_snapshot())),
            "debug.command.get" => self.command_get(request_id, request.params),
            "debug.trace.query" => self.trace_query(request_id, request.params),
            "debug.interaction.get" => self.interaction_get(request_id, request.params),
            "debug.interaction.query" => self.interaction_query(request_id, request.params),
            "wgpu.ui.submit_fragment" => self.submit_fragment(request_id, request.params),
            "wgpu.ui.remove_fragment" => self.remove_fragment(request_id, request.params),
            "wgpu.ui.semantic_event.validate" | "test.ui.semantic_event.inject" => { self.inject_semantic_event(request_id, request.params)
            }
            "test.ui.hit_sample.request" => self.hit_sample_request(request_id, request.params),
            "test.ui.hit_sample.complete" => self.hit_sample_complete(request_id, request.params),
            "test.ui.pointer.down" => self.pointer_down(request_id),
            "test.ui.pointer.up" => self.pointer_up(request_id, request.params),
            "test.ui.focus.loss" => self.focus_loss(request_id),
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
            event_id: params.get("event_id").and_then(Value::as_str).map(str::to_owned),
            pointer_id: params.get("pointer_id").and_then(Value::as_u64),
            fragment_revision: params.get("fragment_revision").and_then(Value::as_u64).map(Revision),
            composition_revision: params.get("composition_revision").and_then(Value::as_u64).map(Revision),
            ..JournalFilter::default()
        };
        self.accept(request_id, json!(self.traces(&filter)))
    }

    fn interaction_get(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(interaction_id) = params.get("interaction_id").and_then(Value::as_str) else {
            return self.reject(request_id, "invalid_request", "interaction_id is required", None);
        };
        let interaction_id = InteractionId(interaction_id.into());
        let records = self.interaction_traces.lock().ok().map(|traces| traces.get(&interaction_id)).unwrap_or_default();
        if records.is_empty() {
            return self.reject(request_id, "not_found", "interaction trace was not found", None);
        }
        self.accept(request_id, json!({"interaction_id": interaction_id, "records": records}))
    }

    fn interaction_query(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let query: InteractionTraceQuery = match serde_json::from_value(params) {
            Ok(query) => query,
            Err(_) => return self.reject(request_id, "invalid_request", "invalid interaction trace query", None),
        };
        let records = self.interaction_traces.lock().ok().map(|traces| traces.query(&query)).unwrap_or_default();
        self.accept(request_id, json!({"records": records}))
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
        let UiCommand::SubmitFragment { submission } = command else {
            unreachable!()
        };
        if submission.validate().is_err() {
            return self.reject(request_id, "invalid_request", "invalid UI fragment submission", None);
        }
        let fragment = submission.fragment;
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
        self.hit_target_generation += 1;
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
            self.hit_target_generation += 1;
            self.input.cancel();
        }
        self.accept(request_id, diagnostics_value(self.diagnostics()))
    }

    fn fragment_snapshot(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(fragment_id) = params.get("fragment_id").and_then(Value::as_str) else {
            return self.reject(request_id, "invalid_request", "fragment_id is required", None);
        };
        let Some(fragment) = self.fragments.get(&UiFragmentId(fragment_id.into())) else {
            return self.reject(request_id, "not_found", "fragment is not present", None);
        };
        self.accept(request_id, json!({
                "epoch": self.epoch,
                "sequence": self.graph_revision,
                "fragment_revision": fragment.revision,
                "fragment": fragment,
        }))
    }

    /// Test-only U1 scenario bridge. It accepts a semantic event, never a hit ID or node key.
    fn inject_semantic_event(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let event: UiSemanticEvent = match serde_json::from_value(params) {
            Ok(event) => event,
            Err(_) => { return self.reject(request_id, "invalid_request", "invalid UI semantic event", None);
            }
        };
        let Some(fragment) = self.fragments.get(&event.fragment.id) else {
            return self.reject(request_id, "fragment_revision_stale", "fragment is not present", None);
        };
        if fragment.revision != event.fragment.revision {
            return self.reject(request_id, "fragment_revision_stale", "fragment revision is stale", Some(fragment.revision));
        }
        if !fragment.effects.iter().any(|effect| matches!(effect, neon_ui_schema::UiEffect::SemanticIntent { intent } | neon_ui_schema::UiEffect::BoundSemanticIntent { intent, .. } if intent == &event.intent)) {
            return self.reject(request_id, "intent_not_bound", "semantic intent is not bound", Some(fragment.revision));
        }
        self.accept(request_id, json!(event))
    }

    fn hit_sample_request(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(pointer_id) = params.get("pointer_id").and_then(Value::as_u64) else { return self.reject(request_id, "invalid_request", "pointer_id is required", None); };
        let Some(sequence) = params.get("sequence").and_then(Value::as_u64) else { return self.reject(request_id, "invalid_request", "sequence is required", None); };
        self.input.request_sample(HitSampleRequest { pointer_id, sequence, composition_revision: self.graph_revision, target_generation: self.hit_target_generation });
        self.journal.append(TraceLevel::Info, "ui.hit_sample.requested", Some(request_id.clone()), None, None, None, Some(self.graph_revision), None, json!({"pointer_id": pointer_id, "sequence": sequence, "composition_revision": self.graph_revision.0, "fragment_revision": self.graph_revision.0}));
        self.accept(request_id, json!({"state": self.input.state_name(), "pointer_id": pointer_id, "sequence": sequence}))
    }

    fn hit_sample_complete(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(pointer_id) = params.get("pointer_id").and_then(Value::as_u64) else { return self.reject(request_id, "invalid_request", "pointer_id is required", None); };
        let hit_id = params.get("test_hit_id").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()).unwrap_or(RENDER_HIT_NONE);
        match self.input.complete_sample(pointer_id, self.graph_revision, self.hit_target_generation, hit_id) {
            Ok(()) => {
                self.journal.append(TraceLevel::Info, "ui.hit_sample.completed", Some(request_id.clone()), None, None, None, Some(self.graph_revision), Some(self.graph_revision), json!({"pointer_id": pointer_id, "composition_revision": self.graph_revision.0, "fragment_revision": self.graph_revision.0}));
                self.accept(request_id, json!({"state": self.input.state_name(), "hovered": self.input.hover_id.is_some()}))
            }
            Err(code) => self.reject(request_id, code, "hit sample was rejected", None),
        }
    }

    fn pointer_down(&mut self, request_id: RequestId) -> RpcResponse {
        match self.input.pointer_down() {
            Ok(()) => self.accept(request_id, json!({"state": self.input.state_name(), "captured": true})),
            Err(code) => self.reject(request_id, code, "pointer down was rejected", None),
        }
    }

    fn pointer_up(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let eligible = params.get("eligible").and_then(Value::as_bool).unwrap_or(false);
        match self.input.pointer_up(eligible) {
            Ok(()) => self.accept(request_id, json!({"state": self.input.state_name(), "semantic_event": "ui.pointer.click"})),
            Err(code) => self.reject(request_id, code, "pointer interaction cancelled", None),
        }
    }

    fn focus_loss(&mut self, request_id: RequestId) -> RpcResponse {
        self.input.cancel();
        self.accept(request_id, json!({"state": self.input.state_name(), "semantic_event": "ui.interaction.cancelled"}))
    }

    fn target_capture(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(target) = params.get("target").and_then(Value::as_str) else {
            return self.reject(request_id, "invalid_request", "target is required", None);
        };
        if !matches!(target, UI_COLOR_TARGET | UI_HIT_TARGET) {
            return self.reject(request_id, "not_found", "target is not available", None);
        }
        self.accept(request_id, json!({"target": target, "format": if target == UI_HIT_TARGET { "r32uint" } else { "rgba8unorm" }, "graph_revision": self.graph_revision, "hit_target_generation": self.hit_target_generation, "test_target": true}))
    }

    fn resource_preload(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let asset: AssetRef = match serde_json::from_value(params) {
            Ok(asset) => asset,
            Err(_) => { return self.reject(request_id, "invalid_request", "a stable AssetRef is required", None);
            }
        };
        if !matches!(asset.kind.as_str(), "font" | "image") {
            return self.reject(request_id, "unsupported_resource_kind", "only font and image resources are supported", None);
        }
        self.reject(request_id, "asset_content_required", "query project/resource for revisioned asset bytes before preloading", Some(asset.revision))
    }

    fn resource_wait_ready(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(asset_id) = params.get("asset_id").and_then(Value::as_u64) else { return self.reject(request_id, "invalid_request", "asset_id is required", None); };
        let Some(record) = self.resources.get(&asset_id) else { return self.reject(request_id, "not_found", "resource is not resident", None); };
        self.accept(request_id, json!({"job_id": record.job_id, "state": record.state.as_str()}))
    }

    fn resource_inspect(&mut self, request_id: RequestId) -> RpcResponse {
        let resources = self.resources.values().map(|record| json!({"asset_id": record.asset.asset_id, "revision": record.asset.revision, "kind": record.asset.kind, "job_id": record.job_id, "state": record.state.as_str()})).collect::<Vec<_>>();
        self.accept(request_id, json!({"resources": resources}))
    }

    fn target_assert(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(target) = params.get("target").and_then(Value::as_str) else {
            return self.reject(request_id, "invalid_request", "target is required", None);
        };
        if target != UI_HIT_TARGET { return self.reject(request_id, "unsupported_target", "only the UI hit target has semantic assertions", None); }
        self.accept(request_id, json!({"target": UI_HIT_TARGET, "graph_revision": self.graph_revision, "hit_target_generation": self.hit_target_generation, "assertions": "accepted"}))
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
        "mode": "headless",
        "hit_target_generation": diagnostics.hit_target_generation
    })
}

fn composition_graph_snapshot(graph_revision: Revision, hit_target_generation: u64) -> Value {
    json!({
        "graph_revision": graph_revision, "hit_target_generation": hit_target_generation,
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

    #[test]
    fn pointer_probe_accepts_logical_or_physical_coordinates_and_rejects_invalid_input() {
        assert_eq!(
            probe_position(&json!({"logical_position": {"x": 40.0, "y": 60.0}})),
            Ok((Some([40.0, 60.0]), None))
        );
        assert_eq!(
            probe_position(&json!({"physical_position": {"x": 850.0, "y": 70.0}})),
            Ok((None, Some([850.0, 70.0])))
        );
        assert_eq!(
            probe_position(&json!({"logical_position": {"x": "bad", "y": 1.0}})),
            Err("position x must be a number")
        );
    }

    #[test]
    fn data_grid_window_scheduler_coalesces_to_the_latest_request() {
        let now = Instant::now();
        let base = UiDataGridWindowRequest {
            renderer_epoch: 1,
            composition_revision: Revision(4),
            fragment: neon_ui_schema::UiFragmentRevision { id: UiFragmentId("virtual-list".into()), revision: Revision(2) },
            source_key: "asset_window".into(), expected_list_revision: Revision(1),
            requested_first_row: 8, max_window_rows: 12, sequence: 1,
        };
        let mut latest = LatestDataGridWindowRequests::default();
        latest.schedule(base.clone(), now);
        let mut replacement = base;
        replacement.requested_first_row = 80;
        replacement.sequence = 2;
        latest.schedule(replacement, now + Duration::from_millis(5));
        assert!(latest.take_ready(now + Duration::from_millis(24)).is_empty());
        let ready = latest.take_ready(now + Duration::from_millis(29));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].requested_first_row, 80);
        assert_eq!(ready[0].sequence, 2);
    }

    #[test]
    fn data_grid_text_commit_event_keeps_declared_target_and_typed_handle() {
        let text_handle = neon_ui_schema::UiTextHandle {
            id: 7,
            generation: 2,
        };
        let binding = UiHitBinding {
            node_path: "grid/assets/data-grid-row-asset-42/cell-name".into(),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: UiFragmentId("grid".into()),
                revision: Revision(4),
            },
            intent: Some(neon_ui_schema::UiIntent::Invoke {
                action: "asset.name.edit".into(),
                params: json!({}),
            }),
            text_input: Some(ui_renderer::UiTextInputBinding {
                node_path: "grid/assets/data-grid-row-asset-42/cell-name".into(),
                max_length: 8,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 80.0,
                    height: 24.0,
                },
            }),
            data_grid_cell: Some(neon_ui_schema::UiDataGridCellTarget {
                source_key: "assets_window".into(),
                stable_row_key: "asset-42".into(),
                column_key: "name".into(),
            }),
            control_value: Some(neon_ui_schema::UiSemanticPayloadValue::TextHandle {
                value: text_handle,
            }),
            max_text_length: Some(8),
        };
        assert!(diagnostic_node_path(&binding).is_none());
        let event = text_input_commit_event(9, Revision(12), 3, binding, "renamed".into())
            .expect("a declared edit intent must produce an event");
        assert_eq!(
            event.event,
            neon_ui_schema::UiSemanticEventType::TextInputCommit
        );
        assert_eq!(event.renderer_epoch, 9);
        assert_eq!(event.composition_revision, Revision(12));
        assert_eq!(event.data_grid_cell.unwrap().stable_row_key, "asset-42");
        assert_eq!(event.text.unwrap().value, "renamed");
        assert_eq!(
            event.control_value,
            Some(neon_ui_schema::UiSemanticPayloadValue::TextHandle { value: text_handle })
        );
    }

    fn test_device(label: &'static str) -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::LowPower, compatible_surface: None, force_fallback_adapter: true, apply_limit_buckets: false }))
            .or_else(|_| { pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::LowPower, compatible_surface: None, force_fallback_adapter: false, apply_limit_buckets: false }))
        })
        .expect("a headless WGPU adapter is required");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some(label), required_features: wgpu::Features::empty(), required_limits: wgpu::Limits::downlevel_defaults(), experimental_features: wgpu::ExperimentalFeatures::default(), memory_hints: wgpu::MemoryHints::MemoryUsage, trace: wgpu::Trace::Off })).expect("the selected adapter must create a device and queue")
    }

    fn ai_test_device(label: &'static str) -> (wgpu::Device, wgpu::Queue) {
        let backends = if cfg!(target_os = "windows") {
            wgpu::Backends::VULKAN
        } else {
            wgpu::Backends::PRIMARY
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("AI GPU acceptance requires a compute adapter");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some(label),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .expect("the AI acceptance adapter must create a device and queue")
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
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
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
                submission: UiFragmentSubmission::new(fragment(revision))
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
            json!([CAPABILITY_UI_FRAGMENT, "wgpu.render.diagnostics", CAPABILITY_UI_HIT_TARGET, CAPABILITY_UI_SEMANTIC_EVENT, CAPABILITY_UI_PROGRAM_SEMANTIC_EVENT, CAPABILITY_UI_RENDER_SURFACE])
        );
        assert_eq!(snapshot.status, RpcStatus::Accepted);
        assert_eq!(
            snapshot.result.unwrap()["capabilities"],
            json!([CAPABILITY_UI_FRAGMENT, "wgpu.render.diagnostics", CAPABILITY_UI_HIT_TARGET, CAPABILITY_UI_SEMANTIC_EVENT, CAPABILITY_UI_PROGRAM_SEMANTIC_EVENT, CAPABILITY_UI_RENDER_SURFACE])
        );
    }

    #[test]
    fn window_control_advertises_gpu_generation_capability_only_there() {
        let headless = WgpuRuntime::headless(1);
        let window = WgpuRuntime::window_control(1, Arc::new(Mutex::new(InteractionTraceStore::new())));
        assert!(!headless.service_description().capabilities.iter().any(|capability| capability == CAPABILITY_AI_TERRAIN_GENERATION));
        assert!(window.service_description().capabilities.iter().any(|capability| capability == CAPABILITY_AI_TERRAIN_GENERATION));
        assert!(window.service_description().capabilities.iter().any(|capability| capability == CAPABILITY_DEBUG_INTERACTION));
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
    fn window_mailbox_ignores_stale_composition_and_idles_without_dirty_work() {
        let mut window = WindowedRuntime::new(1);
        window.redraw_pending = false;
        assert!(!window.needs_redraw());
        assert!(window.apply_fragments(Revision(4), HashMap::from([(UiFragmentId("fresh".into()), fragment(4))])));
        assert!(window.needs_redraw());
        window.redraw_pending = false;
        assert!(!window.apply_fragments(Revision(3), HashMap::from([(UiFragmentId("stale".into()), fragment(3))])));
        assert!(window.fragments.contains_key(&UiFragmentId("fresh".into())));
        assert!(!window.fragments.contains_key(&UiFragmentId("stale".into())));
        assert!(!window.needs_redraw());
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
    fn react_client_cannot_bypass_ui_runtime_to_submit_a_fragment() {
        let mut runtime = WgpuRuntime::headless(1);
        let mut request = submit("react-direct", 1);
        request.client.kind = ClientKind::UiReactClient;
        request.client.origin = "neon-ui-react-client".into();
        let response = runtime.handle(request);
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "renderer_submission_requires_ui_runtime");
        assert_eq!(runtime.diagnostics().fragment_count, 0);
    }

    #[test]
    fn submit_rejects_unsupported_ui_schema_version() {
        let mut runtime = WgpuRuntime::headless(1);
        let request = RpcRequest {
            idempotency_key: Some("unsupported-schema-key".into()),
            ..request(
                "unsupported-schema",
                "wgpu.ui.submit_fragment",
                json!(UiCommand::SubmitFragment {
                    submission: UiFragmentSubmission {
                        schema_version: neon_ui_schema::UI_FRAGMENT_SCHEMA_VERSION + 1,
                        fragment: fragment(1),
                    }
                }),
            )
        };
        let response = runtime.handle(request);
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "invalid_request");
        assert_eq!(runtime.diagnostics().fragment_count, 0);
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
        let (_device, _queue) = test_device("neon3-headless-acceptance");
    }

    #[test]
    fn film_applies_scale_and_bias_by_nchw_channel() {
        let (device, queue) = ai_test_device("neon3-film-nchw-acceptance");
        let mut ctx = neon_wgpu_ai::GpuCtx::new(device, queue);
        let input = [1.0f32, 2.0, 3.0, 10.0, 20.0, 30.0];
        let params = [0.5f32, -0.25, 1.0, -2.0];
        let input_buffer = ctx.upload(bytemuck::cast_slice(&input), "film-acceptance-input");
        let params_buffer = ctx.upload(bytemuck::cast_slice(&params), "film-acceptance-params");
        ctx.begin_batch();
        let output = neon_wgpu_ai::ops::film(
            &mut ctx,
            &input_buffer,
            &params_buffer,
            2,
            input.len() as u64,
        );
        ctx.submit_batch();
        assert_eq!(ctx.submission_count(), 1);
        let actual = ctx
            .readback_f32(&output.buffer, input.len())
            .expect("FiLM output must read back");
        let expected = [2.5f32, 4.0, 5.5, 5.5, 13.0, 20.5];
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() < 1e-6,
                "FiLM NCHW channel mismatch at {index}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn tiled_conv2d_matches_nchw_cpu_reference() {
        let (device, queue) = ai_test_device("neon3-tiled-conv2d-acceptance");
        let mut ctx = neon_wgpu_ai::GpuCtx::new(device, queue);
        let (in_c, out_c, h, w, k) = (2u32, 3u32, 4u32, 5u32, 3u32);
        let input: Vec<f32> = (0..in_c * h * w)
            .map(|index| index as f32 * 0.03125 - 0.5)
            .collect();
        let weights: Vec<f32> = (0..out_c * in_c * k * k)
            .map(|index| (index as i32 % 11 - 5) as f32 * 0.025)
            .collect();
        let bias = [0.1f32, -0.2, 0.3];
        let input_buffer = ctx.upload(bytemuck::cast_slice(&input), "conv-acceptance-input");
        let weight_buffer = ctx.upload(bytemuck::cast_slice(&weights), "conv-acceptance-weights");
        let bias_buffer = ctx.upload(bytemuck::cast_slice(&bias), "conv-acceptance-bias");
        ctx.begin_batch();
        let output = neon_wgpu_ai::ops::conv2d(
            &mut ctx,
            &input_buffer,
            &weight_buffer,
            &bias_buffer,
            in_c,
            out_c,
            h,
            w,
            k,
            k,
            1,
            1,
        );
        ctx.submit_batch();
        let actual = ctx
            .readback_f32(&output.buffer, (out_c * h * w) as usize)
            .expect("conv2d output must read back");
        let mut expected = vec![0.0f32; actual.len()];
        for oc in 0..out_c {
            for oy in 0..h {
                for ox in 0..w {
                    let mut sum = bias[oc as usize];
                    for ic in 0..in_c {
                        for ky in 0..k {
                            for kx in 0..k {
                                let iy = oy as i32 + ky as i32 - 1;
                                let ix = ox as i32 + kx as i32 - 1;
                                if iy >= 0 && iy < h as i32 && ix >= 0 && ix < w as i32 {
                                    let input_index = (ic * h * w + iy as u32 * w + ix as u32) as usize;
                                    let weight_index = (oc * in_c * k * k + ic * k * k + ky * k + kx) as usize;
                                    sum += input[input_index] * weights[weight_index];
                                }
                            }
                        }
                    }
                    expected[(oc * h * w + oy * w + ox) as usize] = sum;
                }
            }
        }
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() < 1e-4,
                "tiled conv2d mismatch at {index}: {actual} != {expected}"
            );
        }
    }

    #[test]
    fn persistent_render_surface_updates_content_without_replacing_the_slot() {
        use wgpu::util::DeviceExt;

        let (device, queue) = test_device("neon3-persistent-render-surface");
        let converter = HeightmapPreviewConverter::new(&device);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let root = UiNode {
            node_id: UiNodeId("preview".into()),
            kind: UiNodeKind::RenderSurface,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 64.0, height: 64.0 },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: Some(neon_ui_schema::RenderSurfaceRef { target_id: "ai.terrain.preview".into() }),
            style: UiStyle { opacity: 1.0, ..UiStyle::default() },
            enter_transition: None,
            children: Vec::new(),
        };
        let fragments = HashMap::from([(
            UiFragmentId("persistent-preview".into()),
            UiFragment {
                fragment_id: UiFragmentId("persistent-preview".into()),
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let mut rendered = Vec::new();
        for value in [-3.0f32, 3.0f32] {
            let source = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("neon3-persistent-render-surface-source"),
                contents: bytemuck::cast_slice(&vec![value; 64 * 64]),
                usage: wgpu::BufferUsages::STORAGE,
            });
            let output = renderer.ensure_render_surface(&device, "ai.terrain.preview", [64, 64]);
            converter.convert_into(&device, &queue, &source, 64, &output);
            rendered.push(ui_renderer::render_renderer_offscreen_for_test(
                &mut renderer,
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                &fragments,
                [64, 64],
                1.0,
            ));
        }
        let center = 4 * (32 * 64 + 32);
        assert_eq!(&rendered[0][center..center + 4], [0, 0, 0, 255]);
        assert_eq!(&rendered[1][center..center + 4], [255, 255, 255, 255]);
    }

    #[test]
    #[ignore = "loads the 257 MB real model pack"]
    fn real_ai_generation_composes_through_a_gpu_render_surface() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("a Vulkan adapter is required for the real-model acceptance test");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-ai-render-surface-acceptance"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .expect("the Vulkan adapter must create a device and queue");
        let pack_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/ai/terrain_run1/terrain_run1.pack");
        let pack = std::fs::read(&pack_path).expect("real terrain model pack must exist");
        let mut engine = neon_wgpu_ai::AiEngine::new(device.clone(), queue.clone());
        engine.load_model(&pack).expect("real terrain model must load");
        let converter = gpu_preview::HeightmapPreviewConverter::new(&device);
        let root = UiNode {
            node_id: UiNodeId("terrain-preview".into()),
            kind: UiNodeKind::RenderSurface,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 64.0, height: 64.0 },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: Some(neon_ui_schema::RenderSurfaceRef {
                target_id: "ai.terrain.preview".into(),
            }),
            style: UiStyle { opacity: 1.0, ..UiStyle::default() },
            enter_transition: None,
            children: Vec::new(),
        };
        let fragments = HashMap::from([(
            UiFragmentId("ai-terrain-preview".into()),
            UiFragment {
                fragment_id: UiFragmentId("ai-terrain-preview".into()),
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let mut renderer = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let mut previous = None;
        for seed in [42, 43, 44] {
            let generation = engine
                .generate_gpu(neon_wgpu_ai::GenerateRequest {
                    cond: neon_wgpu_ai::format::TerrainCond {
                        sub: None,
                        parent: Some(1),
                        relief: None,
                        texture: None,
                        water: None,
                    },
                    guidance: 7.0,
                    steps: 1,
                    seed,
                    size: 32,
                    preview_every: 0,
                })
                .expect("GPU-resident terrain generation must complete repeatedly");
            let texture = converter.convert(&device, &queue, &generation.heightmap, generation.size);
            renderer.register_render_surface(&device, "ai.terrain.preview", texture);
            let pixels = ui_renderer::render_renderer_offscreen_for_test(
                &mut renderer,
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                &fragments,
                [64, 64],
                1.0,
            );
            let mut minimum = u8::MAX;
            let mut maximum = u8::MIN;
            for pixel in pixels.chunks_exact(4) {
                minimum = minimum.min(pixel[0]);
                maximum = maximum.max(pixel[0]);
                assert_eq!(pixel[3], 255, "the composed preview must remain opaque");
            }
            assert!(maximum.saturating_sub(minimum) > 16, "the AI preview must contain visible height variation");
            if let Some(previous) = previous.as_ref() {
                assert_ne!(previous, &pixels, "a new seed must replace the existing surface pixels");
            }
            previous = Some(pixels);
        }
    }

    #[test]
    fn ui_fragment_renders_visible_pixels_to_offscreen_target() {
        let (device, queue) = test_device("neon3-ui-render-acceptance");
        let pixels = ui_renderer::render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &HashMap::from([(UiFragmentId("acceptance".into()), fragment(1))]),
            [64, 64],
            1.0,
            &[],
            Vec::new(),
        );
        assert!(pixels.iter().any(|value| *value != 0), "UI render target must contain visible pixels");
    }

    #[test]
    fn ui_fragment_renders_visible_pixels_to_srgb_surface_format() {
        let (device, queue) = test_device("neon3-srgb-ui-acceptance");
        let root = UiNode { node_id: UiNodeId("srgb-root".into()), kind: UiNodeKind::Panel, bounds: UiBounds { x: 0.0, y: 0.0, width: 64.0, height: 64.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, surface: None, style: UiStyle { background_color: [0.0, 0.7, 0.9, 1.0], border_color: [1.0; 4], border_width: 0.0, corner_radius: 0.0, opacity: 1.0 }, enter_transition: None, children: Vec::new() };
        let fragments = HashMap::from([(UiFragmentId("srgb-acceptance".into()), UiFragment { fragment_id: UiFragmentId("srgb-acceptance".into()), revision: Revision(1), root, effects: Vec::new() })]);
        let pixels = ui_renderer::render_offscreen_for_test(&device, &queue, wgpu::TextureFormat::Bgra8UnormSrgb, &fragments, [64, 64], 1.0, &[], Vec::new());
        assert!(pixels.iter().any(|value| *value != 0), "sRGB composition target must contain visible UI pixels");
    }

    #[test]
    fn ui_hit_target_matches_panel_coverage_and_paint_order() {
        let (device, queue) = test_device("neon3-ui-hit-target-acceptance");
        let mut root = fragment(1).root;
        root.kind = UiNodeKind::Panel;
        root.bounds = UiBounds { x: 0.0, y: 0.0, width: 64.0, height: 64.0 };
        root.children = vec![
            UiNode { node_id: UiNodeId("back".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 8.0, y: 8.0, width: 32.0, height: 32.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, surface: None, style: UiStyle { corner_radius: 8.0, ..UiStyle::default() }, enter_transition: None, children: Vec::new() },
            UiNode { node_id: UiNodeId("front".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 16.0, y: 16.0, width: 32.0, height: 32.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, surface: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
            UiNode { node_id: UiNodeId("disabled".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 48.0, y: 48.0, width: 12.0, height: 12.0 }, layout: None, visible: true, enabled: false, text_key: None, text: None, image: None, surface: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
            UiNode { node_id: UiNodeId("transparent".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 48.0, y: 32.0, width: 12.0, height: 12.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, surface: None, style: UiStyle { opacity: 0.0, ..UiStyle::default() }, enter_transition: None, children: Vec::new() },
        ];
        let pixels = ui_renderer::render_hit_ids_for_test(&device, &queue, &HashMap::from([(UiFragmentId("hit-acceptance".into()), UiFragment { fragment_id: UiFragmentId("hit-acceptance".into()), revision: Revision(1), root, effects: Vec::new() })]), [64, 64]);
        let at = |x: usize, y: usize| pixels[y * 64 + x];
        assert_eq!(at(0, 0), RENDER_HIT_NONE, "background must remain no-hit");
        assert_eq!(at(8, 8), RENDER_HIT_NONE, "rounded corner must discard its hit ID");
        assert_ne!(at(12, 20), RENDER_HIT_NONE, "interactive panel interior must receive an ID");
        assert_ne!(at(20, 20), RENDER_HIT_NONE, "front panel interior must receive an ID");
        assert_ne!(at(12, 20), at(20, 20), "front-most panel must replace the lower ID");
        assert_eq!(at(52, 52), RENDER_HIT_NONE, "disabled panel must remain no-hit");
        assert_eq!(at(52, 36), RENDER_HIT_NONE, "transparent panel must remain no-hit");
    }

    #[test]
    fn ui_hit_target_respects_nested_clip_geometry() {
        let (device, queue) = test_device("neon3-ui-clip-acceptance");
        let child = UiNode { node_id: UiNodeId("clipped-button".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 24.0, y: 8.0, width: 24.0, height: 16.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, surface: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() };
        let root = UiNode { node_id: UiNodeId("clip-root".into()), kind: UiNodeKind::Panel, bounds: UiBounds { x: 0.0, y: 0.0, width: 32.0, height: 32.0 }, layout: Some(neon_ui_schema::UiLayout { clip: neon_ui_schema::UiClipPolicy::Bounds, ..neon_ui_schema::UiLayout::default() }), visible: true, enabled: true, text_key: None, text: None, image: None, surface: None, style: UiStyle::default(), enter_transition: None, children: vec![child] };
        let pixels = ui_renderer::render_hit_ids_for_test(&device, &queue, &HashMap::from([(UiFragmentId("clip".into()), UiFragment { fragment_id: UiFragmentId("clip".into()), revision: Revision(1), root, effects: Vec::new() })]), [64, 64]);
        assert_ne!(pixels[12 * 64 + 28], RENDER_HIT_NONE, "child area inside parent clip must be interactive");
        assert_eq!(pixels[12 * 64 + 40], RENDER_HIT_NONE, "child area outside parent clip must be no-hit");
    }

    #[test]
    fn composition_target_apis_are_machine_readable() {
        let mut runtime = WgpuRuntime::headless(3);
        let graph = runtime.handle(request("graph", "wgpu.render.graph.snapshot", json!({})));
        assert_eq!(graph.status, RpcStatus::Accepted);
        assert_eq!(graph.result.as_ref().unwrap()["targets"][1]["id"], UI_HIT_TARGET);
        assert_eq!(graph.result.as_ref().unwrap()["targets"][1]["format"], "r32uint");
        let capture = runtime.handle(request("capture", "wgpu.render.target.capture", json!({"target": UI_HIT_TARGET})));
        assert_eq!(capture.status, RpcStatus::Accepted);
        assert_eq!(capture.result.as_ref().unwrap()["format"], "r32uint");
        let assertion = runtime.handle(request("assert", "wgpu.render.target.assert", json!({"target": UI_HIT_TARGET, "assertions": []})));
        assert_eq!(assertion.status, RpcStatus::Accepted);
        let resources = runtime.handle(request("resources", "wgpu.resource.inspect", json!({})));
        assert_eq!(resources.status, RpcStatus::Accepted);
        assert!(resources.result.unwrap()["resources"].is_array());
    }

    #[test]
    fn fragment_removal_invalidates_hit_target_generation() {
        let mut runtime = WgpuRuntime::headless(1);
        let before = runtime.diagnostics().hit_target_generation;
        runtime.handle(submit("submit", 1));
        let after_submit = runtime.diagnostics().hit_target_generation;
        let mut remove = request("remove", "wgpu.ui.remove_fragment", json!(UiCommand::RemoveFragment { fragment_id: UiFragmentId("static-fragment".into()), revision: Revision(1) }));
        remove.idempotency_key = Some("remove-key".into());
        assert_eq!(runtime.handle(remove).status, RpcStatus::Accepted);
        assert!(after_submit > before);
        assert!(runtime.diagnostics().hit_target_generation > after_submit);
    }

    #[test]
    fn resize_invalidates_hit_target_generation() {
        let mut runtime = WgpuRuntime::headless(1);
        let before = runtime.diagnostics().hit_target_generation;
        runtime.resize_hit_target_for_test();
        assert!(runtime.diagnostics().hit_target_generation > before);
    }

    #[test]
    fn local_input_lifecycle_validates_samples_and_capture() {
        let mut input = LocalInputState::default();
        input.request_sample(HitSampleRequest { pointer_id: 4, sequence: 1, composition_revision: Revision(3), target_generation: 2 });
        input.complete_sample(4, Revision(3), 2, 41).unwrap();
        assert_eq!(input.state, LocalInteractionState::Hovered);
        input.pointer_down().unwrap();
        assert_eq!(input.capture_id, Some(41));
        input.request_sample(HitSampleRequest { pointer_id: 4, sequence: 2, composition_revision: Revision(3), target_generation: 2 });
        input.complete_sample(4, Revision(3), 2, RENDER_HIT_NONE).unwrap();
        assert_eq!(input.capture_id, Some(41), "move outside must retain capture");
        input.pointer_up(true).unwrap();
        assert_eq!(input.state, LocalInteractionState::Idle);
    }

    #[test]
    fn local_input_rejects_stale_samples_and_cancels_explicitly() {
        let mut input = LocalInputState::default();
        input.request_sample(HitSampleRequest { pointer_id: 0, sequence: 1, composition_revision: Revision(3), target_generation: 2 });
        assert_eq!(input.complete_sample(0, Revision(3), 3, 7), Err("hit_target_generation_stale"));
        input.request_sample(HitSampleRequest { pointer_id: 0, sequence: 1, composition_revision: Revision(2), target_generation: 2 });
        assert_eq!(input.complete_sample(0, Revision(3), 2, 7), Err("composition_revision_stale"));
        input.request_sample(HitSampleRequest { pointer_id: 0, sequence: 2, composition_revision: Revision(3), target_generation: 2 });
        input.complete_sample(0, Revision(3), 2, 7).unwrap();
        input.request_sample(HitSampleRequest { pointer_id: 0, sequence: 2, composition_revision: Revision(3), target_generation: 2 });
        assert_eq!(input.complete_sample(0, Revision(3), 2, 7), Err("input_sequence_stale"));
        input.pointer_down().unwrap();
        input.cancel();
        assert_eq!(input.state, LocalInteractionState::Cancelled);
        assert_eq!(input.pointer_up(true), Err("interaction_cancelled"));
    }

    #[test]
    fn test_input_methods_expose_semantic_lifecycle_without_render_ids() {
        let mut runtime = WgpuRuntime::headless(1);
        let request_sample = runtime.handle(request("sample-request", "test.ui.hit_sample.request", json!({"pointer_id": 0, "sequence": 1})));
        assert_eq!(request_sample.status, RpcStatus::Accepted);
        let completed = runtime.handle(request("sample-complete", "test.ui.hit_sample.complete", json!({"pointer_id": 0, "test_hit_id": 17})));
        assert_eq!(completed.status, RpcStatus::Accepted);
        assert!(completed.result.as_ref().unwrap().get("render_hit_id").is_none());
        assert_eq!(runtime.handle(request("down", "test.ui.pointer.down", json!({}))).status, RpcStatus::Accepted);
        let click = runtime.handle(request("up", "test.ui.pointer.up", json!({"eligible": true})));
        assert_eq!(click.status, RpcStatus::Accepted);
        assert_eq!(click.result.unwrap()["semantic_event"], "ui.pointer.click");
        let cancelled = runtime.handle(request("focus-loss", "test.ui.focus.loss", json!({})));
        assert_eq!(cancelled.result.unwrap()["semantic_event"], "ui.interaction.cancelled");
    }

    #[test]
    fn trace_query_filters_u3_pointer_and_revision_metadata() {
        let mut runtime = WgpuRuntime::headless(1);
        runtime.handle(request("sample-request", "test.ui.hit_sample.request", json!({"pointer_id": 9, "sequence": 1})));
        let response = runtime.handle(request("trace", "debug.trace.query", json!({"pointer_id": 9, "fragment_revision": 0, "composition_revision": 0})));
        assert_eq!(response.status, RpcStatus::Accepted);
        let records = response.result.unwrap().as_array().unwrap().clone();
        assert!(records.iter().any(|record| record["event"] == "ui.hit_sample.requested"));
        assert!(records.iter().all(|record| record["data"].get("render_hit_id").is_none()));
    }

    #[test]
    fn interaction_trace_get_reports_the_accepted_real_window_lifecycle() {
        let mut runtime = WgpuRuntime::headless(1);
        let interaction_id = InteractionId("wgpu-window-1-7".into());
        let target = Some(InteractionSemanticTarget {
            node_path: "tools/water/apply".into(),
        });
        let downstream = RequestId("wgpu-pointer-click-7".into());
        let mut traces = runtime.interaction_traces.lock().unwrap();
        traces.append(interaction_id.clone(), InteractionTraceStage::Prepared, InteractionTraceOutcome::Pending, None, None, None, Revision(4), None);
        traces.append(interaction_id.clone(), InteractionTraceStage::HitCaptureResolved, InteractionTraceOutcome::Accepted, None, target.clone(), Some(Revision(9)), Revision(4), None);
        traces.append(interaction_id.clone(), InteractionTraceStage::SemanticEventForwarded, InteractionTraceOutcome::Pending, None, target.clone(), Some(Revision(9)), Revision(4), Some(downstream.clone()));
        traces.append(interaction_id.clone(), InteractionTraceStage::DeliveryAccepted, InteractionTraceOutcome::Accepted, None, target, Some(Revision(9)), Revision(4), Some(downstream));
        traces.delivery_accepted(interaction_id.clone());
        traces.composition_applied(Revision(5));
        drop(traces);

        let response = runtime.handle(request("interaction-get", "debug.interaction.get", json!({"interaction_id": interaction_id.0})));
        assert_eq!(response.status, RpcStatus::Accepted);
        let records = response.result.unwrap()["records"].as_array().unwrap().clone();
        assert_eq!(records.len(), 5);
        assert_eq!(records[4]["stage"], "composition_revision_applied");
        assert_eq!(records[4]["composition_revision"], 5);
        assert_eq!(records[2]["downstream_request_id"], "wgpu-pointer-click-7");
        assert_eq!(records[1]["semantic_target"]["node_path"], "tools/water/apply");
        assert!(records.iter().all(|record| record.get("coordinates").is_none() && record.get("hit_id").is_none()));
    }

    #[test]
    fn interaction_trace_query_filters_rejected_delivery() {
        let mut runtime = WgpuRuntime::headless(1);
        let interaction_id = InteractionId("wgpu-window-1-8".into());
        let downstream = RequestId("wgpu-pointer-click-8".into());
        let mut traces = runtime.interaction_traces.lock().unwrap();
        traces.append(interaction_id.clone(), InteractionTraceStage::Prepared, InteractionTraceOutcome::Pending, None, None, None, Revision(6), None);
        traces.append(interaction_id.clone(), InteractionTraceStage::DeliveryRejected, InteractionTraceOutcome::Rejected, Some(InteractionTraceError { code: "intent_not_bound".into(), message: "semantic intent is not bound".into() }), Some(InteractionSemanticTarget { node_path: "assets/remove".into() }), Some(Revision(2)), Revision(6), Some(downstream));
        drop(traces);

        let response = runtime.handle(request("interaction-query", "debug.interaction.query", json!({"after": 1, "limit": 1, "filters": {"outcome": "rejected", "semantic_node_path": "assets/remove"}})));
        assert_eq!(response.status, RpcStatus::Accepted);
        let records = response.result.unwrap()["records"].as_array().unwrap().clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["stage"], "delivery_rejected");
        assert_eq!(records[0]["error"]["code"], "intent_not_bound");
    }

    #[test]
    fn owner_asset_bytes_preload_reports_readiness_and_trace() {
        let mut runtime = WgpuRuntime::headless(1);
        let asset = AssetRef { project_id: "fixture-project".into(), asset_id: 81, revision: Revision(5), kind: "image".into() };
        let missing_content = runtime.handle(request("missing-content", "wgpu.ui.resource.preload", json!(asset.clone())));
        assert_eq!(missing_content.error.unwrap().code, "asset_content_required");
        let owner_content = AssetBytes { asset: asset.clone(), media_type: "application/x-neon-rgba8".into(), width: Some(2), height: Some(1), bytes: vec![51, 204, 102, 255, 51, 204, 102, 0] };
        let response = runtime.preload_resource_from_owner(request("preload", "wgpu.ui.resource.preload", json!(asset)), owner_content);
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.result.as_ref().unwrap()["state"], "ready");
        let wait = runtime.handle(request("wait", "wgpu.resource.wait_ready", json!({"asset_id": 81})));
        assert_eq!(wait.result.unwrap()["state"], "ready");
        let trace = runtime.handle(request("trace", "debug.trace.query", json!({"request_id": "preload"})));
        assert!(trace.result.unwrap().as_array().unwrap().iter().any(|record| record["event"] == "ui.resource.ready"));
    }

    #[test]
    fn owner_resource_failure_is_queryable_by_job_and_trace() {
        let mut runtime = WgpuRuntime::headless(1);
        let asset = AssetRef { project_id: "fixture-project".into(), asset_id: 82, revision: Revision(5), kind: "font".into() };
        let response = runtime.preload_resource_from_owner(
            request("font-invalid", "wgpu.ui.resource.preload", json!(asset.clone())),
            AssetBytes { asset: asset.clone(), media_type: "font/ttf".into(), width: None, height: None, bytes: Vec::new() },
        );
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "invalid_resource_content");
        let wait = runtime.handle(request("font-wait", "wgpu.resource.wait_ready", json!({"asset_id": 82})));
        assert_eq!(wait.status, RpcStatus::Accepted);
        assert_eq!(wait.result.unwrap()["state"], "failed");
        let trace = runtime.handle(request("font-trace", "debug.trace.query", json!({"request_id": "font-invalid"})));
        assert!(trace.result.unwrap().as_array().unwrap().iter().any(|record| record["event"] == "ui.resource.failed"));
    }

    #[test]
    fn project_asset_bytes_drive_renderer_private_image_residency() {
        let asset = AssetRef { project_id: "fixture-project".into(), asset_id: 81, revision: Revision(5), kind: "image".into() };
        let mut projectd = neon_projectd::Projectd::fixture(3);
        let describe = projectd.handle(request("project-describe", "service.describe", json!({})));
        assert_eq!(describe.status, RpcStatus::Accepted);
        assert!(describe.result.as_ref().unwrap()["capabilities"].as_array().unwrap().iter().any(|capability| capability == neon_projectd::CAPABILITY_ASSET_BYTES));
        let snapshot = projectd.handle(request("project-snapshot", "debug.snapshot.get", json!({})));
        assert_eq!(snapshot.status, RpcStatus::Accepted);
        assert_eq!(snapshot.result.as_ref().unwrap()["revision"], 5);
        let owner_response = projectd.handle(request("project-asset", "asset.get_bytes", json!(asset.clone())));
        assert_eq!(owner_response.status, RpcStatus::Accepted);
        let content: AssetBytes = serde_json::from_value(owner_response.result.unwrap()).unwrap();

        let mut runtime = WgpuRuntime::headless(1);
        let preload = runtime.preload_resource_from_owner(request("preload", "wgpu.ui.resource.preload", json!(asset.clone())), content.clone());
        assert_eq!(preload.status, RpcStatus::Accepted);
        assert_eq!(preload.result.as_ref().unwrap()["job_id"], "ui-resource-81-5");
        let (device, queue) = test_device("neon3-ui-image-alpha");
        let mut image = fragment(1).root;
        image.kind = UiNodeKind::Image;
        image.bounds = UiBounds { x: 0.0, y: 0.0, width: 64.0, height: 64.0 };
        image.image = Some(AssetRef { project_id: "fixture-project".into(), asset_id: 81, revision: Revision(5), kind: "image".into() });
        image.style = UiStyle { background_color: [0.2, 0.8, 0.4, 1.0], border_color: [0.0; 4], border_width: 0.0, corner_radius: 0.0, opacity: 1.0 };
        let fragments = HashMap::from([(UiFragmentId("image".into()), UiFragment { fragment_id: UiFragmentId("image".into()), revision: Revision(1), root: image, effects: Vec::new() })]);
        let unresolved = ui_renderer::render_offscreen_for_test(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, &fragments, [64, 64], 1.0, &[], Vec::new());
        assert_eq!(unresolved[4 * (16 * 64 + 16) + 3], 0, "an unresolved AssetRef must not render a fixture image");
        let pixels = ui_renderer::render_offscreen_for_test(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, &fragments, [64, 64], 1.0, &[content], Vec::new());
        assert!(pixels[4 * (16 * 64 + 16) + 3] > 0, "the opaque image half must render alpha");
        assert_eq!(pixels[4 * (16 * 64 + 48) + 3], 0, "the transparent image half must preserve alpha");
    }

    #[test]
    fn project_font_bytes_drive_renderer_private_readiness() {
        let asset = AssetRef { project_id: "fixture-project".into(), asset_id: 82, revision: Revision(5), kind: "font".into() };
        let mut projectd = neon_projectd::Projectd::fixture(3);
        assert_eq!(projectd.handle(request("font-project-describe", "service.describe", json!({}))).status, RpcStatus::Accepted);
        assert_eq!(projectd.handle(request("font-project-snapshot", "debug.snapshot.get", json!({}))).status, RpcStatus::Accepted);
        let owner_response = projectd.handle(request("font-project-asset", "asset.get_bytes", json!(asset.clone())));
        let content: AssetBytes = serde_json::from_value(owner_response.result.unwrap()).unwrap();
        assert_eq!(content.media_type, "font/ttf");

        let mut runtime = WgpuRuntime::headless(1);
        let preload = runtime.preload_resource_from_owner(request("font-preload", "wgpu.ui.resource.preload", json!(asset)), content);
        assert_eq!(preload.status, RpcStatus::Accepted);
        assert_eq!(preload.result.unwrap()["job_id"], "ui-resource-82-5");
        let wait = runtime.handle(request("font-wait", "wgpu.resource.wait_ready", json!({"asset_id": 82})));
        assert_eq!(wait.result.unwrap()["state"], "ready");
        let trace = runtime.handle(request("font-trace", "debug.trace.query", json!({"request_id": "font-preload"})));
        assert!(trace.result.unwrap().as_array().unwrap().iter().any(|record| record["event"] == "ui.resource.ready"));
    }

    #[test]
    fn project_font_preload_job_can_override_bundled_text_glyph_residency() {
        let asset = AssetRef { project_id: "fixture-project".into(), asset_id: 82, revision: Revision(5), kind: "font".into() };
        let mut projectd = neon_projectd::Projectd::fixture(3);
        assert_eq!(projectd.handle(request("font-glyph-project-describe", "service.describe", json!({}))).status, RpcStatus::Accepted);
        assert_eq!(projectd.handle(request("font-glyph-project-snapshot", "debug.snapshot.get", json!({}))).status, RpcStatus::Accepted);
        let owner_response = projectd.handle(request("font-glyph-project-asset", "asset.get_bytes", json!(asset.clone())));
        assert_eq!(owner_response.status, RpcStatus::Accepted);
        let content: AssetBytes = serde_json::from_value(owner_response.result.unwrap()).unwrap();

        let mut runtime = WgpuRuntime::headless(1);
        let preload = runtime.preload_resource_from_owner(request("font-glyph-preload", "wgpu.ui.resource.preload", json!(asset)), content.clone());
        assert_eq!(preload.status, RpcStatus::Accepted);
        assert_eq!(preload.result.as_ref().unwrap()["job_id"], "ui-resource-82-5");
        let trace = runtime.handle(request("font-glyph-trace", "debug.trace.query", json!({"request_id": "font-glyph-preload"})));
        assert!(trace.result.unwrap().as_array().unwrap().iter().any(|record| record["event"] == "ui.resource.ready"));

        let (device, queue) = test_device("neon3-ui-font-glyph");
        let mut text = fragment(1).root;
        text.kind = UiNodeKind::Label;
        text.bounds = UiBounds { x: 4.0, y: 4.0, width: 56.0, height: 24.0 };
        text.text = Some(neon_ui_schema::TextRef::Literal { value: "A".into() });
        text.style = UiStyle { background_color: [0.0; 4], border_color: [0.0; 4], border_width: 0.0, corner_radius: 0.0, opacity: 1.0 };
        let fragments = HashMap::from([(UiFragmentId("font-glyph".into()), UiFragment { fragment_id: UiFragmentId("font-glyph".into()), revision: Revision(1), root: text, effects: Vec::new() })]);
        let bundled = ui_renderer::render_offscreen_for_test(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, &fragments, [64, 32], 1.0, &[], Vec::new());
        assert!(bundled.chunks_exact(4).any(|pixel| pixel[3] > 0), "bundled UI font must render glyph pixels");
        let pixels = ui_renderer::render_offscreen_for_test(&device, &queue, wgpu::TextureFormat::Rgba8Unorm, &fragments, [64, 32], 1.0, &[content], Vec::new());
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0), "accepted owner font content must drive private glyph pixels");
    }

    #[test]
    fn resource_preload_rejects_non_ui_asset_kinds() {
        let mut runtime = WgpuRuntime::headless(1);
        let asset = AssetRef { project_id: "fixture-project".into(), asset_id: 82, revision: Revision(1), kind: "water_material".into() };
        let response = runtime.handle(request("preload", "wgpu.ui.resource.preload", json!(asset)));
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "unsupported_resource_kind");
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
            json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(semantic_fragment) }),
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
            }),
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: None,
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
