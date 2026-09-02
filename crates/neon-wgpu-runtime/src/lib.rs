//! Command handling and window/GPU bootstrap for Neon3's sole renderer owner.
//! No other Neon3 crate may initialize window or GPU objects.

use std::sync::{Arc, Mutex};

use std::time::{Duration, Instant};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    thread,
};
#[cfg(debug_assertions)]
use std::{io::BufWriter, path::Path};

use neon_ipc::{EventClient, RpcClient};
use neon_observability::{
    CommandJournal, CommandReceipt, CommandState, DebugSnapshot, EVENT_COMMAND_ACCEPTED,
    EVENT_COMMAND_RECEIVED, EVENT_COMMAND_REJECTED, JournalFilter, TraceLevel, TraceRecord,
};
use neon_protocol::{
    AiTerrainGenerateCommand, AiTerrainGenerationResult, AssetBytes, AssetRef, ClientIdentity,
    ClientKind, HealthStatus, InteractionId, InteractionSemanticTarget, InteractionTraceError,
    InteractionTraceFilters, InteractionTraceOutcome, InteractionTraceQuery,
    InteractionTraceRecord, InteractionTraceStage, PROTOCOL_VERSION, RenderBackend,
    RenderBackendNegotiation, RenderSurfaceKind, RenderSurfaceOpen, RenderSurfaceTargetKind,
    RequestId, Revision, RpcError, RpcRequest, RpcResponse, RpcStatus, ServiceDescription,
    ServiceHealth, ServiceName, UiFileDropPayload, UiImageSource, UiImageTextureRef,
    UiImageTextureRegion, UiImageUploadRequest,
};
#[cfg(test)]
use neon_ui_schema::UiFragmentSubmission;
use neon_ui_schema::{
    TextRef, UiBounds, UiCommand, UiDataGridWindowRequest, UiFragment, UiFragmentId, UiHostInbound,
    UiNode, UiNodeId, UiNodeKind, UiPointerEvent, UiPointerEventType, UiSemanticEvent,
    UiSemanticEventType, UiStyle, UiTransition, UiTransitionState, UiWindowRequest,
};
use neon_world_bridge::{
    CameraControlSample, CameraFrame, CameraFramePayload, CameraId, WorldInformationBridge,
    WorldInformationSnapshot, WorldUiAnchor, WorldUiAnchorBatch,
};
use serde_json::{Value, json};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize, PhysicalSize},
    event::{ElementState, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, KeyCode, NamedKey, PhysicalKey},
    window::{Window, WindowId},
};

#[cfg(windows)]
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[cfg(windows)]
mod dx12_interop;
mod gpu_preview;
mod ui_program_gpu;
mod ui_renderer;
mod world_ui_pipeline;
use gpu_preview::HeightmapPreviewConverter;
pub use ui_program_gpu::GpuUiProgramBackend;
use ui_renderer::{
    LocalPresentationCommit, PendingLocalPresentationKey, UiDrawMode, UiHitBinding, UiWgpuRenderer,
};
use world_ui_pipeline::{WorldUiCamera, WorldUiCameraState, WorldUiPipeline};

pub const SERVICE_NAME: &str = "wgpu-runtime";
pub const CAPABILITY_UI_FRAGMENT: &str = "wgpu.ui.fragment.v1";
pub const CAPABILITY_UI_HIT_TARGET: &str = "wgpu.ui.hit_target.v1";
pub const CAPABILITY_UI_SEMANTIC_EVENT: &str = "wgpu.ui.semantic_event.v1";
pub const CAPABILITY_UI_PROGRAM_SEMANTIC_EVENT: &str = "wgpu.ui.program.semantic_event.v1";
pub const CAPABILITY_UI_RENDER_SURFACE: &str = "wgpu.ui.render_surface.v1";
pub const CAPABILITY_EXTERNAL_HOST_BACKEND_MATCH: &str = "wgpu.external_host.backend_match.v1";
pub const CAPABILITY_EXTERNAL_HOST_D3D12_SURFACE: &str =
    "wgpu.external_host.d3d12_shared_texture.v1";
pub const CAPABILITY_AI_TERRAIN_GENERATION: &str = "wgpu.ai.terrain_generation.v1";
pub const CAPABILITY_DEBUG_INTERACTION: &str = "debug.interaction.v1";
pub const CAPABILITY_DEBUG_WINDOW_CAPTURE: &str = "debug.window.capture.v1";
pub const CAPABILITY_WORLD_UI_LAB_CAMERA: &str = "wgpu.world_ui.lab.camera.v1";
/// Statechart-driven transition sampling in the renderer. Explicit so hosts can
/// gate on it before relying on state-transition motion.
pub const CAPABILITY_STATE_ANIMATION: &str = "wgpu.ui.state.animation.v1";
/// Numeric presentation interpolation in the renderer. Explicit so hosts can
/// gate on it before relying on smooth progress-bar updates.
pub const CAPABILITY_NUMERIC_ANIMATION: &str = "wgpu.ui.numeric.animation.v1";
pub const UI_HIT_TARGET: &str = "ui.hit_id.v1";
pub const UI_COLOR_TARGET: &str = "ui.color.v1";
pub const RENDER_HIT_NONE: u32 = u32::MAX;
const DATA_GRID_WINDOW_DEBOUNCE: Duration = Duration::from_millis(24);
const INTERACTION_TRACE_CAPACITY: usize = 256;
const WORLD_UI_LAB_SURFACE_TARGET: &str = "render.world-ui-lab.preview";
const WORLD_UI_LAB_PANEL_TARGET: &str = "world-ui-lab.panel";
const WORLD_UI_LAB_LOGICAL_SIZE: [u32; 2] = [640, 360];
const WORLD_UI_LAB_PANEL_SIZE: [u32; 2] = [1280, 720];
const WORLD_UI_LAB_PREVIEW_SIZE: [u32; 2] = [640, 360];
const HEADLESS_UI_LOGICAL_SIZE: [f32; 2] = [1280.0, 720.0];
/// Explorer normally runs at medium integrity, so Windows blocks its drop
/// messages to an elevated renderer window. Keep this exception inside the
/// sole window owner and allow only the documented file-drop messages.
#[cfg(windows)]
fn enable_explorer_file_drop(window: &Window) -> Result<(), String> {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            ChangeWindowMessageFilterEx, MSGFLT_ALLOW, WM_COPYDATA, WM_DROPFILES,
        },
    };
    let handle = window
        .window_handle()
        .map_err(|error| format!("read native window handle: {error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("expected a Win32 window handle".into());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    // WM_COPYGLOBALDATA (0x0049) is used by Explorer together with WM_COPYDATA.
    for message in [WM_DROPFILES, WM_COPYDATA, 0x0049] {
        unsafe { ChangeWindowMessageFilterEx(hwnd, message, MSGFLT_ALLOW, None) }
            .map_err(|error| format!("allow Explorer message {message:#06x}: {error}"))?;
    }
    Ok(())
}

#[cfg(windows)]
fn choose_image_file(window: &Window) -> Result<Option<PathBuf>, String> {
    use windows::{
        Win32::{
            Foundation::HWND,
            UI::Controls::Dialogs::{
                CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST,
                OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
            },
        },
        core::{PCWSTR, PWSTR},
    };
    let handle = window
        .window_handle()
        .map_err(|error| format!("read native window handle: {error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("expected a Win32 window handle".into());
    };
    let mut filename = vec![0u16; 32_768];
    let filter: Vec<u16> = "Image files (*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.tga)\0*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.tga\0All files (*.*)\0*.*\0\0".encode_utf16().collect();
    let title: Vec<u16> = "Open image for Neon3 UI Slicer\0".encode_utf16().collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(handle.hwnd.get() as *mut _),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(filename.as_mut_ptr()),
        nMaxFile: filename.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        let length = filename.iter().position(|value| *value == 0).unwrap_or(0);
        return Ok(Some(PathBuf::from(String::from_utf16_lossy(&filename[..length]))));
    }
    let error = unsafe { CommDlgExtendedError() };
    if error.0 == 0 {
        Ok(None)
    } else {
        Err(format!("native image picker failed: {error:?}"))
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorldUiLabCameraRegistration {
    udp_endpoint: SocketAddr,
    session_id: String,
    provider_epoch: u64,
    camera_id: CameraId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorldUiLabDragMode {
    None,
    PendingPan,
    Pan,
    Rotate,
}

impl Default for WorldUiLabDragMode {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Default)]
struct WorldUiLabCameraController {
    enabled: bool,
    window_focused: bool,
    surface_focused: bool,
    axes: [f32; 3],
    forward_held: bool,
    backward_held: bool,
    left_held: bool,
    right_held: bool,
    camera_position: [f32; 3],
    yaw: f32,
    pitch: f32,
    vertical_fov: f32,
    movement_speed: f32,
    drag_mode: WorldUiLabDragMode,
    last_pointer: Option<[f32; 2]>,
    drag_origin: Option<[f32; 2]>,
    sequence: u64,
    registration: Option<WorldUiLabCameraRegistration>,
    socket: Option<UdpSocket>,
    last_udp_error: Option<String>,
}

impl WorldUiLabCameraController {
    fn clear_axes(&mut self) {
        self.axes = [0.0; 3];
        self.forward_held = false;
        self.backward_held = false;
        self.left_held = false;
        self.right_held = false;
        self.drag_mode = WorldUiLabDragMode::None;
        self.last_pointer = None;
        self.drag_origin = None;
    }

    fn active(&self) -> bool {
        self.enabled && self.window_focused && self.surface_focused
    }

    fn state(&self) -> WorldUiCameraState {
        WorldUiCameraState {
            position: self.camera_position,
            yaw: self.yaw,
            pitch: self.pitch,
            vertical_fov: if self.vertical_fov == 0.0 {
                35.0f32.to_radians()
            } else {
                self.vertical_fov
            },
        }
    }

    fn move_camera(&mut self) {
        let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
        let right = [cos_yaw, 0.0, sin_yaw];
        let forward = [sin_yaw, 0.0, -cos_yaw];
        let speed = if self.movement_speed == 0.0 {
            0.35
        } else {
            self.movement_speed
        };
        for index in 0..3 {
            self.camera_position[index] +=
                speed * (right[index] * self.axes[0] + forward[index] * self.axes[1]);
        }
        self.camera_position[1] += speed * self.axes[2];
    }

    fn sample(
        &mut self,
        epoch: u64,
        elapsed: Duration,
        look_delta: [f32; 2],
        wheel_delta: f32,
    ) -> Option<CameraControlSample> {
        let registration = self.registration.as_ref()?;
        self.sequence += 1;
        Some(CameraControlSample {
            camera_id: registration.camera_id.clone(),
            session_id: registration.session_id.clone(),
            producer_epoch: epoch,
            sequence: self.sequence,
            timestamp_monotonic_ns: elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,
            movement_axes: self.axes,
            look_delta,
            wheel_delta,
        })
    }

    fn set_drag(&mut self, button: winit::event::MouseButton, pressed: bool) {
        if !self.active() {
            return;
        }
        self.drag_mode = match (button, pressed) {
            (winit::event::MouseButton::Left, true) => WorldUiLabDragMode::PendingPan,
            (winit::event::MouseButton::Right, true) => WorldUiLabDragMode::Rotate,
            (_, false) => WorldUiLabDragMode::None,
            _ => self.drag_mode,
        };
        self.last_pointer = None;
        self.drag_origin = None;
    }

    fn pointer_moved(
        &mut self,
        pointer: [f32; 2],
        epoch: u64,
        elapsed: Duration,
    ) -> Option<CameraControlSample> {
        let Some(previous) = self.last_pointer.replace(pointer) else {
            self.drag_origin = Some(pointer);
            return None;
        };
        if !self.active() {
            return None;
        }
        let delta = [pointer[0] - previous[0], pointer[1] - previous[1]];
        let origin = self.drag_origin.unwrap_or(previous);
        if self.drag_mode == WorldUiLabDragMode::PendingPan
            && (pointer[0] - origin[0]).hypot(pointer[1] - origin[1]) >= 3.0
        {
            self.drag_mode = WorldUiLabDragMode::Pan;
        }
        match self.drag_mode {
            WorldUiLabDragMode::Rotate => {
                self.yaw += delta[0] * 0.008;
                self.pitch = (self.pitch - delta[1] * 0.008).clamp(-1.5, 1.5);
                self.sample(epoch, elapsed, delta, 0.0)
            }
            WorldUiLabDragMode::Pan => {
                let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
                let scale = self.movement_speed.max(0.35) * 0.01;
                self.camera_position[0] -= (cos_yaw * delta[0] - sin_yaw * delta[1]) * scale;
                self.camera_position[1] += delta[1] * scale;
                self.camera_position[2] -= (sin_yaw * delta[0] + cos_yaw * delta[1]) * scale;
                self.sample(epoch, elapsed, delta, 0.0)
            }
            _ => None,
        }
    }

    fn wheel(&mut self, delta: f32, epoch: u64, elapsed: Duration) -> Option<CameraControlSample> {
        if !self.active() {
            return None;
        }
        self.movement_speed = (if self.movement_speed == 0.0 {
            0.35
        } else {
            self.movement_speed
        } * (1.0 + delta * 0.04))
            .clamp(0.05, 10.0);
        self.sample(epoch, elapsed, [0.0; 2], delta)
    }

    fn set_key(
        &mut self,
        key: KeyCode,
        pressed: bool,
        epoch: u64,
        elapsed: Duration,
    ) -> Option<CameraControlSample> {
        if !self.active() {
            return None;
        }
        match key {
            KeyCode::KeyW => self.forward_held = pressed,
            KeyCode::KeyS => self.backward_held = pressed,
            KeyCode::KeyA => self.left_held = pressed,
            KeyCode::KeyD => self.right_held = pressed,
            KeyCode::KeyQ => self.axes[2] = if pressed { -1.0 } else { 0.0 },
            KeyCode::KeyE => self.axes[2] = if pressed { 1.0 } else { 0.0 },
            _ => return None,
        }
        self.axes[..2].copy_from_slice(&[
            self.right_held as u8 as f32 - self.left_held as u8 as f32,
            self.forward_held as u8 as f32 - self.backward_held as u8 as f32,
        ]);
        self.move_camera();
        self.sample(epoch, elapsed, [0.0; 2], 0.0)
    }

    fn send(&mut self, sample: &CameraControlSample) {
        let Some(socket) = self.socket.as_ref() else {
            return;
        };
        match serde_json::to_vec(sample)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                socket
                    .send(&bytes)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }) {
            Ok(()) => self.last_udp_error = None,
            Err(error) => self.last_udp_error = Some(error.to_string()),
        }
    }

    fn register(&mut self, registration: WorldUiLabCameraRegistration) -> Result<(), &'static str> {
        if !registration.udp_endpoint.ip().is_loopback()
            || registration.session_id.trim().is_empty()
            || registration.provider_epoch == 0
            || registration.camera_id.0.trim().is_empty()
        {
            return Err("invalid_world_ui_lab_camera_registration");
        }
        let socket =
            UdpSocket::bind("127.0.0.1:0").map_err(|_| "world_ui_lab_camera_socket_unavailable")?;
        socket
            .connect(registration.udp_endpoint)
            .map_err(|_| "world_ui_lab_camera_endpoint_unavailable")?;
        self.registration = Some(registration);
        self.socket = Some(socket);
        self.last_udp_error = None;
        Ok(())
    }
}
#[cfg(debug_assertions)]
const MAX_WINDOW_CAPTURE_BYTES: u64 = 256 * 1024 * 1024;

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
        self.append_with_intent(
            interaction_id,
            stage,
            outcome,
            error,
            None,
            semantic_target,
            None,
            fragment_revision,
            composition_revision,
            downstream_request_id,
        );
    }

    fn append_with_intent(
        &mut self,
        interaction_id: InteractionId,
        stage: InteractionTraceStage,
        outcome: InteractionTraceOutcome,
        error: Option<InteractionTraceError>,
        semantic_source_key: Option<String>,
        semantic_target: Option<InteractionSemanticTarget>,
        semantic_intent: Option<String>,
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
            semantic_source_key,
            semantic_target,
            semantic_intent,
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
        self.accepted_waiting_for_composition
            .push_back(interaction_id);
    }

    fn composition_applied(&mut self, composition_revision: Revision) {
        let Some(interaction_id) = self.accepted_waiting_for_composition.pop_front() else {
            return;
        };
        let previous = self
            .records
            .iter()
            .rev()
            .find(|record| record.interaction_id == interaction_id);
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

fn append_drag_interaction_record(
    traces: &Arc<Mutex<InteractionTraceStore>>,
    interaction_id: InteractionId,
    stage: InteractionTraceStage,
    outcome: InteractionTraceOutcome,
    semantic_key: Option<String>,
    semantic_intent: Option<String>,
    fragment_revision: Revision,
    composition_revision: Revision,
    error: Option<InteractionTraceError>,
) {
    if let Ok(mut traces) = traces.lock() {
        let source_stage = matches!(
            stage,
            InteractionTraceStage::DragStarted
                | InteractionTraceStage::DragPreviewMoved
                | InteractionTraceStage::DragReleased
                | InteractionTraceStage::DragCancelled
        );
        let (semantic_source_key, semantic_target) = if source_stage {
            (semantic_key, None)
        } else {
            (
                None,
                semantic_key.map(|node_path| InteractionSemanticTarget { node_path }),
            )
        };
        traces.append_with_intent(
            interaction_id,
            stage,
            outcome,
            error,
            semantic_source_key,
            semantic_target,
            semantic_intent,
            Some(fragment_revision),
            composition_revision,
            None,
        );
    }
}

fn semantic_intent_name(intent: &neon_ui_schema::UiIntent) -> String {
    match intent {
        neon_ui_schema::UiIntent::Invoke { action, .. } => action.clone(),
    }
}

fn record_drag_release_lifecycle(
    traces: &Arc<Mutex<InteractionTraceStore>>,
    interaction_id: &InteractionId,
    source_key: &str,
    fragment_revision: Revision,
    moved: bool,
    resolved: Option<&ui_renderer::UiResolvedDragDrop>,
    composition_revision: Revision,
) {
    match resolved {
        Some(target) => append_drag_interaction_record(
            traces,
            interaction_id.clone(),
            InteractionTraceStage::DropTargetResolved,
            InteractionTraceOutcome::Accepted,
            Some(target.target_key.clone()),
            Some(semantic_intent_name(&target.intent)),
            fragment_revision,
            composition_revision,
            None,
        ),
        None => append_drag_interaction_record(
            traces,
            interaction_id.clone(),
            InteractionTraceStage::DropTargetRejected,
            InteractionTraceOutcome::Rejected,
            None,
            None,
            fragment_revision,
            composition_revision,
            Some(InteractionTraceError {
                code: if moved {
                    "drop_target_not_declared"
                } else {
                    "drag_threshold_not_met"
                }
                .into(),
                message: if moved {
                    "drag release did not resolve a declared semantic drop target"
                } else {
                    "drag release occurred before the declared movement threshold"
                }
                .into(),
            }),
        ),
    }
    append_drag_interaction_record(
        traces,
        interaction_id.clone(),
        InteractionTraceStage::DragReleased,
        InteractionTraceOutcome::Accepted,
        Some(source_key.into()),
        None,
        fragment_revision,
        composition_revision,
        None,
    );
}

fn interaction_matches(record: &InteractionTraceRecord, filters: &InteractionTraceFilters) -> bool {
    filters
        .interaction_id
        .as_ref()
        .is_none_or(|id| &record.interaction_id == id)
        && filters.stage.is_none_or(|stage| record.stage == stage)
        && filters
            .outcome
            .is_none_or(|outcome| record.outcome == outcome)
        && filters
            .semantic_source_key
            .as_ref()
            .is_none_or(|key| record.semantic_source_key.as_ref() == Some(key))
        && filters.semantic_node_path.as_ref().is_none_or(|path| {
            record
                .semantic_target
                .as_ref()
                .is_some_and(|target| &target.node_path == path)
        })
        && filters
            .downstream_request_id
            .as_ref()
            .is_none_or(|id| record.downstream_request_id.as_ref() == Some(id))
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
        let ready = self
            .pending
            .iter()
            .filter(|(_, (scheduled, _))| {
                now.duration_since(*scheduled) >= DATA_GRID_WINDOW_DEBOUNCE
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        ready
            .into_iter()
            .filter_map(|key| self.pending.remove(&key).map(|(_, request)| request))
            .collect()
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|(scheduled, _)| *scheduled + DATA_GRID_WINDOW_DEBOUNCE)
            .min()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UiResourceState {
    Loading,
    Ready,
    Failed,
}

impl UiResourceState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
struct UiResourceRecord {
    asset: AssetRef,
    job_id: String,
    state: UiResourceState,
}

#[derive(Clone, Debug)]
struct ExternalImageRecord {
    source: UiImageSource,
    texture: UiImageTextureRef,
}

const EXTERNAL_IMAGE_ATLAS_WIDTH: u32 = 2048;
const EXTERNAL_IMAGE_ATLAS_PADDING: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalInteractionState {
    Idle,
    Hovered,
    Captured,
    Cancelled,
}

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
struct HitSampleRequest {
    pointer_id: u64,
    sequence: u64,
    composition_revision: Revision,
    target_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalInputState {
    state: LocalInteractionState,
    hover_id: Option<u32>,
    capture_id: Option<u32>,
    last_sequence: std::collections::HashMap<u64, u64>,
    pending: std::collections::HashMap<u64, HitSampleRequest>,
}

impl Default for LocalInputState {
    fn default() -> Self {
        Self {
            state: LocalInteractionState::Idle,
            hover_id: None,
            capture_id: None,
            last_sequence: HashMap::new(),
            pending: HashMap::new(),
        }
    }
}

impl LocalInputState {
    fn request_sample(&mut self, request: HitSampleRequest) {
        self.pending.insert(request.pointer_id, request);
    }

    fn complete_sample(
        &mut self,
        pointer_id: u64,
        current_revision: Revision,
        current_generation: u64,
        hit_id: u32,
    ) -> Result<(), &'static str> {
        let Some(request) = self.pending.remove(&pointer_id) else {
            return Err("interaction_cancelled");
        };
        if request.target_generation != current_generation {
            return Err("hit_target_generation_stale");
        }
        if request.composition_revision != current_revision {
            return Err("composition_revision_stale");
        }
        if self
            .last_sequence
            .get(&request.pointer_id)
            .is_some_and(|last| request.sequence <= *last)
        {
            return Err("input_sequence_stale");
        }
        self.last_sequence
            .insert(request.pointer_id, request.sequence);
        // A completed GPU readback is observational only. Once the local
        // pointer is captured, an older ID frame must never overwrite the
        // captured control or move the interaction back to Hovered/Idle.
        if self.capture_id.is_none() {
            self.set_hover_id((hit_id != RENDER_HIT_NONE).then_some(hit_id));
        }
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
        let Some(hit_id) = self.hover_id else {
            return Err("focus_invalid");
        };
        self.capture_id = Some(hit_id);
        self.state = LocalInteractionState::Captured;
        Ok(())
    }

    fn pointer_up(&mut self, eligible: bool) -> Result<(), &'static str> {
        let captured = self.capture_id.take().is_some();
        self.state = LocalInteractionState::Idle;
        if captured && eligible {
            Ok(())
        } else {
            Err("interaction_cancelled")
        }
    }

    fn cancel(&mut self) {
        self.capture_id = None;
        self.hover_id = None;
        self.pending.clear();
        self.state = LocalInteractionState::Cancelled;
    }

    fn state_name(&self) -> &'static str {
        match self.state {
            LocalInteractionState::Idle => "idle",
            LocalInteractionState::Hovered => "hovered",
            LocalInteractionState::Captured => "captured",
            LocalInteractionState::Cancelled => "cancelled",
        }
    }
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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct LogicalViewportRequirement {
    width: f32,
    height: f32,
}

impl LogicalViewportRequirement {
    fn max(self, other: Self) -> Self {
        Self {
            width: self.width.max(other.width),
            height: self.height.max(other.height),
        }
    }

    fn is_larger_than(self, other: Self) -> bool {
        self.width > other.width || self.height > other.height
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct InitialWindowSizing {
    handled_requirement: LogicalViewportRequirement,
    pending_request: Option<LogicalViewportRequirement>,
}

impl InitialWindowSizing {
    fn observe_composition(
        &mut self,
        requirement: LogicalViewportRequirement,
        current: LogicalViewportRequirement,
    ) -> Option<LogicalViewportRequirement> {
        let introduces_larger_requirement = requirement.is_larger_than(self.handled_requirement);
        self.handled_requirement = self.handled_requirement.max(requirement);
        if !introduces_larger_requirement
            || (current.width >= requirement.width && current.height >= requirement.height)
        {
            return None;
        }

        let requested = current.max(requirement);
        self.pending_request = Some(requested);
        Some(requested)
    }

    fn resize_accepted(&mut self) {
        self.pending_request = None;
    }
}

fn aggregate_root_viewport_requirement(
    fragments: &HashMap<UiFragmentId, UiFragment>,
) -> LogicalViewportRequirement {
    fragments
        .values()
        .filter(|fragment| fragment.root.visible)
        .fold(
            LogicalViewportRequirement::default(),
            |aggregate, fragment| {
                let root = &fragment.root;
                let mut requirement = LogicalViewportRequirement {
                    width: root.bounds.width,
                    height: root.bounds.height,
                };
                if let Some(layout) = root.layout {
                    if let Some([width, height]) = layout.min_size {
                        requirement = requirement.max(LogicalViewportRequirement { width, height });
                    }
                    if let Some([width, height]) = layout.preferred_size {
                        requirement = requirement.max(LogicalViewportRequirement { width, height });
                    }
                }
                aggregate.max(requirement)
            },
        )
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
    pending_composition_acks: VecDeque<(Revision, std::sync::mpsc::Sender<()>)>,
    composition_ack_in_flight: bool,
    ui_endpoint: Option<SocketAddr>,
    projectd_endpoint: Option<SocketAddr>,
    pointer_delivery: Arc<Mutex<Value>>,
    interaction_traces: Arc<Mutex<InteractionTraceStore>>,
    next_interaction_id: u64,
    data_grid_window_requests: LatestDataGridWindowRequests,
    next_data_grid_window_sequence: u64,
    data_grid_window_delivery: Arc<Mutex<Value>>,
    initial_window_sizing: InitialWindowSizing,
    event_proxy: Option<EventLoopProxy<WindowCommand>>,
    eventd_endpoint: Option<SocketAddr>,
    next_file_drop_sequence: u64,
    world_ui_lab_camera: Arc<Mutex<WorldUiLabCameraController>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SemanticDeliveryOutcome {
    Accepted,
    Rejected,
    TransportFailed,
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
    ImageDebugSnapshot {
        completed: std::sync::mpsc::Sender<Value>,
    },
    UploadExternalImage {
        source: UiImageSource,
        completed: std::sync::mpsc::Sender<Result<Value, String>>,
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
    InputDebugActivateTarget {
        semantic_node_path: String,
        completed: std::sync::mpsc::Sender<Result<Value, &'static str>>,
    },
    InputDebugScrollToMax {
        semantic_node_path: String,
        completed: std::sync::mpsc::Sender<Result<Value, &'static str>>,
    },
    InputDebugValueGesture {
        semantic_node_path: String,
        target_fraction: f32,
        completed: std::sync::mpsc::Sender<Result<Value, &'static str>>,
    },
    InputDebugDragGesture {
        source_node_key: String,
        target_node_key: String,
        completed: std::sync::mpsc::Sender<Result<Value, &'static str>>,
    },
    CaptureWorldUiLab {
        artifact_path: PathBuf,
        size: [u32; 2],
        completed: std::sync::mpsc::Sender<Result<Value, String>>,
    },
    CaptureFinalTarget {
        artifact_path: Option<PathBuf>,
        redraw: bool,
        completed: std::sync::mpsc::Sender<Result<Value, String>>,
    },
    FileDrop {
        path: PathBuf,
        completed: std::sync::mpsc::Sender<Result<Value, String>>,
    },
    CompositionDrawCompleted {
        acknowledgements: Vec<std::sync::mpsc::Sender<()>>,
    },
    SemanticDeliveryCompleted {
        pending: Option<PendingLocalPresentationKey>,
        outcome: SemanticDeliveryOutcome,
    },
    DataGridWindowDeliveryCompleted {
        sequence: u64,
        accepted: bool,
    },
    RegisterWorldUiLabCamera {
        registration: WorldUiLabCameraRegistration,
        completed: std::sync::mpsc::Sender<Result<Value, &'static str>>,
    },
    OpenExternalSurface {
        open: RenderSurfaceOpen,
        completed: std::sync::mpsc::Sender<Result<Value, String>>,
    },
    AcquireExternalSurface {
        surface_id: String,
        pid: u32,
        completed: std::sync::mpsc::Sender<Result<Value, String>>,
    },
    ExternalSurfaceFrameSnapshot {
        surface_id: String,
        completed: std::sync::mpsc::Sender<Result<Value, String>>,
    },
    ExternalPointerEvent {
        event: UiPointerEvent,
        completed: std::sync::mpsc::Sender<Result<Value, String>>,
    },
    Shutdown,
}

struct WindowGpu {
    _instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: wgpu::Adapter,
    #[cfg(windows)]
    dx12_adapter: dx12_interop::AdapterInfo,
    #[cfg(windows)]
    external_surfaces: HashMap<String, dx12_interop::SharedSurface>,
    #[cfg(windows)]
    external_id_surfaces: HashMap<String, dx12_interop::SharedSurface>,
    #[cfg(windows)]
    external_handle_tokens: HashMap<String, (String, String)>,
    config: wgpu::SurfaceConfiguration,
    scale_factor: f64,
    #[cfg(debug_assertions)]
    final_target: wgpu::Texture,
    #[cfg(debug_assertions)]
    final_target_view: wgpu::TextureView,
    #[cfg(debug_assertions)]
    final_target_blitter: wgpu::util::TextureBlitter,
    #[cfg(debug_assertions)]
    final_target_valid: bool,
    #[cfg(debug_assertions)]
    final_composition_revision: Revision,
    ui: UiWgpuRenderer,
    world_ui: WorldUiPipeline,
    world_ui_lab_panel: wgpu::TextureView,
    world_ui_lab_surface: wgpu::TextureView,
    _world_ui_lab_depth: wgpu::Texture,
    world_ui_lab_depth: wgpu::TextureView,
    world_ui_lab_fragment: HashMap<UiFragmentId, UiFragment>,
    ai: Option<neon_wgpu_ai::AiEngine>,
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
    control_down: bool,
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
    world_ui_lab_camera: Arc<Mutex<WorldUiLabCameraController>>,
}

impl WindowedRuntime {
    fn begin_os_pointer_interaction(&mut self) -> InteractionId {
        self.next_interaction_id += 1;
        let interaction_id = InteractionId(format!(
            "wgpu-window-{}-{}",
            self.epoch, self.next_interaction_id
        ));
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
                gpu.physical_viewport_size(),
                gpu.logical_viewport_size(),
                gpu.started_at.elapsed().as_secs_f32(),
            );
        }
    }

    fn external_pointer_event(&mut self, event: UiPointerEvent) -> Result<Value, String> {
        let Some(gpu) = self.gpu.as_mut() else {
            return Err("window_gpu_unavailable".into());
        };
        if event.generation != gpu.hit_target_generation {
            return Err("ui_pointer_generation_stale".into());
        }
        let pointer = event.pixel;
        gpu.ui.set_pointer_position(pointer);
        gpu.ui.prepare_interaction(
            &self.fragments,
            gpu.physical_viewport_size(),
            gpu.logical_viewport_size(),
            gpu.started_at.elapsed().as_secs_f32(),
        );
        match event.event_type {
            UiPointerEventType::Enter | UiPointerEventType::Move => {
                gpu.input.set_hover_id(gpu.ui.hit_id_at_pointer());
                gpu.pending_hit_pixel = Some([
                    pointer[0]
                        .max(0.0)
                        .min(gpu.config.width.saturating_sub(1) as f32) as u32,
                    pointer[1]
                        .max(0.0)
                        .min(gpu.config.height.saturating_sub(1) as f32) as u32,
                ]);
                self.redraw_pending = true;
                Ok(json!({"state": "observed"}))
            }
            UiPointerEventType::Down => {
                if !matches!(event.button, Some(neon_ui_schema::UiPointerButton::Primary)) {
                    return Err("ui_pointer_button_unsupported".into());
                }
                // Popup rows are renderer-local overlays, so they do not have
                // ordinary hit IDs. Resolve them before the generic hit path;
                // otherwise a public UiPointerEvent sees an open choice menu as
                // `press_without_semantic_hit` and leaves the host unusable.
                if let Some((binding, value)) = gpu.ui.dropdown_option_at_pointer() {
                    gpu.input.set_hover_id(Some(0));
                    gpu.input
                        .pointer_down()
                        .map_err(|_| "choice_option_capture_failed".to_owned())?;
                    gpu.captured_binding = Some(binding);
                    gpu.pending_control_value = Some(value);
                    gpu.ui.close_dropdown();
                    self.redraw_pending = true;
                    return Ok(json!({"state": "choice_option_captured"}));
                }
                if gpu.ui.dismiss_dropdown_at_pointer() {
                    gpu.captured_binding = None;
                    gpu.pending_control_value = None;
                    gpu.input.cancel();
                    self.redraw_pending = true;
                    return Ok(json!({"state": "choice_popup_dismissed"}));
                }
                if gpu.ui.toggle_dropdown_at_pointer() {
                    gpu.captured_binding = None;
                    gpu.pending_control_value = None;
                    gpu.input.cancel();
                    self.redraw_pending = true;
                    return Ok(json!({"state": "choice_popup_toggled"}));
                }
                gpu.input.set_hover_id(gpu.ui.hit_id_at_pointer());
                gpu.input
                    .pointer_down()
                    .map_err(|_| "press_without_semantic_hit".to_owned())?;
                gpu.captured_binding = gpu
                    .input
                    .capture_id
                    .and_then(|hit_id| gpu.ui.hit_binding(hit_id));
                if gpu.captured_binding.is_none() {
                    let _ = gpu.input.pointer_up(false);
                    return Err("press_without_semantic_binding".into());
                }
                if let Some(binding) = gpu.captured_binding.clone() {
                    gpu.ui.press_hovered(gpu.started_at.elapsed().as_secs_f32());
                    gpu.pending_control_value = binding.control_value.clone();
                }
                self.redraw_pending = true;
                Ok(json!({"state": "captured"}))
            }
            UiPointerEventType::Up => {
                let Some(released) = release_captured_binding(gpu) else {
                    return Ok(json!({"state": "released"}));
                };
                let Some(intent) = released.binding.intent.clone() else {
                    return Ok(json!({"state": "released"}));
                };
                let semantic_event = UiSemanticEvent {
                    event: UiSemanticEventType::PointerClick,
                    event_id: format!("wgpu-pointer-click-{}", released.sequence),
                    renderer_epoch: self.epoch,
                    composition_revision: self.applied_composition_revision,
                    fragment: released.binding.fragment,
                    intent,
                    pointer: Some(neon_ui_schema::UiPointerMetadata {
                        id: event.pointer_id,
                        sequence: released.sequence,
                    }),
                    focus: None,
                    data_grid_cell: released.binding.data_grid_cell,
                    text: None,
                    control_value: released.control_value,
                    drag_drop: None,
                };
                self.redraw_pending = true;
                Ok(json!({"semantic_event": semantic_event}))
            }
            UiPointerEventType::Wheel => {
                let delta = match event.delta_mode {
                    neon_ui_schema::UiPointerDeltaMode::Line => {
                        [event.delta[0] * 24.0, event.delta[1] * 24.0]
                    }
                    _ => event.delta,
                };
                if gpu.ui.scroll_wheel_at_pointer(delta) {
                    self.redraw_pending = true;
                }
                Ok(json!({"state": "scrolled"}))
            }
            UiPointerEventType::Leave | UiPointerEventType::Cancel => {
                gpu.captured_binding = None;
                gpu.pending_control_value = None;
                gpu.input.cancel();
                Ok(json!({"state": "cancelled"}))
            }
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
            pending_composition_acks: VecDeque::new(),
            composition_ack_in_flight: false,
            ui_endpoint: None,
            projectd_endpoint: None,
            pointer_delivery: Arc::new(Mutex::new(json!({"state": "none"}))),
            interaction_traces: Arc::new(Mutex::new(InteractionTraceStore::new())),
            next_interaction_id: 0,
            data_grid_window_requests: LatestDataGridWindowRequests::default(),
            next_data_grid_window_sequence: 0,
            data_grid_window_delivery: Arc::new(Mutex::new(json!({"state": "idle"}))),
            initial_window_sizing: InitialWindowSizing::default(),
            event_proxy: None,
            eventd_endpoint: None,
            next_file_drop_sequence: 0,
            world_ui_lab_camera: Arc::new(Mutex::new(WorldUiLabCameraController::default())),
        }
    }

    pub fn run(epoch: u64) -> Result<(), String> {
        Self::run_with_server(epoch, None, None, None, None, true, false)
    }

    pub fn run_server(
        epoch: u64,
        endpoint: SocketAddr,
        ui_endpoint: Option<SocketAddr>,
        projectd_endpoint: Option<SocketAddr>,
        enable_world_ui_lab_camera: bool,
    ) -> Result<(), String> {
        Self::run_server_with_eventd(
            epoch,
            endpoint,
            ui_endpoint,
            projectd_endpoint,
            None,
            enable_world_ui_lab_camera,
        )
    }

    pub fn run_server_with_eventd(
        epoch: u64,
        endpoint: SocketAddr,
        ui_endpoint: Option<SocketAddr>,
        projectd_endpoint: Option<SocketAddr>,
        eventd_endpoint: Option<SocketAddr>,
        enable_world_ui_lab_camera: bool,
    ) -> Result<(), String> {
        Self::run_with_server(
            epoch,
            Some(endpoint),
            ui_endpoint,
            projectd_endpoint,
            eventd_endpoint,
            false,
            enable_world_ui_lab_camera,
        )
    }

    fn run_with_server(
        epoch: u64,
        endpoint: Option<SocketAddr>,
        ui_endpoint: Option<SocketAddr>,
        projectd_endpoint: Option<SocketAddr>,
        eventd_endpoint: Option<SocketAddr>,
        demo: bool,
        enable_world_ui_lab_camera: bool,
    ) -> Result<(), String> {
        let event_loop = EventLoop::<WindowCommand>::with_user_event()
            .build()
            .map_err(|error| format!("create event loop: {error}"))?;
        let proxy = event_loop.create_proxy();
        let mut runtime = Self::new(epoch);
        runtime.event_proxy = Some(proxy.clone());
        runtime.ui_endpoint = ui_endpoint;
        runtime.projectd_endpoint = projectd_endpoint;
        runtime.eventd_endpoint = eventd_endpoint;
        runtime
            .world_ui_lab_camera
            .lock()
            .expect("camera controller lock")
            .enabled = enable_world_ui_lab_camera;
        if demo {
            runtime.fragments = runtime.demo_fragments();
            runtime.applied_composition_revision = Revision(1);
        }
        if let Some(endpoint) = endpoint {
            let interaction_traces = runtime.interaction_traces.clone();
            spawn_window_server(
                epoch,
                endpoint,
                proxy,
                interaction_traces,
                runtime.world_ui_lab_camera.clone(),
            );
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
                    .with_title(format!("Neon3 - WGPU Runtime (epoch {}) - Ctrl+O: Open image", self.epoch))
                    .with_inner_size(PhysicalSize::new(1280, 800)),
            )
            .map_err(|error| format!("create window: {error}"))?;
        #[cfg(windows)]
        if let Err(error) = enable_explorer_file_drop(&window) {
            eprintln!("[neon-wgpu-runtime] Explorer file-drop enablement failed: {error}");
        }
        // External host interop (DX12 shared texture/fence) requires the DX12
        // backend; forcing it avoids wgpu silently selecting Vulkan/GL and then
        // failing `as_hal::<Dx12>()` at surface creation.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
                event_loop.owned_display_handle(),
            ))
        });
        let gpu = WindowGpu::new(&window, instance, self.world_ui_lab_camera.clone())?;
        if let Ok(mut camera) = self.world_ui_lab_camera.lock() {
            camera.window_focused = window.has_focus();
        }
        self.window = Some(window);
        self.gpu = Some(gpu);
        if let Some(endpoint) = self.projectd_endpoint {
            self.preload_fixture_image(endpoint)?;
        }
        self.request_scripted_initial_size();
        self.redraw_pending = true;
        Ok(())
    }

    fn publish_file_drop(&mut self, path: PathBuf) {
        let Some(endpoint) = self.eventd_endpoint else {
            return;
        };
        self.next_file_drop_sequence = self.next_file_drop_sequence.saturating_add(1);
        let drop_sequence = self.next_file_drop_sequence;
        let source_path = path.to_string_lossy().into_owned();
        let file_name = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let media_type = match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "tga" => "image/x-tga",
            _ => "application/octet-stream",
        };
        let is_image = media_type.starts_with("image/");
        let frame_sequence = self.gpu.as_ref().map_or(0, |gpu| gpu.frame_count);
        let payload = UiFileDropPayload {
            drop_sequence,
            source_path,
            file_name,
            extension,
            media_type: media_type.into(),
            is_image,
            renderer_epoch: self.epoch,
            frame_sequence,
        };
        let publisher = ClientIdentity {
            kind: ClientKind::WgpuRuntime,
            instance_id: format!("window-{}", self.epoch),
            pid: std::process::id(),
            origin: "neon-wgpu-runtime".into(),
        };
        thread::spawn(move || {
            let request_id = RequestId(format!(
                "wgpu-file-drop-{}-{}",
                payload.renderer_epoch, payload.drop_sequence
            ));
            let publish = neon_protocol::EventPublish {
                protocol: "neon3.event".into(),
                version: PROTOCOL_VERSION,
                request_id,
                publisher,
                name: "ui.file_drop.accepted".into(),
                schema_version: 1,
                payload: serde_json::to_value(&payload).expect("file drop payload serializes"),
                idempotency_key: Some(format!(
                    "ui-file-drop:{}:{}",
                    payload.renderer_epoch, payload.drop_sequence
                )),
            };
            match EventClient::connect(endpoint).and_then(|mut client| client.publish(&publish)) {
                Ok(ack) if ack.status == neon_protocol::EventAckStatus::Accepted => {}
                Ok(ack) => eprintln!(
                    "[neon-wgpu-runtime] file drop event rejected: {:?}",
                    ack.error
                ),
                Err(error) => {
                    eprintln!("[neon-wgpu-runtime] file drop event publish failed: {error}")
                }
            }
        });
    }

    fn preload_fixture_image(&mut self, endpoint: SocketAddr) -> Result<(), String> {
        let asset = AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 81,
            revision: Revision(5),
            kind: "image".into(),
        };
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("gallery-image-preload".into()),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: format!("window-{}", self.epoch),
                pid: std::process::id(),
                origin: "neon-wgpu-runtime".into(),
            },
            target: ServiceName("projectd".into()),
            method: "asset.get_bytes".into(),
            params: json!(asset.clone()),
            expected_revision: Some(asset.revision),
            idempotency_key: Some("gallery-image-preload".into()),
        };
        let response = RpcClient::connect(endpoint)
            .and_then(|mut client| client.call(&request))
            .map_err(|error| format!("fixture image owner request failed: {error}"))?;
        if response.status != RpcStatus::Accepted {
            return Err(format!(
                "fixture image owner rejected preload: {:?}",
                response.error
            ));
        }
        let content: AssetBytes = serde_json::from_value(
            response
                .result
                .ok_or_else(|| "fixture image owner omitted asset bytes".to_string())?,
        )
        .map_err(|error| format!("fixture image asset bytes were invalid: {error}"))?;
        let gpu = self.gpu.as_mut().ok_or("GPU is not initialized")?;
        gpu.ui
            .preload_image(&gpu.device, &gpu.queue, &content)
            .map_err(|error| format!("fixture image preload failed: {error}"))
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.initial_window_sizing.resize_accepted();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.config.width = size.width;
            gpu.config.height = size.height;
            gpu.surface.configure(&gpu.device, &gpu.config);
            #[cfg(debug_assertions)]
            {
                let (final_target, final_target_view) =
                    create_final_target(&gpu.device, &gpu.config);
                gpu.final_target = final_target;
                gpu.final_target_view = final_target_view;
                gpu.final_target_valid = false;
            }
            let (target, view) = create_hit_target(&gpu.device, size);
            gpu.hit_target = target;
            gpu.hit_target_view = view;
            gpu.hit_target_generation += 1;
            gpu.hit_target_dirty = true;
            self.redraw_pending = true;
        }
    }

    fn request_scripted_initial_size(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let logical_size = gpu.logical_viewport_size();
        let current = LogicalViewportRequirement {
            width: logical_size[0],
            height: logical_size[1],
        };
        let requirement = aggregate_root_viewport_requirement(&self.fragments);
        let Some(requested) = self
            .initial_window_sizing
            .observe_composition(requirement, current)
        else {
            return;
        };
        let accepted = self.window.as_ref().and_then(|window| {
            window.request_inner_size(LogicalSize::new(
                f64::from(requested.width),
                f64::from(requested.height),
            ))
        });
        if let Some(accepted) = accepted {
            self.resize(accepted);
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
                    let _ = gpu.input.complete_sample(
                        0,
                        self.applied_composition_revision,
                        gpu.hit_target_generation,
                        hit_id,
                    );
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
        let surface_view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        #[cfg(debug_assertions)]
        let composition_view = &gpu.final_target_view;
        #[cfg(not(debug_assertions))]
        let composition_view = &surface_view;
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neon3-final-composition"),
            });
        let (lab_active, camera_state) = gpu
            .world_ui_lab_camera
            .lock()
            .map(|camera| (camera.active(), camera.state()))
            .unwrap_or((
                false,
                WorldUiCameraState {
                    position: [0.0; 3],
                    yaw: 0.0,
                    pitch: 0.0,
                    vertical_fov: 35.0f32.to_radians(),
                },
            ));
        if lab_active {
            // The lab preview is demand-driven. When no external lab surface
            // owns focus, skip its panel, scene, and depth passes entirely so
            // pointer interaction on the main gallery is not GPU-bound by an
            // inactive diagnostic preview.
            gpu.world_ui_lab_fragment = world_ui_lab_fragment();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-world-ui-lab-panel-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &gpu.world_ui_lab_panel,
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
            gpu.ui.draw(
                &gpu.device,
                &gpu.queue,
                &mut pass,
                &gpu.world_ui_lab_fragment,
                WORLD_UI_LAB_PANEL_SIZE,
                [
                    WORLD_UI_LAB_LOGICAL_SIZE[0] as f32,
                    WORLD_UI_LAB_LOGICAL_SIZE[1] as f32,
                ],
                gpu.started_at.elapsed().as_secs_f32(),
                UiDrawMode::All,
            );
            drop(pass);
            gpu.world_ui.render_lab_scene(
                &gpu.device,
                &gpu.queue,
                &mut encoder,
                &gpu.world_ui_lab_surface,
                &gpu.world_ui_lab_depth,
                WORLD_UI_LAB_PREVIEW_SIZE,
                &gpu.world_ui_lab_panel,
                world_ui_lab_camera(WORLD_UI_LAB_PREVIEW_SIZE, camera_state),
            )?;
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-final-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: composition_view,
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
                gpu.physical_viewport_size(),
                gpu.logical_viewport_size(),
                gpu.started_at.elapsed().as_secs_f32(),
                UiDrawMode::All,
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
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            gpu.ui.draw_hit_id(
                &gpu.device,
                &gpu.queue,
                &mut pass,
                &self.fragments,
                gpu.physical_viewport_size(),
                gpu.logical_viewport_size(),
                gpu.started_at.elapsed().as_secs_f32(),
            );
            gpu.hit_target_dirty = false;
        }
        gpu.encode_external_surfaces(&mut encoder, &self.fragments)?;
        #[cfg(debug_assertions)]
        gpu.final_target_blitter.copy(
            &gpu.device,
            &mut encoder,
            &gpu.final_target_view,
            &surface_view,
        );
        let queued_readback = gpu.pending_hit_pixel.take().and_then(|pixel| {
            gpu.ui
                .enqueue_hit_readback(&mut encoder, &gpu.hit_target, pixel)
                .map(|slot| (slot, pixel))
        });
        gpu.queue.submit(Some(encoder.finish()));
        let submitted_at = Instant::now();
        if let Some((slot, _)) = queued_readback {
            gpu.next_input_sequence += 1;
            gpu.input.request_sample(HitSampleRequest {
                pointer_id: 0,
                sequence: gpu.next_input_sequence,
                composition_revision: self.applied_composition_revision,
                target_generation: gpu.hit_target_generation,
            });
            if gpu.ui.begin_hit_readback_mapping(slot) {
                gpu.pending_hit_slot = Some((slot, gpu.next_input_sequence));
            }
        }
        gpu.queue.present(surface_texture);
        let now = Instant::now();
        let frame_gap_ms = now.duration_since(gpu.last_present).as_secs_f32() * 1000.0;
        gpu.longest_frame_gap_ms = gpu.longest_frame_gap_ms.max(frame_gap_ms);
        gpu.last_present = now;
        gpu.frame_count += 1;
        #[cfg(debug_assertions)]
        {
            gpu.final_target_valid = true;
            gpu.final_composition_revision = self.applied_composition_revision;
        }
        if self.initial_window_sizing.pending_request.is_none()
            && !self.composition_ack_in_flight
            && let Some(proxy) = self.event_proxy.clone()
        {
            let mut acknowledgements = Vec::new();
            while self
                .pending_composition_acks
                .front()
                .is_some_and(|(revision, _)| *revision <= self.applied_composition_revision)
            {
                if let Some((_, acknowledgement)) = self.pending_composition_acks.pop_front() {
                    acknowledgements.push(acknowledgement);
                }
            }
            if !acknowledgements.is_empty() {
                self.composition_ack_in_flight = true;
                gpu.queue.on_submitted_work_done(move || {
                    let _ = proxy
                        .send_event(WindowCommand::CompositionDrawCompleted { acknowledgements });
                });
            }
        }
        self.redraw_pending = false;
        self.animation_active = gpu
            .ui
            .has_active_animation(gpu.started_at.elapsed().as_secs_f32());
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

    #[cfg(debug_assertions)]
    fn capture_final_target(&mut self, artifact_path: Option<PathBuf>) -> Result<Value, String> {
        let gpu = self
            .gpu
            .as_mut()
            .ok_or_else(|| "window GPU is unavailable".to_owned())?;
        if !gpu.final_target_valid {
            return Err("the window has not produced a capturable final frame".into());
        }
        let physical_size = gpu.physical_viewport_size();
        let logical_viewport = gpu.logical_viewport_size();
        let rgba = gpu.read_final_target_rgba()?;
        let checksum = fnv1a64(&rgba);
        let artifact_path = match artifact_path {
            Some(path) => path,
            None => {
                default_capture_path(self.epoch, gpu.frame_count, gpu.final_composition_revision)?
            }
        };
        let artifact_path = write_capture_png(&artifact_path, physical_size, &rgba)?;
        Ok(json!({
            "target": UI_COLOR_TARGET,
            "source": "window_final_composition",
            "format": texture_format_name(gpu.config.format),
            "color_space": if gpu.config.format.is_srgb() { "srgb" } else { "linear" },
            "physical_size": {"width": physical_size[0], "height": physical_size[1]},
            "logical_viewport": {"width": logical_viewport[0], "height": logical_viewport[1]},
            "scale_factor": gpu.scale_factor,
            "frame_sequence": gpu.frame_count,
            "composition_revision": gpu.final_composition_revision.0,
            "checksum": {"algorithm": "fnv1a64", "value": format!("{checksum:016x}")},
            "rgba_bytes": rgba.len(),
            "artifact_path": artifact_path.to_string_lossy(),
        }))
    }

    #[cfg(not(debug_assertions))]
    fn capture_final_target(&mut self, _: Option<PathBuf>) -> Result<Value, String> {
        Err("window capture is only available in debug builds".into())
    }

    #[cfg(debug_assertions)]
    fn capture_world_ui_lab(
        &mut self,
        artifact_path: PathBuf,
        size: [u32; 2],
    ) -> Result<Value, String> {
        let rgba = {
            let gpu = self
                .gpu
                .as_mut()
                .ok_or_else(|| "window GPU is unavailable".to_owned())?;
            gpu.render_world_ui_lab_panel();
            let camera_state = gpu
                .world_ui_lab_camera
                .lock()
                .map(|camera| camera.state())
                .unwrap_or(WorldUiCameraState {
                    position: [0.0; 3],
                    yaw: 0.0,
                    pitch: 0.0,
                    vertical_fov: 35.0f32.to_radians(),
                });
            gpu.world_ui.capture_lab(
                &gpu.device,
                &gpu.queue,
                size,
                &gpu.world_ui_lab_panel,
                world_ui_lab_camera(size, camera_state),
            )?
        };
        let artifact_path = write_capture_png(&artifact_path, size, &rgba)?;
        Ok(json!({
            "target": "world-ui-lab",
            "format": "rgba8unorm",
            "size": {"width": size[0], "height": size[1]},
            "rgba_bytes": rgba.len(),
            "artifact_path": artifact_path.to_string_lossy(),
        }))
    }

    #[cfg(not(debug_assertions))]
    fn capture_world_ui_lab(&mut self, _: PathBuf, _: [u32; 2]) -> Result<Value, String> {
        Err("world UI lab capture is only available in debug builds".into())
    }

    fn apply_fragments(
        &mut self,
        composition_revision: Revision,
        fragments: HashMap<UiFragmentId, UiFragment>,
    ) -> bool {
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
            gpu.ui
                .reconcile_pending_local_presentations(&self.fragments);
            gpu.hit_target_dirty = true;
        }
        true
    }

    fn schedule_data_grid_window_requests(&mut self) -> usize {
        self.schedule_data_grid_window_requests_for(None)
    }

    fn schedule_data_grid_window_requests_for(&mut self, grid_path: Option<&str>) -> usize {
        let requests = self
            .gpu
            .as_mut()
            .map(|gpu| {
                gpu.ui.data_grid_window_requests(
                    &self.fragments,
                    self.epoch,
                    self.applied_composition_revision,
                    &mut self.next_data_grid_window_sequence,
                    grid_path,
                    grid_path.is_some(),
                )
            })
            .unwrap_or_default();
        let now = Instant::now();
        let count = requests.len();
        for request in requests {
            self.data_grid_window_requests.schedule(request, now);
        }
        count
    }

    fn dispatch_ready_data_grid_window_requests(&mut self) {
        let Some(endpoint) = self.ui_endpoint else {
            return;
        };
        for request in self.data_grid_window_requests.take_ready(Instant::now()) {
            forward_data_grid_window_request(
                endpoint,
                request,
                self.data_grid_window_delivery.clone(),
                self.event_proxy.clone(),
            );
        }
    }

    fn dispatch_data_grid_window_requests_for_test(&mut self) {
        let Some(endpoint) = self.ui_endpoint else {
            return;
        };
        for request in self
            .data_grid_window_requests
            .take_ready(Instant::now() + DATA_GRID_WINDOW_DEBOUNCE)
        {
            forward_data_grid_window_request(
                endpoint,
                request,
                self.data_grid_window_delivery.clone(),
                self.event_proxy.clone(),
            );
        }
    }

    fn needs_redraw(&self) -> bool {
        self.redraw_pending
            || self.animation_active
            || self.composition_ack_in_flight
            || self
                .gpu
                .as_ref()
                .is_some_and(|gpu| gpu.pending_hit_slot.is_some())
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
        let physical_size = gpu.physical_viewport_size();
        let logical_size = gpu.logical_viewport_size();
        let requirement = aggregate_root_viewport_requirement(&self.fragments);
        let active_drag = gpu
            .ui
            .active_drag_semantic_source()
            .map(|(source_key, fragment)| {
                let candidate = gpu
                    .ui
                    .current_drag_drop_target(&self.fragments)
                    .map(|target| {
                        json!({
                            "target_key": target.target_key,
                            "intent": semantic_intent_name(&target.intent),
                        })
                    });
                json!({
                    "source_key": source_key,
                    "fragment_revision": fragment.revision.0,
                    "current_candidate_target": candidate,
                })
            });
        json!({
            "state": gpu.input.state_name(),
            "world_ui_lab_camera": self.world_ui_lab_camera.lock().ok().map(|camera| world_ui_lab_camera_status(&camera)),
            "applied_composition_revision": self.applied_composition_revision.0,
            "viewport": {
                "physical_size": {"width": physical_size[0], "height": physical_size[1]},
                "logical_size": {"width": logical_size[0], "height": logical_size[1]},
                "scale_factor": gpu.scale_factor,
            },
            "scripted_initial_sizing": {
                "aggregate_requirement": {"width": requirement.width, "height": requirement.height},
                "handled_requirement": {
                    "width": self.initial_window_sizing.handled_requirement.width,
                    "height": self.initial_window_sizing.handled_requirement.height,
                },
                "pending_request": self.initial_window_sizing.pending_request.map(|pending| json!({
                    "width": pending.width,
                    "height": pending.height,
                })),
            },
            "hovered_node_path": node_path(gpu.input.hover_id),
            "captured_node_path": node_path(gpu.input.capture_id),
            "last_pointer_outcome": gpu.last_pointer_outcome,
            "last_pointer_node_path": gpu.last_pointer_node_path,
            "active_drag": active_drag,
            "pending_hit_readback": gpu.pending_hit_slot.is_some(),
            "pending_hit_pixel": gpu.pending_hit_pixel.is_some(),
            "layout_counters": {
                "window_ui": gpu.ui.layout_counters(),
            },
            "layout": gpu.ui.layout_snapshot(),
            "dropdown": gpu.ui.dropdown_debug_snapshot(),
            "text_input": gpu.ui.text_input_debug_snapshot(),
            "active_transitions": gpu
                .ui
                .active_transition_debug_snapshot(gpu.started_at.elapsed().as_secs_f32()),
            "pointer_delivery": self.pointer_delivery.lock().ok().map(|state| state.clone()),
            "data_grid_window_delivery": self.data_grid_window_delivery.lock().ok().map(|state| state.clone()),
        })
    }

    fn input_debug_probe(
        &mut self,
        logical_position: Option<[f64; 2]>,
        physical_position: Option<[f64; 2]>,
    ) -> Value {
        let Some(gpu) = self.gpu.as_mut() else {
            return json!({"state": "uninitialized"});
        };
        let scale_factor = self
            .window
            .as_ref()
            .map(Window::scale_factor)
            .unwrap_or(1.0);
        let physical_position = physical_position
            .or_else(|| {
                logical_position
                    .map(|position| [position[0] * scale_factor, position[1] * scale_factor])
            })
            .expect("probe position is validated before dispatch");
        let logical_position = logical_position.unwrap_or([
            physical_position[0] / scale_factor,
            physical_position[1] / scale_factor,
        ]);
        let physical_position = [physical_position[0] as f32, physical_position[1] as f32];
        gpu.ui
            .set_pointer_position([logical_position[0] as f32, logical_position[1] as f32]);
        gpu.ui.prepare_interaction(
            &self.fragments,
            gpu.physical_viewport_size(),
            gpu.logical_viewport_size(),
            gpu.started_at.elapsed().as_secs_f32(),
        );
        let mut probe = gpu.ui.pointer_probe_snapshot();
        probe["pointer"] = json!({
            "logical": {"x": logical_position[0], "y": logical_position[1]},
            "physical": {"x": physical_position[0], "y": physical_position[1]},
        });
        let x = physical_position[0]
            .max(0.0)
            .min(gpu.config.width.saturating_sub(1) as f32) as u32;
        let y = physical_position[1]
            .max(0.0)
            .min(gpu.config.height.saturating_sub(1) as f32) as u32;
        gpu.pending_hit_pixel = Some([x, y]);
        probe["gpu_hit_readback"] = json!({
            "status": "queued",
            "available": true,
            "pending": gpu.pending_hit_slot.is_some() || gpu.pending_hit_pixel.is_some(),
        });
        probe["last_pointer_delivery"] = self
            .pointer_delivery
            .lock()
            .ok()
            .map(|state| state.clone())
            .unwrap_or_else(|| json!({"state": "unavailable"}));
        self.redraw_pending = true;
        probe
    }

    /// Debug-only automation entry point. The point is resolved through the
    /// same prepared CPU binding and capture state used by pointer release;
    /// renderer hit identifiers never leave this process.
    fn input_debug_activate(&mut self, logical_position: [f64; 2]) -> Result<Value, &'static str> {
        let endpoint = self.ui_endpoint.ok_or("ui_host_unavailable")?;
        let proxy = self
            .event_proxy
            .clone()
            .ok_or("window_event_proxy_unavailable")?;
        let Some(gpu) = self.gpu.as_mut() else {
            return Err("window_gpu_unavailable");
        };
        gpu.ui
            .set_pointer_position([logical_position[0] as f32, logical_position[1] as f32]);
        gpu.ui.prepare_interaction(
            &self.fragments,
            gpu.physical_viewport_size(),
            gpu.logical_viewport_size(),
            gpu.started_at.elapsed().as_secs_f32(),
        );
        let Some(hit_id) = gpu.ui.hit_id_at_pointer() else {
            return Err("press_without_semantic_hit");
        };
        gpu.input.set_hover_id(Some(hit_id));
        gpu.input
            .pointer_down()
            .map_err(|_| "pointer_down_rejected")?;
        gpu.captured_binding = gpu.input.capture_id.and_then(|id| gpu.ui.hit_binding(id));
        gpu.pending_control_value = gpu
            .captured_binding
            .as_ref()
            .and_then(|binding| binding.control_value.clone());
        let Some(binding) = gpu.captured_binding.as_ref() else {
            return Err("press_without_semantic_binding");
        };
        if binding.text_input.is_some()
            || (gpu.ui.requires_value_gesture(binding) && !gpu.ui.begin_value_gesture(binding))
        {
            gpu.captured_binding = None;
            gpu.input.cancel();
            return Err("debug_activation_not_supported_for_control");
        }
        let released = release_captured_binding(gpu).ok_or("interaction_cancelled")?;
        let node_path = diagnostic_node_path(&released.binding);
        gpu.last_pointer_node_path = node_path.clone();
        gpu.last_pointer_outcome = if released.binding.intent.is_some() {
            "semantic_event_forwarded".into()
        } else {
            "release_without_semantic_binding".into()
        };
        let interaction_id = InteractionId(format!("debug-window-input-{}", released.sequence));
        append_interaction_record(
            &self.interaction_traces,
            interaction_id.clone(),
            InteractionTraceStage::HitCaptureResolved,
            InteractionTraceOutcome::Accepted,
            None,
            Some(&released.binding),
            self.applied_composition_revision,
        );
        let pending = if released.binding.intent.is_some() {
            released.local_presentation.map(|presentation| {
                gpu.ui.retain_local_presentation(
                    released.sequence,
                    &released.binding.fragment,
                    presentation,
                )
            })
        } else {
            if let Some(presentation) = released.local_presentation.as_ref() {
                gpu.ui.rollback_local_presentation(presentation);
            }
            None
        };
        forward_pointer_click(
            endpoint,
            self.epoch,
            self.applied_composition_revision,
            released.sequence,
            Some(interaction_id.clone()),
            released.binding,
            released.control_value,
            self.pointer_delivery.clone(),
            self.interaction_traces.clone(),
            Some(proxy),
            pending,
        );
        Ok(
            json!({"state": "forwarded", "sequence": released.sequence, "node_path": node_path, "interaction_id": interaction_id}),
        )
    }

    fn input_debug_activate_target(
        &mut self,
        semantic_node_path: String,
    ) -> Result<Value, &'static str> {
        let binding = {
            let gpu = self.gpu.as_mut().ok_or("window_gpu_unavailable")?;
            gpu.ui.prepare_interaction(
                &self.fragments,
                gpu.physical_viewport_size(),
                gpu.logical_viewport_size(),
                gpu.started_at.elapsed().as_secs_f32(),
            );
            gpu.ui.debug_semantic_target_binding(&semantic_node_path)?
        };
        let endpoint = self.ui_endpoint.ok_or("ui_host_unavailable")?;
        let proxy = self
            .event_proxy
            .clone()
            .ok_or("window_event_proxy_unavailable")?;
        let gpu = self.gpu.as_mut().ok_or("window_gpu_unavailable")?;
        gpu.next_semantic_sequence += 1;
        let sequence = gpu.next_semantic_sequence;
        let node_path = diagnostic_node_path(&binding);
        let interaction_id = InteractionId(format!("debug-window-input-{sequence}"));
        append_interaction_record(
            &self.interaction_traces,
            interaction_id.clone(),
            InteractionTraceStage::HitCaptureResolved,
            InteractionTraceOutcome::Accepted,
            None,
            Some(&binding),
            self.applied_composition_revision,
        );
        forward_pointer_click(
            endpoint,
            self.epoch,
            self.applied_composition_revision,
            sequence,
            Some(interaction_id.clone()),
            binding.clone(),
            binding.control_value.clone(),
            self.pointer_delivery.clone(),
            self.interaction_traces.clone(),
            Some(proxy),
            None,
        );
        Ok(
            json!({"state": "forwarded", "sequence": sequence, "node_path": node_path, "interaction_id": interaction_id}),
        )
    }

    fn input_debug_scroll_to_max(
        &mut self,
        semantic_node_path: String,
    ) -> Result<Value, &'static str> {
        let result = self
            .gpu
            .as_mut()
            .ok_or("window_gpu_unavailable")?
            .ui
            .debug_scroll_to_max(&semantic_node_path)?;
        self.redraw_pending = true;
        let scheduled = self.schedule_data_grid_window_requests();
        self.dispatch_data_grid_window_requests_for_test();
        Ok(json!({"scroll": result, "scheduled_window_requests": scheduled}))
    }

    fn input_debug_value_gesture(
        &mut self,
        semantic_node_path: String,
        target_fraction: f32,
    ) -> Result<Value, &'static str> {
        let endpoint = self.ui_endpoint.ok_or("ui_host_unavailable")?;
        let proxy = self
            .event_proxy
            .clone()
            .ok_or("window_event_proxy_unavailable")?;
        let gpu = self.gpu.as_mut().ok_or("window_gpu_unavailable")?;
        gpu.ui.prepare_interaction(
            &self.fragments,
            gpu.physical_viewport_size(),
            gpu.logical_viewport_size(),
            gpu.started_at.elapsed().as_secs_f32(),
        );
        let (start, end) = gpu
            .ui
            .debug_value_gesture_points(&semantic_node_path, target_fraction)?;
        gpu.ui.set_pointer_position(start);
        let hit_id = gpu
            .ui
            .hit_id_at_pointer()
            .ok_or("press_without_semantic_hit")?;
        gpu.input.set_hover_id(Some(hit_id));
        gpu.input
            .pointer_down()
            .map_err(|_| "pointer_down_rejected")?;
        gpu.captured_binding = gpu.input.capture_id.and_then(|id| gpu.ui.hit_binding(id));
        let binding = gpu
            .captured_binding
            .clone()
            .ok_or("press_without_semantic_binding")?;
        if binding.node_path != semantic_node_path {
            gpu.captured_binding = None;
            gpu.input.cancel();
            return Err("gesture_target_hit_mismatch");
        }
        gpu.pending_control_value = binding.control_value.clone();
        if !gpu.ui.begin_value_gesture(&binding) {
            gpu.captured_binding = None;
            gpu.input.cancel();
            return Err("value_gesture_not_started");
        }
        gpu.ui.set_pointer_position(end);
        if !gpu.ui.update_value_gesture() {
            gpu.ui.cancel_value_gesture();
            gpu.captured_binding = None;
            gpu.input.cancel();
            return Err("value_gesture_not_updated");
        }
        let released = release_captured_binding(gpu).ok_or("interaction_cancelled")?;
        let interaction_id = InteractionId(format!("debug-window-input-{}", released.sequence));
        append_interaction_record(
            &self.interaction_traces,
            interaction_id.clone(),
            InteractionTraceStage::HitCaptureResolved,
            InteractionTraceOutcome::Accepted,
            None,
            Some(&released.binding),
            self.applied_composition_revision,
        );
        gpu.last_pointer_node_path = diagnostic_node_path(&released.binding);
        gpu.last_pointer_outcome = "semantic_event_forwarded".into();
        self.redraw_pending = true;
        let pending = if released.binding.intent.is_some() {
            released.local_presentation.map(|presentation| {
                gpu.ui.retain_local_presentation(
                    released.sequence,
                    &released.binding.fragment,
                    presentation,
                )
            })
        } else {
            if let Some(presentation) = released.local_presentation.as_ref() {
                gpu.ui.rollback_local_presentation(presentation);
            }
            None
        };
        forward_pointer_click(
            endpoint,
            self.epoch,
            self.applied_composition_revision,
            released.sequence,
            Some(interaction_id.clone()),
            released.binding,
            released.control_value.clone(),
            self.pointer_delivery.clone(),
            self.interaction_traces.clone(),
            Some(proxy),
            pending,
        );
        Ok(json!({
            "state": "forwarded",
            "sequence": released.sequence,
            "node_path": semantic_node_path,
            "interaction_id": interaction_id,
            "pointer": {"start": start, "end": end},
            "committed_value": released.control_value,
        }))
    }

    fn input_debug_drag_gesture(
        &mut self,
        source_node_key: String,
        target_node_key: String,
    ) -> Result<Value, &'static str> {
        let endpoint = self.ui_endpoint.ok_or("ui_host_unavailable")?;
        let proxy = self
            .event_proxy
            .clone()
            .ok_or("window_event_proxy_unavailable")?;
        let gpu = self.gpu.as_mut().ok_or("window_gpu_unavailable")?;
        gpu.ui.prepare_interaction(
            &self.fragments,
            gpu.physical_viewport_size(),
            gpu.logical_viewport_size(),
            gpu.started_at.elapsed().as_secs_f32(),
        );
        let (source, target) = gpu
            .ui
            .debug_drag_gesture_points(&source_node_key, &target_node_key)?;
        gpu.ui.set_pointer_position(source);
        if !gpu.ui.begin_drag_at_pointer(&self.fragments) {
            return Err("drag_source_not_declared");
        }
        gpu.ui.set_pointer_position(target);
        if !gpu.ui.update_drag_preview() {
            gpu.ui.cancel_drag();
            return Err("drag_preview_not_started");
        }
        let resolved = gpu
            .ui
            .finish_drag_at_pointer(&self.fragments)
            .ok_or("drop_target_not_declared")?;
        gpu.next_semantic_sequence += 1;
        let sequence = gpu.next_semantic_sequence;
        let pending = gpu.ui.retain_local_presentation(
            sequence,
            &resolved.fragment,
            resolved.local_presentation.clone(),
        );
        let interaction_id = InteractionId(format!("debug-window-drag-{sequence}"));
        append_interaction_record(
            &self.interaction_traces,
            interaction_id.clone(),
            InteractionTraceStage::HitCaptureResolved,
            InteractionTraceOutcome::Accepted,
            None,
            None,
            self.applied_composition_revision,
        );
        self.redraw_pending = true;
        forward_drag_drop(
            endpoint,
            self.epoch,
            self.applied_composition_revision,
            sequence,
            None,
            resolved,
            self.pointer_delivery.clone(),
            self.interaction_traces.clone(),
            Some(proxy),
            pending,
        );
        Ok(
            json!({"state": "forwarded", "sequence": sequence, "source_node_key": source_node_key, "target_node_key": target_node_key, "interaction_id": interaction_id}),
        )
    }
}

#[cfg(windows)]
#[derive(Default, Clone)]
struct ExternalFrameTiming {
    frame_sequence: u64,
    snapshot_ms: f32,
    invalidate_plan_ms: f32,
    refresh_plan_ms: f32,
    compose_visuals_ms: f32,
    text_layout_ms: f32,
    group_sort_ms: f32,
    buffer_upload_ms: f32,
    color_pass_ms: f32,
    external_depth_pass_ms: f32,
    hit_pass_ms: f32,
    dropped: bool,
    skipped_static: bool,
    skipped_throttled: bool,
}

/// Per-frame counters aggregated over a 60-frame window. Every 60 render
/// frames the headless render loop emits a single structured `ui_perf_window`
/// JSONL line with these counters. This replaces the earlier human-readable
/// `eprintln!` summary so automated probes can consume the data.
#[cfg(windows)]
#[derive(Default, Clone, Debug)]
struct UiPerfCounters {
    /// Total frames attempted (including skipped and dropped).
    render_frames: u64,
    /// Frames that actually produced a color pass (not skipped, not dropped).
    rendered_frames: u64,
    /// Frames where all ring buffers were busy.
    dropped_frames: u64,
    /// Frames skipped because the snapshot was unchanged and no animation active.
    skipped_static_frames: u64,
    /// Frames skipped by the 16ms throttle.
    skipped_throttled_frames: u64,
    /// wgpu.world.camera.submit_frame requests received.
    camera_frames_received: u64,
    /// wgpu.world.ui.anchor.submit(_batch) requests received.
    anchor_batches_received: u64,
    /// Pointer down events received.
    pointer_down_received: u64,
    /// Pointer up events received.
    pointer_up_received: u64,
    /// Semantic click events resolved from pointer hits.
    semantic_clicks: u64,
    /// Unified ID pass executions (one per rendered frame with external ID targets).
    unified_id_passes: u64,
    /// Draw calls in the ID pass (should be 1 for the batch).
    unified_id_draw_calls: u64,
    /// Instance count in the ID pass.
    unified_id_instances: u64,
    /// ID readback operations (one per pointer down).
    unified_id_readbacks: u64,
    /// Presentation transitions started.
    transition_begins: u64,
    /// Presentation transitions completed.
    transition_ends: u64,
}

#[cfg(windows)]
struct HeadlessExternalGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: wgpu::Adapter,
    ui: UiWgpuRenderer,
    screen_ui: UiWgpuRenderer,
    world_ui: UiWgpuRenderer,
    pointer_hit_target: wgpu::Texture,
    pointer_hit_target_view: wgpu::TextureView,
    input: LocalInputState,
    captured_binding: Option<UiHitBinding>,
    pending_control_value: Option<neon_ui_schema::UiSemanticPayloadValue>,
    next_semantic_sequence: u64,
    external_surfaces: HashMap<String, Vec<dx12_interop::SharedSurface>>,
    external_id_surfaces: HashMap<String, Vec<dx12_interop::SharedSurface>>,
    external_depth_surfaces: HashMap<String, Vec<dx12_interop::SharedSurface>>,
    surface_kinds: HashMap<String, RenderSurfaceKind>,
    color_depth_targets: HashMap<String, Vec<wgpu::Texture>>,
    external_handle_tokens: HashMap<String, (String, String)>,
    next_external_frame_sequence: u64,
    last_external_render_at: Option<Instant>,
    last_rendered_fragments: Option<HashMap<RenderSurfaceKind, HashMap<UiFragmentId, UiFragment>>>,
    last_rendered_all_surfaces: bool,
    frame_timings: VecDeque<ExternalFrameTiming>,
    started_at: Instant,
    perf: UiPerfCounters,
    /// Frame sequence of the latest completed persistent unified ID texture.
    /// The render loop owns the ID pass; pointer events only read a pixel from
    /// the most recent completed frame and pair it with `id_frame_bindings`
    /// from the same frame (never a fresh redraw on pointer down).
    id_frame_sequence: u64,
    /// Binding map of the latest completed unified ID frame. Paired with
    /// `id_frame_sequence` so a pointer readback and its numeric-ID lookup
    /// always come from the same composition frame.
    id_frame_bindings: std::collections::HashMap<u32, UiHitBinding>,
    /// False until the render loop has produced at least one completed ID
    /// frame into `pointer_hit_target`.
    id_frame_ready: bool,
    /// External image ids already preloaded into the renderer-owned atlases.
    /// The upload RPC records residency metadata in `WgpuRuntime`; this set
    /// tracks which of those have actually been placed into the GPU atlas so
    /// the render loop only preloads new images once.
    preloaded_external_images: HashSet<String>,
}

#[cfg(windows)]
impl HeadlessExternalGpu {
    fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|error| format!("headless external adapter: {error}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-headless-external-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|error| format!("headless external device: {error}"))?;
        let pointer_hit_target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-external-pointer-hit-id"),
            size: wgpu::Extent3d {
                width: 2560,
                height: 1440,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Uint,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let pointer_hit_target_view = pointer_hit_target.create_view(&Default::default());
        Ok(Self {
            ui: UiWgpuRenderer::new_unified(&device, wgpu::TextureFormat::Rgba8Unorm),
            screen_ui: UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm),
            world_ui: UiWgpuRenderer::new_with_depth(
                &device,
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureFormat::R32Float,
            ),
            pointer_hit_target,
            pointer_hit_target_view,
            input: LocalInputState::default(),
            captured_binding: None,
            pending_control_value: None,
            next_semantic_sequence: 0,
            device,
            queue,
            adapter,
            external_surfaces: HashMap::new(),
            external_id_surfaces: HashMap::new(),
            external_depth_surfaces: HashMap::new(),
            surface_kinds: HashMap::new(),
            color_depth_targets: HashMap::new(),
            external_handle_tokens: HashMap::new(),
            next_external_frame_sequence: 0,
            last_external_render_at: None,
            last_rendered_fragments: None,
            last_rendered_all_surfaces: false,
            frame_timings: VecDeque::new(),
            started_at: Instant::now(),
            perf: UiPerfCounters::default(),
            id_frame_sequence: 0,
            id_frame_bindings: std::collections::HashMap::new(),
            id_frame_ready: false,
            preloaded_external_images: HashSet::new(),
        })
    }

    /// Reads a single pixel from the latest completed unified ID frame. The
    /// render loop owns the ID pass; pointer-down events must NOT redraw the
    /// whole ID image (plan §4.1). This only copies one pixel and maps the
    /// readback buffer, then returns the numeric ID (RENDER_HIT_NONE on miss).
    fn read_completed_id_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pointer_hit_target: &wgpu::Texture,
        id_frame_ready: bool,
        renderer: &mut UiWgpuRenderer,
        pixel: [f32; 2],
    ) -> Result<u32, String> {
        if !id_frame_ready {
            return Err("pointer_id_frame_unavailable".into());
        }
        let x = pixel[0].clamp(0.0, 2559.0) as u32;
        let y = pixel[1].clamp(0.0, 1439.0) as u32;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-external-pointer-hit-readback"),
        });
        // No clear, no draw: the persistent `pointer_hit_target` already
        // contains the current frame's unified ID content from `render()`.
        let Some(slot) = renderer.enqueue_hit_readback(&mut encoder, pointer_hit_target, [x, y])
        else {
            return Err("pointer_hit_readback_busy".into());
        };
        queue.submit(Some(encoder.finish()));
        if !renderer.begin_hit_readback_mapping(slot) {
            return Err("pointer_hit_readback_mapping_failed".into());
        }
        let deadline = Instant::now() + Duration::from_millis(16);
        while Instant::now() < deadline {
            let _ = device.poll(wgpu::PollType::Poll);
            if let Some(result) = renderer.try_complete_hit_readback(slot) {
                return result.map_err(|error| format!("pointer_hit_readback_failed: {error}"));
            }
            thread::yield_now();
        }
        Err("pointer_hit_readback_timeout".into())
    }

    fn pointer_event(
        &mut self,
        event: UiPointerEvent,
        fragments: &HashMap<UiFragmentId, UiFragment>,
        surface_kind: RenderSurfaceKind,
    ) -> Result<Value, String> {
        if event.generation != 1 {
            return Err("ui_pointer_generation_stale".into());
        }
        // Use the same monotonic clock as the render loop. A fixed 0.0 here
        // resets every ActiveTransition's elapsed time on each pointer move,
        // so a hover/drag during an animation snaps the node back to its start
        // pose and the render loop then completes it immediately — the
        // start/complete spam observed on pointer movement.
        let now = self.started_at.elapsed().as_secs_f32();
        // Screen and World UI share one hit image. The event surface selects
        // the color/depth composition target, but never selects a separate
        // interaction tree. This makes the topmost panel win across both UI
        // kinds and prevents parent/node bubbling from producing a second hit.
        self.ui.invalidate_plan();
        let ui = &mut self.ui;
        ui.set_pointer_position(event.pixel);
        ui.prepare_interaction(fragments, [1280, 720], [1280.0, 720.0], now);
        // Hit testing is unified, but the visible color pass uses the
        // renderer for the selected surface. Value previews must live in that
        // renderer or the semantic event succeeds while the visible control
        // remains at the old fragment value.
        let presentation_ui = match surface_kind {
            RenderSurfaceKind::WorldUi => &mut self.world_ui,
            _ => &mut self.screen_ui,
        };
        presentation_ui.set_pointer_position(event.pixel);
        presentation_ui.prepare_interaction(fragments, [1280, 720], [1280.0, 720.0], now);
        match event.event_type {
            UiPointerEventType::Enter | UiPointerEventType::Move => {
                // Headless external input still owns the same local value
                // gesture path as the windowed renderer. Without this update,
                // slider/drag-value moves are observed but never change the
                // preview that is committed on pointer-up.
                if presentation_ui.value_gesture_active() {
                    presentation_ui.update_value_gesture();
                }
                self.input.set_hover_id(None);
                Ok(json!({"state": "observed"}))
            }
            UiPointerEventType::Down => {
                self.perf.pointer_down_received += 1;
                if !matches!(event.button, Some(neon_ui_schema::UiPointerButton::Primary)) {
                    return Err("ui_pointer_button_unsupported".into());
                }
                if let Some((binding, value)) = presentation_ui.dropdown_option_at_pointer() {
                    self.input.set_hover_id(Some(0));
                    self.input
                        .pointer_down()
                        .map_err(|_| "choice_option_capture_failed".to_owned())?;
                    self.captured_binding = Some(binding);
                    self.pending_control_value = Some(value);
                    presentation_ui.close_dropdown();
                    return Ok(json!({"state": "choice_option_captured"}));
                }
                if presentation_ui.dismiss_dropdown_at_pointer() {
                    self.captured_binding = None;
                    self.pending_control_value = None;
                    self.input.cancel();
                    return Ok(json!({"state": "choice_popup_dismissed"}));
                }
                if presentation_ui.toggle_dropdown_at_pointer() {
                    self.captured_binding = None;
                    self.pending_control_value = None;
                    self.input.cancel();
                    return Ok(json!({"state": "choice_popup_toggled"}));
                }
                let current_hit = ui.hit_binding_at_pointer();
                self.input
                    .set_hover_id(current_hit.as_ref().map(|(hit_id, _)| *hit_id));
                self.input.pointer_down().map_err(|_| {
                    format!(
                        "press_without_semantic_hit:pointer=({:.1},{:.1}) cpu_hit={:?}",
                        event.pixel[0],
                        event.pixel[1],
                        current_hit
                            .as_ref()
                            .map(|(hit_id, binding)| (*hit_id, binding.node_path.clone())),
                    )
                })?;
                self.captured_binding = current_hit.map(|(_, binding)| binding);
                if self.captured_binding.is_none() {
                    let _ = self.input.pointer_up(false);
                    return Err("press_without_semantic_binding".into());
                }
                if let Some(binding) = self.captured_binding.clone() {
                    self.pending_control_value = binding.control_value.clone();
                    if presentation_ui.requires_value_gesture(&binding)
                        && !presentation_ui.begin_value_gesture(&binding)
                    {
                        self.captured_binding = None;
                        self.pending_control_value = None;
                        self.input.cancel();
                        return Err("value_gesture_not_started".into());
                    }
                    presentation_ui.press_hovered(now);
                }
                Ok(json!({"state": "captured"}))
            }
            UiPointerEventType::Up => {
                self.perf.pointer_up_received += 1;
                let capture_id = self.input.capture_id;
                let binding = self
                    .captured_binding
                    .take()
                    .or_else(|| capture_id.and_then(|hit_id| ui.hit_binding(hit_id)));
                let control_value = self.pending_control_value.take();
                if self
                    .input
                    .pointer_up(binding.is_some() || capture_id.is_some())
                    .is_err()
                {
                    return Err("pointer_release_rejected".into());
                }
                let Some(binding) = binding else {
                    return Ok(json!({"state": "released"}));
                };
                let Some(intent) = binding.intent else {
                    return Ok(json!({"state": "released"}));
                };
                let finished_value = presentation_ui.finish_value_gesture();
                let toggle_value = presentation_ui.finish_toggle_control(&binding.node_path);
                let (control_value, local_presentation) =
                    if let Some((value, presentation)) = finished_value {
                        (Some(value), Some(presentation))
                    } else if let Some((value, presentation)) = toggle_value {
                        (Some(value), Some(presentation))
                    } else {
                        (control_value.or(binding.control_value), None)
                    };
                self.perf.semantic_clicks += 1;
                self.next_semantic_sequence = self.next_semantic_sequence.saturating_add(1);
                if let Some(presentation) = local_presentation {
                    presentation_ui.retain_local_presentation(
                        self.next_semantic_sequence,
                        &binding.fragment,
                        presentation,
                    );
                }
                let event_id = format!("wgpu-pointer-click-{}", self.next_semantic_sequence);
                if binding.node_path.ends_with("/p0")
                    || binding.node_path.contains("/p")
                        && binding.node_path.split('/').next_back().is_some_and(|key| {
                            key.strip_prefix('p').is_some_and(|index| {
                                !index.is_empty() && index.chars().all(|c| c.is_ascii_digit())
                            })
                        })
                {
                    eprintln!(
                        "[neon-wgpu-runtime] world-ui-click id={} node={} interaction_id={} ",
                        capture_id.unwrap_or(RENDER_HIT_NONE),
                        binding.node_path,
                        event_id,
                    );
                }
                eprintln!(
                    "{}",
                    json!({
                        "event": "wgpu_semantic_click",
                        "event_id": event_id,
                        "node_path": binding.node_path,
                        "hit_id": capture_id.unwrap_or(RENDER_HIT_NONE),
                        "timestamp_monotonic_ns": self.started_at.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                    })
                );
                Ok(json!({"semantic_event": UiSemanticEvent {
                    event: UiSemanticEventType::PointerClick,
                    event_id,
                    renderer_epoch: 1,
                    composition_revision: binding.fragment.revision,
                    fragment: binding.fragment,
                    intent,
                    pointer: Some(neon_ui_schema::UiPointerMetadata { id: event.pointer_id, sequence: self.next_semantic_sequence }),
                    focus: None,
                    data_grid_cell: binding.data_grid_cell,
                    text: None,
                    control_value,
                    drag_drop: None,
                }}))
            }
            UiPointerEventType::Wheel => {
                let delta = match event.delta_mode {
                    neon_ui_schema::UiPointerDeltaMode::Line => {
                        [event.delta[0] * 24.0, event.delta[1] * 24.0]
                    }
                    _ => event.delta,
                };
                ui.scroll_wheel_at_pointer(delta);
                Ok(json!({"state": "scrolled"}))
            }
            UiPointerEventType::Leave | UiPointerEventType::Cancel => {
                self.captured_binding = None;
                self.pending_control_value = None;
                self.input.cancel();
                Ok(json!({"state": "cancelled"}))
            }
        }
    }

    fn open(&mut self, open: RenderSurfaceOpen) -> Result<Value, String> {
        if open.format != "rgba8unorm" || !(2..=3).contains(&open.buffer_count) {
            return Err("headless_external_surface_format_or_buffer_unsupported".into());
        }
        if self.external_surfaces.contains_key(&open.surface_id) {
            return Err("surface_already_open".into());
        }
        let mut surfaces = Vec::with_capacity(open.buffer_count as usize);
        for _ in 0..open.buffer_count {
            surfaces.push(
                dx12_interop::create_shared_surface(
                    &self.device,
                    &self.adapter,
                    open.size.width.max(1),
                    open.size.height.max(1),
                    wgpu::TextureFormat::Rgba8Unorm,
                )
                .map_err(|error| error.to_string())?,
            );
        }
        self.external_surfaces
            .insert(open.surface_id.clone(), surfaces);
        self.surface_kinds
            .insert(open.surface_id.clone(), open.kind);
        self.last_rendered_all_surfaces = false;
        self.last_rendered_fragments = None;
        let color_depth_targets = (0..open.buffer_count)
            .map(|_| create_color_depth_target(&self.device, open.size.width, open.size.height))
            .collect::<Vec<_>>();
        self.color_depth_targets
            .insert(open.surface_id.clone(), color_depth_targets);
        let generation = 1;
        let texture_token = format!("surface:{}:texture:g{}", open.surface_id, generation);
        let fence_token = format!("surface:{}:fence:g{}", open.surface_id, generation);
        self.external_handle_tokens.insert(
            open.surface_id.clone(),
            (texture_token.clone(), fence_token.clone()),
        );
        if open
            .targets
            .iter()
            .any(|target| target.kind == RenderSurfaceTargetKind::Id)
        {
            let mut id_surfaces = Vec::with_capacity(open.buffer_count as usize);
            for _ in 0..open.buffer_count {
                id_surfaces.push(
                    dx12_interop::create_shared_surface(
                        &self.device,
                        &self.adapter,
                        open.size.width.max(1),
                        open.size.height.max(1),
                        wgpu::TextureFormat::R32Uint,
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
            self.external_id_surfaces
                .insert(open.surface_id.clone(), id_surfaces);
        }
        if open.depth {
            let mut depth_surfaces = Vec::with_capacity(open.buffer_count as usize);
            for _ in 0..open.buffer_count {
                depth_surfaces.push(
                    dx12_interop::create_shared_surface(
                        &self.device,
                        &self.adapter,
                        open.size.width.max(1),
                        open.size.height.max(1),
                        wgpu::TextureFormat::R32Float,
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
            self.external_depth_surfaces
                .insert(open.surface_id.clone(), depth_surfaces);
        }
        Ok(json!({
            "surface_id": open.surface_id,
            "generation": generation,
            "producer_epoch": 1,
            "buffer_count": open.buffer_count,
            "transport": "d3d12_shared_texture_v1",
            "texture_token": texture_token,
            "fence_token": fence_token
        }))
    }

    fn acquire(&self, surface_id: &str, pid: u32) -> Result<Value, String> {
        let shared = self
            .external_surfaces
            .get(surface_id)
            .ok_or("surface_not_found")?;
        let (texture_token, fence_token) = self
            .external_handle_tokens
            .get(surface_id)
            .ok_or("surface_broker_token_not_found")?;
        let buffers = shared
            .iter()
            .enumerate()
            .map(|(index, shared)| {
                let mut buffer = json!({
                    "buffer_index": index,
                    "color_texture_handle": dx12_interop::duplicate_handle_to_process(shared.texture_handle, pid).map_err(|error| error.to_string())?,
                    "color_fence_handle": dx12_interop::duplicate_handle_to_process(shared.fence_handle, pid).map_err(|error| error.to_string())?,
                    "consumer_release_fence_handle": dx12_interop::duplicate_handle_to_process(shared.consumer_fence_handle, pid).map_err(|error| error.to_string())?
                });
                if let Some(id_surfaces) = self.external_id_surfaces.get(surface_id)
                    && let Some(id_surface) = id_surfaces.get(index)
                {
                    buffer["id_texture_handle"] = json!(dx12_interop::duplicate_handle_to_process(id_surface.texture_handle, pid).map_err(|error| error.to_string())?);
                    buffer["id_fence_handle"] = json!(dx12_interop::duplicate_handle_to_process(id_surface.fence_handle, pid).map_err(|error| error.to_string())?);
                    buffer["id_consumer_release_fence_handle"] = json!(dx12_interop::duplicate_handle_to_process(id_surface.consumer_fence_handle, pid).map_err(|error| error.to_string())?);
                }
                if let Some(depth_surfaces) = self.external_depth_surfaces.get(surface_id)
                    && let Some(depth_surface) = depth_surfaces.get(index)
                {
                    buffer["depth_texture_handle"] = json!(dx12_interop::duplicate_handle_to_process(depth_surface.texture_handle, pid).map_err(|error| error.to_string())?);
                    buffer["depth_fence_handle"] = json!(dx12_interop::duplicate_handle_to_process(depth_surface.fence_handle, pid).map_err(|error| error.to_string())?);
                    buffer["depth_consumer_release_fence_handle"] = json!(dx12_interop::duplicate_handle_to_process(depth_surface.consumer_fence_handle, pid).map_err(|error| error.to_string())?);
                }
                Ok(buffer)
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut result = json!({
            "surface_id": surface_id,
            "texture_token": texture_token,
            "fence_token": fence_token,
            "buffers": buffers
        });
        // Transitional compatibility for consumers that have not yet upgraded
        // to the ring response. Remove once the three-buffer consumer lands.
        if let Some((texture, fence, id_texture, id_fence)) = result["buffers"]
            .as_array()
            .and_then(|buffers| buffers.first())
            .map(|first| {
                (
                    first["color_texture_handle"].clone(),
                    first["color_fence_handle"].clone(),
                    first["id_texture_handle"].clone(),
                    first["id_fence_handle"].clone(),
                )
            })
        {
            result["texture_handle"] = texture;
            result["fence_handle"] = fence;
            result["id_texture_handle"] = id_texture;
            result["id_fence_handle"] = id_fence;
        }
        Ok(result)
    }

    /// Places newly uploaded external images into each renderer's GPU atlas.
    ///
    /// `upload_external_image` on the runtime only records residency metadata
    /// (region/uv) and returns it to the host; it never touches the GPU atlas.
    /// This method is the headless counterpart to the window path's
    /// `WindowCommand::UploadExternalImage`: the render loop calls it before
    /// `render` so the color passes can actually sample the image. Idempotent
    /// per `image_id` via `preloaded_external_images`.
    fn preload_external_images(&mut self, images: &[UiImageSource]) {
        for source in images {
            if self.preloaded_external_images.contains(&source.image_id) {
                continue;
            }
            // Screen and world panels render from independent atlases, so the
            // same bytes must be placed in both. The unified ID renderer only
            // emits hit ids and never samples image color, so it is skipped.
            let screen = self
                .screen_ui
                .preload_external_image(&self.device, &self.queue, source);
            let world = self
                .world_ui
                .preload_external_image(&self.device, &self.queue, source);
            match (screen, world) {
                (Ok(_), Ok(_)) => {
                    self.preloaded_external_images
                        .insert(source.image_id.clone());
                    // The fragment tree can be unchanged while its image atlas
                    // gains a new resident entry. A cached transparent frame
                    // from before this upload is no longer valid.
                    self.last_rendered_all_surfaces = false;
                }
                (screen, world) => {
                    eprintln!(
                        "[neon-wgpu-runtime] external image preload failed for {}: screen={:?} world={:?}",
                        source.image_id,
                        screen.err(),
                        world.err()
                    );
                }
            }
        }
    }

    fn render(
        &mut self,
        snapshots: &HashMap<RenderSurfaceKind, HashMap<UiFragmentId, UiFragment>>,
        unified_hit_fragments: &HashMap<UiFragmentId, UiFragment>,
        snapshot_ms: f32,
    ) -> Result<(), String> {
        if self.external_surfaces.is_empty() {
            return Ok(());
        }
        // Monotonic wall-clock for the render loop. Transition sampling and
        // press/hover animations consume this every frame; a fixed value (or
        // zero) freezes every ActiveTransition at its start pose.
        let time_seconds = self.started_at.elapsed().as_secs_f32();
        let mut timing = ExternalFrameTiming::default();
        timing.snapshot_ms = snapshot_ms;
        // Pointer hit testing uses the process-local persistent ID frame below.
        // External ID rings are optional and require a consumer to import and
        // release every slot; do not make the render loop depend on them.
        let has_external_id_targets = !self.external_surfaces.is_empty();
        // A static resolved snapshot does not need another layout, text upload,
        // color pass, depth pass, or ID pass. The consumer keeps sampling the
        // last complete buffer from the ring. Exception: while a transition or
        // press animation is still in flight the snapshot is unchanged but the
        // frame must keep rendering so the interpolation advances.
        let has_active_animation = self.screen_ui.has_active_animation(time_seconds)
            || self.world_ui.has_active_animation(time_seconds)
            || (has_external_id_targets && self.ui.has_active_animation(time_seconds));
        let has_pointer_visual_dirty = self.screen_ui.pointer_visual_dirty()
            || self.world_ui.pointer_visual_dirty()
            || (has_external_id_targets && self.ui.pointer_visual_dirty());
        if !has_active_animation
            && !has_pointer_visual_dirty
            && self.last_rendered_all_surfaces
            && self
                .last_rendered_fragments
                .as_ref()
                .is_some_and(|previous| previous == snapshots)
        {
            timing.skipped_static = true;
            timing.frame_sequence = self.next_external_frame_sequence;
            self.push_frame_timing(timing);
            return Ok(());
        }
        // Keep the external UI responsive to camera and anchor motion. The
        // consumer ring still provides back-pressure when a buffer is busy.
        if self
            .last_external_render_at
            .is_some_and(|last| last.elapsed() < Duration::from_millis(16))
        {
            timing.skipped_throttled = true;
            timing.frame_sequence = self.next_external_frame_sequence;
            self.push_frame_timing(timing);
            return Ok(());
        }
        self.last_external_render_at = Some(Instant::now());
        // The filtered snapshot contains live world-panel bounds derived from
        // the latest camera and anchor, not a new UI program revision. The
        // renderers cache their flattened plan keyed by `fragment.revision`,
        // which does NOT change when only the projected bounds move. Each
        // renderer must therefore invalidate its plan so the re-projected
        // bounds actually reach the color/depth pass; otherwise the world UI
        // freezes at its first projected position.
        let stage = Instant::now();
        if !self.external_surfaces.is_empty() {
            self.ui.invalidate_plan();
        }
        self.world_ui.invalidate_plan_for_world_transform();
        // World anchors change only the WorldUi snapshot. Invalidating the
        // fixed ScreenUi plan here clears its text/layout cache every camera
        // update and makes a static console (including image nodes) re-layout
        // at the world-frame rate. Screen fragment revisions and viewport
        // changes already invalidate this renderer through `refresh_plan`.
        timing.invalidate_plan_ms = stage.elapsed().as_secs_f32() * 1000.0;
        self.next_external_frame_sequence = self.next_external_frame_sequence.saturating_add(1);
        let frame_sequence = self.next_external_frame_sequence;
        timing.frame_sequence = frame_sequence;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neon3-headless-external-ui"),
            });
        // A combined Flow is published as one atomic frame. Every external
        // target must use the same ring slot, otherwise screen and world UI can
        // visibly come from different camera/layout revisions.
        let write_index = self.external_surfaces.values().next().and_then(|ring| {
            ring.iter().enumerate().find_map(|(index, _shared)| {
                let color_free = self.external_surfaces.values().all(|ring| {
                    ring.get(index).is_some_and(|color| unsafe {
                        color.consumer_fence.GetCompletedValue() >= color.frame_sequence
                    })
                });
                // A ring slot is one published frame: the matching color,
                // occlusion depth, and hit-ID textures must all be released
                // before any of them can be overwritten. Reusing a slot after
                // only its color fence completes races the host's depth sample
                // and makes scene occlusion compare different frame revisions.
                let depth_free = self.external_depth_surfaces.values().all(|ring| {
                    ring.get(index).is_none_or(|depth| unsafe {
                        depth.consumer_fence.GetCompletedValue() >= depth.frame_sequence
                    })
                });
                let id_free = self.external_id_surfaces.values().all(|ring| {
                    ring.get(index).is_none_or(|id| unsafe {
                        id.consumer_fence.GetCompletedValue() >= id.frame_sequence
                    })
                });
                (color_free && depth_free && id_free).then_some(index)
            })
        });
        let Some(write_index) = write_index else {
            timing.dropped = true;
            self.push_frame_timing(timing);
            return Ok(());
        };
        let mut rendered_all_surfaces = true;
        for (surface_id, shared_ring) in self.external_surfaces.iter_mut() {
            // Acquire a buffer the consumer has actually released. The consumer
            // signals its per-buffer release fence once it has finished sampling
            // that buffer, so a buffer is reusable when the fence value has
            // reached (or passed) the frame sequence last written to it.
            //
            // This is a CPU read of a GPU-signaled fence. It deliberately does
            // NOT GPU-wait on the oldest buffer: the consumer only samples the
            // newest buffer, so waiting on `min(frame_sequence)` can deadlock
            // (the producer blocks on buffer 0's release while the consumer
            // keeps releasing only buffer 2). Dropping the frame keeps the
            // overlay alive and simply shows the last completed frame.
            if shared_ring.get(write_index).is_none() {
                rendered_all_surfaces = false;
                timing.dropped = true;
                continue;
            }
            let shared = &mut shared_ring[write_index];
            let hal_queue = unsafe { self.queue.as_hal::<wgpu::hal::api::Dx12>() }
                .ok_or("dx12_queue_unavailable")?;
            let view = shared
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let kind = self
                .surface_kinds
                .get(surface_id)
                .copied()
                .unwrap_or(RenderSurfaceKind::ScreenUi);
            let mode = match kind {
                RenderSurfaceKind::WorldUi => UiDrawMode::World,
                _ => UiDrawMode::Screen,
            };
            let color_depth_view = if matches!(mode, UiDrawMode::World | UiDrawMode::All) {
                self.color_depth_targets
                    .get(surface_id)
                    .and_then(|targets| targets.get(write_index))
                    .map(|target| target.create_view(&wgpu::TextureViewDescriptor::default()))
            } else {
                None
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-headless-external-ui-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: color_depth_view.as_ref().map(|view| {
                    wgpu::RenderPassDepthStencilAttachment {
                        view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let stage = Instant::now();
            let fragments = snapshots.get(&kind);
            // A surface whose kind has no snapshot (e.g. a world surface opened
            // while the Flow only declares screen UI) renders its clear color
            // only. Never fall back to another kind's snapshot: that would
            // paint screen UI into the world target (and vice versa).
            if let Some(fragments) = fragments {
                let renderer = match mode {
                    UiDrawMode::World => &mut self.world_ui,
                    UiDrawMode::Screen => &mut self.screen_ui,
                    UiDrawMode::All => &mut self.ui,
                };
                renderer.draw(
                    &self.device,
                    &self.queue,
                    &mut pass,
                    fragments,
                    [shared.width, shared.height],
                    HEADLESS_UI_LOGICAL_SIZE,
                    time_seconds,
                    mode,
                );
                timing.color_pass_ms += stage.elapsed().as_secs_f32() * 1000.0;
            }
            drop(pass);
            // Occlusion depth pass: re-emit the same instances into the matched
            // R32Float ring buffer so the consumer can depth-test the overlay
            // against its scene depth. Uses the same buffer index as the color
            // ring so both stay frame-coherent. This MUST run on every frame
            // the color pass runs, even with no world fragments: gating it on
            // fragments lets the depth ring fall behind the color ring (e.g.
            // during startup), wedging that ring slot forever and throttling
            // the pipeline to a subset of ring slots.
            if let Some(depth_ring) = self.external_depth_surfaces.get_mut(surface_id)
                && let Some(depth_shared) = depth_ring.get_mut(write_index)
            {
                let depth_view = depth_shared
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut depth_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("neon3-headless-external-ui-depth-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &depth_view,
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
                let stage = Instant::now();
                // Only draw when the color pass actually rendered fragments
                // this frame. `draw_depth` replays the renderer's flattened
                // instances, which are stale on fragment-less frames; the
                // clear-only pass still advances the ring so it stays in
                // lockstep with the color ring.
                if fragments.is_some() {
                    self.world_ui.draw_depth(&self.queue, &mut depth_pass);
                }
                timing.external_depth_pass_ms += stage.elapsed().as_secs_f32() * 1000.0;
                drop(depth_pass);
                depth_shared.frame_sequence = frame_sequence;
                hal_queue.add_signal_fence(depth_shared.fence.clone(), frame_sequence);
            }
            // Pick-ID pass: emit the same resolved hit bindings into the matched
            // R32Uint ring buffer so the consumer can read a single pixel to
            // resolve the hovered/clicked panel. Same buffer index and frame
            // sequence as color/depth keeps the three targets frame-coherent.
            if let Some(id_ring) = self.external_id_surfaces.get_mut(surface_id)
                && let Some(id_shared) = id_ring.get_mut(write_index)
            {
                let id_view = id_shared
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut id_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("neon3-headless-external-ui-id-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &id_view,
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
                let stage = Instant::now();
                // The ID target is the unified pick buffer for the whole
                // combined Flow: it carries every hit binding (screen and
                // world) so the consumer resolves pointer hits from one
                // texture regardless of which color target the panel came
                // from. Same buffer index / frame sequence as color.
                self.ui.draw_hit_id(
                    &self.device,
                    &self.queue,
                    &mut id_pass,
                    &unified_hit_fragments,
                    [id_shared.width, id_shared.height],
                    HEADLESS_UI_LOGICAL_SIZE,
                    time_seconds,
                );
                self.perf.unified_id_passes += 1;
                timing.hit_pass_ms += stage.elapsed().as_secs_f32() * 1000.0;
                drop(id_pass);
                self.perf.unified_id_instances =
                    (self.ui.hit_binding_count() as u64).max(self.perf.unified_id_instances);
                id_shared.frame_sequence = frame_sequence;
                hal_queue.add_signal_fence(id_shared.fence.clone(), frame_sequence);
            }
            shared.frame_sequence = frame_sequence;
            hal_queue.add_signal_fence(shared.fence.clone(), shared.frame_sequence);
        }
        // Persistent unified ID frame: re-emit the same unified ID pass into the
        // process-local pointer texture so pointer-down events can read a single
        // pixel of an already-completed frame instead of redrawing the whole ID
        // image on every click (plan §4.1 / §4.4). Both the external ring
        // buffers and this local frame come from the same `unified_hit_fragments`
        // at the same frame sequence, so a pointer readback and its binding map
        // lookup pair to the same composition frame.
        if has_external_id_targets {
            let pointer_view = self.pointer_hit_target_view.clone();
            let mut pointer_id_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-headless-external-pointer-id-frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &pointer_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.ui.draw_hit_id(
                &self.device,
                &self.queue,
                &mut pointer_id_pass,
                &unified_hit_fragments,
                [1280, 720],
                [1280.0, 720.0],
                time_seconds,
            );
            drop(pointer_id_pass);
            self.id_frame_sequence = frame_sequence;
            self.id_frame_bindings = self.ui.hit_bindings_snapshot();
            self.id_frame_ready = true;
            // The persistent process-local ID frame is the active pointer
            // path. Count it here even when no external ID ring was opened;
            // otherwise diagnostics incorrectly report zero ID passes while
            // pointer hit testing is fully operational.
            self.perf.unified_id_passes += 1;
            self.perf.unified_id_instances = self
                .perf
                .unified_id_instances
                .max(self.id_frame_bindings.len() as u64);
        }
        self.queue.submit(Some(encoder.finish()));
        let stage_timings = self.screen_ui.last_stage_timings();
        timing.refresh_plan_ms = stage_timings.refresh_plan_ms;
        timing.compose_visuals_ms = stage_timings.compose_visuals_ms;
        timing.text_layout_ms = stage_timings.text_layout_ms;
        timing.group_sort_ms = stage_timings.group_sort_ms;
        timing.buffer_upload_ms = stage_timings.buffer_upload_ms;
        self.last_rendered_fragments = Some(snapshots.clone());
        self.last_rendered_all_surfaces = rendered_all_surfaces;
        self.push_frame_timing(timing);
        Ok(())
    }

    fn push_frame_timing(&mut self, timing: ExternalFrameTiming) {
        const TIMING_CAPACITY: usize = 256;
        if self.frame_timings.len() >= TIMING_CAPACITY {
            self.frame_timings.pop_front();
        }
        self.frame_timings.push_back(timing);
    }

    fn diagnostics(&self) -> Value {
        let timings = self
            .frame_timings
            .iter()
            .map(|timing| {
                json!({
                    "frame_sequence": timing.frame_sequence,
                    "snapshot_ms": timing.snapshot_ms,
                    "invalidate_plan_ms": timing.invalidate_plan_ms,
                    "refresh_plan_ms": timing.refresh_plan_ms,
                    "compose_visuals_ms": timing.compose_visuals_ms,
                    "text_layout_ms": timing.text_layout_ms,
                    "group_sort_ms": timing.group_sort_ms,
                    "buffer_upload_ms": timing.buffer_upload_ms,
                    "color_pass_ms": timing.color_pass_ms,
                    "external_depth_pass_ms": timing.external_depth_pass_ms,
                    "hit_pass_ms": timing.hit_pass_ms,
                    "dropped": timing.dropped,
                    "skipped_static": timing.skipped_static,
                    "skipped_throttled": timing.skipped_throttled,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "frame_sequence": self.next_external_frame_sequence,
            "last_rendered_all_surfaces": self.last_rendered_all_surfaces,
            "layout_counters": {
                "screen": self.screen_ui.layout_counters(),
                "world": self.world_ui.layout_counters(),
                "unified": self.ui.layout_counters(),
            },
            "frames": timings,
        })
    }

    fn depth_probe(&self) -> Value {
        json!({
            "frame_sequence": self.next_external_frame_sequence,
            "world_ui": self.world_ui.depth_diagnostics(),
            "screen_ui": self.screen_ui.depth_diagnostics(),
        })
    }

    /// Renderer-side external-image diagnostic for headless host integrations.
    /// This distinguishes an accepted upload from an image that has actually
    /// been sampled by the screen/world color renderers.
    fn external_image_debug(&self) -> Value {
        json!({
            "frame_sequence": self.next_external_frame_sequence,
            "preloaded_image_ids": self.preloaded_external_images,
            "screen_ui": self.screen_ui.image_debug_snapshot(),
            "world_ui": self.world_ui.image_debug_snapshot(),
        })
    }

    /// Samples concrete pixels from the producer-owned shared color target.
    /// This distinguishes atlas residency from a color pass that actually wrote
    /// the image before an external host samples the D3D12 resource.
    fn sample_external_color_target(
        &mut self,
        surface_id: &str,
        points: &[[u32; 2]],
    ) -> Result<Value, String> {
        if points.is_empty() || points.len() > 16 {
            return Err("color_sample_points_must_contain_1_to_16_points".into());
        }
        let ring = self
            .external_surfaces
            .get(surface_id)
            .ok_or_else(|| "surface_not_found".to_owned())?;
        let (buffer_index, shared) = ring
            .iter()
            .enumerate()
            .filter(|(_, shared)| shared.frame_sequence != 0)
            .max_by_key(|(_, shared)| shared.frame_sequence)
            .ok_or_else(|| "surface_has_no_completed_frame".to_owned())?;
        let frame_sequence = shared.frame_sequence;
        let width = shared.width;
        let height = shared.height;
        let points = points
            .iter()
            .map(|point| {
                [
                    point[0].min(width.saturating_sub(1)),
                    point[1].min(height.saturating_sub(1)),
                ]
            })
            .collect::<Vec<_>>();
        let bytes_per_row = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let mirror = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-external-color-sample-mirror"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mirror_view = mirror.create_view(&wgpu::TextureViewDescriptor::default());
        let source_view = shared
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let source_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("neon3-external-color-sample-layout"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        },
                        count: None,
                    }],
                });
        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("neon3-external-color-sample-shader"),
                source: wgpu::ShaderSource::Wgsl(
                    r#"
                @group(0) @binding(0) var source: texture_2d<f32>;
                @vertex fn vs(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
                    var positions = array<vec2<f32>, 3>(
                        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0)
                    );
                    return vec4<f32>(positions[index], 0.0, 1.0);
                }
                @fragment fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
                    return textureLoad(source, vec2<i32>(position.xy), 0);
                }
            "#
                    .into(),
                ),
            });
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("neon3-external-color-sample-pipeline-layout"),
                bind_group_layouts: &[Some(&source_layout)],
                immediate_size: 0,
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("neon3-external-color-sample-pipeline"),
                layout: Some(&pipeline_layout),
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
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("neon3-external-color-sample-bind-group"),
            layout: &source_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&source_view),
            }],
        });
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-external-color-sample-readback"),
            size: u64::from(bytes_per_row) * points.len() as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neon3-external-color-sample-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-external-color-sample-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &mirror_view,
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
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        for (index, point) in points.iter().enumerate() {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &mirror,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: point[0],
                        y: point[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: u64::from(bytes_per_row) * index as u64,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(1),
                    },
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.queue.submit(Some(encoder.finish()));
        let (mapped_tx, mapped_rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = mapped_tx.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|error| format!("wait for external color sample: {error}"))?;
        mapped_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "external color sample mapping timed out".to_owned())?
            .map_err(|error| format!("map external color sample: {error}"))?;
        let mapped = readback
            .slice(..)
            .get_mapped_range()
            .map_err(|error| format!("read external color sample: {error}"))?;
        let samples = points
            .iter()
            .enumerate()
            .map(|(index, point)| {
                let offset = index * bytes_per_row as usize;
                json!({"point": point, "rgba": [mapped[offset], mapped[offset + 1], mapped[offset + 2], mapped[offset + 3]]})
            })
            .collect::<Vec<_>>();
        drop(mapped);
        readback.unmap();
        Ok(json!({
            "surface_id": surface_id,
            "frame_sequence": frame_sequence,
            "buffer_index": buffer_index,
            "size": [width, height],
            "samples": samples,
        }))
    }

    /// Returns the latest completed unified ID frame snapshot for diagnostics.
    /// Only used by the `debug.unified_id.inspect` RPC method (plan §5.3).
    /// Exposes numeric_id, node_path, and intent; never exposes bounds, clip,
    /// or renderer-local topology to the protocol.
    pub(crate) fn unified_id_inspect(&self) -> Value {
        let id_map = self
            .id_frame_bindings
            .iter()
            .filter(|(numeric_id, _)| **numeric_id != RENDER_HIT_NONE)
            .map(|(numeric_id, binding)| {
                json!({
                    "numeric_id": numeric_id,
                    "node_path": binding.node_path,
                    "intent": binding.intent.as_ref().map(|intent| format!("{intent:?}")),
                    "interaction_key": binding.node_path.rsplit('/').next().unwrap_or(""),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "frame_sequence": self.id_frame_sequence,
            "ready": self.id_frame_ready,
            "binding_count": self.id_frame_bindings.len(),
            "id_map": id_map,
        })
    }

    /// Whether the most recently recorded frame was dropped (no free ring slot).
    fn last_frame_dropped(&self) -> bool {
        self.frame_timings
            .back()
            .is_some_and(|timing| timing.dropped)
    }

    /// Whether the most recently recorded frame was skipped as a static no-op.
    fn last_frame_skipped_static(&self) -> bool {
        self.frame_timings
            .back()
            .is_some_and(|timing| timing.skipped_static)
    }

    /// Whether the most recently recorded frame was skipped by the 16 ms throttle.
    fn last_frame_skipped_throttled(&self) -> bool {
        self.frame_timings
            .back()
            .is_some_and(|timing| timing.skipped_throttled)
    }
}

#[cfg(windows)]
pub fn spawn_headless_external_server(
    endpoint: SocketAddr,
) -> std::thread::JoinHandle<Result<(), String>> {
    std::thread::Builder::new()
        .name("neon3-headless-external-gpu".into())
        .spawn(move || {
            let server =
                neon_ipc::BlockingRpcServer::bind(endpoint).map_err(|error| error.to_string())?;
            let runtime = Arc::new(Mutex::new(WgpuRuntime::headless(1)));
            let gpu = Arc::new(Mutex::new(HeadlessExternalGpu::new()?));

            // Continuous render loop. Rendering only on RPC requests stalls after
            // a startup burst: the consumer releases ring buffers on its own frame
            // schedule, so on-demand rendering fills the ring and drops the frames
            // that actually carry camera/anchor state, then goes idle with no new
            // request to recover. A dedicated loop keeps the shared surface fresh
            // by re-rendering the latest snapshot every ~16 ms and simply dropping
            // a frame when the consumer has not yet released a buffer.
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let render_runtime = Arc::clone(&runtime);
            let render_gpu = Arc::clone(&gpu);
            let render_stop = Arc::clone(&stop);
            let render_thread = std::thread::Builder::new()
                .name("neon3-headless-external-render".into())
                .spawn(move || {
                    // Rolling diagnostics for the render loop. Aggregates over
                    // a fixed window so we can observe frame pacing, snapshot
                    // cost and GPU-lock contention without spamming every frame.
                    let mut diag_frames = 0u32;
                    let mut diag_interval_sum_ms = 0f32;
                    let mut diag_interval_max_ms = 0f32;
                    let mut diag_snapshot_sum_ms = 0f32;
                    let mut diag_snapshot_max_ms = 0f32;
                    let mut diag_render_sum_ms = 0f32;
                    let mut diag_render_max_ms = 0f32;
                    let mut diag_lock_wait_sum_ms = 0f32;
                    let mut diag_lock_wait_max_ms = 0f32;
                    let mut diag_dropped = 0u32;
                    let mut diag_skipped_static = 0u32;
                    let mut diag_skipped_throttled = 0u32;
                    let mut last_frame_at = Instant::now();
                    while !render_stop.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(16));
                        let frame_start = Instant::now();
                        let interval_ms = (frame_start - last_frame_at).as_secs_f32() * 1000.0;
                        last_frame_at = frame_start;
                        let snapshot_start = Instant::now();
                        let (snapshots, _combined, unified_hit_fragments, images) = match render_runtime.lock() {
                            Ok(runtime) => {
                                let (snapshots, combined, unified) = runtime.external_surface_snapshots();
                                let images = runtime
                                    .external_images
                                    .values()
                                    .map(|record| record.source.clone())
                                    .collect::<Vec<_>>();
                                (snapshots, combined, unified, images)
                            }
                            Err(_) => break,
                        };
                        let snapshot_ms = snapshot_start.elapsed().as_secs_f32() * 1000.0;
                        let render_start = Instant::now();
                        let mut dropped = false;
                        let mut skipped_static = false;
                        let mut skipped_throttled = false;
                        let (lock_wait_ms, gpu_perf) = loop {
                            match render_gpu.try_lock() {
                                Ok(mut gpu) => {
                                    let wait_ms =
                                        render_start.elapsed().as_secs_f32() * 1000.0;
                                    gpu.preload_external_images(&images);
                                    if let Err(error) =
                                        gpu.render(
                                            &snapshots,
                                            &unified_hit_fragments,
                                            snapshot_ms,
                                        )
                                    {
                                        eprintln!("[neon-wgpu-runtime] render error: {error}");
                                    }
                                    dropped = gpu.last_frame_dropped();
                                    skipped_static = gpu.last_frame_skipped_static();
                                    skipped_throttled = gpu.last_frame_skipped_throttled();
                                    // Update GPU-side perf counters inside the lock.
                                    gpu.perf.render_frames += 1;
                                    if dropped {
                                        gpu.perf.dropped_frames += 1;
                                    } else if skipped_static {
                                        gpu.perf.skipped_static_frames += 1;
                                    } else if skipped_throttled {
                                        gpu.perf.skipped_throttled_frames += 1;
                                    } else {
                                        gpu.perf.rendered_frames += 1;
                                    }
                                    // Track camera/anchor from the runtime.
                                    if let Ok(runtime) = render_runtime.lock() {
                                        gpu.perf.camera_frames_received =
                                            runtime.camera_frames_received;
                                        gpu.perf.anchor_batches_received =
                                            runtime.anchor_batches_received;
                                    }
let perf = gpu.perf.clone();
                    break (wait_ms, perf);
                                }
                                Err(std::sync::TryLockError::WouldBlock) => {
                                    std::thread::yield_now();
                                    continue;
                                }
                                Err(_) => return,
                            }
                        };
                        let render_ms = render_start.elapsed().as_secs_f32() * 1000.0;
                        diag_frames += 1;
                        diag_interval_sum_ms += interval_ms;
                        diag_interval_max_ms = diag_interval_max_ms.max(interval_ms);
                        diag_snapshot_sum_ms += snapshot_ms;
                        diag_snapshot_max_ms = diag_snapshot_max_ms.max(snapshot_ms);
                        diag_render_sum_ms += render_ms;
                        diag_render_max_ms = diag_render_max_ms.max(render_ms);
                        diag_lock_wait_sum_ms += lock_wait_ms;
                        diag_lock_wait_max_ms = diag_lock_wait_max_ms.max(lock_wait_ms);
                        if dropped {
                            diag_dropped += 1;
                        }
                        if skipped_static {
                            diag_skipped_static += 1;
                        }
                        if skipped_throttled {
                            diag_skipped_throttled += 1;
                        }
                        if diag_frames >= 60 {
                            // Structured JSONL window — replaces the earlier
                            // human-only eprintln! so automated probes can
                            // consume frame-pacing and GPU-cost data.
                            let avg_interval = diag_interval_sum_ms / diag_frames as f32;
                            let avg_snapshot = diag_snapshot_sum_ms / diag_frames as f32;
                            let avg_render = diag_render_sum_ms / diag_frames as f32;
                            let avg_lock = diag_lock_wait_sum_ms / diag_frames as f32;
                            println!(
                                "{}",
                                serde_json::json!({
                                    "event": "ui_perf_window",
                                    "window_frames": diag_frames,
                                    "rendered_frames": gpu_perf.rendered_frames,
                                    "dropped_frames": gpu_perf.dropped_frames,
                                    "skipped_static_frames": gpu_perf.skipped_static_frames,
                                    "skipped_throttled_frames": gpu_perf.skipped_throttled_frames,
                                    "camera_frames_received": gpu_perf.camera_frames_received,
                                    "anchor_batches_received": gpu_perf.anchor_batches_received,
                                    "unified_id_passes": gpu_perf.unified_id_passes,
                                    "unified_id_instances": gpu_perf.unified_id_instances,
                                    "pointer_down_received": gpu_perf.pointer_down_received,
                                    "semantic_clicks": gpu_perf.semantic_clicks,
                                    "transition_begins": gpu_perf.transition_begins,
                                    "transition_ends": gpu_perf.transition_ends,
                                    "interval_avg_ms": format!("{:.1}", avg_interval),
                                    "interval_max_ms": format!("{:.1}", diag_interval_max_ms),
                                    "snapshot_avg_ms": format!("{:.2}", avg_snapshot),
                                    "snapshot_max_ms": format!("{:.2}", diag_snapshot_max_ms),
                                    "render_avg_ms": format!("{:.2}", avg_render),
                                    "render_max_ms": format!("{:.2}", diag_render_max_ms),
                                    "lock_wait_avg_ms": format!("{:.2}", avg_lock),
                                    "lock_wait_max_ms": format!("{:.2}", diag_lock_wait_max_ms),
                                    "diag_dropped": diag_dropped,
                                    "diag_skipped_static": diag_skipped_static,
                                    "diag_skipped_throttled": diag_skipped_throttled,
                                })
                            );
                            diag_frames = 0;
                            diag_interval_sum_ms = 0.0;
                            diag_interval_max_ms = 0.0;
                            diag_snapshot_sum_ms = 0.0;
                            diag_snapshot_max_ms = 0.0;
                            diag_render_sum_ms = 0.0;
                            diag_render_max_ms = 0.0;
                            diag_lock_wait_sum_ms = 0.0;
                            diag_lock_wait_max_ms = 0.0;
                            diag_dropped = 0;
                            diag_skipped_static = 0;
                            diag_skipped_throttled = 0;
                        }
                    }
                })
                .expect("start headless external render thread");

            let result = server
                .serve_until(
                    move |request| {
                    let response = match request.method.as_str() {
                        "render.surface.open" => {
                            let request_id = request.request_id;
                            match serde_json::from_value(request.params)
                                .map_err(|_| "invalid_surface_open".to_string())
                                .and_then(|open| gpu.lock().expect("gpu lock").open(open))
                            {
                                Ok(result) => runtime.lock().expect("runtime lock").accept(request_id, result),
                                Err(error) => runtime.lock().expect("runtime lock").reject(request_id, "external_surface_open_failed", &error, None),
                            }
                        }
                        "render.surface.acquire" => {
                            let request_id = request.request_id;
                            let surface_id = request.params.get("surface_id").and_then(Value::as_str).unwrap_or_default();
                            let pid = request.params.get("pid").and_then(Value::as_u64).unwrap_or_default() as u32;
                            match gpu.lock().expect("gpu lock").acquire(surface_id, pid) {
                                Ok(result) => runtime.lock().expect("runtime lock").accept(request_id, result),
                                Err(error) => runtime.lock().expect("runtime lock").reject(request_id, "external_surface_acquire_failed", &error, None),
                            }
                        }
                        "render.diagnostics" => {
                            let request_id = request.request_id;
                            let result = gpu.lock().expect("gpu lock").diagnostics();
                            runtime.lock().expect("runtime lock").accept(request_id, result)
                        }
                        "render.depth_probe" => {
                            let request_id = request.request_id;
                            let result = gpu.lock().expect("gpu lock").depth_probe();
                            runtime.lock().expect("runtime lock").accept(request_id, result)
                        }
                        "debug.external.images" => {
                            let request_id = request.request_id;
                            let result = gpu.lock().expect("gpu lock").external_image_debug();
                            runtime.lock().expect("runtime lock").accept(request_id, result)
                        }
                        "debug.external.color.sample" => {
                            let request_id = request.request_id;
                            let surface_id = request
                                .params
                                .get("surface_id")
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                            let points = request
                                .params
                                .get("points")
                                .and_then(Value::as_array)
                                .and_then(|points| {
                                    points
                                        .iter()
                                        .map(|point| {
                                            let point = point.as_array()?;
                                            Some([
                                                u32::try_from(point.first()?.as_u64()?).ok()?,
                                                u32::try_from(point.get(1)?.as_u64()?).ok()?,
                                            ])
                                        })
                                        .collect::<Option<Vec<_>>>()
                                });
                            match surface_id.zip(points) {
                                Some((surface_id, points)) => match gpu
                                    .lock()
                                    .expect("gpu lock")
                                    .sample_external_color_target(&surface_id, &points)
                                {
                                    Ok(result) => runtime
                                        .lock()
                                        .expect("runtime lock")
                                        .accept(request_id, result),
                                    Err(error) => runtime.lock().expect("runtime lock").reject(
                                        request_id,
                                        "external_color_sample_failed",
                                        &error,
                                        None,
                                    ),
                                },
                                None => runtime.lock().expect("runtime lock").reject(
                                    request_id,
                                    "invalid_request",
                                    "surface_id and u32 [x, y] points are required",
                                    None,
                                ),
                            }
                        }
                        "debug.unified_id.inspect" => {
                            let request_id = request.request_id;
                            let result = gpu.lock().expect("gpu lock").unified_id_inspect();
                            runtime.lock().expect("runtime lock").accept(request_id, result)
                        }
                        "ui.host.pointer_event" => {
                            let request_id = request.request_id;
                            let event = request
                                .params
                                .get("event")
                                .cloned()
                                .unwrap_or(request.params);
                            // Timestamp the request arrival to measure end-to-end
                            // pointer latency: RPC -> runtime lock -> snapshot ->
                            // gpu lock -> hit resolution. Printed on the Down
                            // path so we see press latency without spamming on
                            // every mouse-move sample.
                            let arrived_at = Instant::now();
                            match serde_json::from_value::<UiPointerEvent>(event)
                                .map_err(|error| format!("invalid_pointer_event: {error}"))
                                .and_then(|event| {
                                    let snapshot_start = Instant::now();
                                    let (mut surface_kind, surface_mapping) = {
                                        let gpu = gpu.lock().expect("gpu lock");
                                        match gpu.surface_kinds.get(&event.surface_id.0).copied() {
                                            Some(kind) => (kind, "registered"),
                                            None => (
                                                RenderSurfaceKind::ScreenUi,
                                                "missing_surface_mapping",
                                            ),
                                        }
                                    };
                                    let (_, combined_fragments, unified_hit_fragments) = runtime
                                        .lock()
                                        .expect("runtime lock")
                                        .external_surface_snapshots();
                                    let inferred_world = surface_mapping
                                        == "missing_surface_mapping"
                                        && combined_fragments.values().any(|fragment| {
                                            fragment.effects.iter().any(|effect| {
                                                matches!(
                                                    effect,
                                                    neon_ui_schema::UiEffect::CameraVisibility { .. }
                                                )
                                            })
                                        });
                                    if inferred_world {
                                        surface_kind = RenderSurfaceKind::WorldUi;
                                    }
                                    let snapshot_ms =
                                        snapshot_start.elapsed().as_secs_f32() * 1000.0;
                                    let gpu_wait_start = Instant::now();
                                    let result = if surface_mapping == "missing_surface_mapping"
                                        && !inferred_world
                                    {
                                        Err(format!(
                                            "pointer_surface_mapping_missing:surface_id={} known_surfaces={:?}",
                                            event.surface_id.0,
                                            gpu.lock()
                                                .expect("gpu lock")
                                                .surface_kinds
                                                .keys()
                                                .collect::<Vec<_>>(),
                                        ))
                                    } else {
                                        let mut gpu = loop {
                                            match gpu.try_lock() {
                                                Ok(guard) => break guard,
                                                Err(std::sync::TryLockError::WouldBlock) => {
                                                    std::thread::yield_now();
                                                    continue;
                                                }
                                                Err(_) => {
                                                    return Err(
                                                        "gpu_lock_poisoned".to_string(),
                                                    )
                                                }
                                            }
                                        };
                                        gpu.pointer_event(
                                            event,
                                            &unified_hit_fragments,
                                            surface_kind,
                                        )
                                    };
                                    let total_ms = arrived_at.elapsed().as_secs_f32() * 1000.0;
                                    if result
                                        .as_ref()
                                        .map(|value| {
                                            value
                                                .get("semantic_event")
                                                .or_else(|| value.get("state"))
                                                .is_some_and(|state| {
                                                    state
                                                        .as_str()
                                                        .is_some_and(|s| s == "captured")
                                                })
                                        })
                                        .unwrap_or(false)
                                    {
                                        eprintln!(
                                            "[neon-wgpu-runtime] pointer latency: total={:.2}ms snapshot={:.2}ms gpu_lock_wait={:.2}ms",
                                            total_ms,
                                            snapshot_ms,
                                            gpu_wait_start.elapsed().as_secs_f32() * 1000.0,
                                        );
                                    }
                                    result
                                })
                            {
                                Ok(result) => runtime.lock().expect("runtime lock").accept(request_id, result),
                                Err(error) => runtime.lock().expect("runtime lock").reject(request_id, "pointer_event_rejected", &error, None),
                            }
                        }
                        _ => runtime.lock().expect("runtime lock").handle(request),
                    };
                    response
                },
                |request| request.method == "service.shutdown",
            )
            .map_err(|error| error.to_string());

            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = render_thread.join();
            result
        })
        .expect("start headless external GPU server")
}

/// Finalizes a captured pointer using the normal release semantics. Both OS
/// release and debug activation use this so tests cannot bypass control state.
struct ReleasedBinding {
    binding: UiHitBinding,
    control_value: Option<neon_ui_schema::UiSemanticPayloadValue>,
    sequence: u64,
    local_presentation: Option<LocalPresentationCommit>,
}

fn release_captured_binding(gpu: &mut WindowGpu) -> Option<ReleasedBinding> {
    let initial_value = gpu.pending_control_value.take();
    let finished_value = gpu.ui.finish_value_gesture();
    let binding = gpu.captured_binding.take();
    let toggle_value = binding
        .as_ref()
        .and_then(|binding| gpu.ui.finish_toggle_control(&binding.node_path));
    let control_value = toggle_value
        .as_ref()
        .map(|(value, _)| value.clone())
        .or_else(|| finished_value.as_ref().map(|(value, _)| value.clone()))
        .or(initial_value);
    let local_presentation = toggle_value
        .map(|(_, presentation)| presentation)
        .or_else(|| finished_value.map(|(_, presentation)| presentation));
    if gpu.input.pointer_up(binding.is_some()).is_err() {
        if let Some(presentation) = local_presentation.as_ref() {
            gpu.ui.rollback_local_presentation(presentation);
        }
        return None;
    }
    let binding = binding?;
    if binding.text_input.is_some() {
        if let Some(presentation) = local_presentation.as_ref() {
            gpu.ui.rollback_local_presentation(presentation);
        }
        return None;
    }
    gpu.next_semantic_sequence += 1;
    Some(ReleasedBinding {
        binding,
        control_value,
        sequence: gpu.next_semantic_sequence,
        local_presentation,
    })
}

fn diagnostic_node_path(binding: &UiHitBinding) -> Option<String> {
    binding
        .data_grid_cell
        .is_none()
        .then(|| binding.node_path.clone())
}

fn semantic_target(binding: &UiHitBinding) -> InteractionSemanticTarget {
    InteractionSemanticTarget {
        node_path: binding.node_path.clone(),
    }
}

#[cfg(debug_assertions)]
fn create_final_target(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("neon3-window-final-composition-target"),
        size: wgpu::Extent3d {
            width: config.width.max(1),
            height: config.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: config.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

#[cfg(debug_assertions)]
fn capture_format_supported(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba8Unorm
            | wgpu::TextureFormat::Rgba8UnormSrgb
            | wgpu::TextureFormat::Bgra8Unorm
            | wgpu::TextureFormat::Bgra8UnormSrgb
    )
}

#[cfg(debug_assertions)]
fn texture_format_name(format: wgpu::TextureFormat) -> &'static str {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => "rgba8unorm",
        wgpu::TextureFormat::Rgba8UnormSrgb => "rgba8unorm-srgb",
        wgpu::TextureFormat::Bgra8Unorm => "bgra8unorm",
        wgpu::TextureFormat::Bgra8UnormSrgb => "bgra8unorm-srgb",
        _ => "unsupported",
    }
}

#[cfg(debug_assertions)]
fn normalize_capture_rgba(format: wgpu::TextureFormat, pixels: &mut [u8]) -> Result<(), String> {
    match format {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => Ok(()),
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            Ok(())
        }
        _ => Err(format!("unsupported window capture format: {format:?}")),
    }
}

#[cfg(debug_assertions)]
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(debug_assertions)]
fn default_capture_path(epoch: u64, frame: u64, revision: Revision) -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("resolve capture artifact directory: {error}"))?;
    let captured_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("resolve capture artifact timestamp: {error}"))?
        .as_millis();
    let target = executable
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "resolve target directory for capture artifacts".to_owned())?;
    Ok(target.join("neon-dev").join("captures").join(format!(
        "window-e{epoch}-f{frame}-r{}-{captured_at_unix_ms}.png",
        revision.0
    )))
}

#[cfg(debug_assertions)]
fn write_capture_png(path: &Path, size: [u32; 2], rgba: &[u8]) -> Result<PathBuf, String> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("png") {
        return Err("window capture artifact path must use the .png extension".into());
    }
    let expected_len = u64::from(size[0]) * u64::from(size[1]) * 4;
    if rgba.len() as u64 != expected_len {
        return Err(format!(
            "window capture contains {} RGBA bytes, expected {expected_len}",
            rgba.len()
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| "window capture artifact path has no parent directory".to_owned())?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "create window capture directory {}: {error}",
            parent.display()
        )
    })?;
    let file = std::fs::File::create(path)
        .map_err(|error| format!("create window capture artifact {}: {error}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), size[0], size[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("write window capture PNG header: {error}"))?;
    writer
        .write_image_data(rgba)
        .map_err(|error| format!("write window capture PNG pixels: {error}"))?;
    drop(writer);
    path.canonicalize().map_err(|error| {
        format!(
            "resolve window capture artifact {}: {error}",
            path.display()
        )
    })
}

impl WindowGpu {
    #[cfg(windows)]
    fn encode_external_surfaces(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        fragments: &HashMap<UiFragmentId, UiFragment>,
    ) -> Result<(), String> {
        for shared in self.external_surfaces.values_mut() {
            eprintln!(
                "[neon-wgpu-runtime] export color surface size={}x{} fragments={} previous_frame={}",
                shared.width,
                shared.height,
                fragments.len(),
                shared.frame_sequence
            );
            let view = shared
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-external-host-surface-pass"),
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
            self.ui.draw(
                &self.device,
                &self.queue,
                &mut pass,
                fragments,
                [shared.width, shared.height],
                HEADLESS_UI_LOGICAL_SIZE,
                self.started_at.elapsed().as_secs_f32(),
                UiDrawMode::All,
            );
            drop(pass);
            shared.frame_sequence = shared.frame_sequence.saturating_add(1);
            let fence_value = shared.frame_sequence;
            let hal_queue = unsafe { self.queue.as_hal::<wgpu::hal::api::Dx12>() }
                .ok_or_else(|| "dx12_queue_unavailable".to_owned())?;
            hal_queue.add_signal_fence(shared.fence.clone(), fence_value);
        }
        for (surface_id, shared) in self.external_id_surfaces.iter_mut() {
            let view = shared
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-external-host-id-surface-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let fragments = fragments;
            self.ui.draw_hit_id(
                &self.device,
                &self.queue,
                &mut pass,
                fragments,
                [shared.width, shared.height],
                HEADLESS_UI_LOGICAL_SIZE,
                self.started_at.elapsed().as_secs_f32(),
            );
            drop(pass);
            shared.frame_sequence = self
                .external_surfaces
                .get(surface_id)
                .map_or(shared.frame_sequence.saturating_add(1), |color| {
                    color.frame_sequence
                });
            let hal_queue = unsafe { self.queue.as_hal::<wgpu::hal::api::Dx12>() }
                .ok_or_else(|| "dx12_queue_unavailable".to_owned())?;
            hal_queue.add_signal_fence(shared.fence.clone(), shared.frame_sequence);
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn encode_external_surfaces(
        &mut self,
        _encoder: &mut wgpu::CommandEncoder,
        _fragments: &HashMap<UiFragmentId, UiFragment>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn render_world_ui_lab_panel(&mut self) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neon3-world-ui-lab-panel-capture"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-world-ui-lab-panel-capture-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.world_ui_lab_panel,
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
            self.ui.draw(
                &self.device,
                &self.queue,
                &mut pass,
                &self.world_ui_lab_fragment,
                WORLD_UI_LAB_PANEL_SIZE,
                [
                    WORLD_UI_LAB_LOGICAL_SIZE[0] as f32,
                    WORLD_UI_LAB_LOGICAL_SIZE[1] as f32,
                ],
                self.started_at.elapsed().as_secs_f32(),
                UiDrawMode::All,
            );
        }
        self.queue.submit(Some(encoder.finish()));
    }

    fn physical_viewport_size(&self) -> [u32; 2] {
        [self.config.width.max(1), self.config.height.max(1)]
    }

    fn logical_viewport_size(&self) -> [f32; 2] {
        let scale_factor = self.scale_factor.max(f64::EPSILON);
        [
            (f64::from(self.config.width.max(1)) / scale_factor) as f32,
            (f64::from(self.config.height.max(1)) / scale_factor) as f32,
        ]
    }

    #[cfg(debug_assertions)]
    fn read_final_target_rgba(&mut self) -> Result<Vec<u8>, String> {
        let size = self.physical_viewport_size();
        let row_bytes = size[0]
            .checked_mul(4)
            .ok_or_else(|| "window capture row size overflowed".to_owned())?;
        let padded_bytes_per_row = row_bytes
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .checked_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .ok_or_else(|| "window capture row alignment overflowed".to_owned())?;
        let buffer_size = u64::from(padded_bytes_per_row)
            .checked_mul(u64::from(size[1]))
            .ok_or_else(|| "window capture buffer size overflowed".to_owned())?;
        if buffer_size > MAX_WINDOW_CAPTURE_BYTES {
            return Err(format!(
                "window capture requires {buffer_size} bytes, above the {MAX_WINDOW_CAPTURE_BYTES} byte debug limit"
            ));
        }
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neon3-final-composition-readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("neon3-final-composition-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.final_target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(size[1]),
                },
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        let (mapped_tx, mapped_rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = mapped_tx.send(result);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|error| format!("wait for window capture readback: {error}"))?;
        mapped_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "window capture readback mapping timed out".to_owned())?
            .map_err(|error| format!("map window capture readback: {error}"))?;
        let mapped = readback
            .slice(..)
            .get_mapped_range()
            .map_err(|error| format!("read window capture mapping: {error}"))?;
        let mut rgba = Vec::with_capacity((row_bytes as usize) * (size[1] as usize));
        for row in mapped.chunks_exact(padded_bytes_per_row as usize) {
            rgba.extend_from_slice(&row[..row_bytes as usize]);
        }
        drop(mapped);
        readback.unmap();
        normalize_capture_rgba(self.config.format, &mut rgba)?;
        Ok(rgba)
    }

    fn new(
        window: &Window,
        instance: wgpu::Instance,
        world_ui_lab_camera: Arc<Mutex<WorldUiLabCameraController>>,
    ) -> Result<Self, String> {
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
        #[cfg(windows)]
        let dx12_adapter = dx12_interop::adapter_info(&adapter)
            .map_err(|error| format!("inspect DX12 adapter for external host interop: {error}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-wgpu-runtime-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|error| format!("request device: {error}"))?;
        let capabilities = surface.get_capabilities(&adapter);
        #[cfg(debug_assertions)]
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb() && capture_format_supported(*format))
            .or_else(|| {
                capabilities
                    .formats
                    .iter()
                    .copied()
                    .find(|format| capture_format_supported(*format))
            })
            .ok_or_else(|| "surface reported no capturable 8-bit RGBA/BGRA format".to_owned())?;
        #[cfg(not(debug_assertions))]
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
        #[cfg(debug_assertions)]
        let (final_target, final_target_view) = create_final_target(&device, &config);
        #[cfg(debug_assertions)]
        let final_target_blitter = wgpu::util::TextureBlitter::new(&device, config.format);
        let ui = UiWgpuRenderer::new(&device, config.format);
        let mut ui = ui;
        // Keep the normal UI pipeline in logical units while supersampling its private
        // transparent texture for the world quad's linear sampler.
        let world_ui_lab_panel = ui.ensure_ui_render_surface(
            &device,
            WORLD_UI_LAB_PANEL_TARGET,
            WORLD_UI_LAB_PANEL_SIZE,
        );
        let world_ui_lab_surface = ui.ensure_render_surface(
            &device,
            WORLD_UI_LAB_SURFACE_TARGET,
            WORLD_UI_LAB_PREVIEW_SIZE,
        );
        let world_ui_lab_depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-world-ui-lab-preview-depth"),
            size: wgpu::Extent3d {
                width: WORLD_UI_LAB_PREVIEW_SIZE[0],
                height: WORLD_UI_LAB_PREVIEW_SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let world_ui_lab_depth_view = world_ui_lab_depth.create_view(&Default::default());
        let world_ui = WorldUiPipeline::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let heightmap_preview = HeightmapPreviewConverter::new(&device);
        // AI pipeline creation is expensive and the DX12 FXC backend cannot
        // compile the optional conv2d path on every adapter. Windowed UI cases
        // do not need terrain AI, so keep it lazy and opt in explicitly.
        let mut ai = if std::env::var("NEON_ENABLE_AI").as_deref() == Ok("1") {
            Some(neon_wgpu_ai::AiEngine::new(device.clone(), queue.clone()))
        } else {
            None
        };
        let configured_pack = std::env::var_os("NEON_AI_PACK")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                let path = std::path::PathBuf::from("assets/ai/terrain_run1/terrain_run1.pack");
                path.exists().then_some(path)
            });
        if let (Some(ai), Some(path)) = (ai.as_mut(), configured_pack) {
            match std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    ai.load_model(&bytes)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                }) {
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
            adapter,
            #[cfg(windows)]
            dx12_adapter,
            #[cfg(windows)]
            external_surfaces: HashMap::new(),
            #[cfg(windows)]
            external_id_surfaces: HashMap::new(),
            #[cfg(windows)]
            external_handle_tokens: HashMap::new(),
            config,
            scale_factor: window.scale_factor(),
            #[cfg(debug_assertions)]
            final_target,
            #[cfg(debug_assertions)]
            final_target_view,
            #[cfg(debug_assertions)]
            final_target_blitter,
            #[cfg(debug_assertions)]
            final_target_valid: false,
            #[cfg(debug_assertions)]
            final_composition_revision: Revision(0),
            ui,
            world_ui,
            world_ui_lab_panel,
            world_ui_lab_surface,
            _world_ui_lab_depth: world_ui_lab_depth,
            world_ui_lab_depth: world_ui_lab_depth_view,
            world_ui_lab_fragment: world_ui_lab_fragment(),
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
            control_down: false,
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
            world_ui_lab_camera,
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
        let Some(ai) = self.ai.as_mut() else {
            return Err("ai_disabled".into());
        };
        if !ai.has_model() {
            return Err("ai_model_not_loaded".into());
        }
        let generation = ai
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

    #[cfg(windows)]
    fn open_external_surface(&mut self, open: RenderSurfaceOpen) -> Result<Value, String> {
        eprintln!(
            "[neon-wgpu-runtime] render.surface.open surface={} kind={:?} size={}x{} targets={}",
            open.surface_id,
            open.kind,
            open.size.width,
            open.size.height,
            open.targets.len()
        );
        if open.session_id.trim().is_empty() || open.surface_id.trim().is_empty() {
            return Err("invalid_surface_identity".into());
        }
        if self.external_surfaces.contains_key(&open.surface_id) {
            return Err("surface_already_open".into());
        }
        if open.kind == RenderSurfaceKind::WorldUi && open.placement.is_none() {
            return Err("world_ui_placement_required".into());
        }
        if open.buffer_count == 0 || open.buffer_count > 3 {
            return Err("invalid_surface_buffer_count".into());
        }
        if open.format != "rgba8unorm" {
            return Err("format_unsupported".into());
        }
        if open.buffer_count != 1 {
            return Err("buffer_ring_not_ready".into());
        }
        let id_target = open
            .targets
            .iter()
            .find(|target| target.kind == neon_protocol::RenderSurfaceTargetKind::Id);
        if let Some(id_target) = id_target
            && id_target.format != "r32uint"
        {
            return Err("id_target_format_unsupported".into());
        }
        let shared = dx12_interop::create_shared_surface(
            &self.device,
            &self.adapter,
            open.size.width.max(1),
            open.size.height.max(1),
            wgpu::TextureFormat::Rgba8Unorm,
        )
        .map_err(|error| error.to_string())?;
        let generation = 1;
        let surface_id = open.surface_id.clone();
        self.external_surfaces.insert(surface_id, shared);
        if id_target.is_some() {
            let id_surface = dx12_interop::create_shared_surface(
                &self.device,
                &self.adapter,
                open.size.width.max(1),
                open.size.height.max(1),
                wgpu::TextureFormat::R32Uint,
            )
            .map_err(|error| error.to_string())?;
            self.external_id_surfaces
                .insert(open.surface_id.clone(), id_surface);
            let id_texture_token =
                format!("surface:{}:id-texture:g{}", open.surface_id, generation);
            let id_fence_token = format!("surface:{}:id-fence:g{}", open.surface_id, generation);
            self.external_handle_tokens.insert(
                format!("{}:id", open.surface_id),
                (id_texture_token.clone(), id_fence_token.clone()),
            );
        }
        let texture_token = format!("surface:{}:texture:g{}", open.surface_id, generation);
        let fence_token = format!("surface:{}:fence:g{}", open.surface_id, generation);
        self.external_handle_tokens.insert(
            open.surface_id.clone(),
            (texture_token.clone(), fence_token.clone()),
        );
        Ok(json!({
            "session_id": open.session_id,
            "surface_id": open.surface_id,
            "generation": generation,
            "transport": "d3d12_shared_texture_v1",
            "adapter_luid": self.dx12_adapter.luid,
            "texture": {
                "format": open.format,
                "size": open.size,
                "mip_levels": 1,
                "buffer_index": 0,
                "broker_token": texture_token
            },
            "fence": {
                "kind": "d3d12_shared_fence",
                "broker_token": fence_token,
                "initial_value": 0
            },
            "targets": {
                "color_target_id": open.targets.iter().find(|target| target.kind == neon_protocol::RenderSurfaceTargetKind::Color).map(|target| target.target_id.clone()).unwrap_or_else(|| open.surface_id.clone()),
                "id_target_id": id_target.map(|target| target.target_id.clone())
            }
        }))
    }

    #[cfg(windows)]
    fn acquire_external_surface(&mut self, surface_id: &str, pid: u32) -> Result<Value, String> {
        eprintln!(
            "[neon-wgpu-runtime] render.surface.acquire surface={} consumer_pid={}",
            surface_id, pid
        );
        if pid == 0 {
            return Err("invalid_consumer_pid".into());
        }
        let Some(shared) = self.external_surfaces.get(surface_id) else {
            return Err("surface_not_found".into());
        };
        let Some((texture_token, fence_token)) = self.external_handle_tokens.get(surface_id) else {
            return Err("surface_broker_token_not_found".into());
        };
        let texture_handle = dx12_interop::duplicate_handle_to_process(shared.texture_handle, pid)
            .map_err(|error| error.to_string())?;
        let fence_handle = dx12_interop::duplicate_handle_to_process(shared.fence_handle, pid)
            .map_err(|error| error.to_string())?;
        let mut result = json!({
            "surface_id": surface_id,
            "pid": pid,
            "texture_token": texture_token,
            "fence_token": fence_token,
            "texture_handle": texture_handle,
            "fence_handle": fence_handle
        });
        if let Some(id_surface) = self.external_id_surfaces.get(surface_id) {
            let id_texture_handle =
                dx12_interop::duplicate_handle_to_process(id_surface.texture_handle, pid)
                    .map_err(|error| error.to_string())?;
            let id_fence_handle =
                dx12_interop::duplicate_handle_to_process(id_surface.fence_handle, pid)
                    .map_err(|error| error.to_string())?;
            let Some((id_texture_token, id_fence_token)) = self
                .external_handle_tokens
                .get(&format!("{}:id", surface_id))
            else {
                return Err("id_surface_broker_token_not_found".into());
            };
            result["id_texture_token"] = json!(id_texture_token);
            result["id_fence_token"] = json!(id_fence_token);
            result["id_texture_handle"] = json!(id_texture_handle);
            result["id_fence_handle"] = json!(id_fence_handle);
        }
        Ok(result)
    }

    #[cfg(windows)]
    fn external_surface_frame_snapshot(&self, surface_id: &str) -> Result<Value, String> {
        let Some(shared) = self.external_surfaces.get(surface_id) else {
            return Err("surface_not_found".into());
        };
        Ok(json!({
            "surface_id": surface_id,
            "generation": 1,
            "frame_sequence": shared.frame_sequence,
            "buffer_index": 0,
            "fence_value": shared.frame_sequence
            ,"id_frame_sequence": self.external_id_surfaces.get(surface_id).map(|id| id.frame_sequence)
        }))
    }

    #[cfg(not(windows))]
    fn open_external_surface(&mut self, _open: RenderSurfaceOpen) -> Result<Value, String> {
        Err("backend_not_available".into())
    }

    #[cfg(not(windows))]
    fn acquire_external_surface(&mut self, _surface_id: &str, _pid: u32) -> Result<Value, String> {
        Err("backend_not_available".into())
    }

    #[cfg(not(windows))]
    fn external_surface_frame_snapshot(&self, _surface_id: &str) -> Result<Value, String> {
        Err("backend_not_available".into())
    }
}

fn create_hit_target(
    device: &wgpu::Device,
    size: PhysicalSize<u32>,
) -> (wgpu::Texture, wgpu::TextureView) {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(UI_HIT_TARGET),
        size: wgpu::Extent3d {
            width: size.width.max(1),
            height: size.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Uint,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    (target, view)
}

fn create_color_depth_target(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("neon3-ui-color-depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

fn world_ui_lab_fragment() -> HashMap<UiFragmentId, UiFragment> {
    let label = |id: &str, x, y, width, height, value: &str, color| UiNode {
        node_id: UiNodeId(id.into()),
        kind: UiNodeKind::Label,
        bounds: UiBounds {
            x,
            y,
            width,
            height,
        },
        layout: None,
        visible: true,
        enabled: false,
        text_key: None,
        text: Some(TextRef::Literal {
            value: value.into(),
        }),
        image: None,
        surface: None,
        style: UiStyle {
            background_color: color,
            border_color: [0.0; 4],
            border_width: 0.0,
            corner_radius: 2.0,
            opacity: 1.0,
        },
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        children: Vec::new(),
    };
    let root = UiNode {
        node_id: UiNodeId("world-ui-lab-panel".into()),
        kind: UiNodeKind::Panel,
        bounds: UiBounds {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 360.0,
        },
        layout: None,
        visible: true,
        enabled: false,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: UiStyle {
            background_color: [0.035, 0.06, 0.12, 0.96],
            border_color: [0.12, 0.72, 0.94, 1.0],
            border_width: 2.0,
            corner_radius: 8.0,
            opacity: 1.0,
        },
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        children: vec![
            label(
                "callsign",
                24.0,
                22.0,
                280.0,
                34.0,
                "VANGUARD-07",
                [0.04, 0.22, 0.34, 1.0],
            ),
            label(
                "class",
                24.0,
                62.0,
                300.0,
                26.0,
                "LEVEL 24 / ASSAULT",
                [0.02, 0.08, 0.16, 1.0],
            ),
            label(
                "health",
                24.0,
                116.0,
                270.0,
                42.0,
                "742 / 900",
                [0.08, 0.30, 0.17, 1.0],
            ),
            label(
                "shield",
                24.0,
                166.0,
                270.0,
                42.0,
                "SHIELD 180 / 250",
                [0.08, 0.18, 0.34, 1.0],
            ),
            label(
                "status",
                24.0,
                246.0,
                136.0,
                30.0,
                "ONLINE",
                [0.06, 0.36, 0.21, 1.0],
            ),
            label(
                "squad",
                170.0,
                246.0,
                150.0,
                30.0,
                "SQUAD LEAD",
                [0.26, 0.18, 0.04, 1.0],
            ),
            label(
                "sector",
                24.0,
                300.0,
                420.0,
                28.0,
                "SECTOR: ORBITAL RELAY",
                [0.08, 0.11, 0.20, 1.0],
            ),
        ],
    };
    HashMap::from([(
        UiFragmentId("world-ui-lab.panel".into()),
        UiFragment {
            fragment_id: UiFragmentId("world-ui-lab.panel".into()),
            revision: Revision(1),
            root,
            effects: Vec::new(),
        },
    )])
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
    proxy: Option<EventLoopProxy<WindowCommand>>,
    pending: Option<PendingLocalPresentationKey>,
) {
    let queued_at = Instant::now();
    let node_path = diagnostic_node_path(&binding);
    let Some(intent) = binding.intent.clone() else {
        if let Ok(mut state) = delivery.lock() {
            *state = json!({"state": "not_sent", "reason": "semantic_binding_missing"});
        }
        return;
    };
    if let Ok(mut state) = delivery.lock() {
        *state = json!({
            "state": "pending",
            "sequence": sequence,
            "node_path": node_path.clone(),
            "queued": true,
        });
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
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: format!("window-{renderer_epoch}"),
                pid: std::process::id(),
                origin: "neon-wgpu-runtime".into(),
            },
            target: ServiceName("ui-runtime".into()),
            method: "ui.host.inbound".into(),
            params: json!(&event),
            expected_revision: Some(event.fragment.revision),
            idempotency_key: Some(format!("wgpu-pointer-click:{renderer_epoch}:{sequence}")),
        };
        let rpc_result = RpcClient::connect(endpoint)
            .and_then(|client| client.with_timeout(Duration::from_millis(250)))
            .and_then(|mut client| client.call(&request));
        let elapsed_ms = queued_at.elapsed().as_secs_f64() * 1000.0;
        match rpc_result {
            Ok(response) if response.status == RpcStatus::Accepted => {
                if let Ok(mut state) = delivery.lock() {
                    *state = json!({
                        "state": "accepted",
                        "sequence": sequence,
                        "node_path": node_path,
                        "revision": response.revision,
                        "elapsed_ms": elapsed_ms,
                    });
                }
                if let (Some(interaction_id), Ok(mut traces)) =
                    (interaction_id.clone(), traces.lock())
                {
                    traces.append(
                        interaction_id.clone(),
                        InteractionTraceStage::DeliveryAccepted,
                        InteractionTraceOutcome::Accepted,
                        None,
                        Some(semantic_target.clone()),
                        Some(fragment_revision),
                        composition_revision,
                        Some(request_id),
                    );
                    traces.delivery_accepted(interaction_id);
                }
                report_semantic_delivery(
                    proxy.as_ref(),
                    pending.as_ref(),
                    SemanticDeliveryOutcome::Accepted,
                );
            }
            Ok(response) => {
                let error = response.error;
                if let Ok(mut state) = delivery.lock() {
                    *state = json!({
                        "state": "rejected",
                        "sequence": sequence,
                        "node_path": node_path,
                        "error": error,
                        "elapsed_ms": elapsed_ms,
                    });
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
                    traces.append(
                        interaction_id,
                        InteractionTraceStage::DeliveryRejected,
                        InteractionTraceOutcome::Rejected,
                        Some(error),
                        Some(semantic_target.clone()),
                        Some(fragment_revision),
                        composition_revision,
                        Some(request_id),
                    );
                }
                report_semantic_delivery(
                    proxy.as_ref(),
                    pending.as_ref(),
                    SemanticDeliveryOutcome::Rejected,
                );
            }
            Err(error) => {
                if let Ok(mut state) = delivery.lock() {
                    *state = json!({
                        "state": "transport_failed",
                        "sequence": sequence,
                        "node_path": node_path,
                        "error": error.to_string(),
                        "elapsed_ms": elapsed_ms,
                    });
                }
                if let (Some(interaction_id), Ok(mut traces)) = (interaction_id, traces.lock()) {
                    traces.append(
                        interaction_id,
                        InteractionTraceStage::TransportFailed,
                        InteractionTraceOutcome::Failed,
                        Some(InteractionTraceError {
                            code: "transport_failed".into(),
                            message: error.to_string(),
                        }),
                        Some(semantic_target),
                        Some(fragment_revision),
                        composition_revision,
                        Some(request_id),
                    );
                }
                report_semantic_delivery(
                    proxy.as_ref(),
                    pending.as_ref(),
                    SemanticDeliveryOutcome::TransportFailed,
                );
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
        sequence,
        binding,
        value,
    ) else {
        return;
    };
    thread::spawn(move || {
        let request_id = RequestId(event.event_id.clone());
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: format!("window-{renderer_epoch}"),
                pid: std::process::id(),
                origin: "neon-wgpu-runtime".into(),
            },
            target: ServiceName("ui-runtime".into()),
            method: "ui.host.inbound".into(),
            params: json!(&event),
            expected_revision: Some(event.fragment.revision),
            idempotency_key: Some(format!("wgpu-text-input:{renderer_epoch}:{sequence}")),
        };
        if let Err(error) =
            RpcClient::connect(endpoint).and_then(|mut client| client.call(&request))
        {
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
    interaction_id: Option<InteractionId>,
    resolved: ui_renderer::UiResolvedDragDrop,
    delivery: Arc<Mutex<Value>>,
    traces: Arc<Mutex<InteractionTraceStore>>,
    proxy: Option<EventLoopProxy<WindowCommand>>,
    pending: PendingLocalPresentationKey,
) {
    let request_id = RequestId(format!("wgpu-drag-drop-{sequence}"));
    let semantic_target = InteractionSemanticTarget {
        node_path: resolved.target_key.clone(),
    };
    let semantic_intent = semantic_intent_name(&resolved.intent);
    let fragment_revision = resolved.fragment.revision;
    if let Ok(mut current) = delivery.lock() {
        *current = json!({
            "state": "pending",
            "sequence": sequence,
            "source_key": resolved.source_key,
            "target_key": resolved.target_key,
        });
    }
    if let (Some(interaction_id), Ok(mut traces)) = (interaction_id.clone(), traces.lock()) {
        traces.append_with_intent(
            interaction_id,
            InteractionTraceStage::SemanticEventForwarded,
            InteractionTraceOutcome::Pending,
            None,
            None,
            Some(semantic_target.clone()),
            Some(semantic_intent.clone()),
            Some(fragment_revision),
            composition_revision,
            Some(request_id.clone()),
        );
    }
    thread::spawn(move || {
        let event = UiSemanticEvent {
            event: neon_ui_schema::UiSemanticEventType::DragDrop,
            event_id: request_id.0.clone(),
            renderer_epoch,
            composition_revision,
            fragment: resolved.fragment.clone(),
            intent: resolved.intent,
            pointer: Some(neon_ui_schema::UiPointerMetadata { id: 0, sequence }),
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: Some(neon_ui_schema::UiDragDropPayload {
                source_key: resolved.source_key,
                target_key: resolved.target_key,
                placement: resolved.placement,
                presentation_template_key: resolved.presentation_template_key,
            }),
        };
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: format!("window-{renderer_epoch}"),
                pid: std::process::id(),
                origin: "neon-wgpu-runtime".into(),
            },
            target: ServiceName("ui-runtime".into()),
            method: "ui.host.inbound".into(),
            params: json!(&event),
            expected_revision: Some(event.fragment.revision),
            idempotency_key: Some(format!("wgpu-drag-drop:{renderer_epoch}:{sequence}")),
        };
        let (outcome, delivery_outcome) = match RpcClient::connect(endpoint)
            .and_then(|mut client| client.call(&request))
        {
            Ok(response) if response.status == RpcStatus::Accepted => {
                if let (Some(interaction_id), Ok(mut traces)) =
                    (interaction_id.clone(), traces.lock())
                {
                    traces.append_with_intent(
                        interaction_id.clone(),
                        InteractionTraceStage::DeliveryAccepted,
                        InteractionTraceOutcome::Accepted,
                        None,
                        None,
                        Some(semantic_target.clone()),
                        Some(semantic_intent.clone()),
                        Some(fragment_revision),
                        composition_revision,
                        Some(request_id.clone()),
                    );
                    traces.delivery_accepted(interaction_id);
                }
                (
                    json!({"state": "accepted", "response": response}),
                    SemanticDeliveryOutcome::Accepted,
                )
            }
            Ok(response) => {
                if let (Some(interaction_id), Ok(mut traces)) =
                    (interaction_id.clone(), traces.lock())
                {
                    let error = response
                        .error
                        .as_ref()
                        .map(|error| InteractionTraceError {
                            code: error.code.clone(),
                            message: error.message.clone(),
                        })
                        .unwrap_or(InteractionTraceError {
                            code: "delivery_rejected".into(),
                            message: "UI host rejected drag/drop delivery".into(),
                        });
                    traces.append_with_intent(
                        interaction_id,
                        InteractionTraceStage::DeliveryRejected,
                        InteractionTraceOutcome::Rejected,
                        Some(error),
                        None,
                        Some(semantic_target.clone()),
                        Some(semantic_intent.clone()),
                        Some(fragment_revision),
                        composition_revision,
                        Some(request_id.clone()),
                    );
                }
                (
                    json!({"state": "rejected", "response": response}),
                    SemanticDeliveryOutcome::Rejected,
                )
            }
            Err(error) => {
                if let (Some(interaction_id), Ok(mut traces)) = (interaction_id, traces.lock()) {
                    traces.append_with_intent(
                        interaction_id,
                        InteractionTraceStage::TransportFailed,
                        InteractionTraceOutcome::Failed,
                        Some(InteractionTraceError {
                            code: "transport_failed".into(),
                            message: error.to_string(),
                        }),
                        None,
                        Some(semantic_target),
                        Some(semantic_intent),
                        Some(fragment_revision),
                        composition_revision,
                        Some(request_id),
                    );
                }
                (
                    json!({"state": "transport_failed", "error": error.to_string()}),
                    SemanticDeliveryOutcome::TransportFailed,
                )
            }
        };
        if let Ok(mut current) = delivery.lock() {
            *current = outcome;
        }
        report_semantic_delivery(proxy.as_ref(), Some(&pending), delivery_outcome);
    });
}

fn report_semantic_delivery(
    proxy: Option<&EventLoopProxy<WindowCommand>>,
    pending: Option<&PendingLocalPresentationKey>,
    outcome: SemanticDeliveryOutcome,
) {
    if let Some(proxy) = proxy {
        let _ = proxy.send_event(WindowCommand::SemanticDeliveryCompleted {
            pending: pending.cloned(),
            outcome,
        });
    }
}

fn forward_data_grid_window_request(
    endpoint: SocketAddr,
    window_request: UiDataGridWindowRequest,
    delivery: Arc<Mutex<Value>>,
    proxy: Option<EventLoopProxy<WindowCommand>>,
) {
    thread::spawn(move || {
        let sequence = window_request.sequence;
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
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
            params: json!(UiHostInbound::WindowRequest {
                request: UiWindowRequest::DataGrid {
                    request: window_request
                }
            }),
        };
        let result = RpcClient::connect(endpoint).and_then(|mut client| client.call(&request));
        let accepted = matches!(&result, Ok(response) if response.status == RpcStatus::Accepted);
        if let Ok(mut delivery) = delivery.lock() {
            *delivery = match result {
                Ok(response) if response.status == RpcStatus::Accepted => {
                    json!({"state": "accepted", "sequence": sequence, "revision": response.revision})
                }
                Ok(response) => {
                    json!({"state": "rejected", "sequence": sequence, "error": response.error})
                }
                Err(error) => {
                    json!({"state": "transport_failed", "sequence": sequence, "error": error.to_string()})
                }
            };
        }
        if let Some(proxy) = proxy {
            let _ = proxy
                .send_event(WindowCommand::DataGridWindowDeliveryCompleted { sequence, accepted });
        }
    });
}

impl ApplicationHandler<WindowCommand> for WindowedRuntime {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            eprintln!("neon-wgpu window initialization failed: {error}");
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
            WindowEvent::DroppedFile(path) => self.publish_file_drop(path),
            WindowEvent::Resized(size) => self.resize(size),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.scale_factor = scale_factor;
                }
                if let Some(size) = self.window.as_ref().map(Window::inner_size) {
                    self.resize(size);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let mut scroll_changed = false;
                let mut drag_preview = None;
                if let Some(gpu) = self.gpu.as_mut() {
                    let logical = position.to_logical::<f32>(gpu.scale_factor);
                    gpu.ui.set_pointer_position([logical.x, logical.y]);
                    if let Ok(mut camera) = self.world_ui_lab_camera.lock()
                        && let Some(sample) = camera.pointer_moved(
                            [logical.x, logical.y],
                            self.epoch,
                            gpu.started_at.elapsed(),
                        )
                    {
                        camera.send(&sample);
                        self.redraw_pending = true;
                    }
                    if gpu.text_selection_drag {
                        gpu.ui
                            .set_text_input_caret_from_pointer([logical.x, logical.y], true);
                    }
                    if gpu.ui.drag_active() && gpu.ui.update_drag_preview() {
                        drag_preview =
                            gpu.active_interaction_id
                                .clone()
                                .and_then(|interaction_id| {
                                    gpu.ui.active_drag_semantic_source().map(
                                        |(source_key, fragment)| {
                                            (interaction_id, source_key, fragment.revision)
                                        },
                                    )
                                });
                    }
                    scroll_changed = gpu.ui.update_scroll_drag() || gpu.ui.update_scroll_pan();
                    if scroll_changed {
                        self.redraw_pending = true;
                    }
                    gpu.ui.update_value_gesture();
                    let x = (position.x * gpu.scale_factor)
                        .max(0.0)
                        .min(gpu.config.width.saturating_sub(1) as f64)
                        as u32;
                    let y = (position.y * gpu.scale_factor)
                        .max(0.0)
                        .min(gpu.config.height.saturating_sub(1) as f64)
                        as u32;
                    gpu.pending_hit_pixel = Some([x, y]);
                    self.redraw_pending = true;
                }
                if let Some((interaction_id, source_key, fragment_revision)) = drag_preview {
                    append_drag_interaction_record(
                        &self.interaction_traces,
                        interaction_id,
                        InteractionTraceStage::DragPreviewMoved,
                        InteractionTraceOutcome::Pending,
                        Some(source_key),
                        None,
                        fragment_revision,
                        self.applied_composition_revision,
                        None,
                    );
                }
                let data_grid_thumb_drag = self
                    .gpu
                    .as_ref()
                    .is_some_and(|gpu| gpu.ui.data_grid_scroll_drag_active());
                if scroll_changed && !data_grid_thumb_drag {
                    self.schedule_data_grid_window_requests();
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == winit::event::ElementState::Pressed
                    && button == winit::event::MouseButton::Middle =>
            {
                self.prepare_pointer_interaction();
                if self
                    .gpu
                    .as_mut()
                    .is_some_and(|gpu| gpu.ui.begin_scroll_pan_at_pointer())
                {
                    self.redraw_pending = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == winit::event::ElementState::Pressed
                    && button == winit::event::MouseButton::Left =>
            {
                if let Some(gpu) = self.gpu.as_ref()
                    && let Some(pointer) = gpu.ui.pointer_position()
                    && let Ok(mut camera) = self.world_ui_lab_camera.lock()
                {
                    camera.surface_focused = camera.enabled
                        && camera.window_focused
                        && gpu
                            .ui
                            .render_surface_contains(WORLD_UI_LAB_SURFACE_TARGET, pointer);
                    if !camera.surface_focused {
                        camera.clear_axes();
                    }
                    #[cfg(debug_assertions)]
                    eprintln!(
                        "world-ui-lab camera focus: window={} surface={} target={}",
                        camera.window_focused, camera.surface_focused, WORLD_UI_LAB_SURFACE_TARGET
                    );
                    self.redraw_pending = true;
                }
                if let Ok(mut camera) = self.world_ui_lab_camera.lock() {
                    camera.set_drag(winit::event::MouseButton::Left, true);
                }
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
                    append_interaction_record(
                        &interaction_traces,
                        interaction_id,
                        InteractionTraceStage::HitCaptureResolved,
                        InteractionTraceOutcome::Rejected,
                        Some(InteractionTraceError {
                            code: "modal_outside_press".into(),
                            message: "pointer press was consumed by the active modal".into(),
                        }),
                        None,
                        composition_revision,
                    );
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
                if self
                    .gpu
                    .as_mut()
                    .is_some_and(|gpu| gpu.ui.begin_scroll_drag_at_pointer())
                {
                    append_interaction_record(
                        &interaction_traces,
                        interaction_id,
                        InteractionTraceStage::HitCaptureResolved,
                        InteractionTraceOutcome::Accepted,
                        None,
                        None,
                        composition_revision,
                    );
                    self.redraw_pending = true;
                    return;
                }
                let text_input = self.gpu.as_mut().and_then(|gpu| {
                    let input = gpu.ui.text_input_at_pointer()?;
                    // Text selection is renderer-local and must not inherit a
                    // stale semantic pointer capture from the preceding
                    // control. Reset the local pointer state before focusing
                    // the virtual-list cell.
                    gpu.input.cancel();
                    gpu.captured_binding = None;
                    gpu.active_interaction_id = None;
                    gpu.pending_control_value = None;
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
                    let rect = self
                        .gpu
                        .as_ref()
                        .and_then(|gpu| gpu.ui.text_input_ime_rect())
                        .unwrap_or(input.bounds);
                    window.set_ime_cursor_area(
                        LogicalPosition::new(rect.x, rect.y),
                        LogicalSize::new(rect.width.max(1.0), rect.height.max(1.0)),
                    );
                    append_interaction_record(
                        &interaction_traces,
                        interaction_id,
                        InteractionTraceStage::HitCaptureResolved,
                        InteractionTraceOutcome::Accepted,
                        None,
                        None,
                        composition_revision,
                    );
                } else if let Some(gpu) = self.gpu.as_mut() {
                    if let Some((binding, value)) = gpu
                        .ui
                        .dropdown_option_at_pointer()
                        .or_else(|| gpu.ui.tab_option_at_pointer())
                        .or_else(|| gpu.ui.list_option_at_pointer())
                    {
                        gpu.input.set_hover_id(Some(0));
                        let _ = gpu.input.pointer_down();
                        gpu.captured_binding = Some(binding);
                        gpu.active_interaction_id = Some(interaction_id.clone());
                        gpu.pending_control_value = Some(value);
                        let binding = gpu
                            .captured_binding
                            .clone()
                            .expect("binding was just assigned");
                        append_interaction_record(
                            &interaction_traces,
                            interaction_id,
                            InteractionTraceStage::HitCaptureResolved,
                            InteractionTraceOutcome::Accepted,
                            None,
                            Some(&binding),
                            composition_revision,
                        );
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
                        gpu.active_interaction_id = Some(interaction_id.clone());
                        if let Some((source_key, fragment)) = gpu.ui.active_drag_semantic_source() {
                            append_drag_interaction_record(
                                &interaction_traces,
                                interaction_id,
                                InteractionTraceStage::DragStarted,
                                InteractionTraceOutcome::Accepted,
                                Some(source_key),
                                None,
                                fragment.revision,
                                composition_revision,
                                None,
                            );
                        }
                        self.redraw_pending = true;
                    } else {
                        // Resolve the current composed visual and semantic binding
                        // together. Do not use an asynchronous GPU ID as the
                        // authoritative window-click target: that ID may belong
                        // to the previous frame after scroll, resize, or a
                        // presentation update.
                        let current_hit = gpu.ui.hit_binding_at_pointer();
                        gpu.input
                            .set_hover_id(current_hit.as_ref().map(|(id, _)| *id));
                        if gpu.input.pointer_down().is_err() {
                            gpu.last_pointer_outcome = "press_without_semantic_hit".into();
                            gpu.last_pointer_node_path = None;
                            append_interaction_record(
                                &interaction_traces,
                                interaction_id,
                                InteractionTraceStage::HitCaptureResolved,
                                InteractionTraceOutcome::Rejected,
                                Some(InteractionTraceError {
                                    code: "press_without_semantic_hit".into(),
                                    message: "pointer press did not resolve a semantic target"
                                        .into(),
                                }),
                                None,
                                composition_revision,
                            );
                            return;
                        }
                        gpu.captured_binding = current_hit.map(|(_, binding)| binding);
                        gpu.pending_control_value = gpu
                            .captured_binding
                            .as_ref()
                            .and_then(|binding| binding.control_value.clone());
                        if gpu.captured_binding.is_none() {
                            let _ = gpu.input.pointer_up(false);
                            gpu.last_pointer_outcome = "press_without_semantic_binding".into();
                            gpu.last_pointer_node_path = None;
                            append_interaction_record(
                                &interaction_traces,
                                interaction_id,
                                InteractionTraceStage::HitCaptureResolved,
                                InteractionTraceOutcome::Rejected,
                                Some(InteractionTraceError {
                                    code: "press_without_semantic_binding".into(),
                                    message: "captured hit did not resolve a semantic binding"
                                        .into(),
                                }),
                                None,
                                composition_revision,
                            );
                            return;
                        }
                        let binding = gpu.captured_binding.clone().expect("binding checked above");
                        gpu.active_interaction_id = Some(interaction_id.clone());
                        append_interaction_record(
                            &interaction_traces,
                            interaction_id,
                            InteractionTraceStage::HitCaptureResolved,
                            InteractionTraceOutcome::Accepted,
                            None,
                            Some(&binding),
                            composition_revision,
                        );
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
                if state == winit::event::ElementState::Pressed
                    && button == winit::event::MouseButton::Right =>
            {
                if let Ok(mut camera) = self.world_ui_lab_camera.lock() {
                    camera.set_drag(winit::event::MouseButton::Right, true);
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
                if let Ok(mut camera) = self.world_ui_lab_camera.lock() {
                    camera.set_drag(winit::event::MouseButton::Left, false);
                }
                if let Some(gpu) = self.gpu.as_mut()
                    && gpu.ui.scroll_drag_active()
                {
                    let released_data_grid = gpu.ui.end_scroll_drag();
                    self.redraw_pending = true;
                    if let Some(grid_path) = released_data_grid {
                        self.schedule_data_grid_window_requests_for(Some(&grid_path));
                    }
                    return;
                }
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.text_selection_drag = false;
                }
                let drag = self.gpu.as_mut().and_then(|gpu| {
                    if !gpu.ui.drag_active() {
                        return None;
                    }
                    let (source_key, fragment) = gpu.ui.active_drag_semantic_source()?;
                    let moved = gpu.ui.active_drag_moved();
                    let resolved = gpu.ui.finish_drag_at_pointer(&self.fragments);
                    gpu.next_semantic_sequence += 1;
                    let interaction_id = gpu.active_interaction_id.take()?;
                    Some((
                        gpu.next_semantic_sequence,
                        interaction_id,
                        source_key,
                        fragment.revision,
                        moved,
                        resolved,
                    ))
                });
                if let Some((
                    sequence,
                    interaction_id,
                    source_key,
                    fragment_revision,
                    moved,
                    resolved,
                )) = drag
                {
                    record_drag_release_lifecycle(
                        &self.interaction_traces,
                        &interaction_id,
                        &source_key,
                        fragment_revision,
                        moved,
                        resolved.as_ref(),
                        self.applied_composition_revision,
                    );
                    if let Some(resolved) = resolved {
                        if let (Some(endpoint), Some(proxy)) =
                            (self.ui_endpoint, self.event_proxy.clone())
                        {
                            let pending = self
                                .gpu
                                .as_mut()
                                .map(|gpu| {
                                    gpu.ui.retain_local_presentation(
                                        sequence,
                                        &resolved.fragment,
                                        resolved.local_presentation.clone(),
                                    )
                                })
                                .expect("window GPU exists while finishing a drag");
                            forward_drag_drop(
                                endpoint,
                                self.epoch,
                                self.applied_composition_revision,
                                sequence,
                                Some(interaction_id),
                                resolved,
                                self.pointer_delivery.clone(),
                                self.interaction_traces.clone(),
                                Some(proxy),
                                pending,
                            );
                        } else {
                            if let Some(gpu) = self.gpu.as_mut() {
                                gpu.ui
                                    .rollback_local_presentation(&resolved.local_presentation);
                            }
                            append_drag_interaction_record(
                                &self.interaction_traces,
                                interaction_id,
                                InteractionTraceStage::TransportFailed,
                                InteractionTraceOutcome::Failed,
                                Some(resolved.target_key),
                                Some(semantic_intent_name(&resolved.intent)),
                                fragment_revision,
                                self.applied_composition_revision,
                                Some(InteractionTraceError {
                                    code: "ui_host_unavailable".into(),
                                    message: "UI host endpoint is unavailable".into(),
                                }),
                            );
                        }
                    }
                    self.redraw_pending = true;
                    return;
                }
                let binding = self.gpu.as_mut().and_then(|gpu| {
                    let text_binding = gpu
                        .captured_binding
                        .as_ref()
                        .is_some_and(|binding| binding.text_input.is_some());
                    if text_binding {
                        let binding = gpu.captured_binding.take()?;
                        gpu.input.pointer_up(true).ok()?;
                        gpu.active_interaction_id = None;
                        gpu.pending_control_value = None;
                        return Some(Err(binding));
                    }
                    release_captured_binding(gpu).map(Ok)
                });
                if let Some(Err(binding)) = &binding
                    && let (Some(window), Some(input)) =
                        (self.window.as_ref(), binding.text_input.as_ref())
                {
                    window.set_ime_allowed(true);
                    let rect = self
                        .gpu
                        .as_ref()
                        .and_then(|gpu| gpu.ui.text_input_ime_rect())
                        .unwrap_or(input.bounds);
                    window.set_ime_cursor_area(
                        LogicalPosition::new(rect.x, rect.y),
                        LogicalSize::new(rect.width.max(1.0), rect.height.max(1.0)),
                    );
                }
                if let Some(Ok(released)) = binding {
                    if let Some(window) = self.window.as_ref() {
                        window.set_ime_allowed(false);
                    }
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.last_pointer_node_path = diagnostic_node_path(&released.binding);
                        gpu.last_pointer_outcome = if released.binding.intent.is_some() {
                            "semantic_event_forwarded".into()
                        } else {
                            "release_without_semantic_binding".into()
                        };
                    }
                    let interaction_id = self
                        .gpu
                        .as_mut()
                        .and_then(|gpu| gpu.active_interaction_id.take());
                    if let (Some(endpoint), Some(proxy)) =
                        (self.ui_endpoint, self.event_proxy.clone())
                        && released.binding.intent.is_some()
                    {
                        let pending = released.local_presentation.map(|presentation| {
                            self.gpu
                                .as_mut()
                                .expect("window GPU exists while releasing a control")
                                .ui
                                .retain_local_presentation(
                                    released.sequence,
                                    &released.binding.fragment,
                                    presentation,
                                )
                        });
                        forward_pointer_click(
                            endpoint,
                            self.epoch,
                            self.applied_composition_revision,
                            released.sequence,
                            interaction_id,
                            released.binding,
                            released.control_value,
                            self.pointer_delivery.clone(),
                            self.interaction_traces.clone(),
                            Some(proxy),
                            pending,
                        );
                    } else if let Some(presentation) = released.local_presentation.as_ref()
                        && let Some(gpu) = self.gpu.as_mut()
                    {
                        gpu.ui.rollback_local_presentation(presentation);
                    }
                    self.redraw_pending = true;
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == winit::event::ElementState::Released
                    && button == winit::event::MouseButton::Right =>
            {
                if let Ok(mut camera) = self.world_ui_lab_camera.lock() {
                    camera.set_drag(winit::event::MouseButton::Right, false);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = match delta {
                    MouseScrollDelta::LineDelta(x, y) => [x * 24.0, y * 24.0],
                    MouseScrollDelta::PixelDelta(position) => {
                        let scale_factor = self.gpu.as_ref().map_or(1.0, |gpu| gpu.scale_factor);
                        let logical = position.to_logical::<f32>(scale_factor);
                        [logical.x, logical.y]
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
                if let Ok(mut camera) = self.world_ui_lab_camera.lock()
                    && let Some(sample) = camera.wheel(
                        delta[1],
                        self.epoch,
                        self.gpu
                            .as_ref()
                            .map(|gpu| gpu.started_at.elapsed())
                            .unwrap_or_default(),
                    )
                {
                    camera.send(&sample);
                    self.redraw_pending = true;
                }
            }
            WindowEvent::Ime(winit::event::Ime::Enabled) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.ime_active = true;
                }
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
                    if result.is_some() {
                        gpu.next_semantic_sequence += 1;
                    }
                    result.map(|(binding, text)| (gpu.next_semantic_sequence, binding, text))
                });
                if let (Some(endpoint), Some((sequence, binding, value))) =
                    (self.ui_endpoint, committed)
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
                if let (Some(window), Some(rect)) = (
                    self.window.as_ref(),
                    self.gpu
                        .as_ref()
                        .and_then(|gpu| gpu.ui.text_input_ime_rect()),
                ) {
                    window.set_ime_cursor_area(
                        LogicalPosition::new(rect.x, rect.y),
                        LogicalSize::new(rect.width.max(1.0), rect.height.max(1.0)),
                    );
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
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key
                    && let Ok(mut camera) = self.world_ui_lab_camera.lock()
                {
                    let sample = camera.set_key(
                        code,
                        event.state == ElementState::Pressed,
                        self.epoch,
                        self.gpu
                            .as_ref()
                            .map(|gpu| gpu.started_at.elapsed())
                            .unwrap_or_default(),
                    );
                    #[cfg(debug_assertions)]
                    if matches!(
                        code,
                        KeyCode::KeyW
                            | KeyCode::KeyA
                            | KeyCode::KeyS
                            | KeyCode::KeyD
                            | KeyCode::KeyQ
                            | KeyCode::KeyE
                    ) {
                        eprintln!(
                            "world-ui-lab camera key: code={code:?} pressed={} enabled={} window={} surface={} axes={:?} position={:?} sample={}",
                            event.state == ElementState::Pressed,
                            camera.enabled,
                            camera.window_focused,
                            camera.surface_focused,
                            camera.axes,
                            camera.camera_position,
                            sample.as_ref().map_or(0, |value| value.sequence),
                        );
                    }
                    if let Some(sample) = sample {
                        camera.send(&sample);
                        self.redraw_pending = true;
                    }
                }
                if event.state != ElementState::Pressed {
                    return;
                }
                if self
                    .gpu
                    .as_ref()
                    .is_some_and(|gpu| gpu.control_down)
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyO))
                {
                    #[cfg(windows)]
                    match self.window.as_ref().map(choose_image_file) {
                        Some(Ok(Some(path))) => {
                            eprintln!(
                                "{{\"event\":\"ui.file_picker.selected\",\"source_path\":{}}}",
                                serde_json::to_string(&path.to_string_lossy()).unwrap_or_else(|_| "\"\"".into())
                            );
                            self.publish_file_drop(path);
                        }
                        Some(Ok(None)) => eprintln!("{{\"event\":\"ui.file_picker.cancelled\"}}"),
                        Some(Err(error)) => eprintln!(
                            "{{\"event\":\"ui.file_picker.failed\",\"error\":{}}}",
                            serde_json::to_string(&error).unwrap_or_else(|_| "\"unknown\"".into())
                        ),
                        None => {}
                    }
                    return;
                }
                if matches!(&event.logical_key, Key::Named(NamedKey::Escape)) {
                    let mut cancelled_drag = None;
                    let cancelled = self.gpu.as_mut().is_some_and(|gpu| {
                        if gpu.ui.drag_active() {
                            cancelled_drag = gpu.active_interaction_id.take().and_then(|id| {
                                gpu.ui
                                    .active_drag_semantic_source()
                                    .map(|(source, fragment)| (id, source, fragment.revision))
                            });
                        }
                        let active = gpu.ui.drag_active()
                            || gpu.ui.value_gesture_active()
                            || gpu.ui.data_grid_text_input_active();
                        let cancelled = gpu.ui.cancel_pending_local_presentations() || active;
                        gpu.ui.cancel_drag();
                        gpu.ui.cancel_value_gesture();
                        let cancelled_text = gpu.ui.cancel_data_grid_text_input();
                        if cancelled {
                            gpu.captured_binding = None;
                            gpu.pending_control_value = None;
                            gpu.text_selection_drag = false;
                            gpu.input.cancel();
                        }
                        if cancelled_text {
                            gpu.text_selection_drag = false;
                        }
                        cancelled
                    });
                    if let Some((interaction_id, source_key, fragment_revision)) = cancelled_drag {
                        append_drag_interaction_record(
                            &self.interaction_traces,
                            interaction_id,
                            InteractionTraceStage::DragCancelled,
                            InteractionTraceOutcome::Rejected,
                            Some(source_key),
                            None,
                            fragment_revision,
                            self.applied_composition_revision,
                            Some(InteractionTraceError {
                                code: "interaction_cancelled".into(),
                                message: "drag was cancelled explicitly".into(),
                            }),
                        );
                    }
                    if cancelled {
                        if let Some(window) = self.window.as_ref() {
                            window.set_ime_allowed(false);
                        }
                        self.redraw_pending = true;
                        return;
                    }
                }
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
                        gpu.text_selection_drag = false;
                        return None;
                    }
                    let result = match &event.logical_key {
                        Key::Named(NamedKey::Backspace) => gpu.ui.backspace_text_input(),
                        Key::Named(NamedKey::Delete) => gpu.ui.delete_text_input(),
                        Key::Named(NamedKey::ArrowLeft) => {
                            gpu.ui.move_text_input_cursor(-1, gpu.shift_down);
                            None
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            gpu.ui.move_text_input_cursor(1, gpu.shift_down);
                            None
                        }
                        Key::Named(NamedKey::Home) => {
                            gpu.ui.move_text_input_to_edge(false, gpu.shift_down);
                            None
                        }
                        Key::Named(NamedKey::End) => {
                            gpu.ui.move_text_input_to_edge(true, gpu.shift_down);
                            None
                        }
                        Key::Character(value) if !gpu.ime_active && event.text.is_some() => {
                            gpu.ui.commit_ime_text(value)
                        }
                        _ => None,
                    };
                    if result.is_some() {
                        gpu.next_semantic_sequence += 1;
                    }
                    result.map(|(binding, text)| (gpu.next_semantic_sequence, binding, text))
                });
                if let (Some(endpoint), Some((sequence, binding, value))) =
                    (self.ui_endpoint, committed)
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
                if ending_data_grid_edit && let Some(window) = self.window.as_ref() {
                    window.set_ime_allowed(false);
                }
                if let (Some(window), Some(rect)) = (
                    self.window.as_ref(),
                    self.gpu
                        .as_ref()
                        .and_then(|gpu| gpu.ui.text_input_ime_rect()),
                ) {
                    window.set_ime_cursor_area(
                        LogicalPosition::new(rect.x, rect.y),
                        LogicalSize::new(rect.width.max(1.0), rect.height.max(1.0)),
                    );
                }
                self.redraw_pending = true;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.shift_down = modifiers.state().shift_key();
                    gpu.control_down = modifiers.state().control_key();
                }
            }
            WindowEvent::Focused(false) => {
                if let Ok(mut camera) = self.world_ui_lab_camera.lock() {
                    camera.window_focused = false;
                    camera.surface_focused = false;
                    camera.clear_axes();
                }
                let mut cancelled_drag = None;
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
                if let Some(gpu) = self.gpu.as_mut() {
                    if gpu.ui.drag_active() {
                        cancelled_drag =
                            gpu.active_interaction_id.take().and_then(|interaction_id| {
                                gpu.ui.active_drag_semantic_source().map(
                                    |(source_key, fragment)| {
                                        (interaction_id, source_key, fragment.revision)
                                    },
                                )
                            });
                    }
                    gpu.ui.clear_text_focus();
                    gpu.ui.cancel_drag();
                    gpu.ui.cancel_value_gesture();
                    gpu.ui.cancel_pending_local_presentations();
                    gpu.ui.cancel_scroll_drag();
                    gpu.ui.end_scroll_pan();
                    gpu.captured_binding = None;
                    gpu.pending_control_value = None;
                    gpu.input.cancel();
                    self.redraw_pending = true;
                }
                if let Some((interaction_id, source_key, fragment_revision)) = cancelled_drag {
                    append_drag_interaction_record(
                        &self.interaction_traces,
                        interaction_id,
                        InteractionTraceStage::DragCancelled,
                        InteractionTraceOutcome::Rejected,
                        Some(source_key),
                        None,
                        fragment_revision,
                        self.applied_composition_revision,
                        Some(InteractionTraceError {
                            code: "window_focus_lost".into(),
                            message: "drag was cancelled when the window lost focus".into(),
                        }),
                    );
                }
            }
            WindowEvent::Focused(true) => {
                if let Ok(mut camera) = self.world_ui_lab_camera.lock() {
                    camera.window_focused = true;
                    camera.clear_axes();
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
            WindowCommand::Fragments {
                composition_revision,
                fragments,
                applied,
            } => {
                let accepted = self.apply_fragments(composition_revision, fragments);
                if let Some(applied) = applied {
                    self.pending_composition_acks
                        .push_back((composition_revision, applied));
                    self.redraw_pending = true;
                }
                if accepted {
                    self.request_scripted_initial_size();
                }
            }
            WindowCommand::GenerateTerrainPreview {
                command,
                job_id,
                completed,
            } => {
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
                let status = self
                    .gpu
                    .as_ref()
                    .and_then(|gpu| gpu.ai.as_ref().and_then(|ai| ai.model_info()));
                let _ = completed.send(status);
            }
            WindowCommand::RegisterWorldUiLabCamera {
                registration,
                completed,
            } => {
                let result = self
                    .world_ui_lab_camera
                    .lock()
                    .map_err(|_| "world_ui_lab_camera_unavailable")
                    .and_then(|mut camera| {
                        if !camera.enabled {
                            return Err("world_ui_lab_camera_disabled");
                        }
                        camera.register(registration)?;
                        Ok(world_ui_lab_camera_status(&camera))
                    });
                let _ = completed.send(result);
            }
            WindowCommand::OpenExternalSurface { open, completed } => {
                let result = self
                    .gpu
                    .as_mut()
                    .ok_or_else(|| "window_gpu_unavailable".to_owned())
                    .and_then(|gpu| gpu.open_external_surface(open));
                if result.is_ok() {
                    self.redraw_pending = true;
                }
                let _ = completed.send(result);
            }
            WindowCommand::AcquireExternalSurface {
                surface_id,
                pid,
                completed,
            } => {
                let result = self
                    .gpu
                    .as_mut()
                    .ok_or_else(|| "window_gpu_unavailable".to_owned())
                    .and_then(|gpu| gpu.acquire_external_surface(&surface_id, pid));
                let _ = completed.send(result);
            }
            WindowCommand::ExternalSurfaceFrameSnapshot {
                surface_id,
                completed,
            } => {
                let result = self
                    .gpu
                    .as_ref()
                    .ok_or_else(|| "window_gpu_unavailable".to_owned())
                    .and_then(|gpu| gpu.external_surface_frame_snapshot(&surface_id));
                let _ = completed.send(result);
            }
            WindowCommand::ExternalPointerEvent { event, completed } => {
                let result = self.external_pointer_event(event);
                let _ = completed.send(result);
            }
            WindowCommand::InputDebugSnapshot { completed } => {
                let _ = completed.send(self.input_debug_snapshot());
            }
            WindowCommand::ImageDebugSnapshot { completed } => {
                let value = self.gpu.as_ref().map_or_else(
                    || json!({"error": "window_gpu_unavailable"}),
                    |gpu| gpu.ui.image_debug_snapshot(),
                );
                let _ = completed.send(value);
            }
            WindowCommand::UploadExternalImage { source, completed } => {
                let result = self
                    .gpu
                    .as_mut()
                    .ok_or_else(|| "window_gpu_unavailable".to_string())
                    .and_then(|gpu| {
                        gpu.ui
                            .preload_external_image(&gpu.device, &gpu.queue, &source)
                            .map(|texture| {
                                json!({
                                    "state": "ready",
                                    "producer": "ui-runtime",
                                    "consumer": "wgpu-runtime",
                                    "gpu_owner": "neon-wgpu-runtime-window",
                                    "texture": texture,
                                })
                            })
                            .map_err(str::to_owned)
                    });
                let _ = completed.send(result);
            }
            WindowCommand::InputDebugProbe {
                logical_position,
                physical_position,
                completed,
            } => {
                let _ = completed.send(self.input_debug_probe(logical_position, physical_position));
            }
            WindowCommand::InputDebugActivate {
                logical_position,
                completed,
            } => {
                let _ = completed.send(self.input_debug_activate(logical_position));
            }
            WindowCommand::InputDebugActivateTarget {
                semantic_node_path,
                completed,
            } => {
                let _ = completed.send(self.input_debug_activate_target(semantic_node_path));
            }
            WindowCommand::InputDebugScrollToMax {
                semantic_node_path,
                completed,
            } => {
                let _ = completed.send(self.input_debug_scroll_to_max(semantic_node_path));
            }
            WindowCommand::InputDebugValueGesture {
                semantic_node_path,
                target_fraction,
                completed,
            } => {
                let _ = completed
                    .send(self.input_debug_value_gesture(semantic_node_path, target_fraction));
            }
            WindowCommand::InputDebugDragGesture {
                source_node_key,
                target_node_key,
                completed,
            } => {
                let _ =
                    completed.send(self.input_debug_drag_gesture(source_node_key, target_node_key));
            }
            WindowCommand::CaptureFinalTarget {
                artifact_path,
                redraw,
                completed,
            } => {
                if redraw && let Err(error) = self.redraw() {
                    let _ = completed.send(Err(error));
                    return;
                }
                let _ = completed.send(self.capture_final_target(artifact_path));
            }
            WindowCommand::FileDrop { path, completed } => {
                self.publish_file_drop(path.clone());
                let _ = completed.send(Ok(json!({
                    "state": "published",
                    "file_name": path.file_name().map(|value| value.to_string_lossy()),
                    "event": "ui.file_drop.accepted"
                })));
            }
            WindowCommand::CaptureWorldUiLab {
                artifact_path,
                size,
                completed,
            } => {
                let _ = completed.send(self.capture_world_ui_lab(artifact_path, size));
            }
            WindowCommand::CompositionDrawCompleted { acknowledgements } => {
                self.composition_ack_in_flight = false;
                for acknowledgement in acknowledgements {
                    let _ = acknowledgement.send(());
                }
                if !self.pending_composition_acks.is_empty() {
                    self.redraw_pending = true;
                }
            }
            WindowCommand::SemanticDeliveryCompleted { pending, outcome } => {
                if let (Some(gpu), Some(pending)) = (self.gpu.as_mut(), pending) {
                    gpu.ui.complete_local_presentation(
                        &pending,
                        outcome == SemanticDeliveryOutcome::Accepted,
                        &self.fragments,
                    );
                    gpu.hit_target_dirty = true;
                    self.redraw_pending = true;
                }
            }
            WindowCommand::DataGridWindowDeliveryCompleted { sequence, accepted } => {
                if !accepted
                    && let Some(gpu) = self.gpu.as_mut()
                    && gpu.ui.fail_data_grid_window_request(sequence)
                {
                    gpu.hit_target_dirty = true;
                    self.redraw_pending = true;
                }
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
        return runtime.reject(
            request_id,
            "invalid_request",
            "idempotency_key is required",
            None,
        );
    };
    if request.expected_revision.is_none() {
        return runtime.reject(
            request_id,
            "invalid_request",
            "expected_revision is required",
            None,
        );
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
            runtime
                .idempotent_responses
                .insert(idempotency_key, response.clone());
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
            runtime.reject(
                request_id,
                "ai_generation_failed",
                &error,
                Some(runtime.graph_revision),
            )
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
        .send_event(WindowCommand::AiModelStatus {
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

fn handle_window_image_debug_snapshot(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
) -> RpcResponse {
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::ImageDebugSnapshot {
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
            "window compositor did not report image state",
            None,
        ),
    }
}

fn handle_window_external_image_upload(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request: RpcRequest,
) -> RpcResponse {
    let upload = match serde_json::from_value::<UiImageUploadRequest>(request.params) {
        Ok(upload) => upload,
        Err(error) => {
            return runtime.reject(
                request.request_id,
                "invalid_request",
                &format!("invalid external image upload request: {error}"),
                None,
            );
        }
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::UploadExternalImage {
            source: upload.source,
            completed: completed_tx,
        })
        .is_err()
    {
        return runtime.reject(
            request.request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request.request_id, result),
        Ok(Err(error)) => runtime.reject(request.request_id, &error, &error, None),
        Err(_) => runtime.reject(
            request.request_id,
            "window_compositor_timeout",
            "window compositor did not upload the external image",
            None,
        ),
    }
}

fn handle_window_debug_snapshot(
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
        Ok(window) => {
            let mut snapshot = json!(runtime.debug_snapshot());
            snapshot["window"] = window;
            runtime.accept(request_id, snapshot)
        }
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not report its debug snapshot",
            None,
        ),
    }
}

fn probe_position(params: &Value) -> Result<(Option<[f64; 2]>, Option<[f64; 2]>), &'static str> {
    let point = |name: &str| -> Result<Option<[f64; 2]>, &'static str> {
        let Some(value) = params.get(name) else {
            return Ok(None);
        };
        let x = value
            .get("x")
            .and_then(Value::as_f64)
            .ok_or("position x must be a number")?;
        let y = value
            .get("y")
            .and_then(Value::as_f64)
            .ok_or("position y must be a number")?;
        if !x.is_finite() || !y.is_finite() {
            return Err("position coordinates must be finite");
        }
        Ok(Some([x, y]))
    };
    let logical = point("logical_position")?;
    let physical = point("physical_position")?;
    if logical.is_none() && physical.is_none() {
        return Err("logical_position or physical_position is required");
    }
    Ok((logical, physical))
}

fn handle_window_input_debug_probe(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(
            request_id,
            "debug_endpoint_unavailable",
            "window input probing is only available in debug builds",
            None,
        );
    }
    let (logical_position, physical_position) = match probe_position(&params) {
        Ok(position) => position,
        Err(message) => return runtime.reject(request_id, "invalid_request", message, None),
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::InputDebugProbe {
            logical_position,
            physical_position,
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
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(probe) => runtime.accept(request_id, probe),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not report input probe",
            None,
        ),
    }
}

fn handle_window_input_debug_activate(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(
            request_id,
            "debug_endpoint_unavailable",
            "window input activation is only available in debug builds",
            None,
        );
    }
    let logical_position = match probe_position(&params) {
        Ok((Some(position), None)) => position,
        Ok(_) => {
            return runtime.reject(
                request_id,
                "invalid_request",
                "logical_position is required for debug activation",
                None,
            );
        }
        Err(message) => return runtime.reject(request_id, "invalid_request", message, None),
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::InputDebugActivate {
            logical_position,
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
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request_id, result),
        Ok(Err(code)) => runtime.reject(
            request_id,
            code,
            "debug activation did not resolve an eligible semantic control",
            None,
        ),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not activate the prepared binding",
            None,
        ),
    }
}

fn debug_semantic_node_path(params: &Value) -> Result<String, &'static str> {
    let node_path = params
        .get("semantic_node_path")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .ok_or("semantic_node_path is required")?;
    Ok(node_path.to_owned())
}

fn handle_window_input_debug_target_command(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
    scroll_to_max: bool,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(
            request_id,
            "debug_endpoint_unavailable",
            "window input test commands are only available in debug builds",
            None,
        );
    }
    let semantic_node_path = match debug_semantic_node_path(&params) {
        Ok(path) => path,
        Err(message) => return runtime.reject(request_id, "invalid_request", message, None),
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    let command = if scroll_to_max {
        WindowCommand::InputDebugScrollToMax {
            semantic_node_path,
            completed: completed_tx,
        }
    } else {
        WindowCommand::InputDebugActivateTarget {
            semantic_node_path,
            completed: completed_tx,
        }
    };
    if proxy.send_event(command).is_err() {
        return runtime.reject(
            request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request_id, result),
        Ok(Err(code)) => runtime.reject(
            request_id,
            code,
            "debug semantic target did not resolve an eligible control",
            None,
        ),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not process the test command",
            None,
        ),
    }
}

fn handle_window_input_debug_value_gesture(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(
            request_id,
            "debug_endpoint_unavailable",
            "window input test commands are only available in debug builds",
            None,
        );
    }
    let semantic_node_path = match debug_semantic_node_path(&params) {
        Ok(path) => path,
        Err(message) => return runtime.reject(request_id, "invalid_request", message, None),
    };
    let Some(target_fraction) = params
        .get("target_fraction")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
    else {
        return runtime.reject(
            request_id,
            "invalid_request",
            "target_fraction must be a finite value from zero through one",
            None,
        );
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::InputDebugValueGesture {
            semantic_node_path,
            target_fraction: target_fraction as f32,
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
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request_id, result),
        Ok(Err(code)) => runtime.reject(
            request_id,
            code,
            "debug value gesture did not complete",
            None,
        ),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not process the test command",
            None,
        ),
    }
}

fn handle_window_input_debug_drag_gesture(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(
            request_id,
            "debug_endpoint_unavailable",
            "window input test commands are only available in debug builds",
            None,
        );
    }
    let Some(source_node_key) = params
        .get("source_node_key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
    else {
        return runtime.reject(
            request_id,
            "invalid_request",
            "source_node_key is required",
            None,
        );
    };
    let Some(target_node_key) = params
        .get("target_node_key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
    else {
        return runtime.reject(
            request_id,
            "invalid_request",
            "target_node_key is required",
            None,
        );
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::InputDebugDragGesture {
            source_node_key: source_node_key.into(),
            target_node_key: target_node_key.into(),
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
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request_id, result),
        Ok(Err(code)) => runtime.reject(
            request_id,
            code,
            "debug drag gesture did not resolve a declared target",
            None,
        ),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not process the test command",
            None,
        ),
    }
}

fn handle_window_target_capture(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(
            request_id,
            "debug_endpoint_unavailable",
            "window target capture is only available in debug builds",
            None,
        );
    }
    let Some(target) = params.get("target").and_then(Value::as_str) else {
        return runtime.reject(request_id, "invalid_request", "target is required", None);
    };
    if target != UI_COLOR_TARGET {
        return runtime.reject(
            request_id,
            "unsupported_target",
            "the window server captures only the final color composition target",
            None,
        );
    }
    let artifact_path = match params.get("path") {
        None | Some(Value::Null) => None,
        Some(Value::String(path)) if !path.trim().is_empty() => Some(PathBuf::from(path)),
        _ => {
            return runtime.reject(
                request_id,
                "invalid_request",
                "path must be a non-empty PNG path",
                None,
            );
        }
    };
    let redraw = params
        .get("redraw")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::CaptureFinalTarget {
            artifact_path,
            redraw,
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
    match completed_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(capture)) => runtime.accept(request_id, capture),
        Ok(Err(error)) => runtime.reject(
            request_id,
            "window_capture_failed",
            &error,
            Some(runtime.graph_revision),
        ),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not complete target capture",
            Some(runtime.graph_revision),
        ),
    }
}

fn handle_world_ui_lab_capture(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request_id: RequestId,
    params: Value,
) -> RpcResponse {
    if !cfg!(debug_assertions) {
        return runtime.reject(
            request_id,
            "debug_endpoint_unavailable",
            "world UI lab capture is only available in debug builds",
            None,
        );
    }
    let Some(path) = params
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty() && path.ends_with(".png"))
    else {
        return runtime.reject(
            request_id,
            "invalid_request",
            "path must be a non-empty PNG path",
            None,
        );
    };
    let width = match params.get("width") {
        None => 1920,
        Some(value) => match value.as_u64() {
            Some(width) => width,
            None => {
                return runtime.reject(
                    request_id,
                    "invalid_request",
                    "width must be an integer",
                    None,
                );
            }
        },
    };
    let height = match params.get("height") {
        None => 1080,
        Some(value) => match value.as_u64() {
            Some(height) => height,
            None => {
                return runtime.reject(
                    request_id,
                    "invalid_request",
                    "height must be an integer",
                    None,
                );
            }
        },
    };
    if width == 0 || height == 0 || width > 3840 || height > 2160 {
        return runtime.reject(
            request_id,
            "invalid_request",
            "width and height must be between 1 and 3840x2160",
            None,
        );
    }
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::CaptureWorldUiLab {
            artifact_path: PathBuf::from(path),
            size: [width as u32, height as u32],
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
    match completed_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(capture)) => runtime.accept(request_id, capture),
        Ok(Err(error)) => runtime.reject(request_id, "world_ui_lab_capture_failed", &error, None),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not complete world UI lab capture",
            None,
        ),
    }
}

fn world_ui_lab_camera_status(camera: &WorldUiLabCameraController) -> Value {
    json!({
        "surface_target_id": WORLD_UI_LAB_SURFACE_TARGET,
        "enabled": camera.enabled,
        "window_focused": camera.window_focused,
        "surface_focused": camera.surface_focused,
        "udp": if camera.registration.is_some() { "available" } else { "unavailable" },
        "session_id": camera.registration.as_ref().map(|registration| registration.session_id.clone()),
        "camera_id": camera.registration.as_ref().map(|registration| registration.camera_id.0.clone()),
        "provider_epoch": camera.registration.as_ref().map(|registration| registration.provider_epoch),
        "last_udp_error": camera.last_udp_error,
        "sequence": camera.sequence,
        "position": camera.camera_position,
        "yaw_radians": camera.yaw,
        "pitch_radians": camera.pitch,
        "vertical_fov_radians": camera.state().vertical_fov,
        "movement_speed": if camera.movement_speed == 0.0 { 0.35 } else { camera.movement_speed },
        "active_drag_mode": match camera.drag_mode { WorldUiLabDragMode::None => "none", WorldUiLabDragMode::PendingPan => "pending_pan", WorldUiLabDragMode::Pan => "pan", WorldUiLabDragMode::Rotate => "rotate" },
    })
}

fn world_ui_lab_camera(size: [u32; 2], state: WorldUiCameraState) -> WorldUiCamera {
    WorldUiCamera::perspective(size, state)
}

/// Projects a world-space point to a fullscreen pixel using a 3D camera frame.
///
/// The camera looks along -Z (right-handed Y-up), matching Bevy and the
/// `RightHandedYUpNegativeZForward` coordinate system. `orientation` is a unit
/// quaternion in `(x, y, z, w)` order. Returns `None` when the point is behind
/// the camera, outside the near/far range, or outside the view frustum.
fn project_world_point_to_screen(
    anchor_position: [f64; 3],
    camera_position: [f64; 3],
    orientation: [f32; 4],
    vertical_fov_radians: f32,
    near: f32,
    far: f32,
    viewport: [u32; 2],
) -> Option<[f32; 2]> {
    project_world_point_to_screen_with_depth(
        anchor_position,
        camera_position,
        orientation,
        vertical_fov_radians,
        near,
        far,
        viewport,
    )
    .map(|(screen, _)| screen)
}

fn project_world_point_from_frame(
    anchor: &WorldUiAnchor,
    frame: &CameraFrame,
    viewport: [u32; 2],
) -> Option<[f32; 2]> {
    let CameraFramePayload::ThreeDimensional {
        position,
        orientation,
        vertical_fov_radians,
        near,
        far,
    } = frame.payload
    else {
        return None;
    };
    project_world_point_to_screen(
        anchor.position,
        position,
        orientation,
        vertical_fov_radians,
        near,
        far,
        viewport,
    )
}

fn project_world_point_to_screen_with_depth(
    anchor_position: [f64; 3],
    camera_position: [f64; 3],
    orientation: [f32; 4],
    vertical_fov_radians: f32,
    near: f32,
    far: f32,
    viewport: [u32; 2],
) -> Option<([f32; 2], f32)> {
    let [qx, qy, qz, qw] = orientation;
    let norm = (qx * qx + qy * qy + qz * qz + qw * qw).sqrt();
    if norm <= 0.0 {
        return None;
    }
    let (qx, qy, qz, qw) = (qx / norm, qy / norm, qz / norm, qw / norm);
    // Rotation matrix `world = R * local`, row-major.
    let r00 = 1.0 - 2.0 * (qy * qy + qz * qz);
    let r01 = 2.0 * (qx * qy - qw * qz);
    let r02 = 2.0 * (qx * qz + qw * qy);
    let r10 = 2.0 * (qx * qy + qw * qz);
    let r11 = 1.0 - 2.0 * (qx * qx + qz * qz);
    let r12 = 2.0 * (qy * qz - qw * qx);
    let r20 = 2.0 * (qx * qz - qw * qy);
    let r21 = 2.0 * (qy * qz + qw * qx);
    let r22 = 1.0 - 2.0 * (qx * qx + qy * qy);
    let dx = (anchor_position[0] - camera_position[0]) as f32;
    let dy = (anchor_position[1] - camera_position[1]) as f32;
    let dz = (anchor_position[2] - camera_position[2]) as f32;
    // Camera-local = R^T * (p - c).
    let lx = r00 * dx + r10 * dy + r20 * dz;
    let ly = r01 * dx + r11 * dy + r21 * dz;
    let lz = r02 * dx + r12 * dy + r22 * dz;
    // The camera looks along -Z, so a visible point has a negative local z.
    let depth = -lz;
    if depth < near || depth > far {
        return None;
    }
    let focal = 1.0 / (vertical_fov_radians * 0.5).tan();
    let aspect = viewport[0].max(1) as f32 / viewport[1].max(1) as f32;
    let ndc_x = lx * focal / (aspect * depth);
    let ndc_y = ly * focal / depth;
    if ndc_x.abs() > 1.0 || ndc_y.abs() > 1.0 {
        return None;
    }
    let screen_x = (ndc_x * 0.5 + 0.5) * viewport[0] as f32;
    let screen_y = (1.0 - (ndc_y * 0.5 + 0.5)) * viewport[1] as f32;
    Some(([screen_x, screen_y], depth))
}

fn handle_world_ui_lab_camera_register(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request: RpcRequest,
) -> RpcResponse {
    let request_id = request.request_id.clone();
    if request.expected_revision != Some(runtime.graph_revision) {
        return runtime.reject(
            request_id,
            "revision_conflict",
            "expected_revision must match the current WGPU graph revision",
            Some(runtime.graph_revision),
        );
    }
    let Some(key) = request.idempotency_key.as_ref() else {
        return runtime.reject(
            request_id,
            "invalid_request",
            "idempotency_key is required",
            None,
        );
    };
    if let Some(existing) = runtime.idempotent_responses.get(key) {
        let mut response = existing.clone();
        response.request_id = request_id;
        return response;
    }
    let registration = match serde_json::from_value::<WorldUiLabCameraRegistration>(request.params)
    {
        Ok(value) => value,
        Err(_) => {
            return runtime.reject(
                request_id,
                "invalid_request",
                "invalid world UI lab camera registration",
                None,
            );
        }
    };
    let (tx, rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::RegisterWorldUiLabCamera {
            registration,
            completed: tx,
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
    let response = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(status)) => runtime.accept(request_id, status),
        Ok(Err(code)) => runtime.reject(
            request_id,
            code,
            "world UI lab camera registration was rejected",
            None,
        ),
        Err(_) => runtime.reject(
            request_id,
            "window_compositor_timeout",
            "window compositor did not register camera provider",
            None,
        ),
    };
    if response.status == RpcStatus::Accepted {
        runtime
            .idempotent_responses
            .insert(key.clone(), response.clone());
    }
    response
}

fn spawn_window_server(
    epoch: u64,
    endpoint: SocketAddr,
    proxy: EventLoopProxy<WindowCommand>,
    interaction_traces: Arc<Mutex<InteractionTraceStore>>,
    world_ui_lab_camera: Arc<Mutex<WorldUiLabCameraController>>,
) {
    thread::spawn(move || {
        let server = match neon_ipc::BlockingRpcServer::bind(endpoint) {
            Ok(server) => server,
            Err(error) => {
                eprintln!("window RPC server bind failed: {error}");
                let _ = proxy.send_event(WindowCommand::Shutdown);
                return;
            }
        };
        let runtime = Arc::new(Mutex::new(WgpuRuntime::window_control(
            epoch,
            interaction_traces,
            world_ui_lab_camera,
        )));
        let handler_proxy = proxy.clone();
        if let Err(error) = server.serve_until(
            move |request| {
                let mut runtime = runtime.lock().expect("runtime lock");
                let proxy = &handler_proxy;
                let mutates_composition = matches!(
                    request.method.as_str(),
                    "wgpu.ui.submit_fragment"
                        | "wgpu.ui.remove_fragment"
                        | "wgpu.world.info.configure"
                        | "wgpu.world.camera.submit_frame"
                        | "wgpu.world.ui.anchor.submit"
                        | "wgpu.world.ui.anchor.submit_batch"
                        | "wgpu.world_ui.lab.camera.register"
                        | "debug.window.file_drop"
                );
                let response = if request.method == "wgpu.ai.terrain.generate" {
                    handle_window_ai_generate(&mut runtime, &proxy, request)
                } else if request.method == "render.surface.open" {
                    handle_window_external_surface_open(&mut runtime, &proxy, request)
                } else if request.method == "render.surface.acquire" {
                    handle_window_external_surface_acquire(&mut runtime, &proxy, request)
                } else if request.method == "render.surface.frame" {
                    handle_window_external_surface_frame(&mut runtime, &proxy, request)
                } else if request.method == "ui.host.pointer_event" {
                    handle_window_external_pointer(&mut runtime, &proxy, request)
                } else if request.method == "wgpu.ai.model.status" {
                    handle_window_ai_model_status(&mut runtime, &proxy, request.request_id)
                } else if request.method == "debug.snapshot.get" {
                    handle_window_debug_snapshot(&mut runtime, &proxy, request.request_id)
                } else if request.method == "debug.window.input.snapshot" {
                    handle_window_input_debug_snapshot(&mut runtime, &proxy, request.request_id)
                } else if request.method == "debug.window.images" {
                    handle_window_image_debug_snapshot(&mut runtime, &proxy, request.request_id)
                } else if request.method == "debug.window.file_drop" {
                    WgpuRuntime::handle_window_file_drop(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                    )
                } else if request.method == "wgpu.ui.image.upload" {
                    handle_window_external_image_upload(&mut runtime, &proxy, request)
                } else if request.method == "debug.window.input.probe" {
                    handle_window_input_debug_probe(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                    )
                } else if request.method == "debug.window.input.activate" {
                    handle_window_input_debug_activate(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                    )
                } else if request.method == "debug.window.input.activate_target" {
                    handle_window_input_debug_target_command(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                        false,
                    )
                } else if request.method == "debug.window.input.scroll_to_max" {
                    handle_window_input_debug_target_command(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                        true,
                    )
                } else if request.method == "debug.window.input.value_gesture" {
                    handle_window_input_debug_value_gesture(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                    )
                } else if request.method == "debug.window.input.drag_gesture" {
                    handle_window_input_debug_drag_gesture(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                    )
                } else if request.method == "wgpu.render.target.capture" {
                    handle_window_target_capture(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                    )
                } else if request.method == "wgpu.world_ui.lab.capture" {
                    handle_world_ui_lab_capture(
                        &mut runtime,
                        &proxy,
                        request.request_id,
                        request.params,
                    )
                } else if request.method == "wgpu.world_ui.lab.camera.register" {
                    handle_world_ui_lab_camera_register(&mut runtime, &proxy, request)
                } else if request.method == "wgpu.world_ui.lab.camera.snapshot" {
                    let status = runtime
                        .world_ui_lab_camera
                        .lock()
                        .ok()
                        .map(|camera| world_ui_lab_camera_status(&camera));
                    match status {
                        Some(status) => runtime.accept(request.request_id, status),
                        None => runtime.reject(
                            request.request_id,
                            "world_ui_lab_camera_unavailable",
                            "camera controller is unavailable",
                            None,
                        ),
                    }
                } else {
                    runtime.handle(request)
                };
                if mutates_composition && response.status == RpcStatus::Accepted {
                    let send = proxy.send_event(WindowCommand::Fragments {
                        composition_revision: runtime.diagnostics().graph_revision,
                        fragments: runtime.fragments_snapshot(),
                        // Composition is applied by the window event loop. Do not
                        // make the control-plane request wait for a frame or GPU
                        // submission; visual feedback must continue independently
                        // of RPC response latency.
                        applied: None,
                    });
                    if send.is_err() {
                        return runtime.reject(
                            response.request_id,
                            "window_compositor_unavailable",
                            "window compositor is unavailable",
                            None,
                        );
                    }
                }
                response
            },
            |request| request.method == "service.shutdown",
        ) {
            eprintln!("window RPC server request failed: {error}");
        }
        let _ = proxy.send_event(WindowCommand::Shutdown);
    });
}

fn handle_window_external_surface_open(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request: RpcRequest,
) -> RpcResponse {
    if request.client.kind != ClientKind::ExternalHost {
        return runtime.reject(
            request.request_id,
            "external_host_required",
            "render.surface.open requires an external host client",
            None,
        );
    }
    let open = match serde_json::from_value::<RenderSurfaceOpen>(request.params) {
        Ok(open) => open,
        Err(error) => {
            return runtime.reject(
                request.request_id,
                "invalid_surface_open",
                &format!("surface open request is invalid: {error}"),
                None,
            );
        }
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::OpenExternalSurface {
            open,
            completed: completed_tx,
        })
        .is_err()
    {
        return runtime.reject(
            request.request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request.request_id, result),
        Ok(Err(error)) => runtime.reject(request.request_id, &error, &error, None),
        Err(_) => runtime.reject(
            request.request_id,
            "window_compositor_timeout",
            "window compositor did not open the external surface",
            None,
        ),
    }
}

fn handle_window_external_surface_acquire(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request: RpcRequest,
) -> RpcResponse {
    if request.client.kind != ClientKind::ExternalHost {
        return runtime.reject(
            request.request_id,
            "external_host_required",
            "render.surface.acquire requires an external host client",
            None,
        );
    }
    let surface_id = request
        .params
        .get("surface_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let pid = request
        .params
        .get("pid")
        .and_then(Value::as_u64)
        .unwrap_or_default() as u32;
    if surface_id.is_empty() || pid == 0 {
        return runtime.reject(
            request.request_id,
            "invalid_surface_acquire",
            "surface_id and pid are required",
            None,
        );
    }
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::AcquireExternalSurface {
            surface_id,
            pid,
            completed: completed_tx,
        })
        .is_err()
    {
        return runtime.reject(
            request.request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request.request_id, result),
        Ok(Err(error)) => runtime.reject(request.request_id, &error, &error, None),
        Err(_) => runtime.reject(
            request.request_id,
            "window_compositor_timeout",
            "window compositor did not acquire the external surface",
            None,
        ),
    }
}

fn handle_window_external_surface_frame(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request: RpcRequest,
) -> RpcResponse {
    if request.client.kind != ClientKind::ExternalHost {
        return runtime.reject(
            request.request_id,
            "external_host_required",
            "render.surface.frame requires an external host client",
            None,
        );
    }
    let surface_id = request
        .params
        .get("surface_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if surface_id.is_empty() {
        return runtime.reject(
            request.request_id,
            "invalid_surface_frame",
            "surface_id is required",
            None,
        );
    }
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::ExternalSurfaceFrameSnapshot {
            surface_id,
            completed: completed_tx,
        })
        .is_err()
    {
        return runtime.reject(
            request.request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request.request_id, result),
        Ok(Err(error)) => runtime.reject(request.request_id, &error, &error, None),
        Err(_) => runtime.reject(
            request.request_id,
            "window_compositor_timeout",
            "window compositor did not return the external surface frame",
            None,
        ),
    }
}

fn handle_window_external_pointer(
    runtime: &mut WgpuRuntime,
    proxy: &EventLoopProxy<WindowCommand>,
    request: RpcRequest,
) -> RpcResponse {
    let event = match serde_json::from_value::<UiPointerEvent>(
        request
            .params
            .get("event")
            .cloned()
            .unwrap_or(request.params.clone()),
    ) {
        Ok(event) => event,
        Err(error) => {
            return runtime.reject(
                request.request_id,
                "invalid_pointer_event",
                &format!("pointer event is invalid: {error}"),
                None,
            );
        }
    };
    let (completed_tx, completed_rx) = std::sync::mpsc::channel();
    if proxy
        .send_event(WindowCommand::ExternalPointerEvent {
            event,
            completed: completed_tx,
        })
        .is_err()
    {
        return runtime.reject(
            request.request_id,
            "window_compositor_unavailable",
            "window compositor is unavailable",
            None,
        );
    }
    match completed_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(result)) => runtime.accept(request.request_id, result),
        Ok(Err(error)) => runtime.reject(request.request_id, &error, &error, None),
        Err(_) => runtime.reject(
            request.request_id,
            "window_compositor_timeout",
            "window compositor did not process the pointer event",
            None,
        ),
    }
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
                motion_key: None,
            }),
            world_depth: None,
            world_scale: None,
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
                        motion_key: None,
                    }),
                    world_depth: None,
                    world_scale: None,
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
                        motion_key: None,
                    }),
                    world_depth: None,
                    world_scale: None,
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
    external_images: HashMap<String, ExternalImageRecord>,
    external_image_generation: u64,
    interaction_traces: Arc<Mutex<InteractionTraceStore>>,
    world_bridge: WorldInformationBridge,
    world_ui_lab_camera: Arc<Mutex<WorldUiLabCameraController>>,
    viewport: [u32; 2],

    // Performance counters updated by the RPC handlers and sampled by the
    // headless render loop's 60-frame ui_perf_window. These are monotonic
    // counters that only advance; the render loop reads the latest value.
    camera_frames_received: u64,
    anchor_batches_received: u64,
    pointer_down_received: u64,
    pointer_up_received: u64,
    semantic_clicks: u64,
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
            external_images: HashMap::new(),
            external_image_generation: 0,
            interaction_traces: Arc::new(Mutex::new(InteractionTraceStore::new())),
            world_bridge: WorldInformationBridge::new(),
            world_ui_lab_camera: Arc::new(Mutex::new(WorldUiLabCameraController::default())),
            // The external host surface is 1280x720. World anchors must be
            // projected in the shared-texture viewport, not the window lab's
            // historical 1280x800 viewport.
            viewport: [1280, 720],
            camera_frames_received: 0,
            anchor_batches_received: 0,
            pointer_down_received: 0,
            pointer_up_received: 0,
            semantic_clicks: 0,
        }
    }

    fn window_control(
        epoch: u64,
        interaction_traces: Arc<Mutex<InteractionTraceStore>>,
        world_ui_lab_camera: Arc<Mutex<WorldUiLabCameraController>>,
    ) -> Self {
        let mut runtime = Self::headless(epoch);
        runtime.window_gpu_available = true;
        runtime.interaction_traces = interaction_traces;
        runtime.world_ui_lab_camera = world_ui_lab_camera;
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
            "wgpu.ui.image.upload.v1".into(),
            "wgpu.ui.image.inspect.v1".into(),
            neon_ui_schema::UI_NINE_SLICE_CAPABILITY_NAME.into(),
            neon_ui_schema::UI_CANVAS_POINTS_LINES_CAPABILITY_NAME.into(),
            CAPABILITY_EXTERNAL_HOST_BACKEND_MATCH.into(),
            "wgpu.world.info.bridge".into(),
            "wgpu.world.ui.anchor.batch.v1".into(),
            CAPABILITY_STATE_ANIMATION.into(),
            CAPABILITY_NUMERIC_ANIMATION.into(),
        ];
        if self.window_gpu_available {
            capabilities.push(CAPABILITY_AI_TERRAIN_GENERATION.into());
            capabilities.push(CAPABILITY_DEBUG_INTERACTION.into());
            if self
                .world_ui_lab_camera
                .lock()
                .is_ok_and(|camera| camera.enabled)
            {
                capabilities.push(CAPABILITY_WORLD_UI_LAB_CAMERA.into());
            }
            if cfg!(debug_assertions) {
                capabilities.push(CAPABILITY_DEBUG_WINDOW_CAPTURE.into());
            }
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
        self.fragments
            .iter()
            .map(|(id, fragment)| (id.clone(), self.filter_world_panels(fragment)))
            .collect()
    }

    pub fn external_surface_snapshots(
        &self,
    ) -> (
        HashMap<RenderSurfaceKind, HashMap<UiFragmentId, UiFragment>>,
        HashMap<UiFragmentId, UiFragment>,
        HashMap<UiFragmentId, UiFragment>,
    ) {
        // Use the RAW fragments, not `fragments_snapshot()`. The latter already
        // applies `filter_world_panels`, which strips the CameraVisibility
        // effects; splitting from that would leave `retain_surface_nodes`
        // unable to identify world panels and delete the entire world UI.
        let combined = self.fragments.clone();
        let split = [
            (
                RenderSurfaceKind::ScreenUi,
                combined
                    .iter()
                    .map(|(id, fragment)| {
                        let mut fragment = fragment.clone();
                        retain_surface_nodes(
                            &mut fragment.root,
                            &fragment.effects,
                            RenderSurfaceKind::ScreenUi,
                            false,
                        );
                        fragment.effects.retain(|effect| {
                            !matches!(effect, neon_ui_schema::UiEffect::CameraVisibility { .. })
                        });
                        // The fragment root is only a structural full-viewport
                        // container. Its schema default style is opaque, which would
                        // turn the screen surface into a full-screen blue overlay.
                        fragment.root.style.background_color = [0.0; 4];
                        fragment.root.style.border_color = [0.0; 4];
                        (id.clone(), fragment)
                    })
                    .collect(),
            ),
            (
                RenderSurfaceKind::WorldUi,
                combined
                    .iter()
                    .map(|(id, fragment)| {
                        let mut fragment = fragment.clone();
                        retain_surface_nodes(
                            &mut fragment.root,
                            &fragment.effects,
                            RenderSurfaceKind::WorldUi,
                            false,
                        );
                        (id.clone(), self.filter_world_panels(&fragment))
                    })
                    .collect(),
            ),
        ]
        .into_iter()
        .collect();
        let unified_hit_fragments = self.unified_hit_fragments(&combined);
        (split, combined, unified_hit_fragments)
    }

    /// Produces the single interaction composition used by both ScreenUi and
    /// WorldUi pointer hit testing. World anchor placement is resolved once at
    /// this boundary; the renderer then draws one flat R32Uint ID image.
    fn unified_hit_fragments(
        &self,
        combined: &HashMap<UiFragmentId, UiFragment>,
    ) -> HashMap<UiFragmentId, UiFragment> {
        combined
            .iter()
            .map(|(id, fragment)| (id.clone(), self.filter_world_panels(fragment)))
            .collect()
    }

    fn filter_world_panels(&self, fragment: &UiFragment) -> UiFragment {
        let mut filtered = fragment.clone();
        let has_world_panel = filtered
            .effects
            .iter()
            .any(|effect| matches!(effect, neon_ui_schema::UiEffect::CameraVisibility { .. }));
        fn visit(
            node: &mut neon_ui_schema::UiNode,
            effects: &[neon_ui_schema::UiEffect],
            bridge: &WorldInformationBridge,
            viewport: [u32; 2],
            depths: &mut HashMap<String, f32>,
        ) {
            let binding = effects.iter().find_map(|effect| {
                let neon_ui_schema::UiEffect::CameraVisibility { binding } = effect else {
                    return None;
                };
                (binding.node_id == node.node_id).then_some(binding)
            });
            let mut projected = None;
            let mut hidden = false;
            if let Some(binding) = binding {
                if !bridge.camera_is_available(&binding.camera_id, binding.camera_kind) {
                    hidden = true;
                } else if let Some(anchor_id) = &binding.anchor_id {
                    let Some(anchor) = bridge.anchor(anchor_id) else {
                        node.visible = false;
                        return;
                    };
                    let Some(frame) = bridge.camera(&binding.camera_id) else {
                        node.visible = false;
                        return;
                    };
                    let CameraFramePayload::ThreeDimensional { near, .. } = frame.payload else {
                        node.visible = false;
                        return;
                    };
                    // Prefer the host's authoritative normalized placement.
                    // Older producers may omit it; when the world bridge has a
                    // valid camera frame, project the stable world position as
                    // a deterministic compatibility fallback. This keeps many
                    // independent world panels hittable without inventing a
                    // second layout or GPU ownership path.
                    let normalized = if (0.0..=1.0).contains(&anchor.screen_x)
                        && (0.0..=1.0).contains(&anchor.screen_y)
                        && anchor.view_distance > 0.0
                    {
                        Some(([anchor.screen_x, anchor.screen_y], anchor.view_distance))
                    } else {
                        project_world_point_from_frame(anchor, frame, viewport).map(|pixel| {
                            (
                                [pixel[0] / viewport[0] as f32, pixel[1] / viewport[1] as f32],
                                anchor.view_distance,
                            )
                        })
                    };
                    let Some(([norm_x, norm_y], depth)) = normalized else {
                        hidden = true;
                        return;
                    };
                    if !(0.0..=1.0).contains(&norm_x)
                        || !(0.0..=1.0).contains(&norm_y)
                        || !(depth > 0.0)
                    {
                        hidden = true;
                    } else {
                        let x = norm_x * viewport[0] as f32;
                        let y = norm_y * viewport[1] as f32;
                        projected = Some(([x, y], depth));
                        depths.insert(node.node_id.0.clone(), depth);
                        // Occlusion depth uses the same infinite reversed-Z
                        // convention as Bevy's scene depth: near / view_distance
                        // (near -> 1, far/infinity -> 0). A zero remains the
                        // sentinel for an always-visible panel.
                        let occlusion_depth = if anchor.occlusion == "depth_tested" {
                            (near / depth).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        node.world_depth = Some(occlusion_depth);
                        // A billboard world panel is a screen-stable overlay:
                        // keep its authored pixel size fixed while retaining
                        // the real anchor distance for depth-tested occlusion.
                        // Non-billboard compatibility paths retain the legacy
                        // distance scale.
                        let scale = if anchor.billboard {
                            1.0
                        } else {
                            (6.0 / depth).clamp(0.5, 2.0)
                        };
                        // Keep the authored panel topology and text layout at
                        // one stable logical size. Distance changes are a
                        // single uniform subtree scale applied by the renderer;
                        // they must not mutate padding, gaps, or child layout.
                        node.world_scale = Some(scale);
                        if let Some(layout) = &mut node.layout {
                            layout.clip = neon_ui_schema::UiClipPolicy::None;
                        }
                    }
                }
            }
            if hidden {
                node.visible = false;
            } else {
                if let Some(([x, y], _)) = projected {
                    node.bounds.x = x - node.bounds.width * 0.5;
                    node.bounds.y = y - node.bounds.height;
                }
                for child in &mut node.children {
                    visit(child, effects, bridge, viewport, depths);
                }
            }
        }
        let mut depths: HashMap<String, f32> = HashMap::new();
        visit(
            &mut filtered.root,
            &filtered.effects,
            &self.world_bridge,
            self.viewport,
            &mut depths,
        );
        // Draw order: world panels are otherwise emitted in tree order, which
        // lets a far panel (drawn later) cover a near one. `view_distance` is
        // smaller for near panels, so sort far -> near (descending distance).
        if has_world_panel {
            filtered.root.children.sort_by(|a, b| {
                let depth_a = depths.get(&a.node_id.0).copied().unwrap_or(f32::MAX);
                let depth_b = depths.get(&b.node_id.0).copied().unwrap_or(f32::MAX);
                depth_b
                    .partial_cmp(&depth_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        // The world bridge has already applied the camera gate and anchor
        // projection above. Leaving this effect in the renderer snapshot makes
        // UiWgpuRenderer apply a second, unrelated camera-availability gate.
        filtered
            .effects
            .retain(|effect| !matches!(effect, neon_ui_schema::UiEffect::CameraVisibility { .. }));
        if has_world_panel {
            // World panels use renderer-projected coordinates, so their root
            // must not reflow them as ordinary column children. The root is a
            // transparent layout container rather than a visible full-screen
            // panel in the exported texture.
            let layout = filtered.root.layout.get_or_insert_default();
            layout.mode = neon_ui_schema::UiLayoutMode::Overlay;
            layout.clip = neon_ui_schema::UiClipPolicy::None;
            // The root is structurally transparent because it has no fill. Do
            // not set opacity to zero here: UiWgpuRenderer propagates parent
            // opacity to descendants, which would hide every world panel.
            filtered.root.style.opacity = 1.0;
            filtered.root.style.background_color[3] = 0.0;
            filtered.root.style.border_color[3] = 0.0;
        }
        filtered
    }

    fn world_bridge_snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "world": self.world_bridge.world(),
        })
    }

    fn configure_world_information(
        &mut self,
        request_id: RequestId,
        params: serde_json::Value,
    ) -> RpcResponse {
        let snapshot: WorldInformationSnapshot = match serde_json::from_value(params) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "invalid world information snapshot",
                    None,
                );
            }
        };
        match self.world_bridge.configure_world(snapshot.clone()) {
            Ok(()) => self.accept(
                request_id,
                serde_json::json!({
                    "world_space_id": snapshot.world_space_id,
                    "revision": snapshot.revision,
                    "state": "accepted"
                }),
            ),
            Err(error) => self.reject(
                request_id,
                "invalid_world_information",
                &format!("{error:?}"),
                Some(snapshot.revision),
            ),
        }
    }

    fn submit_world_camera_frame(
        &mut self,
        request_id: RequestId,
        params: serde_json::Value,
    ) -> RpcResponse {
        let frame: CameraFrame = match serde_json::from_value(params) {
            Ok(frame) => frame,
            Err(_) => {
                return self.reject(request_id, "invalid_request", "invalid camera frame", None);
            }
        };
        let camera_id = frame.camera_id.clone();
        let kind = frame.payload.kind();
        let sequence = frame.sequence;
        match self.world_bridge.submit_camera_frame(frame) {
            Ok(()) => self.accept(
                request_id,
                serde_json::json!({
                    "camera_id": camera_id,
                    "kind": kind,
                    "sequence": sequence,
                    "state": "accepted"
                }),
            ),
            Err(error) => self.reject(
                request_id,
                "camera_frame_rejected",
                &format!("{error:?}"),
                None,
            ),
        }
    }

    fn submit_world_ui_anchor(
        &mut self,
        request_id: RequestId,
        params: serde_json::Value,
    ) -> RpcResponse {
        let anchor: WorldUiAnchor = match serde_json::from_value(params) {
            Ok(anchor) => anchor,
            Err(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "invalid world UI anchor",
                    None,
                );
            }
        };
        let anchor_id = anchor.anchor_id.clone();
        let sequence = anchor.sequence;
        match self.world_bridge.submit_anchor(anchor) {
            Ok(()) => self.accept(
                request_id,
                serde_json::json!({
                    "anchor_id": anchor_id,
                    "sequence": sequence,
                    "state": "accepted"
                }),
            ),
            Err(error) => self.reject(
                request_id,
                "world_anchor_rejected",
                &format!("{error:?}"),
                None,
            ),
        }
    }

    fn submit_world_ui_anchor_batch(
        &mut self,
        request_id: RequestId,
        params: serde_json::Value,
    ) -> RpcResponse {
        let batch: WorldUiAnchorBatch = match serde_json::from_value(params) {
            Ok(batch) => batch,
            Err(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "invalid world UI anchor batch",
                    None,
                );
            }
        };
        let sequence = batch.sequence;
        let timestamp_monotonic_ns = batch.timestamp_monotonic_ns;
        let count = batch.anchors.len();
        let first_anchor = batch.anchors.first().map(|anchor| {
            format!(
                "id={} position={:?} screen=({:.3},{:.3}) distance={:.3}",
                anchor.anchor_id.0,
                anchor.position,
                anchor.screen_x,
                anchor.screen_y,
                anchor.view_distance,
            )
        });
        match self.world_bridge.submit_anchor_batch(batch) {
            Ok(()) => self.accept(
                    request_id,
                    serde_json::json!({"sequence": sequence, "anchor_count": count, "state": "accepted"}),
                ),
            Err(error) => self.reject(
                request_id,
                "world_anchor_batch_rejected",
                &format!(
                    "{error:?}: sequence={sequence} timestamp_monotonic_ns={timestamp_monotonic_ns} anchor_count={count} first_anchor={first_anchor:?}"
                ),
                None,
            )
        }
    }

    pub fn command_receipt(&self, request_id: &RequestId) -> Option<&CommandReceipt> {
        self.receipts.get(request_id)
    }

    pub fn traces(&self, filter: &JournalFilter) -> Vec<TraceRecord> {
        self.journal.query(filter)
    }

    /// Accepts an owner response that was obtained by querying project/resource through the public protocol.
    /// The raw bytes stay in the renderer process and are never exposed by a WGPU RPC response.
    pub fn preload_resource_from_owner(
        &mut self,
        request: RpcRequest,
        content: AssetBytes,
    ) -> RpcResponse {
        let request_id = request.request_id.clone();
        let asset: AssetRef = match serde_json::from_value(request.params) {
            Ok(asset) => asset,
            Err(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "a stable AssetRef is required",
                    None,
                );
            }
        };
        let job_id = format!("ui-resource-{}-{}", asset.asset_id, asset.revision.0);
        self.resources.insert(
            asset.asset_id,
            UiResourceRecord {
                asset: asset.clone(),
                job_id: job_id.clone(),
                state: UiResourceState::Loading,
            },
        );
        self.journal.append(
            TraceLevel::Info,
            "ui.resource.loading",
            Some(request_id.clone()),
            None,
            Some(job_id.clone()),
            None,
            Some(asset.revision),
            None,
            json!({"asset_id": asset.asset_id, "kind": asset.kind}),
        );
        if asset != content.asset {
            return self.fail_resource(
                request_id,
                asset,
                job_id,
                "asset_revision_mismatch",
                "owner content does not match the requested AssetRef",
            );
        }
        if !matches!(asset.kind.as_str(), "font" | "image") || content.bytes.is_empty() {
            return self.fail_resource(
                request_id,
                asset,
                job_id,
                "invalid_resource_content",
                "owner returned unusable UI resource content",
            );
        }
        self.resources.insert(
            asset.asset_id,
            UiResourceRecord {
                asset: asset.clone(),
                job_id: job_id.clone(),
                state: UiResourceState::Ready,
            },
        );
        self.journal.append(TraceLevel::Info, "ui.resource.ready", Some(request_id.clone()), None, Some(job_id.clone()), None, Some(asset.revision), Some(asset.revision), json!({"asset_id": asset.asset_id, "kind": asset.kind, "media_type": content.media_type}));
        self.accept(request_id, json!({"job_id": job_id, "state": "ready"}))
    }

    fn upload_external_image(
        &mut self,
        request: &RpcRequest,
        upload: UiImageUploadRequest,
    ) -> RpcResponse {
        let request_id = request.request_id.clone();
        let Some(idempotency_key) = request.idempotency_key.clone() else {
            return self.reject(
                request_id,
                "invalid_request",
                "idempotency_key is required",
                None,
            );
        };
        if let Some(response) = self.idempotent_responses.get(&idempotency_key) {
            let mut response = response.clone();
            response.request_id = request_id;
            return response;
        }
        let source = upload.source;
        let Some(byte_len) = (source.width as usize)
            .checked_mul(source.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            return self.reject(
                request_id,
                "invalid_image_bytes",
                "image dimensions overflow",
                None,
            );
        };
        if source.image_id.trim().is_empty()
            || source.media_type != "application/x-neon-rgba8"
            || source.width == 0
            || source.height == 0
            || source.width > EXTERNAL_IMAGE_ATLAS_WIDTH - EXTERNAL_IMAGE_ATLAS_PADDING * 2
            || source.bytes.len() != byte_len
        {
            return self.reject(
                request_id,
                "invalid_image_source",
                "external image must be non-empty RGBA8 with matching dimensions",
                None,
            );
        }
        let image_id = source.image_id.clone();
        self.external_images.insert(
            image_id.clone(),
            ExternalImageRecord {
                source,
                texture: UiImageTextureRef {
                    image_id: String::new(),
                    texture_index: 0,
                    generation: 0,
                    atlas_size: [EXTERNAL_IMAGE_ATLAS_WIDTH, 1],
                    region: UiImageTextureRegion {
                        x: 0,
                        y: 0,
                        width: 0,
                        height: 0,
                    },
                    uv: [0.0; 4],
                },
            },
        );
        self.rebuild_external_image_residency();
        self.graph_revision = Revision(self.graph_revision.0.saturating_add(1));
        let texture = self
            .external_images
            .get(&image_id)
            .map(|record| record.texture.clone())
            .expect("uploaded external image has residency");
        let response = self.accept(
            request_id,
            json!({
                "state": "ready",
                "texture": texture,
                "producer": "ui-runtime",
                "consumer": "wgpu-runtime"
            }),
        );
        self.idempotent_responses
            .insert(idempotency_key, response.clone());
        response
    }

    fn rebuild_external_image_residency(&mut self) {
        let mut keys = self.external_images.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        self.external_image_generation = self.external_image_generation.saturating_add(1).max(1);
        let generation = self.external_image_generation;
        let mut x = EXTERNAL_IMAGE_ATLAS_PADDING;
        let mut y = EXTERNAL_IMAGE_ATLAS_PADDING;
        let mut row_height = 0;
        let mut placements = Vec::with_capacity(keys.len());
        for key in &keys {
            let source = &self.external_images[key].source;
            if x + source.width + EXTERNAL_IMAGE_ATLAS_PADDING > EXTERNAL_IMAGE_ATLAS_WIDTH {
                x = EXTERNAL_IMAGE_ATLAS_PADDING;
                y = y.saturating_add(row_height + EXTERNAL_IMAGE_ATLAS_PADDING);
                row_height = 0;
            }
            placements.push((key.clone(), x, y));
            x = x.saturating_add(source.width + EXTERNAL_IMAGE_ATLAS_PADDING);
            row_height = row_height.max(source.height);
        }
        let atlas_height = y
            .saturating_add(row_height + EXTERNAL_IMAGE_ATLAS_PADDING)
            .max(1);
        for (texture_index, (key, x, y)) in placements.into_iter().enumerate() {
            let source = self.external_images[&key].source.clone();
            self.external_images
                .get_mut(&key)
                .expect("external image exists")
                .texture = UiImageTextureRef {
                image_id: key.clone(),
                texture_index: texture_index as u32,
                generation,
                atlas_size: [EXTERNAL_IMAGE_ATLAS_WIDTH, atlas_height],
                region: UiImageTextureRegion {
                    x,
                    y,
                    width: source.width,
                    height: source.height,
                },
                uv: [
                    (x as f32 + 0.5) / EXTERNAL_IMAGE_ATLAS_WIDTH as f32,
                    (y as f32 + 0.5) / atlas_height as f32,
                    (source.width as f32 - 1.0) / EXTERNAL_IMAGE_ATLAS_WIDTH as f32,
                    (source.height as f32 - 1.0) / atlas_height as f32,
                ],
            };
        }
    }

    fn external_image_inspect(&mut self, request_id: RequestId) -> RpcResponse {
        let mut images = self
            .external_images
            .values()
            .map(|record| {
                json!({
                    "image_id": record.texture.image_id,
                    "texture_index": record.texture.texture_index,
                    "generation": record.texture.generation,
                    "atlas_size": record.texture.atlas_size,
                    "region": record.texture.region,
                    "uv": record.texture.uv,
                    "width": record.source.width,
                    "height": record.source.height,
                })
            })
            .collect::<Vec<_>>();
        images.sort_by(|left, right| left["image_id"].as_str().cmp(&right["image_id"].as_str()));
        self.accept(
            request_id,
            json!({"generation": self.external_image_generation, "images": images}),
        )
    }

    fn fail_resource(
        &mut self,
        request_id: RequestId,
        asset: AssetRef,
        job_id: String,
        code: &'static str,
        message: &'static str,
    ) -> RpcResponse {
        self.resources.insert(
            asset.asset_id,
            UiResourceRecord {
                asset: asset.clone(),
                job_id: job_id.clone(),
                state: UiResourceState::Failed,
            },
        );
        self.journal.append(
            TraceLevel::Error,
            "ui.resource.failed",
            Some(request_id.clone()),
            None,
            Some(job_id),
            None,
            Some(asset.revision),
            Some(asset.revision),
            json!({"asset_id": asset.asset_id, "kind": asset.kind, "code": code}),
        );
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
            "wgpu.ui.submit_fragment"
                | "wgpu.ui.remove_fragment"
                | "wgpu.world.info.configure"
                | "wgpu.world.camera.submit_frame"
        ) {
            if matches!(
                request.method.as_str(),
                "wgpu.ui.submit_fragment" | "wgpu.ui.remove_fragment"
            ) && request.client.kind == ClientKind::UiReactClient
            {
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
            "render.backend.negotiate" => {
                self.negotiate_external_backend(request_id, request.client, request.params)
            }
            "wgpu.render.diagnostics" => {
                self.accept(request_id, diagnostics_value(self.diagnostics()))
            }
            "wgpu.render.graph.snapshot" => self.accept(
                request_id,
                json!(composition_graph_snapshot(
                    self.graph_revision,
                    self.hit_target_generation
                )),
            ),
            "wgpu.ui.fragment.snapshot" => self.fragment_snapshot(request_id, request.params),
            "wgpu.render.target.capture" => self.target_capture(request_id, request.params),
            "wgpu.render.target.assert" => self.target_assert(request_id, request.params),
            "wgpu.world.info.snapshot" => self.accept(request_id, self.world_bridge_snapshot()),
            "wgpu.world.info.configure" => {
                self.configure_world_information(request_id, request.params)
            }
            "wgpu.world.camera.submit_frame" => {
                self.camera_frames_received += 1;
                self.submit_world_camera_frame(request_id, request.params)
            }
            "wgpu.world.ui.anchor.submit" => {
                self.anchor_batches_received += 1;
                self.submit_world_ui_anchor(request_id, request.params)
            }
            "wgpu.world.ui.anchor.submit_batch" => {
                self.anchor_batches_received += 1;
                self.submit_world_ui_anchor_batch(request_id, request.params)
            }
            "wgpu.resource.inspect" => self.resource_inspect(request_id),
            "wgpu.ui.resource.preload" => self.resource_preload(request_id, request.params),
            "wgpu.resource.wait_ready" => self.resource_wait_ready(request_id, request.params),
            "wgpu.ui.image.upload" => {
                match serde_json::from_value::<UiImageUploadRequest>(request.params.clone()) {
                    Ok(upload) => self.upload_external_image(&request, upload),
                    Err(_) => self.reject(
                        request_id,
                        "invalid_request",
                        "invalid external image upload request",
                        None,
                    ),
                }
            }
            "wgpu.ui.image.inspect" => self.external_image_inspect(request_id),
            "debug.snapshot.get" => self.accept(request_id, json!(self.debug_snapshot())),
            "debug.command.get" => self.command_get(request_id, request.params),
            "debug.trace.query" => self.trace_query(request_id, request.params),
            "debug.interaction.get" => self.interaction_get(request_id, request.params),
            "debug.interaction.query" => self.interaction_query(request_id, request.params),
            "wgpu.ui.submit_fragment" => self.submit_fragment(request_id, request.params),
            "wgpu.ui.remove_fragment" => self.remove_fragment(request_id, request.params),
            "wgpu.ui.semantic_event.validate" | "test.ui.semantic_event.inject" => {
                self.inject_semantic_event(request_id, request.params)
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
            "wgpu.ui.submit_fragment"
                | "wgpu.ui.remove_fragment"
                | "wgpu.world.info.configure"
                | "wgpu.world.camera.submit_frame"
                | "wgpu.world.ui.anchor.submit"
                | "wgpu.world.ui.anchor.submit_batch"
                | "wgpu.ui.image.upload"
        ) && response.status == RpcStatus::Accepted
            && let Some(idempotency_key) = request.idempotency_key
        {
            self.idempotent_responses
                .insert(idempotency_key, response.clone());
        }
        response
    }

    fn negotiate_external_backend(
        &mut self,
        request_id: RequestId,
        client: ClientIdentity,
        params: Value,
    ) -> RpcResponse {
        if client.kind != ClientKind::ExternalHost {
            return self.reject(
                request_id,
                "external_host_required",
                "render.backend.negotiate requires an external host client",
                None,
            );
        }
        let negotiation = match serde_json::from_value::<RenderBackendNegotiation>(params) {
            Ok(negotiation) => negotiation,
            Err(error) => {
                return self.reject(
                    request_id,
                    "invalid_backend_negotiation",
                    &format!("backend negotiation is invalid: {error}"),
                    None,
                );
            }
        };
        if negotiation.session_id.trim().is_empty() {
            return self.reject(
                request_id,
                "invalid_backend_negotiation",
                "session_id must not be empty",
                None,
            );
        }
        if negotiation.preferred_backends.is_empty() {
            return self.reject(
                request_id,
                "backend_not_requested",
                "preferred_backends must contain at least one backend",
                None,
            );
        }
        if !negotiation
            .preferred_backends
            .contains(&RenderBackend::Dx12)
        {
            return self.reject(
                request_id,
                "backend_not_available",
                "the first external transport requires a DX12 host backend",
                None,
            );
        }
        if !cfg!(target_os = "windows") {
            return self.reject(
                request_id,
                "backend_not_available",
                "the D3D12 external transport is only available on Windows",
                None,
            );
        }
        self.reject(
            request_id,
            "external_gpu_transport_not_ready",
            "DX12 backend matching is defined, but native shared texture export is not initialized",
            None,
        )
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
            event_id: params
                .get("event_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            pointer_id: params.get("pointer_id").and_then(Value::as_u64),
            fragment_revision: params
                .get("fragment_revision")
                .and_then(Value::as_u64)
                .map(Revision),
            composition_revision: params
                .get("composition_revision")
                .and_then(Value::as_u64)
                .map(Revision),
            ..JournalFilter::default()
        };
        self.accept(request_id, json!(self.traces(&filter)))
    }

    fn interaction_get(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(interaction_id) = params.get("interaction_id").and_then(Value::as_str) else {
            return self.reject(
                request_id,
                "invalid_request",
                "interaction_id is required",
                None,
            );
        };
        let interaction_id = InteractionId(interaction_id.into());
        let records = self
            .interaction_traces
            .lock()
            .ok()
            .map(|traces| traces.get(&interaction_id))
            .unwrap_or_default();
        if records.is_empty() {
            return self.reject(
                request_id,
                "not_found",
                "interaction trace was not found",
                None,
            );
        }
        self.accept(
            request_id,
            json!({"interaction_id": interaction_id, "records": records}),
        )
    }

    fn interaction_query(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let query: InteractionTraceQuery = match serde_json::from_value(params) {
            Ok(query) => query,
            Err(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "invalid interaction trace query",
                    None,
                );
            }
        };
        let records = self
            .interaction_traces
            .lock()
            .ok()
            .map(|traces| traces.query(&query))
            .unwrap_or_default();
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
            return self.reject(
                request_id,
                "invalid_request",
                "invalid UI fragment submission",
                None,
            );
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
            return self.reject(
                request_id,
                "invalid_request",
                "fragment_id is required",
                None,
            );
        };
        let Some(fragment) = self.fragments.get(&UiFragmentId(fragment_id.into())) else {
            return self.reject(request_id, "not_found", "fragment is not present", None);
        };
        self.accept(
            request_id,
            json!({
                    "epoch": self.epoch,
                    "sequence": self.graph_revision,
                    "fragment_revision": fragment.revision,
                    "fragment": fragment,
            }),
        )
    }

    /// Test-only U1 scenario bridge. It accepts a semantic event, never a hit ID or node key.
    fn inject_semantic_event(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let event: UiSemanticEvent = match serde_json::from_value(params) {
            Ok(event) => event,
            Err(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "invalid UI semantic event",
                    None,
                );
            }
        };
        let Some(fragment) = self.fragments.get(&event.fragment.id) else {
            return self.reject(
                request_id,
                "fragment_revision_stale",
                "fragment is not present",
                None,
            );
        };
        if fragment.revision != event.fragment.revision {
            return self.reject(
                request_id,
                "fragment_revision_stale",
                "fragment revision is stale",
                Some(fragment.revision),
            );
        }
        if !fragment.effects.iter().any(|effect| matches!(effect, neon_ui_schema::UiEffect::SemanticIntent { intent } | neon_ui_schema::UiEffect::BoundSemanticIntent { intent, .. } if intent == &event.intent)) {
            return self.reject(request_id, "intent_not_bound", "semantic intent is not bound", Some(fragment.revision));
        }
        self.accept(request_id, json!(event))
    }

    fn hit_sample_request(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(pointer_id) = params.get("pointer_id").and_then(Value::as_u64) else {
            return self.reject(
                request_id,
                "invalid_request",
                "pointer_id is required",
                None,
            );
        };
        let Some(sequence) = params.get("sequence").and_then(Value::as_u64) else {
            return self.reject(request_id, "invalid_request", "sequence is required", None);
        };
        self.input.request_sample(HitSampleRequest {
            pointer_id,
            sequence,
            composition_revision: self.graph_revision,
            target_generation: self.hit_target_generation,
        });
        self.journal.append(TraceLevel::Info, "ui.hit_sample.requested", Some(request_id.clone()), None, None, None, Some(self.graph_revision), None, json!({"pointer_id": pointer_id, "sequence": sequence, "composition_revision": self.graph_revision.0, "fragment_revision": self.graph_revision.0}));
        self.accept(request_id, json!({"state": self.input.state_name(), "pointer_id": pointer_id, "sequence": sequence}))
    }

    fn hit_sample_complete(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(pointer_id) = params.get("pointer_id").and_then(Value::as_u64) else {
            return self.reject(
                request_id,
                "invalid_request",
                "pointer_id is required",
                None,
            );
        };
        let hit_id = params
            .get("test_hit_id")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .unwrap_or(RENDER_HIT_NONE);
        match self.input.complete_sample(
            pointer_id,
            self.graph_revision,
            self.hit_target_generation,
            hit_id,
        ) {
            Ok(()) => {
                self.journal.append(TraceLevel::Info, "ui.hit_sample.completed", Some(request_id.clone()), None, None, None, Some(self.graph_revision), Some(self.graph_revision), json!({"pointer_id": pointer_id, "composition_revision": self.graph_revision.0, "fragment_revision": self.graph_revision.0}));
                self.accept(request_id, json!({"state": self.input.state_name(), "hovered": self.input.hover_id.is_some()}))
            }
            Err(code) => self.reject(request_id, code, "hit sample was rejected", None),
        }
    }

    fn pointer_down(&mut self, request_id: RequestId) -> RpcResponse {
        match self.input.pointer_down() {
            Ok(()) => self.accept(
                request_id,
                json!({"state": self.input.state_name(), "captured": true}),
            ),
            Err(code) => self.reject(request_id, code, "pointer down was rejected", None),
        }
    }

    fn pointer_up(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let eligible = params
            .get("eligible")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        match self.input.pointer_up(eligible) {
            Ok(()) => self.accept(
                request_id,
                json!({"state": self.input.state_name(), "semantic_event": "ui.pointer.click"}),
            ),
            Err(code) => self.reject(request_id, code, "pointer interaction cancelled", None),
        }
    }

    fn focus_loss(&mut self, request_id: RequestId) -> RpcResponse {
        self.input.cancel();
        self.accept(
            request_id,
            json!({"state": self.input.state_name(), "semantic_event": "ui.interaction.cancelled"}),
        )
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

    fn handle_window_file_drop(
        runtime: &mut WgpuRuntime,
        proxy: &EventLoopProxy<WindowCommand>,
        request_id: RequestId,
        params: Value,
    ) -> RpcResponse {
        let Some(source_path) = params.get("source_path").and_then(Value::as_str) else {
            return runtime.reject(
                request_id,
                "invalid_request",
                "source_path is required",
                None,
            );
        };
        if source_path.trim().is_empty() {
            return runtime.reject(
                request_id,
                "invalid_request",
                "source_path is required",
                None,
            );
        }
        let (completed_tx, completed_rx) = std::sync::mpsc::channel();
        if proxy
            .send_event(WindowCommand::FileDrop {
                path: PathBuf::from(source_path),
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
        match completed_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(result)) => runtime.accept(request_id, result),
            Ok(Err(error)) => runtime.reject(request_id, "file_drop_event_failed", &error, None),
            Err(_) => runtime.reject(
                request_id,
                "window_compositor_timeout",
                "window compositor did not publish the file drop event",
                None,
            ),
        }
    }

    fn resource_preload(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let asset: AssetRef = match serde_json::from_value(params) {
            Ok(asset) => asset,
            Err(_) => {
                return self.reject(
                    request_id,
                    "invalid_request",
                    "a stable AssetRef is required",
                    None,
                );
            }
        };
        if !matches!(asset.kind.as_str(), "font" | "image") {
            return self.reject(
                request_id,
                "unsupported_resource_kind",
                "only font and image resources are supported",
                None,
            );
        }
        self.reject(
            request_id,
            "asset_content_required",
            "query project/resource for revisioned asset bytes before preloading",
            Some(asset.revision),
        )
    }

    fn resource_wait_ready(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(asset_id) = params.get("asset_id").and_then(Value::as_u64) else {
            return self.reject(request_id, "invalid_request", "asset_id is required", None);
        };
        let Some(record) = self.resources.get(&asset_id) else {
            return self.reject(request_id, "not_found", "resource is not resident", None);
        };
        self.accept(
            request_id,
            json!({"job_id": record.job_id, "state": record.state.as_str()}),
        )
    }

    fn resource_inspect(&mut self, request_id: RequestId) -> RpcResponse {
        let resources = self.resources.values().map(|record| json!({"asset_id": record.asset.asset_id, "revision": record.asset.revision, "kind": record.asset.kind, "job_id": record.job_id, "state": record.state.as_str()})).collect::<Vec<_>>();
        self.accept(request_id, json!({"resources": resources}))
    }

    fn target_assert(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(target) = params.get("target").and_then(Value::as_str) else {
            return self.reject(request_id, "invalid_request", "target is required", None);
        };
        if target != UI_HIT_TARGET {
            return self.reject(
                request_id,
                "unsupported_target",
                "only the UI hit target has semantic assertions",
                None,
            );
        }
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

fn retain_surface_nodes(
    node: &mut neon_ui_schema::UiNode,
    effects: &[neon_ui_schema::UiEffect],
    kind: RenderSurfaceKind,
    inherited_world: bool,
) -> bool {
    // A node belongs to the world surface if it is a projected world panel or
    // any ancestor is. World subtrees must move as one unit: the panel's leaf
    // children (labels, bars, images) have no CameraVisibility effect of their
    // own yet must follow the panel into the world target.
    let is_world = inherited_world
        || effects.iter().any(|effect| {
            matches!(effect, neon_ui_schema::UiEffect::CameraVisibility { binding } if binding.node_id == node.node_id)
        });
    node.children
        .retain_mut(|child| retain_surface_nodes(child, effects, kind, is_world));
    let keep = match kind {
        // Structural containers (the transparent fragment root) are not world
        // nodes themselves but must survive so their world descendants stay
        // reachable by the layout pass.
        RenderSurfaceKind::WorldUi => is_world || !node.children.is_empty(),
        RenderSurfaceKind::ScreenUi => !is_world,
    };
    node.visible &= keep;
    keep
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
    use neon_ui_schema::{UiBounds, UiEffect, UiLayout, UiNode, UiNodeId, UiNodeKind, UiStyle};

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

    fn registered_camera_controller() -> WorldUiLabCameraController {
        let mut controller = WorldUiLabCameraController {
            enabled: true,
            window_focused: true,
            surface_focused: true,
            ..Default::default()
        };
        controller
            .register(WorldUiLabCameraRegistration {
                udp_endpoint: "127.0.0.1:9".parse().unwrap(),
                session_id: "test-session".into(),
                provider_epoch: 1,
                camera_id: CameraId("world-ui-lab".into()),
            })
            .unwrap();
        controller
    }

    fn surface_split_fragment() -> UiFragment {
        fn node(id: &str, kind: UiNodeKind, children: Vec<UiNode>) -> UiNode {
            UiNode {
                node_id: UiNodeId(id.into()),
                kind,
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
                world_depth: None,
                world_scale: None,
                children,
            }
        }
        UiFragment {
            fragment_id: UiFragmentId("surface-split".into()),
            revision: Revision(1),
            effects: vec![neon_ui_schema::UiEffect::CameraVisibility {
                binding: neon_ui_schema::UiCameraVisibilityBinding {
                    node_id: UiNodeId("world-panel".into()),
                    camera_id: CameraId("cam".into()),
                    camera_kind: neon_world_bridge::CameraKind::ThreeDimensional,
                    anchor_id: None,
                },
            }],
            root: node(
                "root",
                UiNodeKind::Panel,
                vec![
                    node(
                        "world-panel",
                        UiNodeKind::Panel,
                        vec![node("world-label", UiNodeKind::Label, vec![])],
                    ),
                    node("screen-button", UiNodeKind::Button, vec![]),
                ],
            ),
        }
    }

    fn retained_ids(root: &UiNode, ids: &mut Vec<String>) {
        if root.visible {
            ids.push(root.node_id.0.clone());
        }
        for child in &root.children {
            retained_ids(child, ids);
        }
    }

    #[test]
    fn retain_surface_nodes_keeps_world_subtrees_and_screen_ui_separate() {
        let fragment = surface_split_fragment();

        let mut world = fragment.clone();
        retain_surface_nodes(
            &mut world.root,
            &world.effects,
            RenderSurfaceKind::WorldUi,
            false,
        );
        let mut world_ids = Vec::new();
        retained_ids(&world.root, &mut world_ids);
        // The transparent root container survives so the layout pass can reach
        // the world subtree; the whole world subtree (panel + leaf label) is
        // kept; the screen button is dropped.
        assert!(world_ids.contains(&"root".to_string()));
        assert!(world_ids.contains(&"world-panel".to_string()));
        assert!(world_ids.contains(&"world-label".to_string()));
        assert!(!world_ids.contains(&"screen-button".to_string()));

        let mut screen = fragment.clone();
        retain_surface_nodes(
            &mut screen.root,
            &screen.effects,
            RenderSurfaceKind::ScreenUi,
            false,
        );
        let mut screen_ids = Vec::new();
        retained_ids(&screen.root, &mut screen_ids);
        assert!(screen_ids.contains(&"root".to_string()));
        assert!(!screen_ids.contains(&"world-panel".to_string()));
        assert!(!screen_ids.contains(&"world-label".to_string()));
        assert!(screen_ids.contains(&"screen-button".to_string()));
    }

    #[test]
    fn lab_camera_does_not_move_when_disabled_or_surface_is_unfocused() {
        let mut disabled = registered_camera_controller();
        disabled.enabled = false;
        assert!(
            disabled
                .set_key(KeyCode::KeyW, true, 1, Duration::from_secs(1))
                .is_none()
        );
        assert_eq!(disabled.camera_position, [0.0; 3]);
        let mut unfocused = registered_camera_controller();
        unfocused.surface_focused = false;
        assert!(
            unfocused
                .set_key(KeyCode::KeyW, true, 1, Duration::from_secs(1))
                .is_none()
        );
        assert_eq!(unfocused.camera_position, [0.0; 3]);
    }

    #[test]
    fn lab_camera_moves_and_emits_only_after_surface_focus() {
        let mut controller = registered_camera_controller();
        let sample = controller
            .set_key(KeyCode::KeyD, true, 7, Duration::from_nanos(20))
            .unwrap();
        assert_eq!(sample.camera_id.0, "world-ui-lab");
        assert_eq!(sample.producer_epoch, 7);
        assert_eq!(sample.movement_axes, [1.0, 0.0, 0.0]);
        assert!(controller.camera_position[0] > 0.0);
    }

    #[test]
    fn lab_camera_focus_loss_clears_axes_and_blocks_samples() {
        let mut controller = registered_camera_controller();
        controller.set_key(KeyCode::KeyW, true, 1, Duration::ZERO);
        controller.window_focused = false;
        controller.surface_focused = false;
        controller.clear_axes();
        assert_eq!(controller.axes, [0.0; 3]);
        assert!(
            controller
                .set_key(KeyCode::KeyW, false, 1, Duration::ZERO)
                .is_none()
        );
    }

    #[test]
    fn lab_camera_release_preserves_the_opposite_held_axis() {
        let mut controller = registered_camera_controller();
        controller.set_key(KeyCode::KeyW, true, 1, Duration::ZERO);
        controller.set_key(KeyCode::KeyS, true, 1, Duration::ZERO);
        let sample = controller
            .set_key(KeyCode::KeyW, false, 1, Duration::ZERO)
            .unwrap();
        assert_eq!(sample.movement_axes, [0.0, -1.0, 0.0]);
    }

    #[test]
    fn lab_camera_moves_in_its_yaw_oriented_horizontal_plane() {
        let mut controller = registered_camera_controller();
        controller.yaw = std::f32::consts::FRAC_PI_2;
        controller.set_key(KeyCode::KeyW, true, 1, Duration::ZERO);
        assert!(controller.camera_position[0] > 0.3);
        assert!(controller.camera_position[2].abs() < 0.001);
        controller.set_key(KeyCode::KeyE, true, 1, Duration::ZERO);
        assert!(controller.camera_position[1] > 0.3);
    }

    #[test]
    fn lab_camera_drag_modes_require_focus_and_clamp_pitch() {
        let mut controller = registered_camera_controller();
        controller.set_drag(winit::event::MouseButton::Right, true);
        controller.pointer_moved([0.0, 0.0], 1, Duration::ZERO);
        let sample = controller
            .pointer_moved([10.0, -10_000.0], 1, Duration::ZERO)
            .unwrap();
        assert_eq!(controller.drag_mode, WorldUiLabDragMode::Rotate);
        assert_eq!(controller.pitch, 1.5);
        assert_ne!(sample.look_delta, [0.0; 2]);
        controller.set_drag(winit::event::MouseButton::Left, true);
        controller.pointer_moved([0.0, 0.0], 1, Duration::ZERO);
        assert_eq!(controller.drag_mode, WorldUiLabDragMode::PendingPan);
        assert!(
            controller
                .pointer_moved([1.0, 1.0], 1, Duration::ZERO)
                .is_none()
        );
        assert_eq!(controller.drag_mode, WorldUiLabDragMode::PendingPan);
        assert!(
            controller
                .pointer_moved([8.0, 1.0], 1, Duration::ZERO)
                .is_some()
        );
        assert_eq!(controller.drag_mode, WorldUiLabDragMode::Pan);
        controller.surface_focused = false;
        controller.set_drag(winit::event::MouseButton::Right, true);
        assert_eq!(controller.drag_mode, WorldUiLabDragMode::Pan);
    }

    #[test]
    fn lab_camera_wheel_bounds_speed_and_emits_semantic_delta() {
        let mut controller = registered_camera_controller();
        let faster = controller.wheel(1000.0, 1, Duration::ZERO).unwrap();
        assert_eq!(controller.movement_speed, 10.0);
        assert_eq!(faster.wheel_delta, 1000.0);
        let slower = controller.wheel(-1000.0, 1, Duration::ZERO).unwrap();
        assert_eq!(controller.movement_speed, 0.05);
        assert_eq!(slower.wheel_delta, -1000.0);
    }

    #[test]
    fn lab_camera_sends_compact_json_to_the_registered_loopback_provider() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut controller = WorldUiLabCameraController {
            enabled: true,
            window_focused: true,
            surface_focused: true,
            ..Default::default()
        };
        controller
            .register(WorldUiLabCameraRegistration {
                udp_endpoint: receiver.local_addr().unwrap(),
                session_id: "test-session".into(),
                provider_epoch: 1,
                camera_id: CameraId("world-ui-lab".into()),
            })
            .unwrap();
        let sample = controller
            .set_key(KeyCode::KeyW, true, 3, Duration::from_nanos(4))
            .unwrap();
        controller.send(&sample);
        let mut bytes = [0_u8; 1024];
        let (count, _) = receiver.recv_from(&mut bytes).unwrap();
        let observed: CameraControlSample = serde_json::from_slice(&bytes[..count]).unwrap();
        assert_eq!(observed, sample);
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
    fn final_capture_normalizes_rgba_pixels_and_has_stable_checksum() {
        let mut bgra = vec![3, 2, 1, 255, 30, 20, 10, 128];
        normalize_capture_rgba(wgpu::TextureFormat::Bgra8UnormSrgb, &mut bgra).unwrap();
        assert_eq!(bgra, [1, 2, 3, 255, 10, 20, 30, 128]);
        assert_eq!(fnv1a64(&bgra), 0xd62200689a0b5a1c);

        let path = std::env::temp_dir().join(format!(
            "neon3-window-capture-test-{}.png",
            std::process::id()
        ));
        let artifact = write_capture_png(&path, [2, 1], &bgra).unwrap();
        let encoded = std::fs::read(&artifact).unwrap();
        assert_eq!(&encoded[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::remove_file(artifact).unwrap();
    }

    #[test]
    fn data_grid_window_scheduler_coalesces_to_the_latest_request() {
        let now = Instant::now();
        let base = UiDataGridWindowRequest {
            renderer_epoch: 1,
            composition_revision: Revision(4),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: UiFragmentId("virtual-list".into()),
                revision: Revision(2),
            },
            source_key: "asset_window".into(),
            expected_list_revision: Revision(1),
            requested_first_row: 8,
            max_window_rows: 12,
            sequence: 1,
        };
        let mut latest = LatestDataGridWindowRequests::default();
        latest.schedule(base.clone(), now);
        let mut replacement = base;
        replacement.requested_first_row = 80;
        replacement.sequence = 2;
        latest.schedule(replacement, now + Duration::from_millis(5));
        assert!(
            latest
                .take_ready(now + Duration::from_millis(24))
                .is_empty()
        );
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
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
            apply_limit_buckets: false,
        }))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            }))
        })
        .expect("a headless WGPU adapter is required");
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some(label),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        }))
        .expect("the selected adapter must create a device and queue")
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
                world_depth: None,
                world_scale: None,
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
        let expected_capabilities = json!([
            CAPABILITY_UI_FRAGMENT,
            "wgpu.render.diagnostics",
            CAPABILITY_UI_HIT_TARGET,
            CAPABILITY_UI_SEMANTIC_EVENT,
            CAPABILITY_UI_PROGRAM_SEMANTIC_EVENT,
            CAPABILITY_UI_RENDER_SURFACE,
            CAPABILITY_EXTERNAL_HOST_BACKEND_MATCH,
            "wgpu.world.info.bridge"
        ]);
        let capabilities = described["capabilities"].as_array().unwrap();
        for capability in expected_capabilities.as_array().unwrap() {
            assert!(capabilities.contains(capability));
        }
        assert_eq!(
            capabilities.contains(&json!(CAPABILITY_EXTERNAL_HOST_D3D12_SURFACE)),
            false
        );
        assert_eq!(snapshot.status, RpcStatus::Accepted);
        assert_eq!(
            snapshot.result.unwrap()["capabilities"]
                .as_array()
                .unwrap()
                .contains(&json!(CAPABILITY_EXTERNAL_HOST_BACKEND_MATCH)),
            true
        );
    }

    #[test]
    fn external_backend_negotiation_has_a_hard_transport_gate() {
        let mut runtime = WgpuRuntime::headless(1);
        let mut request = request(
            "backend-negotiate",
            "render.backend.negotiate",
            json!(neon_protocol::RenderBackendNegotiation {
                session_id: "host-session-001".into(),
                preferred_backends: vec![neon_protocol::RenderBackend::Dx12],
                required_features: vec!["shared_texture".into(), "shared_fence".into()],
                host: neon_protocol::RenderHostIdentity {
                    kind: neon_protocol::HostEngineKind::Godot,
                    pid: 12345,
                    adapter: neon_protocol::RenderAdapterIdentity {
                        vendor_id: Some(4318),
                        device_id: Some(1234),
                        luid: Some("adapter-luid".into()),
                        name: Some("test adapter".into()),
                    },
                    plugin_version: "test".into(),
                },
            }),
        );
        request.client.kind = ClientKind::ExternalHost;
        let response = runtime.handle(request);
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(
            response.error.unwrap().code,
            if cfg!(target_os = "windows") {
                "external_gpu_transport_not_ready"
            } else {
                "backend_not_available"
            }
        );
    }

    #[test]
    fn backend_negotiation_rejects_non_host_clients() {
        let mut runtime = WgpuRuntime::headless(1);
        let response = runtime.handle(request(
            "backend-client-reject",
            "render.backend.negotiate",
            json!({}),
        ));
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "external_host_required");
    }

    #[test]
    fn camera_gated_world_panel_is_hidden_until_a_matching_frame_arrives() {
        let mut runtime = WgpuRuntime::headless(1);
        runtime
            .world_bridge
            .configure_world(WorldInformationSnapshot {
                world_space_id: neon_world_bridge::WorldSpaceId("project.world.main".into()),
                revision: Revision(1),
                coordinate_system:
                    neon_world_bridge::CoordinateSystem::RightHandedYUpNegativeZForward,
                units_per_meter: 1.0,
                precision_mode: neon_world_bridge::WorldPrecisionMode::CameraRelativeF64,
            })
            .unwrap();
        let mut gated = fragment(1);
        let marker = UiNode {
            node_id: UiNodeId("marker".into()),
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
            world_depth: None,
            world_scale: None,
            children: Vec::new(),
        };
        gated.root.children.push(marker);
        gated.effects.push(UiEffect::CameraVisibility {
            binding: neon_ui_schema::UiCameraVisibilityBinding {
                node_id: UiNodeId("marker".into()),
                camera_id: neon_world_bridge::CameraId("editor".into()),
                camera_kind: neon_world_bridge::CameraKind::ThreeDimensional,
                anchor_id: None,
            },
        });
        runtime.fragments.insert(gated.fragment_id.clone(), gated);
        assert!(
            !runtime.fragments_snapshot()[&UiFragmentId("static-fragment".into())]
                .root
                .children[0]
                .visible
        );

        runtime
            .world_bridge
            .submit_camera_frame(CameraFrame {
                camera_id: neon_world_bridge::CameraId("editor".into()),
                world_space_id: neon_world_bridge::WorldSpaceId("project.world.main".into()),
                producer_epoch: 1,
                sequence: 1,
                timestamp_monotonic_ns: 1,
                payload: neon_world_bridge::CameraFramePayload::ThreeDimensional {
                    position: [0.0, 0.0, 0.0],
                    orientation: [0.0, 0.0, 0.0, 1.0],
                    vertical_fov_radians: 1.0,
                    near: 0.1,
                    far: 1000.0,
                },
            })
            .unwrap();
        assert!(
            runtime.fragments_snapshot()[&UiFragmentId("static-fragment".into())]
                .root
                .children[0]
                .visible
        );
    }

    #[test]
    fn world_anchor_projects_to_screen_center_when_dead_ahead() {
        let screen = project_world_point_to_screen(
            [0.0, 0.0, -2.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
            std::f32::consts::FRAC_PI_2,
            0.1,
            1000.0,
            [640, 360],
        )
        .expect("anchor is dead ahead");
        assert!((screen[0] - 320.0).abs() < 0.01);
        assert!((screen[1] - 180.0).abs() < 0.01);
    }

    #[test]
    fn world_anchor_behind_camera_is_not_projected() {
        assert!(
            project_world_point_to_screen(
                [0.0, 0.0, 2.0],
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
                std::f32::consts::FRAC_PI_2,
                0.1,
                1000.0,
                [640, 360],
            )
            .is_none()
        );
    }

    #[test]
    fn world_anchor_outside_the_frustum_is_not_projected() {
        assert!(
            project_world_point_to_screen(
                [10.0, 0.0, -2.0],
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
                std::f32::consts::FRAC_PI_2,
                0.1,
                1000.0,
                [640, 360],
            )
            .is_none()
        );
    }

    #[test]
    fn world_anchor_submit_is_stored_and_queryable() {
        let mut runtime = WgpuRuntime::headless(1);
        runtime
            .world_bridge
            .configure_world(WorldInformationSnapshot {
                world_space_id: neon_world_bridge::WorldSpaceId("project.world.main".into()),
                revision: Revision(1),
                coordinate_system:
                    neon_world_bridge::CoordinateSystem::RightHandedYUpNegativeZForward,
                units_per_meter: 1.0,
                precision_mode: neon_world_bridge::WorldPrecisionMode::CameraRelativeF64,
            })
            .unwrap();
        let response = runtime.handle(request(
            "anchor-submit",
            "wgpu.world.ui.anchor.submit",
            json!(WorldUiAnchor {
                anchor_id: neon_world_bridge::WorldAnchorId("player.main".into()),
                world_space_id: neon_world_bridge::WorldSpaceId("project.world.main".into()),
                producer_epoch: 1,
                sequence: 1,
                timestamp_monotonic_ns: 1,
                position: [1.0, 2.0, 3.0],
                billboard: true,
                occlusion: "always_visible".into(),
                screen_x: 0.5,
                screen_y: 0.5,
                view_distance: 10.0,
            }),
        ));
        assert_eq!(response.status, RpcStatus::Accepted);
        let stored = runtime
            .world_bridge
            .anchor(&neon_world_bridge::WorldAnchorId("player.main".into()))
            .expect("anchor stored");
        assert_eq!(stored.position, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn world_panel_is_placed_at_the_host_provided_anchor_placement() {
        let mut runtime = WgpuRuntime::headless(1);
        runtime
            .world_bridge
            .configure_world(WorldInformationSnapshot {
                world_space_id: neon_world_bridge::WorldSpaceId("project.world.main".into()),
                revision: Revision(1),
                coordinate_system:
                    neon_world_bridge::CoordinateSystem::RightHandedYUpNegativeZForward,
                units_per_meter: 1.0,
                precision_mode: neon_world_bridge::WorldPrecisionMode::CameraRelativeF64,
            })
            .unwrap();
        // Camera available (availability gate only; placement comes from the host).
        runtime
            .world_bridge
            .submit_camera_frame(CameraFrame {
                camera_id: neon_world_bridge::CameraId("editor".into()),
                world_space_id: neon_world_bridge::WorldSpaceId("project.world.main".into()),
                producer_epoch: 1,
                sequence: 1,
                timestamp_monotonic_ns: 1,
                payload: neon_world_bridge::CameraFramePayload::ThreeDimensional {
                    position: [0.0, 0.0, 0.0],
                    orientation: [0.0, 0.0, 0.0, 1.0],
                    vertical_fov_radians: std::f32::consts::FRAC_PI_2,
                    near: 0.1,
                    far: 1000.0,
                },
            })
            .unwrap();
        // Host placement: dead center (0.5, 0.5), view distance 2 m.
        runtime
            .world_bridge
            .submit_anchor(WorldUiAnchor {
                anchor_id: neon_world_bridge::WorldAnchorId("player.main".into()),
                world_space_id: neon_world_bridge::WorldSpaceId("project.world.main".into()),
                producer_epoch: 1,
                sequence: 1,
                timestamp_monotonic_ns: 1,
                position: [0.0, 0.0, -2.0],
                billboard: true,
                occlusion: "depth_tested".into(),
                screen_x: 0.5,
                screen_y: 0.5,
                view_distance: 2.0,
            })
            .unwrap();
        let mut gated = fragment(1);
        let marker = UiNode {
            node_id: UiNodeId("status-root".into()),
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
            world_depth: None,
            world_scale: None,
            children: Vec::new(),
        };
        gated.root.children.push(marker);
        gated.effects.push(UiEffect::CameraVisibility {
            binding: neon_ui_schema::UiCameraVisibilityBinding {
                node_id: UiNodeId("status-root".into()),
                camera_id: neon_world_bridge::CameraId("editor".into()),
                camera_kind: neon_world_bridge::CameraKind::ThreeDimensional,
                anchor_id: Some(neon_world_bridge::WorldAnchorId("player.main".into())),
            },
        });
        gated.effects.push(UiEffect::BoundSemanticIntent {
            node_id: UiNodeId("status-root".into()),
            intent: neon_ui_schema::UiIntent::Invoke {
                action: "world.test.click".into(),
                params: json!({}),
            },
        });
        runtime.fragments.insert(gated.fragment_id.clone(), gated);
        let snapshot = runtime.fragments_snapshot();
        let bounds = &snapshot[&UiFragmentId("static-fragment".into())]
            .root
            .children[0]
            .bounds;
        let depth = snapshot[&UiFragmentId("static-fragment".into())]
            .root
            .children[0]
            .world_depth
            .expect("depth-tested world panel exports a depth");
        assert_eq!(
            snapshot[&UiFragmentId("static-fragment".into())]
                .root
                .children[0]
                .world_scale,
            Some(2.0)
        );
        // viewport [1280, 720]: center (640, 360). depth 2 → uniform scale 2.0.
        // The anchor point remains the panel's bottom-center; authored x/y are
        // not rewritten by the projection filter.
        assert!((bounds.x - 635.0).abs() < 0.5, "x was {}", bounds.x);
        assert!((bounds.y - 350.0).abs() < 0.5, "y was {}", bounds.y);
        assert!((depth - 0.05).abs() < f32::EPSILON, "depth was {depth}");
        let (device, queue) = test_device("neon3-world-ui-id-target");
        let mut id_renderer =
            ui_renderer::UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let pixels = ui_renderer::render_hit_ids_with_renderer_for_test(
            &mut id_renderer,
            &device,
            &queue,
            &snapshot,
            [1280, 720],
        );
        let hit_id = pixels[350 * 1280 + 640];
        assert_ne!(
            hit_id, RENDER_HIT_NONE,
            "the projected WorldUi panel must write a non-clear ID"
        );
        assert_eq!(
            id_renderer
                .hit_binding(hit_id)
                .expect("ID must map to a semantic binding")
                .node_path,
            "static-fragment/status-root"
        );
    }

    #[test]
    fn window_control_advertises_gpu_generation_capability_only_there() {
        let headless = WgpuRuntime::headless(1);
        let window = WgpuRuntime::window_control(
            1,
            Arc::new(Mutex::new(InteractionTraceStore::new())),
            Arc::new(Mutex::new(WorldUiLabCameraController::default())),
        );
        assert!(
            !headless
                .service_description()
                .capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_AI_TERRAIN_GENERATION)
        );
        assert!(
            window
                .service_description()
                .capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_AI_TERRAIN_GENERATION)
        );
        assert!(
            window
                .service_description()
                .capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_DEBUG_INTERACTION)
        );
        assert!(
            window
                .service_description()
                .capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_DEBUG_WINDOW_CAPTURE)
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
    fn next_fragment_revision_atomically_replaces_the_registry_entry() {
        let mut runtime = WgpuRuntime::headless(1);
        assert_eq!(
            runtime.handle(submit("initial", 1)).status,
            RpcStatus::Accepted
        );
        assert_eq!(
            runtime.handle(submit("replacement", 2)).status,
            RpcStatus::Accepted
        );

        let fragments = runtime.fragments_snapshot();
        assert_eq!(fragments.len(), 1);
        let active = fragments
            .get(&UiFragmentId("static-fragment".into()))
            .unwrap();
        assert_eq!(active.revision, Revision(2));
        assert_eq!(runtime.diagnostics().fragment_count, 1);
    }

    #[test]
    fn window_mailbox_ignores_stale_composition_and_idles_without_dirty_work() {
        let mut window = WindowedRuntime::new(1);
        window.redraw_pending = false;
        assert!(!window.needs_redraw());
        assert!(window.apply_fragments(
            Revision(4),
            HashMap::from([(UiFragmentId("fresh".into()), fragment(4))])
        ));
        assert!(window.needs_redraw());
        window.redraw_pending = false;
        assert!(!window.apply_fragments(
            Revision(3),
            HashMap::from([(UiFragmentId("stale".into()), fragment(3))])
        ));
        assert!(window.fragments.contains_key(&UiFragmentId("fresh".into())));
        assert!(!window.fragments.contains_key(&UiFragmentId("stale".into())));
        assert!(!window.needs_redraw());
    }

    #[test]
    fn root_viewport_requirement_aggregates_bounds_minimums_and_preferences() {
        let mut first = fragment(1);
        first.fragment_id = UiFragmentId("first".into());
        first.root.bounds.width = 800.0;
        first.root.bounds.height = 600.0;
        first.root.layout = Some(UiLayout {
            min_size: Some([1000.0, 0.0]),
            preferred_size: Some([0.0, 720.0]),
            ..UiLayout::default()
        });

        let mut second = fragment(1);
        second.fragment_id = UiFragmentId("second".into());
        second.root.bounds.width = 1200.0;
        second.root.bounds.height = 640.0;

        let mut hidden = fragment(1);
        hidden.fragment_id = UiFragmentId("hidden".into());
        hidden.root.visible = false;
        hidden.root.bounds.width = 4000.0;
        hidden.root.bounds.height = 3000.0;

        assert_eq!(
            aggregate_root_viewport_requirement(&HashMap::from([
                (first.fragment_id.clone(), first),
                (second.fragment_id.clone(), second),
                (hidden.fragment_id.clone(), hidden),
            ])),
            LogicalViewportRequirement {
                width: 1200.0,
                height: 720.0,
            }
        );
    }

    #[test]
    fn component_gallery_declares_its_window_requirement() {
        let document = neon_ui_runtime::parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/imgui-component-gallery.nui"
        ))
        .expect("component gallery must parse");
        let fragment = UiFragment {
            fragment_id: UiFragmentId("component-gallery".into()),
            revision: Revision(1),
            root: document.ir.root,
            effects: Vec::new(),
        };
        assert_eq!(
            aggregate_root_viewport_requirement(&HashMap::from([(
                fragment.fragment_id.clone(),
                fragment,
            )])),
            LogicalViewportRequirement {
                width: 2048.0,
                height: 1080.0,
            }
        );
    }

    #[test]
    fn scripted_initial_window_sizing_converges_without_shrinking_or_retrying() {
        let mut sizing = InitialWindowSizing::default();
        let gallery = LogicalViewportRequirement {
            width: 1668.0,
            height: 900.0,
        };
        assert_eq!(
            sizing.observe_composition(
                gallery,
                LogicalViewportRequirement {
                    width: 1280.0,
                    height: 800.0,
                }
            ),
            Some(gallery)
        );
        assert_eq!(sizing.pending_request, Some(gallery));
        assert_eq!(
            sizing.observe_composition(
                gallery,
                LogicalViewportRequirement {
                    width: 1280.0,
                    height: 800.0,
                }
            ),
            None
        );

        sizing.resize_accepted();
        assert_eq!(
            sizing.observe_composition(
                gallery,
                LogicalViewportRequirement {
                    width: 1600.0,
                    height: 900.0,
                }
            ),
            None,
            "an OS-constrained accepted size must not cause a resize loop"
        );

        let wider = LogicalViewportRequirement {
            width: 1800.0,
            height: 900.0,
        };
        assert_eq!(
            sizing.observe_composition(
                wider,
                LogicalViewportRequirement {
                    width: 1700.0,
                    height: 1200.0,
                }
            ),
            Some(LogicalViewportRequirement {
                width: 1800.0,
                height: 1200.0,
            }),
            "growth must preserve a larger user-selected axis"
        );

        let mut already_large = InitialWindowSizing::default();
        assert_eq!(
            already_large.observe_composition(
                gallery,
                LogicalViewportRequirement {
                    width: 2200.0,
                    height: 1400.0,
                }
            ),
            None
        );
        assert_eq!(
            already_large.observe_composition(
                gallery,
                LogicalViewportRequirement {
                    width: 1200.0,
                    height: 700.0,
                }
            ),
            None,
            "later compositions must not fight a user's resize for an already handled requirement"
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
    fn react_client_cannot_bypass_ui_runtime_to_submit_a_fragment() {
        let mut runtime = WgpuRuntime::headless(1);
        let mut request = submit("react-direct", 1);
        request.client.kind = ClientKind::UiReactClient;
        request.client.origin = "neon-ui-react-client".into();
        let response = runtime.handle(request);
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(
            response.error.unwrap().code,
            "renderer_submission_requires_ui_runtime"
        );
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
                                    let input_index =
                                        (ic * h * w + iy as u32 * w + ix as u32) as usize;
                                    let weight_index =
                                        (oc * in_c * k * k + ic * k * k + ky * k + kx) as usize;
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
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 64.0,
                height: 64.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: Some(neon_ui_schema::RenderSurfaceRef {
                target_id: "ai.terrain.preview".into(),
            }),
            style: UiStyle {
                opacity: 1.0,
                ..UiStyle::default()
            },
            enter_transition: None,
            world_depth: None,
            world_scale: None,
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
    fn world_ui_panel_text_survives_supersampled_world_composition_and_occlusion() {
        let (device, queue) = test_device("neon3-world-ui-panel-composition");
        let fragments = world_ui_lab_fragment();
        let mut ui = UiWgpuRenderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let panel_pixels = ui_renderer::render_renderer_with_viewport_offscreen_for_test(
            &mut ui,
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            WORLD_UI_LAB_PANEL_SIZE,
            [
                WORLD_UI_LAB_LOGICAL_SIZE[0] as f32,
                WORLD_UI_LAB_LOGICAL_SIZE[1] as f32,
            ],
            0.0,
        );
        let glyph_pixels = |pixels: &[u8]| {
            pixels
                .chunks_exact(4)
                .filter(|pixel| {
                    pixel[0] > 150 && pixel[1] > 180 && pixel[2] > 190 && pixel[3] > 200
                })
                .count()
        };
        assert!(
            glyph_pixels(&panel_pixels) > 100,
            "the supersampled panel texture must contain visible glyph pixels"
        );

        let panel = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("neon3-world-ui-panel-composition-source"),
            size: wgpu::Extent3d {
                width: WORLD_UI_LAB_PANEL_SIZE[0],
                height: WORLD_UI_LAB_PANEL_SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let panel_view = panel.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("neon3-world-ui-panel-composition-source-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("neon3-world-ui-panel-composition-source-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &panel_view,
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
            ui.draw(
                &device,
                &queue,
                &mut pass,
                &fragments,
                WORLD_UI_LAB_PANEL_SIZE,
                [
                    WORLD_UI_LAB_LOGICAL_SIZE[0] as f32,
                    WORLD_UI_LAB_LOGICAL_SIZE[1] as f32,
                ],
                0.0,
                UiDrawMode::All,
            );
        }
        queue.submit(Some(encoder.finish()));

        let pipeline = WorldUiPipeline::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let composed = pipeline
            .capture_lab(
                &device,
                &queue,
                WORLD_UI_LAB_PREVIEW_SIZE,
                &panel_view,
                world_ui_lab_camera(
                    WORLD_UI_LAB_PREVIEW_SIZE,
                    WorldUiCameraState {
                        position: [0.0; 3],
                        yaw: 0.0,
                        pitch: 0.0,
                        vertical_fov: 35.0f32.to_radians(),
                    },
                ),
            )
            .expect("world UI composition must render");
        assert!(
            glyph_pixels(&composed) >= 8,
            "the composed world preview must retain visible panel glyph pixels; found {} pixels",
            glyph_pixels(&composed)
        );
        let occluder_pixels = composed
            .chunks_exact(4)
            .filter(|pixel| pixel[0] > 180 && pixel[1] < 20 && pixel[2] < 20 && pixel[3] == 255)
            .count();
        assert!(
            occluder_pixels > 100,
            "the minimal scene occluder must remain visible over the world panel; found {occluder_pixels} pixels"
        );
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
        engine
            .load_model(&pack)
            .expect("real terrain model must load");
        let converter = gpu_preview::HeightmapPreviewConverter::new(&device);
        let root = UiNode {
            node_id: UiNodeId("terrain-preview".into()),
            kind: UiNodeKind::RenderSurface,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 64.0,
                height: 64.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: Some(neon_ui_schema::RenderSurfaceRef {
                target_id: "ai.terrain.preview".into(),
            }),
            style: UiStyle {
                opacity: 1.0,
                ..UiStyle::default()
            },
            enter_transition: None,
            world_depth: None,
            world_scale: None,
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
            let texture =
                converter.convert(&device, &queue, &generation.heightmap, generation.size);
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
            assert!(
                maximum.saturating_sub(minimum) > 16,
                "the AI preview must contain visible height variation"
            );
            if let Some(previous) = previous.as_ref() {
                assert_ne!(
                    previous, &pixels,
                    "a new seed must replace the existing surface pixels"
                );
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
        assert!(
            pixels.iter().any(|value| *value != 0),
            "UI render target must contain visible pixels"
        );
    }

    #[test]
    fn final_composition_pixels_have_a_stable_checksum() {
        let (device, queue) = test_device("neon3-final-composition-checksum");
        let fragments = HashMap::from([(UiFragmentId("checksum".into()), fragment(1))]);
        let render = || {
            ui_renderer::render_offscreen_for_test(
                &device,
                &queue,
                wgpu::TextureFormat::Rgba8Unorm,
                &fragments,
                [64, 64],
                1.0,
                &[],
                Vec::new(),
            )
        };
        let first = render();
        let second = render();
        assert!(first.chunks_exact(4).any(|pixel| pixel[3] != 0));
        assert_eq!(
            first, second,
            "unchanged final composition pixels must be stable"
        );
        assert_eq!(fnv1a64(&first), fnv1a64(&second));
    }

    #[test]
    fn ui_fragment_renders_visible_pixels_to_srgb_surface_format() {
        let (device, queue) = test_device("neon3-srgb-ui-acceptance");
        let root = UiNode {
            node_id: UiNodeId("srgb-root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 64.0,
                height: 64.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [0.0, 0.7, 0.9, 1.0],
                border_color: [1.0; 4],
                border_width: 0.0,
                corner_radius: 0.0,
                opacity: 1.0,
            },
            enter_transition: None,
            children: Vec::new(),
            world_depth: None,
            world_scale: None,
        };
        let fragments = HashMap::from([(
            UiFragmentId("srgb-acceptance".into()),
            UiFragment {
                fragment_id: UiFragmentId("srgb-acceptance".into()),
                revision: Revision(1),
                root,
                effects: Vec::new(),
            },
        )]);
        let pixels = ui_renderer::render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            &fragments,
            [64, 64],
            1.0,
            &[],
            Vec::new(),
        );
        assert!(
            pixels.iter().any(|value| *value != 0),
            "sRGB composition target must contain visible UI pixels"
        );
    }

    #[test]
    fn ui_hit_target_matches_panel_coverage_and_paint_order() {
        let (device, queue) = test_device("neon3-ui-hit-target-acceptance");
        let mut root = fragment(1).root;
        root.kind = UiNodeKind::Panel;
        root.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 64.0,
            height: 64.0,
        };
        root.children = vec![
            UiNode {
                node_id: UiNodeId("back".into()),
                kind: UiNodeKind::Button,
                bounds: UiBounds {
                    x: 8.0,
                    y: 8.0,
                    width: 32.0,
                    height: 32.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle {
                    corner_radius: 8.0,
                    ..UiStyle::default()
                },
                enter_transition: None,
                world_depth: None,
                world_scale: None,
                children: Vec::new(),
            },
            UiNode {
                node_id: UiNodeId("front".into()),
                kind: UiNodeKind::Button,
                bounds: UiBounds {
                    x: 16.0,
                    y: 16.0,
                    width: 32.0,
                    height: 32.0,
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
                world_depth: None,
                world_scale: None,
                children: Vec::new(),
            },
            UiNode {
                node_id: UiNodeId("disabled".into()),
                kind: UiNodeKind::Button,
                bounds: UiBounds {
                    x: 48.0,
                    y: 48.0,
                    width: 12.0,
                    height: 12.0,
                },
                layout: None,
                visible: true,
                enabled: false,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle::default(),
                enter_transition: None,
                world_depth: None,
                world_scale: None,
                children: Vec::new(),
            },
            UiNode {
                node_id: UiNodeId("transparent".into()),
                kind: UiNodeKind::Button,
                bounds: UiBounds {
                    x: 48.0,
                    y: 32.0,
                    width: 12.0,
                    height: 12.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle {
                    opacity: 0.0,
                    ..UiStyle::default()
                },
                enter_transition: None,
                children: Vec::new(),
                world_depth: None,
                world_scale: None,
            },
        ];
        let pixels = ui_renderer::render_hit_ids_for_test(
            &device,
            &queue,
            &HashMap::from([(
                UiFragmentId("hit-acceptance".into()),
                UiFragment {
                    fragment_id: UiFragmentId("hit-acceptance".into()),
                    revision: Revision(1),
                    root,
                    effects: Vec::new(),
                },
            )]),
            [64, 64],
        );
        let at = |x: usize, y: usize| pixels[y * 64 + x];
        assert_eq!(at(0, 0), RENDER_HIT_NONE, "background must remain no-hit");
        assert_eq!(
            at(8, 8),
            RENDER_HIT_NONE,
            "rounded corner must discard its hit ID"
        );
        assert_ne!(
            at(12, 20),
            RENDER_HIT_NONE,
            "interactive panel interior must receive an ID"
        );
        assert_ne!(
            at(20, 20),
            RENDER_HIT_NONE,
            "front panel interior must receive an ID"
        );
        assert_ne!(
            at(12, 20),
            at(20, 20),
            "front-most panel must replace the lower ID"
        );
        assert_eq!(
            at(52, 52),
            RENDER_HIT_NONE,
            "disabled panel must remain no-hit"
        );
        assert_eq!(
            at(52, 36),
            RENDER_HIT_NONE,
            "transparent panel must remain no-hit"
        );
    }

    #[test]
    fn ui_hit_target_respects_nested_clip_geometry() {
        let (device, queue) = test_device("neon3-ui-clip-acceptance");
        let child = UiNode {
            node_id: UiNodeId("clipped-button".into()),
            kind: UiNodeKind::Button,
            bounds: UiBounds {
                x: 24.0,
                y: 8.0,
                width: 24.0,
                height: 16.0,
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
            world_depth: None,
            world_scale: None,
            children: Vec::new(),
        };
        let clipper = UiNode {
            node_id: UiNodeId("clipper".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 32.0,
                height: 32.0,
            },
            layout: Some(neon_ui_schema::UiLayout {
                clip: neon_ui_schema::UiClipPolicy::Bounds,
                ..neon_ui_schema::UiLayout::default()
            }),
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            children: vec![child],
            world_depth: None,
            world_scale: None,
        };
        let root = UiNode {
            node_id: UiNodeId("clip-root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 64.0,
                height: 64.0,
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
            children: vec![clipper],
            world_depth: None,
            world_scale: None,
        };
        let pixels = ui_renderer::render_hit_ids_for_test(
            &device,
            &queue,
            &HashMap::from([(
                UiFragmentId("clip".into()),
                UiFragment {
                    fragment_id: UiFragmentId("clip".into()),
                    revision: Revision(1),
                    root,
                    effects: Vec::new(),
                },
            )]),
            [64, 64],
        );
        assert_ne!(
            pixels[12 * 64 + 28],
            RENDER_HIT_NONE,
            "child area inside parent clip must be interactive"
        );
        assert_eq!(
            pixels[12 * 64 + 40],
            RENDER_HIT_NONE,
            "child area outside parent clip must be no-hit"
        );
    }

    #[test]
    fn composition_target_apis_are_machine_readable() {
        let mut runtime = WgpuRuntime::headless(3);
        let graph = runtime.handle(request("graph", "wgpu.render.graph.snapshot", json!({})));
        assert_eq!(graph.status, RpcStatus::Accepted);
        assert_eq!(
            graph.result.as_ref().unwrap()["targets"][1]["id"],
            UI_HIT_TARGET
        );
        assert_eq!(
            graph.result.as_ref().unwrap()["targets"][1]["format"],
            "r32uint"
        );
        let capture = runtime.handle(request(
            "capture",
            "wgpu.render.target.capture",
            json!({"target": UI_HIT_TARGET}),
        ));
        assert_eq!(capture.status, RpcStatus::Accepted);
        assert_eq!(capture.result.as_ref().unwrap()["format"], "r32uint");
        let assertion = runtime.handle(request(
            "assert",
            "wgpu.render.target.assert",
            json!({"target": UI_HIT_TARGET, "assertions": []}),
        ));
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
        let mut remove = request(
            "remove",
            "wgpu.ui.remove_fragment",
            json!(UiCommand::RemoveFragment {
                fragment_id: UiFragmentId("static-fragment".into()),
                revision: Revision(1)
            }),
        );
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
        input.request_sample(HitSampleRequest {
            pointer_id: 4,
            sequence: 1,
            composition_revision: Revision(3),
            target_generation: 2,
        });
        input.complete_sample(4, Revision(3), 2, 41).unwrap();
        assert_eq!(input.state, LocalInteractionState::Hovered);
        input.pointer_down().unwrap();
        assert_eq!(input.capture_id, Some(41));
        input.request_sample(HitSampleRequest {
            pointer_id: 4,
            sequence: 2,
            composition_revision: Revision(3),
            target_generation: 2,
        });
        input
            .complete_sample(4, Revision(3), 2, RENDER_HIT_NONE)
            .unwrap();
        assert_eq!(
            input.capture_id,
            Some(41),
            "move outside must retain capture"
        );
        assert_eq!(
            input.hover_id,
            Some(41),
            "stale GPU readback must not overwrite the captured hover target"
        );
        input.pointer_up(true).unwrap();
        assert_eq!(input.state, LocalInteractionState::Idle);
    }

    #[test]
    fn local_input_rejects_stale_samples_and_cancels_explicitly() {
        let mut input = LocalInputState::default();
        input.request_sample(HitSampleRequest {
            pointer_id: 0,
            sequence: 1,
            composition_revision: Revision(3),
            target_generation: 2,
        });
        assert_eq!(
            input.complete_sample(0, Revision(3), 3, 7),
            Err("hit_target_generation_stale")
        );
        input.request_sample(HitSampleRequest {
            pointer_id: 0,
            sequence: 1,
            composition_revision: Revision(2),
            target_generation: 2,
        });
        assert_eq!(
            input.complete_sample(0, Revision(3), 2, 7),
            Err("composition_revision_stale")
        );
        input.request_sample(HitSampleRequest {
            pointer_id: 0,
            sequence: 2,
            composition_revision: Revision(3),
            target_generation: 2,
        });
        input.complete_sample(0, Revision(3), 2, 7).unwrap();
        input.request_sample(HitSampleRequest {
            pointer_id: 0,
            sequence: 2,
            composition_revision: Revision(3),
            target_generation: 2,
        });
        assert_eq!(
            input.complete_sample(0, Revision(3), 2, 7),
            Err("input_sequence_stale")
        );
        input.pointer_down().unwrap();
        input.cancel();
        assert_eq!(input.state, LocalInteractionState::Cancelled);
        assert_eq!(input.pointer_up(true), Err("interaction_cancelled"));
    }

    #[test]
    fn test_input_methods_expose_semantic_lifecycle_without_render_ids() {
        let mut runtime = WgpuRuntime::headless(1);
        let request_sample = runtime.handle(request(
            "sample-request",
            "test.ui.hit_sample.request",
            json!({"pointer_id": 0, "sequence": 1}),
        ));
        assert_eq!(request_sample.status, RpcStatus::Accepted);
        let completed = runtime.handle(request(
            "sample-complete",
            "test.ui.hit_sample.complete",
            json!({"pointer_id": 0, "test_hit_id": 17}),
        ));
        assert_eq!(completed.status, RpcStatus::Accepted);
        assert!(
            completed
                .result
                .as_ref()
                .unwrap()
                .get("render_hit_id")
                .is_none()
        );
        assert_eq!(
            runtime
                .handle(request("down", "test.ui.pointer.down", json!({})))
                .status,
            RpcStatus::Accepted
        );
        let click = runtime.handle(request(
            "up",
            "test.ui.pointer.up",
            json!({"eligible": true}),
        ));
        assert_eq!(click.status, RpcStatus::Accepted);
        assert_eq!(click.result.unwrap()["semantic_event"], "ui.pointer.click");
        let cancelled = runtime.handle(request("focus-loss", "test.ui.focus.loss", json!({})));
        assert_eq!(
            cancelled.result.unwrap()["semantic_event"],
            "ui.interaction.cancelled"
        );
    }

    #[test]
    fn trace_query_filters_u3_pointer_and_revision_metadata() {
        let mut runtime = WgpuRuntime::headless(1);
        runtime.handle(request(
            "sample-request",
            "test.ui.hit_sample.request",
            json!({"pointer_id": 9, "sequence": 1}),
        ));
        let response = runtime.handle(request(
            "trace",
            "debug.trace.query",
            json!({"pointer_id": 9, "fragment_revision": 0, "composition_revision": 0}),
        ));
        assert_eq!(response.status, RpcStatus::Accepted);
        let records = response.result.unwrap().as_array().unwrap().clone();
        assert!(
            records
                .iter()
                .any(|record| record["event"] == "ui.hit_sample.requested")
        );
        assert!(
            records
                .iter()
                .all(|record| record["data"].get("render_hit_id").is_none())
        );
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
        traces.append(
            interaction_id.clone(),
            InteractionTraceStage::Prepared,
            InteractionTraceOutcome::Pending,
            None,
            None,
            None,
            Revision(4),
            None,
        );
        traces.append(
            interaction_id.clone(),
            InteractionTraceStage::HitCaptureResolved,
            InteractionTraceOutcome::Accepted,
            None,
            target.clone(),
            Some(Revision(9)),
            Revision(4),
            None,
        );
        traces.append(
            interaction_id.clone(),
            InteractionTraceStage::SemanticEventForwarded,
            InteractionTraceOutcome::Pending,
            None,
            target.clone(),
            Some(Revision(9)),
            Revision(4),
            Some(downstream.clone()),
        );
        traces.append(
            interaction_id.clone(),
            InteractionTraceStage::DeliveryAccepted,
            InteractionTraceOutcome::Accepted,
            None,
            target,
            Some(Revision(9)),
            Revision(4),
            Some(downstream),
        );
        traces.delivery_accepted(interaction_id.clone());
        traces.composition_applied(Revision(5));
        drop(traces);

        let response = runtime.handle(request(
            "interaction-get",
            "debug.interaction.get",
            json!({"interaction_id": interaction_id.0}),
        ));
        assert_eq!(response.status, RpcStatus::Accepted);
        let records = response.result.unwrap()["records"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(records.len(), 5);
        assert_eq!(records[4]["stage"], "composition_revision_applied");
        assert_eq!(records[4]["composition_revision"], 5);
        assert_eq!(records[2]["downstream_request_id"], "wgpu-pointer-click-7");
        assert_eq!(
            records[1]["semantic_target"]["node_path"],
            "tools/water/apply"
        );
        assert!(records.iter().all(|record| record.get("coordinates").is_none() && record.get("hit_id").is_none()));
    }

    #[test]
    fn interaction_trace_query_filters_rejected_delivery() {
        let mut runtime = WgpuRuntime::headless(1);
        let interaction_id = InteractionId("wgpu-window-1-8".into());
        let downstream = RequestId("wgpu-pointer-click-8".into());
        let mut traces = runtime.interaction_traces.lock().unwrap();
        traces.append(
            interaction_id.clone(),
            InteractionTraceStage::Prepared,
            InteractionTraceOutcome::Pending,
            None,
            None,
            None,
            Revision(6),
            None,
        );
        traces.append(
            interaction_id.clone(),
            InteractionTraceStage::DeliveryRejected,
            InteractionTraceOutcome::Rejected,
            Some(InteractionTraceError {
                code: "intent_not_bound".into(),
                message: "semantic intent is not bound".into(),
            }),
            Some(InteractionSemanticTarget {
                node_path: "assets/remove".into(),
            }),
            Some(Revision(2)),
            Revision(6),
            Some(downstream),
        );
        drop(traces);

        let response = runtime.handle(request("interaction-query", "debug.interaction.query", json!({"after": 1, "limit": 1, "filters": {"outcome": "rejected", "semantic_node_path": "assets/remove"}})));
        assert_eq!(response.status, RpcStatus::Accepted);
        let records = response.result.unwrap()["records"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["stage"], "delivery_rejected");
        assert_eq!(records[0]["error"]["code"], "intent_not_bound");
    }

    #[test]
    fn accepted_real_drag_lifecycle_records_semantics_and_delivery() {
        let server = neon_ipc::RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = std::thread::spawn(move || {
            server
                .serve_one(|request| RpcResponse {
                    request_id: request.request_id,
                    status: RpcStatus::Accepted,
                    revision: Some(Revision(12)),
                    result: Some(json!({"state": "accepted"})),
                    snapshot: None,
                    error: None,
                })
                .unwrap();
        });
        let traces = Arc::new(Mutex::new(InteractionTraceStore::new()));
        let delivery = Arc::new(Mutex::new(json!({"state": "none"})));
        let interaction_id = InteractionId("wgpu-window-4-1".into());
        traces.lock().unwrap().append(
            interaction_id.clone(),
            InteractionTraceStage::Prepared,
            InteractionTraceOutcome::Pending,
            None,
            None,
            None,
            Revision(8),
            None,
        );
        append_drag_interaction_record(
            &traces,
            interaction_id.clone(),
            InteractionTraceStage::DragStarted,
            InteractionTraceOutcome::Accepted,
            Some("backpack-compass".into()),
            None,
            Revision(3),
            Revision(8),
            None,
        );
        append_drag_interaction_record(
            &traces,
            interaction_id.clone(),
            InteractionTraceStage::DragPreviewMoved,
            InteractionTraceOutcome::Pending,
            Some("backpack-compass".into()),
            None,
            Revision(3),
            Revision(8),
            None,
        );
        let resolved = ui_renderer::UiResolvedDragDrop {
            fragment: neon_ui_schema::UiFragmentRevision {
                id: UiFragmentId("gallery".into()),
                revision: Revision(3),
            },
            intent: neon_ui_schema::UiIntent::Invoke {
                action: "inventory.item.equip".into(),
                params: json!({}),
            },
            source_key: "backpack-compass".into(),
            target_key: "equipment-zone".into(),
            placement: neon_ui_schema::UiDropPlacement::Into,
            presentation_template_key: Some("equipment-item-template".into()),
            local_presentation: LocalPresentationCommit::Drag {
                source_path: "gallery/backpack-compass".into(),
                offset: [24.0, 16.0],
            },
        };
        record_drag_release_lifecycle(
            &traces,
            &interaction_id,
            "backpack-compass",
            Revision(3),
            true,
            Some(&resolved),
            Revision(8),
        );
        forward_drag_drop(
            endpoint,
            4,
            Revision(8),
            11,
            Some(interaction_id.clone()),
            resolved,
            delivery.clone(),
            traces.clone(),
            None,
            PendingLocalPresentationKey {
                semantic_sequence: 11,
                fragment_id: "gallery".into(),
                fragment_revision: 3,
            },
        );
        server_thread.join().unwrap();
        for _ in 0..100 {
            if delivery.lock().unwrap()["state"] == "accepted" {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let records = traces.lock().unwrap().get(&interaction_id);
        let stages = records
            .iter()
            .map(|record| record.stage)
            .collect::<Vec<_>>();
        assert_eq!(
            stages,
            vec![
                InteractionTraceStage::Prepared,
                InteractionTraceStage::DragStarted,
                InteractionTraceStage::DragPreviewMoved,
                InteractionTraceStage::DropTargetResolved,
                InteractionTraceStage::DragReleased,
                InteractionTraceStage::SemanticEventForwarded,
                InteractionTraceStage::DeliveryAccepted,
            ]
        );
        assert_eq!(
            records[3].semantic_target.as_ref().unwrap().node_path,
            "equipment-zone"
        );
        assert_eq!(
            records[1].semantic_source_key.as_deref(),
            Some("backpack-compass")
        );
        assert!(records[1].semantic_target.is_none());
        assert_eq!(
            records[3].semantic_intent.as_deref(),
            Some("inventory.item.equip")
        );
        assert_eq!(records[3].fragment_revision, Some(Revision(3)));
        assert!(records.iter().all(|record| {
            let value = serde_json::to_value(record).unwrap();
            value.get("coordinates").is_none() && value.get("hit_id").is_none()
        }));
    }

    #[test]
    fn rejected_real_drag_lifecycle_records_a_stable_target_reason() {
        let traces = Arc::new(Mutex::new(InteractionTraceStore::new()));
        let interaction_id = InteractionId("wgpu-window-4-2".into());
        append_drag_interaction_record(
            &traces,
            interaction_id.clone(),
            InteractionTraceStage::DragStarted,
            InteractionTraceOutcome::Accepted,
            Some("backpack-gem".into()),
            None,
            Revision(5),
            Revision(9),
            None,
        );
        append_drag_interaction_record(
            &traces,
            interaction_id.clone(),
            InteractionTraceStage::DragPreviewMoved,
            InteractionTraceOutcome::Pending,
            Some("backpack-gem".into()),
            None,
            Revision(5),
            Revision(9),
            None,
        );
        record_drag_release_lifecycle(
            &traces,
            &interaction_id,
            "backpack-gem",
            Revision(5),
            true,
            None,
            Revision(9),
        );

        let records = traces.lock().unwrap().get(&interaction_id);
        assert_eq!(records[2].stage, InteractionTraceStage::DropTargetRejected);
        assert_eq!(
            records[2].error.as_ref().unwrap().code,
            "drop_target_not_declared"
        );
        assert!(
            records
                .iter()
                .all(|record| record.downstream_request_id.is_none())
        );
        assert_eq!(
            records.last().unwrap().stage,
            InteractionTraceStage::DragReleased
        );
    }

    #[test]
    fn owner_asset_bytes_preload_reports_readiness_and_trace() {
        let mut runtime = WgpuRuntime::headless(1);
        let asset = AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 81,
            revision: Revision(5),
            kind: "image".into(),
        };
        let missing_content = runtime.handle(request(
            "missing-content",
            "wgpu.ui.resource.preload",
            json!(asset.clone()),
        ));
        assert_eq!(
            missing_content.error.unwrap().code,
            "asset_content_required"
        );
        let owner_content = AssetBytes {
            asset: asset.clone(),
            media_type: "application/x-neon-rgba8".into(),
            width: Some(2),
            height: Some(1),
            bytes: vec![51, 204, 102, 255, 51, 204, 102, 0],
        };
        let response = runtime.preload_resource_from_owner(
            request("preload", "wgpu.ui.resource.preload", json!(asset)),
            owner_content,
        );
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.result.as_ref().unwrap()["state"], "ready");
        let wait = runtime.handle(request(
            "wait",
            "wgpu.resource.wait_ready",
            json!({"asset_id": 81}),
        ));
        assert_eq!(wait.result.unwrap()["state"], "ready");
        let trace = runtime.handle(request(
            "trace",
            "debug.trace.query",
            json!({"request_id": "preload"}),
        ));
        assert!(
            trace
                .result
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["event"] == "ui.resource.ready")
        );
    }

    #[test]
    fn owner_resource_failure_is_queryable_by_job_and_trace() {
        let mut runtime = WgpuRuntime::headless(1);
        let asset = AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 82,
            revision: Revision(5),
            kind: "font".into(),
        };
        let response = runtime.preload_resource_from_owner(
            request(
                "font-invalid",
                "wgpu.ui.resource.preload",
                json!(asset.clone()),
            ),
            AssetBytes {
                asset: asset.clone(),
                media_type: "font/ttf".into(),
                width: None,
                height: None,
                bytes: Vec::new(),
            },
        );
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "invalid_resource_content");
        let wait = runtime.handle(request(
            "font-wait",
            "wgpu.resource.wait_ready",
            json!({"asset_id": 82}),
        ));
        assert_eq!(wait.status, RpcStatus::Accepted);
        assert_eq!(wait.result.unwrap()["state"], "failed");
        let trace = runtime.handle(request(
            "font-trace",
            "debug.trace.query",
            json!({"request_id": "font-invalid"}),
        ));
        assert!(
            trace
                .result
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["event"] == "ui.resource.failed")
        );
    }

    #[test]
    fn project_asset_bytes_drive_renderer_private_image_residency() {
        let asset = AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 81,
            revision: Revision(5),
            kind: "image".into(),
        };
        let mut projectd = neon_projectd::Projectd::fixture(3);
        let describe = projectd.handle(request("project-describe", "service.describe", json!({})));
        assert_eq!(describe.status, RpcStatus::Accepted);
        assert!(
            describe.result.as_ref().unwrap()["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|capability| capability == neon_projectd::CAPABILITY_ASSET_BYTES)
        );
        let snapshot =
            projectd.handle(request("project-snapshot", "debug.snapshot.get", json!({})));
        assert_eq!(snapshot.status, RpcStatus::Accepted);
        assert_eq!(snapshot.result.as_ref().unwrap()["revision"], 5);
        let owner_response = projectd.handle(request(
            "project-asset",
            "asset.get_bytes",
            json!(asset.clone()),
        ));
        assert_eq!(owner_response.status, RpcStatus::Accepted);
        let content: AssetBytes = serde_json::from_value(owner_response.result.unwrap()).unwrap();

        let mut runtime = WgpuRuntime::headless(1);
        let preload = runtime.preload_resource_from_owner(
            request("preload", "wgpu.ui.resource.preload", json!(asset.clone())),
            content.clone(),
        );
        assert_eq!(preload.status, RpcStatus::Accepted);
        assert_eq!(
            preload.result.as_ref().unwrap()["job_id"],
            "ui-resource-81-5"
        );
        let (device, queue) = test_device("neon3-ui-image-alpha");
        let mut image = fragment(1).root;
        image.kind = UiNodeKind::Image;
        image.bounds = UiBounds {
            x: 0.0,
            y: 0.0,
            width: 64.0,
            height: 64.0,
        };
        image.image = Some(AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 81,
            revision: Revision(5),
            kind: "image".into(),
        });
        image.style = UiStyle {
            background_color: [0.2, 0.8, 0.4, 1.0],
            border_color: [0.0; 4],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        };
        let fragments = HashMap::from([(
            UiFragmentId("image".into()),
            UiFragment {
                fragment_id: UiFragmentId("image".into()),
                revision: Revision(1),
                root: image,
                effects: Vec::new(),
            },
        )]);
        let unresolved = ui_renderer::render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [64, 64],
            1.0,
            &[],
            Vec::new(),
        );
        assert_eq!(
            unresolved[4 * (16 * 64 + 16) + 3],
            0,
            "an unresolved AssetRef must not render a fixture image"
        );
        let pixels = ui_renderer::render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [64, 64],
            1.0,
            &[content],
            Vec::new(),
        );
        assert!(
            pixels[4 * (16 * 64 + 16) + 3] > 0,
            "the opaque image half must render alpha"
        );
        assert_eq!(
            pixels[4 * (16 * 64 + 48) + 3],
            0,
            "the transparent image half must preserve alpha"
        );
    }

    #[test]
    fn project_font_bytes_drive_renderer_private_readiness() {
        let asset = AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 82,
            revision: Revision(5),
            kind: "font".into(),
        };
        let mut projectd = neon_projectd::Projectd::fixture(3);
        assert_eq!(
            projectd
                .handle(request(
                    "font-project-describe",
                    "service.describe",
                    json!({})
                ))
                .status,
            RpcStatus::Accepted
        );
        assert_eq!(
            projectd
                .handle(request(
                    "font-project-snapshot",
                    "debug.snapshot.get",
                    json!({})
                ))
                .status,
            RpcStatus::Accepted
        );
        let owner_response = projectd.handle(request(
            "font-project-asset",
            "asset.get_bytes",
            json!(asset.clone()),
        ));
        let content: AssetBytes = serde_json::from_value(owner_response.result.unwrap()).unwrap();
        assert_eq!(content.media_type, "font/ttf");

        let mut runtime = WgpuRuntime::headless(1);
        let preload = runtime.preload_resource_from_owner(
            request("font-preload", "wgpu.ui.resource.preload", json!(asset)),
            content,
        );
        assert_eq!(preload.status, RpcStatus::Accepted);
        assert_eq!(preload.result.unwrap()["job_id"], "ui-resource-82-5");
        let wait = runtime.handle(request(
            "font-wait",
            "wgpu.resource.wait_ready",
            json!({"asset_id": 82}),
        ));
        assert_eq!(wait.result.unwrap()["state"], "ready");
        let trace = runtime.handle(request(
            "font-trace",
            "debug.trace.query",
            json!({"request_id": "font-preload"}),
        ));
        assert!(
            trace
                .result
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["event"] == "ui.resource.ready")
        );
    }

    #[test]
    fn project_font_preload_job_can_override_bundled_text_glyph_residency() {
        let asset = AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 82,
            revision: Revision(5),
            kind: "font".into(),
        };
        let mut projectd = neon_projectd::Projectd::fixture(3);
        assert_eq!(
            projectd
                .handle(request(
                    "font-glyph-project-describe",
                    "service.describe",
                    json!({})
                ))
                .status,
            RpcStatus::Accepted
        );
        assert_eq!(
            projectd
                .handle(request(
                    "font-glyph-project-snapshot",
                    "debug.snapshot.get",
                    json!({})
                ))
                .status,
            RpcStatus::Accepted
        );
        let owner_response = projectd.handle(request(
            "font-glyph-project-asset",
            "asset.get_bytes",
            json!(asset.clone()),
        ));
        assert_eq!(owner_response.status, RpcStatus::Accepted);
        let content: AssetBytes = serde_json::from_value(owner_response.result.unwrap()).unwrap();

        let mut runtime = WgpuRuntime::headless(1);
        let preload = runtime.preload_resource_from_owner(
            request(
                "font-glyph-preload",
                "wgpu.ui.resource.preload",
                json!(asset),
            ),
            content.clone(),
        );
        assert_eq!(preload.status, RpcStatus::Accepted);
        assert_eq!(
            preload.result.as_ref().unwrap()["job_id"],
            "ui-resource-82-5"
        );
        let trace = runtime.handle(request(
            "font-glyph-trace",
            "debug.trace.query",
            json!({"request_id": "font-glyph-preload"}),
        ));
        assert!(
            trace
                .result
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["event"] == "ui.resource.ready")
        );

        let (device, queue) = test_device("neon3-ui-font-glyph");
        let mut text = fragment(1).root;
        text.kind = UiNodeKind::Label;
        text.bounds = UiBounds {
            x: 4.0,
            y: 4.0,
            width: 56.0,
            height: 24.0,
        };
        text.text = Some(neon_ui_schema::TextRef::Literal { value: "A".into() });
        text.style = UiStyle {
            background_color: [0.0; 4],
            border_color: [0.0; 4],
            border_width: 0.0,
            corner_radius: 0.0,
            opacity: 1.0,
        };
        let fragments = HashMap::from([(
            UiFragmentId("font-glyph".into()),
            UiFragment {
                fragment_id: UiFragmentId("font-glyph".into()),
                revision: Revision(1),
                root: text,
                effects: Vec::new(),
            },
        )]);
        let bundled = ui_renderer::render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [64, 32],
            1.0,
            &[],
            Vec::new(),
        );
        assert!(
            bundled.chunks_exact(4).any(|pixel| pixel[3] > 0),
            "bundled UI font must render glyph pixels"
        );
        let pixels = ui_renderer::render_offscreen_for_test(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
            &fragments,
            [64, 32],
            1.0,
            &[content],
            Vec::new(),
        );
        assert!(
            pixels.chunks_exact(4).any(|pixel| pixel[3] > 0),
            "accepted owner font content must drive private glyph pixels"
        );
    }

    #[test]
    fn resource_preload_rejects_non_ui_asset_kinds() {
        let mut runtime = WgpuRuntime::headless(1);
        let asset = AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 82,
            revision: Revision(1),
            kind: "water_material".into(),
        };
        let response = runtime.handle(request("preload", "wgpu.ui.resource.preload", json!(asset)));
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "unsupported_resource_kind");
    }

    #[test]
    fn test_semantic_injection_accepts_only_bound_semantic_events() {
        use neon_ui_schema::{
            UiFragmentRevision, UiIntent, UiPointerMetadata, UiSemanticEventType,
        };
        let mut runtime = WgpuRuntime::headless(7);
        let mut semantic_fragment = fragment(1);
        semantic_fragment
            .effects
            .push(neon_ui_schema::UiEffect::SemanticIntent {
                intent: UiIntent::Invoke {
                    action: "terrain.tool.select".into(),
                    params: json!({"tool": "water_inject"}),
                },
            });
        let mut submit_request = request(
            "submit",
            "wgpu.ui.submit_fragment",
            json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(semantic_fragment)
            }),
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
            pointer: Some(UiPointerMetadata { id: 0, sequence: 1 }),
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
