//! Bevy host case for Neon3 UI integration.
//!
//! The case deliberately keeps gameplay ECS and Neon UI separate. Bevy owns
//! entities, movement, and camera state. Neon owns NUI declaration, layout,
//! semantic hit resolution, and final UI pixels.

use std::{net::SocketAddr, sync::{mpsc, Arc, Mutex}, thread};

use bevy::prelude::*;
use bevy_render::{
    render_resource::TextureFormat,
    renderer::{RenderDevice, RenderQueue},
    Render, RenderApp,
};
use neon_ipc::{EventClient, RpcClient};
use neon_protocol::{
    ClientIdentity, ClientKind, HostCameraProjection, HostCameraSnapshot, HostUiPointerClick,
    PROTOCOL_VERSION, RenderSurfaceKind, RenderSurfaceOpen, RenderSurfaceSize, RenderSurfaceTarget,
    EventFilter, EventFrame, EventSubscribe, RenderSurfaceTargetKind,
    RequestId, Revision, RpcRequest, RpcResponse, ServiceName, EVENT_PROTOCOL,
};
use neon_ui_schema::{UiInputFrame, UiInputValue, UiProgramRevision};
use serde_json::json;

pub const SCREEN_SURFACE_ID: &str = "case.bevy.screen.ui";
pub const COLOR_TARGET_ID: &str = "case.bevy.screen.ui.color";
pub const ID_TARGET_ID: &str = "case.bevy.screen.ui.id";

nui_flow_vars! {
    CharacterStatusVars => {
        flow: "character-status",
        component: "character.player.main.status",
        fields: {
            health: f32 => "health",
            mana: f32 => "mana",
            level: u32 => "level",
        }
    }
}

#[derive(Clone, Debug)]
pub struct NuiFlowIdentity {
    pub program_revision: UiProgramRevision,
    pub expected_input_revision: Revision,
    pub request_sequence: u64,
}

pub trait NuiFlowVars: Clone + PartialEq {
    const FLOW_NAME: &'static str;
    const COMPONENT_NAME: &'static str;

    fn snapshot(&self, identity: &mut NuiFlowIdentity) -> UiInputFrame;
    fn diff(&self, previous: &Self, identity: &mut NuiFlowIdentity) -> Option<UiInputFrame>;
}

#[macro_export]
macro_rules! nui_flow_vars {
    (
        $name:ident => {
            flow: $flow:literal,
            component: $component:literal,
            fields: {
                $( $field:ident : $ty:ty => $key:literal ),+ $(,)?
            }
        }
    ) => {
        #[derive(Clone, Debug, PartialEq)]
        pub struct $name {
            $( pub $field: $ty, )+
        }

        impl $crate::NuiFlowVars for $name {
            const FLOW_NAME: &'static str = $flow;
            const COMPONENT_NAME: &'static str = $component;

            fn snapshot(&self, identity: &mut $crate::NuiFlowIdentity) -> neon_ui_schema::UiInputFrame {
                identity.request_sequence = identity.request_sequence.saturating_add(1);
                neon_ui_schema::UiInputFrame {
                    program_revision: identity.program_revision.clone(),
                    expected_input_revision: identity.expected_input_revision,
                    request_id: format!("nui-flow:{}:{}", $flow, identity.request_sequence),
                    idempotency_key: format!("nui-flow:{}:{}", $flow, identity.request_sequence),
                    changes: vec![
                        $( neon_ui_schema::UiInputChange { key: $key.into(), value: $crate::nui_flow_value!(self.$field) }, )+
                    ],
                }
            }

            fn diff(&self, previous: &Self, identity: &mut $crate::NuiFlowIdentity) -> Option<neon_ui_schema::UiInputFrame> {
                let mut changes = Vec::new();
                $( if self.$field != previous.$field {
                    changes.push(neon_ui_schema::UiInputChange { key: $key.into(), value: $crate::nui_flow_value!(self.$field) });
                } )+
                if changes.is_empty() {
                    return None;
                }
                identity.request_sequence = identity.request_sequence.saturating_add(1);
                Some(neon_ui_schema::UiInputFrame {
                    program_revision: identity.program_revision.clone(),
                    expected_input_revision: identity.expected_input_revision,
                    request_id: format!("nui-flow:{}:{}", $flow, identity.request_sequence),
                    idempotency_key: format!("nui-flow:{}:{}", $flow, identity.request_sequence),
                    changes,
                })
            }
        }
    };
}

#[macro_export]
macro_rules! nui_flow_value {
    ($value:expr) => {
        $crate::nui_flow_value_ref(&$value)
    };
}

pub fn nui_flow_value_ref<T: NuiFlowValue>(value: &T) -> UiInputValue {
    value.to_ui_input_value()
}

pub trait NuiFlowValue {
    fn to_ui_input_value(&self) -> UiInputValue;
}

impl NuiFlowValue for bool {
    fn to_ui_input_value(&self) -> UiInputValue { UiInputValue::Bool { value: *self } }
}
impl NuiFlowValue for i32 {
    fn to_ui_input_value(&self) -> UiInputValue { UiInputValue::I32 { value: *self } }
}
impl NuiFlowValue for u32 {
    fn to_ui_input_value(&self) -> UiInputValue { UiInputValue::U32 { value: *self } }
}
impl NuiFlowValue for f32 {
    fn to_ui_input_value(&self) -> UiInputValue { UiInputValue::F32 { value: *self } }
}

#[derive(Clone, Debug)]
pub struct Neon3BevyConfig {
    pub wgpu_endpoint: SocketAddr,
    pub ui_endpoint: SocketAddr,
    pub eventd_endpoint: Option<SocketAddr>,
    pub session_id: String,
    pub surface_size: [u32; 2],
}

impl Default for Neon3BevyConfig {
    fn default() -> Self {
        Self {
            wgpu_endpoint: "127.0.0.1:39103".parse().expect("valid default WGPU endpoint"),
            ui_endpoint: "127.0.0.1:39102".parse().expect("valid default UI endpoint"),
            eventd_endpoint: None,
            session_id: "bevy-nui-host".into(),
            surface_size: [1280, 720],
        }
    }
}

#[derive(Resource, Debug)]
pub struct Neon3Session {
    pub config: Neon3BevyConfig,
    pub surface_id: String,
    pub color_target_id: String,
    pub id_target_id: String,
    pub generation: u64,
    pub frame_sequence: u64,
    pub camera_revision: Revision,
    pub connected: bool,
    pub last_error: Option<String>,
}

#[derive(Resource, Default)]
pub struct Neon3IntentQueue {
    pub events: Vec<Neon3Intent>,
}

#[derive(Clone, Debug)]
pub struct Neon3VariableEvent {
    pub name: String,
    pub epoch: u64,
    pub sequence: u64,
    pub payload: serde_json::Value,
}

#[derive(Resource, Default)]
pub struct Neon3VariableEvents {
    pub events: Vec<Neon3VariableEvent>,
}

#[derive(Resource, Clone, Debug)]
pub struct Neon3ExternalSurfaceGpu {
    pub surface_id: String,
    pub color_target_id: String,
    pub id_target_id: String,
    pub size: [u32; 2],
    pub generation: u64,
    pub frame_sequence: u64,
    pub color_format: TextureFormat,
    pub id_format: TextureFormat,
    pub imported: bool,
}

#[derive(Resource, Clone)]
struct Neon3EventQueue(Arc<Mutex<Vec<Neon3VariableEvent>>>);

#[derive(Resource)]
pub struct CharacterStatusBridge {
    pub identity: NuiFlowIdentity,
    pub current: CharacterStatusVars,
    pub sent: Option<CharacterStatusVars>,
}

#[derive(Clone, Debug)]
pub struct Neon3Intent {
    pub action: String,
    pub params: serde_json::Value,
    pub request_id: String,
}

#[derive(Component, Clone, Debug)]
pub struct Neon3HostObject {
    pub object_id: String,
}

#[derive(Component, Clone, Debug)]
pub struct Neon3WorldUi {
    pub surface_id: String,
    pub billboard: bool,
}

#[derive(Component)]
pub struct Neon3WalkableCharacter;

#[derive(Resource)]
struct Neon3Transport {
    requests: mpsc::Sender<TransportRequest>,
    responses: Arc<Mutex<mpsc::Receiver<Result<RpcResponse, String>>>>,
}

struct TransportRequest {
    endpoint: SocketAddr,
    request: RpcRequest,
}

pub struct Neon3BevyPlugin {
    config: Neon3BevyConfig,
}

impl Neon3BevyPlugin {
    pub fn new(config: Neon3BevyConfig) -> Self {
        Self { config }
    }
}

impl Default for Neon3BevyPlugin {
    fn default() -> Self {
        Self::new(Neon3BevyConfig::default())
    }
}

impl Plugin for Neon3BevyPlugin {
    fn build(&self, app: &mut App) {
        let (request_tx, request_rx) = mpsc::channel::<TransportRequest>();
        let (response_tx, response_rx) = mpsc::channel::<Result<RpcResponse, String>>();
        let endpoint = self.config.wgpu_endpoint;
        thread::Builder::new()
            .name("neon3-bevy-rpc".into())
            .spawn(move || transport_worker(endpoint, request_rx, response_tx))
            .expect("start Neon3 Bevy transport worker");

        let variable_events = Arc::new(Mutex::new(Vec::new()));
        if let Some(eventd_endpoint) = self.config.eventd_endpoint {
            let events_for_thread = Arc::clone(&variable_events);
            thread::Builder::new()
                .name("neon3-bevy-eventd".into())
                .spawn(move || eventd_worker(eventd_endpoint, events_for_thread))
                .expect("start Neon3 Bevy eventd worker");
        }

        app.insert_resource(Neon3Session {
            config: self.config.clone(),
            surface_id: SCREEN_SURFACE_ID.into(),
            color_target_id: COLOR_TARGET_ID.into(),
            id_target_id: ID_TARGET_ID.into(),
            generation: 0,
            frame_sequence: 0,
            camera_revision: Revision(0),
            connected: false,
            last_error: None,
        })
        .insert_resource(Neon3IntentQueue::default())
        .insert_resource(Neon3VariableEvents::default())
        .insert_resource(Neon3EventQueue(variable_events))
        .insert_resource(CharacterStatusBridge {
            identity: NuiFlowIdentity {
                program_revision: UiProgramRevision {
                    program_id: "character.player.main.status".into(),
                    revision: Revision(1),
                    schema_version: neon_ui_schema::UI_PROGRAM_SCHEMA_VERSION,
                    capabilities: Vec::new(),
                },
                expected_input_revision: Revision(0),
                request_sequence: 0,
            },
            current: CharacterStatusVars { health: 82.0, mana: 64.0, level: 12 },
            sent: None,
        })
        .insert_resource(Neon3Transport {
            requests: request_tx,
            responses: Arc::new(Mutex::new(response_rx)),
        })
        .add_systems(Startup, request_screen_surface)
        .add_systems(Update, (consume_neon_responses, consume_variable_events, publish_camera_snapshot, flush_intents, flush_character_status));
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.insert_resource(Neon3ExternalSurfaceGpu {
                surface_id: SCREEN_SURFACE_ID.into(),
                color_target_id: COLOR_TARGET_ID.into(),
                id_target_id: ID_TARGET_ID.into(),
                size: self.config.surface_size,
                generation: 0,
                frame_sequence: 0,
                color_format: TextureFormat::Rgba8Unorm,
                id_format: TextureFormat::R32Uint,
                imported: false,
            });
            render_app.add_systems(Render, neon3_external_surface_render_system);
        }
    }
}

fn neon3_external_surface_render_system(
    _surface: Res<Neon3ExternalSurfaceGpu>,
    _render_device: Res<RenderDevice>,
    _render_queue: Res<RenderQueue>,
) {
    // The native import and fullscreen draw are isolated behind this render
    // system. The protocol/ECS case can compile before the Windows D3D12
    // consumer is enabled.
}

fn eventd_worker(
    endpoint: SocketAddr,
    events: Arc<Mutex<Vec<Neon3VariableEvent>>>,
) {
    let Ok(mut client) = EventClient::connect(endpoint) else { return; };
    let subscribe = EventSubscribe {
        protocol: EVENT_PROTOCOL.into(),
        version: PROTOCOL_VERSION,
        request_id: RequestId("bevy-variable-subscribe".into()),
        client: ClientIdentity {
            kind: ClientKind::ExternalHost,
            instance_id: "bevy-nui-host".into(),
            pid: std::process::id(),
            origin: "neon3-bevy-nui-host".into(),
        },
        filters: vec![EventFilter {
            name: None,
            name_prefix: Some("flow.".into()),
            publisher_kinds: None,
        }],
        replay_from_sequence: None,
        max_rate_hz: None,
    };
    let value = serde_json::to_value(EventFrame::Subscribe(subscribe)).unwrap_or_default();
    if client.send_value(&value).is_err() { return; }
    while let Ok(value) = client.recv_value() {
        let Some(event) = value.get("event") else { continue; };
        let Some(name) = event.get("name").and_then(serde_json::Value::as_str) else { continue; };
        let item = Neon3VariableEvent {
            name: name.into(),
            epoch: event.get("epoch").and_then(serde_json::Value::as_u64).unwrap_or_default(),
            sequence: event.get("sequence").and_then(serde_json::Value::as_u64).unwrap_or_default(),
            payload: event.get("payload").cloned().unwrap_or_default(),
        };
        if let Ok(mut queue) = events.lock() { queue.push(item); }
    }
}

fn consume_variable_events(
    mut output: ResMut<Neon3VariableEvents>,
    queue: Res<Neon3EventQueue>,
) {
    let Ok(mut pending) = queue.0.lock() else { return; };
    output.events.extend(pending.drain(..));
}

fn transport_worker(
    _endpoint: SocketAddr,
    requests: mpsc::Receiver<TransportRequest>,
    responses: mpsc::Sender<Result<RpcResponse, String>>,
) {
    for transport_request in requests {
        let result = RpcClient::connect(transport_request.endpoint)
            .and_then(|client| client.with_timeout(std::time::Duration::from_secs(5)))
            .and_then(|mut client| client.call(&transport_request.request))
            .map_err(|error| error.to_string());
        if responses.send(result).is_err() {
            break;
        }
    }
}

fn request_screen_surface(mut session: ResMut<Neon3Session>, transport: Res<Neon3Transport>) {
        let request = rpc_request_for(
        "bevy-surface-open",
        "render.surface.open",
        &session,
        session.config.wgpu_endpoint,
        json!(RenderSurfaceOpen {
            session_id: session.config.session_id.clone(),
            surface_id: session.surface_id.clone(),
            kind: RenderSurfaceKind::ScreenUi,
            size: RenderSurfaceSize {
                width: session.config.surface_size[0],
                height: session.config.surface_size[1],
            },
            format: "rgba8unorm".into(),
            depth: false,
            buffer_count: 1,
            placement: None,
            targets: vec![
                RenderSurfaceTarget {
                    target_id: session.color_target_id.clone(),
                    kind: RenderSurfaceTargetKind::Color,
                    format: "rgba8unorm".into(),
                },
                RenderSurfaceTarget {
                    target_id: session.id_target_id.clone(),
                    kind: RenderSurfaceTargetKind::Id,
                    format: "r32uint".into(),
                },
            ],
        }),
    );
    if transport.requests.send(TransportRequest { endpoint: session.config.wgpu_endpoint, request }).is_err() {
        session.last_error = Some("neon_transport_closed".into());
    }
}

fn consume_neon_responses(
    mut session: ResMut<Neon3Session>,
    transport: Res<Neon3Transport>,
) {
    let Ok(responses) = transport.responses.lock() else {
        session.last_error = Some("neon_response_lock_poisoned".into());
        return;
    };
    while let Ok(response) = responses.try_recv() {
        match response {
            Ok(response) if response.status == neon_protocol::RpcStatus::Accepted => {
                session.connected = true;
                session.last_error = None;
                if let Some(result) = response.result {
                    session.generation = result
                        .get("generation")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(session.generation);
                }
            }
            Ok(response) => {
                session.last_error = response.error.map(|error| error.code);
            }
            Err(error) => session.last_error = Some(error),
        }
    }
}

fn publish_camera_snapshot(
    mut session: ResMut<Neon3Session>,
    transport: Res<Neon3Transport>,
    cameras: Query<&Transform, With<Camera3d>>,
) {
    let Ok(transform) = cameras.single() else {
        return;
    };
    session.camera_revision.0 = session.camera_revision.0.saturating_add(1);
    let request = rpc_request_for(
        format!("bevy-camera-{}", session.camera_revision.0),
        "host.camera.snapshot",
        &session,
        session.config.wgpu_endpoint,
        json!(HostCameraSnapshot {
            session_id: session.config.session_id.clone(),
            camera_id: "bevy.main.camera".into(),
            revision: session.camera_revision,
            position: transform.translation.into(),
            rotation_xyzw: transform.rotation.into(),
            projection: HostCameraProjection::Perspective {
                fov_y_radians: 60.0_f32.to_radians(),
                aspect: session.config.surface_size[0] as f32 / session.config.surface_size[1] as f32,
                near: 0.1,
                far: 1000.0,
            },
        }),
    );
    let _ = transport.requests.send(TransportRequest { endpoint: session.config.wgpu_endpoint, request });
}

fn flush_intents(
    mut queue: ResMut<Neon3IntentQueue>,
    session: Res<Neon3Session>,
    transport: Res<Neon3Transport>,
) {
    for intent in queue.events.drain(..) {
        let request_id = intent.request_id.clone();
        let request = rpc_request_for(
            request_id.clone(),
            "ui.host.inbound",
            &session,
            session.config.ui_endpoint,
            json!({
                "kind": "semantic_intent",
                "event": {
                    "event_id": request_id,
                    "kind": "activate",
                    "intent": intent.action,
                    "source_node_key": "bevy.pointer.click",
                    "payload": intent.params,
                    "program_revision": {
                        "program_id": "case.bevy.screen.ui",
                        "revision": 1,
                        "schema_version": 1,
                        "capabilities": []
                    },
                    "input_revision": 0,
                    "request_id": "bevy-ui-intent",
                    "idempotency_key": "bevy-ui-intent"
                }
            }),
        );
        let _ = transport.requests.send(TransportRequest { endpoint: session.config.ui_endpoint, request });
    }
}

fn flush_character_status(
    mut bridge: ResMut<CharacterStatusBridge>,
    session: Res<Neon3Session>,
    transport: Res<Neon3Transport>,
) {
    let current = bridge.current.clone();
    let sent = bridge.sent.clone();
    let frame = match sent {
        None => Some(current.snapshot(&mut bridge.identity)),
        Some(previous) => current.diff(&previous, &mut bridge.identity),
    };
    let Some(frame) = frame else { return; };
    let request = rpc_request_for(
        frame.request_id.clone(),
        "ui.input.frame",
        &session,
        session.config.ui_endpoint,
        json!(frame),
    );
    if transport.requests.send(TransportRequest { endpoint: session.config.ui_endpoint, request }).is_ok() {
        bridge.sent = Some(bridge.current.clone());
    }
}

pub fn enqueue_pointer_click(
    queue: &mut Neon3IntentQueue,
    click: HostUiPointerClick,
    action: impl Into<String>,
    params: serde_json::Value,
) {
    queue.events.push(Neon3Intent {
        action: action.into(),
        params: json!({
            "surface_id": click.surface_id,
            "id_target_id": click.id_target_id,
            "generation": click.generation,
            "frame_sequence": click.frame_sequence,
            "pixel": click.pixel,
            "pointer_id": click.pointer_id,
            "sequence": click.sequence,
            "declared": params,
        }),
        request_id: format!("bevy-pointer-{}", click.sequence),
    });
}

fn rpc_request_for(
    id: impl Into<String>,
    method: &str,
    session: &Neon3Session,
    _endpoint: SocketAddr,
    params: serde_json::Value,
) -> RpcRequest {
    let id = id.into();
    RpcRequest {
        protocol: neon_protocol::RPC_PROTOCOL.into(),
        version: PROTOCOL_VERSION,
        request_id: RequestId(id.clone()),
        client: ClientIdentity {
            kind: ClientKind::ExternalHost,
            instance_id: session.config.session_id.clone(),
            pid: std::process::id(),
            origin: "neon3-bevy-nui-host".into(),
        },
        target: ServiceName(if method.starts_with("ui.") {
            "ui-runtime".into()
        } else {
            "wgpu-runtime".into()
        }),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(format!("bevy:{method}:{id}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_queue_keeps_surface_and_frame_identity() {
        let mut queue = Neon3IntentQueue::default();
        enqueue_pointer_click(
            &mut queue,
            HostUiPointerClick {
                session_id: "s".into(),
                surface_id: SCREEN_SURFACE_ID.into(),
                id_target_id: ID_TARGET_ID.into(),
                generation: 3,
                frame_sequence: 9,
                pointer_id: 1,
                sequence: 11,
                pixel: [20, 30],
            },
            "character.open_status",
            json!({"character_id": "player.main"}),
        );
        assert_eq!(queue.events.len(), 1);
        assert_eq!(queue.events[0].action, "character.open_status");
        assert_eq!(queue.events[0].params["frame_sequence"], 9);
        assert_eq!(queue.events[0].params["pixel"], json!([20, 30]));
    }

    #[test]
    fn flow_vars_generate_full_snapshot_and_sparse_diff() {
        let mut identity = NuiFlowIdentity {
            program_revision: UiProgramRevision {
                program_id: "character.player.main.status".into(),
                revision: Revision(3),
                schema_version: neon_ui_schema::UI_PROGRAM_SCHEMA_VERSION,
                capabilities: Vec::new(),
            },
            expected_input_revision: Revision(7),
            request_sequence: 0,
        };
        let before = CharacterStatusVars {
            health: 82.0,
            mana: 64.0,
            level: 12,
        };
        let snapshot = before.snapshot(&mut identity);
        assert_eq!(snapshot.changes.len(), 3);
        let after = CharacterStatusVars { health: 76.0, ..before.clone() };
        let diff = after.diff(&before, &mut identity).expect("health changed");
        assert_eq!(diff.changes.len(), 1);
        assert_eq!(diff.changes[0].key, "health");
        assert!(after.diff(&after, &mut identity).is_none());
    }
}
