//! Transport-independent public protocol types belong here.
//! This crate must not create GPU or window objects.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RPC_PROTOCOL: &str = "neon3.rpc";
pub const EVENT_PROTOCOL: &str = "neon3.event";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Cli,
    UiReactClient,
    UiRuntime,
    TerrainRuntime,
    ResourceRuntime,
    Projectd,
    WgpuRuntime,
    ExternalHost,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIdentity {
    pub kind: ClientKind,
    pub instance_id: String,
    pub pid: u32,
    pub origin: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceName(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub u64);

/// Stable identifier assigned by the window owner to one real OS interaction.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InteractionId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionTraceStage {
    Prepared,
    HitCaptureResolved,
    DragStarted,
    DragPreviewMoved,
    DropTargetResolved,
    DropTargetRejected,
    DragReleased,
    DragCancelled,
    SemanticEventForwarded,
    InboundReceived,
    AdapterValidationAccepted,
    AdapterValidationRejected,
    HostForwarded,
    HostResponseAccepted,
    HostResponseRejected,
    PublicationApplied,
    PublicationRejected,
    WgpuFragmentSubmissionAccepted,
    WgpuFragmentSubmissionRejected,
    DeliveryAccepted,
    DeliveryRejected,
    TransportFailed,
    CompositionRevisionApplied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionTraceOutcome {
    Pending,
    Accepted,
    Rejected,
    Failed,
}

/// Renderer-independent target identity. `node_path` is the declared semantic
/// path; renderer hit IDs, coordinates, and private renderer paths are excluded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionSemanticTarget {
    pub node_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionTraceError {
    pub code: String,
    pub message: String,
}

/// One durable lifecycle observation for a real window interaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionTraceRecord {
    pub sequence: u64,
    pub interaction_id: InteractionId,
    pub stage: InteractionTraceStage,
    pub outcome: InteractionTraceOutcome,
    pub error: Option<InteractionTraceError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_source_key: Option<String>,
    pub semantic_target: Option<InteractionSemanticTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_intent: Option<String>,
    pub fragment_revision: Option<Revision>,
    pub composition_revision: Revision,
    pub downstream_request_id: Option<RequestId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InteractionTraceFilters {
    pub interaction_id: Option<InteractionId>,
    pub stage: Option<InteractionTraceStage>,
    pub outcome: Option<InteractionTraceOutcome>,
    pub semantic_source_key: Option<String>,
    pub semantic_node_path: Option<String>,
    pub downstream_request_id: Option<RequestId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InteractionTraceQuery {
    pub after: Option<u64>,
    pub limit: Option<usize>,
    pub filters: Option<InteractionTraceFilters>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRef {
    pub project_id: String,
    pub asset_id: u64,
    pub revision: Revision,
    pub kind: String,
}

/// Revisioned, owner-provided asset content. Only project/resource services create this value.
/// Consumers may use the bytes locally but must not persist or forward renderer residency handles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetBytes {
    pub asset: AssetRef,
    pub media_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: Vec<u8>,
}

/// Image content supplied by an external engine or host. This is a transient
/// render input, not a project asset and not a GPU handle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiImageSource {
    pub image_id: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Reliable control-plane request for uploading one external image to the
/// renderer-owned public image atlas.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiImageUploadRequest {
    pub source: UiImageSource,
}

/// Integer atlas placement. The renderer owns the texture and may rebuild it;
/// consumers must pair this region with `generation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiImageTextureRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Read-only image residency result. `texture_index` is an atlas slot, not a
/// native GPU handle. `uv` is [origin_x, origin_y, size_x, size_y].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiImageTextureRef {
    pub image_id: String,
    pub texture_index: u32,
    pub generation: u64,
    pub atlas_size: [u32; 2],
    pub region: UiImageTextureRegion,
    pub uv: [f32; 4],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiTerrainCondition {
    pub sub: Option<u32>,
    pub parent: Option<u32>,
    pub relief: Option<u32>,
    pub texture: Option<u32>,
    pub water: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiTerrainGenerateCommand {
    pub condition: AiTerrainCondition,
    pub guidance: f32,
    pub steps: u32,
    pub seed: u64,
    pub size: u32,
    pub target_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiTerrainGenerationResult {
    pub job_id: String,
    pub target_id: String,
    pub state: String,
    pub seed: u64,
    pub width: u32,
    pub height: u32,
    pub elapsed_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcRequest {
    pub protocol: String,
    pub version: ProtocolVersion,
    pub request_id: RequestId,
    pub client: ClientIdentity,
    pub target: ServiceName,
    pub method: String,
    pub params: Value,
    pub expected_revision: Option<Revision>,
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcStatus {
    Accepted,
    Rejected,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcError {
    pub code: String,
    pub message: String,
    pub current_revision: Option<Revision>,
    pub object_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcResponse {
    pub request_id: RequestId,
    pub status: RpcStatus,
    pub revision: Option<Revision>,
    pub result: Option<Value>,
    pub snapshot: Option<Value>,
    pub error: Option<RpcError>,
}

/// Engine-independent backend names used during an external host GPU session.
/// The native resource transport is negotiated separately and never represented
/// by a raw OS handle in this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderBackend {
    Dx12,
    Vulkan,
    Metal,
    Gl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostEngineKind {
    Godot,
    Unity,
    Bevy,
    Unreal,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderAdapterIdentity {
    pub vendor_id: Option<u32>,
    pub device_id: Option<u32>,
    pub luid: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderHostIdentity {
    pub kind: HostEngineKind,
    pub pid: u32,
    pub adapter: RenderAdapterIdentity,
    pub plugin_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderBackendNegotiation {
    pub session_id: String,
    pub preferred_backends: Vec<RenderBackend>,
    pub required_features: Vec<String>,
    pub host: RenderHostIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderTransport {
    D3d12SharedTextureV1,
    VulkanExternalMemoryV1,
    MetalSharedSurfaceV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderBackendSelection {
    pub session_id: String,
    pub backend: RenderBackend,
    pub adapter: RenderAdapterIdentity,
    pub transport: RenderTransport,
    pub features: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderSessionState {
    Created,
    Negotiating,
    Matched,
    GpuSessionOpened,
    SurfaceReady,
    Streaming,
    Failed,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderSurfaceKind {
    ScreenUi,
    WorldUi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderSurfaceColorSpace {
    Linear,
    Srgb,
}

impl Default for RenderSurfaceColorSpace {
    fn default() -> Self {
        Self::Srgb
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSurfaceSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSurfacePlacement {
    pub anchor_id: Option<String>,
    pub position: Option<[f32; 3]>,
    pub rotation: Option<[f32; 3]>,
    pub scale: Option<[f32; 3]>,
    pub billboard: bool,
    pub occlusion: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSurfaceOpen {
    pub session_id: String,
    pub surface_id: String,
    pub kind: RenderSurfaceKind,
    pub size: RenderSurfaceSize,
    pub format: String,
    #[serde(default)]
    pub color_space: RenderSurfaceColorSpace,
    pub depth: bool,
    pub buffer_count: u8,
    pub placement: Option<RenderSurfacePlacement>,
    #[serde(default)]
    pub targets: Vec<RenderSurfaceTarget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderSurfaceTargetKind {
    Color,
    Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSurfaceTarget {
    pub target_id: String,
    pub kind: RenderSurfaceTargetKind,
    pub format: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSurfaceTargetsDescriptor {
    pub surface_id: String,
    pub generation: u64,
    pub size: RenderSurfaceSize,
    pub color_target_id: String,
    pub id_target_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostUiPointerClick {
    pub session_id: String,
    pub surface_id: String,
    pub id_target_id: String,
    pub generation: u64,
    pub frame_sequence: u64,
    pub pointer_id: u64,
    pub sequence: u64,
    pub pixel: [u32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCameraSnapshot {
    pub session_id: String,
    pub camera_id: String,
    pub revision: Revision,
    pub position: [f32; 3],
    pub rotation_xyzw: [f32; 4],
    pub projection: HostCameraProjection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostCameraProjection {
    Perspective {
        fov_y_radians: f32,
        aspect: f32,
        near: f32,
        far: f32,
    },
    Orthographic {
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        near: f32,
        far: f32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedTextureDescriptor {
    pub format: String,
    pub size: RenderSurfaceSize,
    pub mip_levels: u32,
    pub buffer_index: u8,
    pub broker_token: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedFenceDescriptor {
    pub kind: String,
    pub broker_token: String,
    pub initial_value: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSurfaceDescriptor {
    pub session_id: String,
    pub surface_id: String,
    pub generation: u64,
    pub transport: RenderTransport,
    pub adapter_luid: Option<String>,
    pub texture: SharedTextureDescriptor,
    pub fence: SharedFenceDescriptor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderFrameReady {
    pub session_id: String,
    pub surface_id: String,
    pub generation: u64,
    pub frame_sequence: u64,
    pub buffer_index: u8,
    pub fence_value: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceHealth {
    pub service: ServiceName,
    pub status: HealthStatus,
    pub epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceDescription {
    pub service: ServiceName,
    pub protocol_version: ProtocolVersion,
    pub endpoint: String,
    pub epoch: u64,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceEvent {
    pub epoch: u64,
    pub sequence: u64,
    pub payload: Value,
}

/// Cursor supplied by a client after it has fetched a full service snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionRequest {
    pub epoch: u64,
    pub from_sequence: Option<u64>,
}

/// A polling subscription result. The current IPC transport has no server push channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionResponse {
    pub epoch: u64,
    pub next_sequence: u64,
    pub events: Vec<ServiceEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSummary {
    pub project_id: String,
    pub revision: Revision,
    pub epoch: u64,
    pub sequence: u64,
    pub asset_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSnapshot {
    pub summary: ProjectSummary,
    pub assets: Vec<AssetRef>,
}

/// Event module (neon-eventd) protocol types. These are the dedicated event
/// protocol `neon3.event`, distinct from the RPC control plane `neon3.rpc`.

/// One subscription filter. Filters in a single subscribe frame are OR-ed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFilter {
    /// Exact dotted event name match.
    pub name: Option<String>,
    /// Prefix match, e.g. `nui.variable.`.
    pub name_prefix: Option<String>,
    /// Publisher client kinds that may deliver, e.g. `["ui_runtime"]`.
    pub publisher_kinds: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub String);

/// A fully-stamped event as assigned and delivered by `neon-eventd`.
/// Publishers supply the content fields; `neon-eventd` fills epoch/sequence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub protocol: String,
    pub version: ProtocolVersion,
    pub event_id: EventId,
    pub name: String,
    pub schema_version: u16,
    pub epoch: u64,
    pub sequence: u64,
    pub timestamp_unix_ms: u64,
    pub publisher: ClientIdentity,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventAckStatus {
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventPublish {
    pub protocol: String,
    pub version: ProtocolVersion,
    pub request_id: RequestId,
    pub publisher: ClientIdentity,
    pub name: String,
    pub schema_version: u16,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSubscribe {
    pub protocol: String,
    pub version: ProtocolVersion,
    pub request_id: RequestId,
    pub client: ClientIdentity,
    pub filters: Vec<EventFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_from_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rate_hz: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventAck {
    pub protocol: String,
    pub version: ProtocolVersion,
    pub request_id: RequestId,
    pub status: EventAckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    /// Current service sequence after an accepted subscribe/publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EventError>,
}

/// A full event delivered to a subscriber on a persistent subscription stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventDelivery {
    pub protocol: String,
    pub version: ProtocolVersion,
    pub event: EventEnvelope,
}

/// A frame sent from a client to `neon-eventd` on the event protocol.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventFrame {
    Publish(EventPublish),
    Subscribe(EventSubscribe),
    Unsubscribe,
    Heartbeat,
}

/// A frame sent from `neon-eventd` back to a client on the event protocol.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventResponse {
    Ack(EventAck),
    Delivery(EventDelivery),
}

/// Control-plane snapshot of the event service state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSnapshot {
    pub epoch: u64,
    pub current_sequence: u64,
    pub registered_namespaces: Vec<String>,
}

/// Retention settings for the event ring buffer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRetention {
    pub capacity: usize,
    pub retained: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    const REQUEST_FIXTURE: &str = include_str!("../../../tests/fixtures/protocol/request.json");
    const ACCEPTED_RESPONSE_FIXTURE: &str =
        include_str!("../../../tests/fixtures/protocol/accepted-response.json");
    const REVISION_CONFLICT_FIXTURE: &str =
        include_str!("../../../tests/fixtures/protocol/revision-conflict-response.json");

    #[test]
    fn only_the_wgpu_runtime_may_declare_gpu_dependencies() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifests = [
            "neon-protocol",
            "neon-ipc",
            "neon-observability",
            "neon-projectd",
            "neon-ui-schema",
            "neon-ui-runtime",
            "neon-cli",
        ];

        for crate_name in manifests {
            let manifest = workspace.join("crates").join(crate_name).join("Cargo.toml");
            let content = fs::read_to_string(&manifest).expect("workspace manifest must exist");
            assert!(
                !content.contains("wgpu") && !content.contains("winit"),
                "{crate_name} must not declare wgpu or winit"
            );
        }
    }

    #[test]
    fn request_fixture_round_trips() {
        let request: RpcRequest = serde_json::from_str(REQUEST_FIXTURE).unwrap();
        assert_eq!(request.protocol, RPC_PROTOCOL);
        assert_eq!(request.request_id.0, "request-001");
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::from_str::<Value>(REQUEST_FIXTURE).unwrap()
        );
    }

    #[test]
    fn accepted_response_fixture_round_trips() {
        let response: RpcResponse = serde_json::from_str(ACCEPTED_RESPONSE_FIXTURE).unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.request_id.0, "request-001");
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::from_str::<Value>(ACCEPTED_RESPONSE_FIXTURE).unwrap()
        );
    }

    #[test]
    fn revision_conflict_preserves_request_and_revision() {
        let response: RpcResponse = serde_json::from_str(REVISION_CONFLICT_FIXTURE).unwrap();
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.request_id.0, "request-001");
        assert_eq!(response.error.unwrap().current_revision, Some(Revision(43)));
    }

    #[test]
    fn missing_required_request_field_is_rejected() {
        let json = r#"{"protocol":"neon3.rpc","version":{"major":1,"minor":0}}"#;
        assert!(serde_json::from_str::<RpcRequest>(json).is_err());
    }

    #[test]
    fn unknown_request_fields_are_rejected_by_contract() {
        let json = r#"{"protocol":"neon3.rpc","version":{"major":1,"minor":0},"request_id":"request-001","client":{"kind":"cli","instance_id":"cli-001","pid":1,"origin":"test"},"target":"wgpu-runtime","method":"service.health","params":{},"expected_revision":null,"idempotency_key":null,"unexpected":true}"#;
        assert!(serde_json::from_str::<RpcRequest>(json).is_err());
    }

    #[test]
    fn asset_ref_is_a_stable_identity_without_a_local_path() {
        let asset = AssetRef {
            project_id: "project-001".into(),
            asset_id: 81,
            revision: Revision(5),
            kind: "water_material".into(),
        };
        let json = serde_json::to_value(asset).unwrap();
        assert!(json.get("path").is_none());
        assert!(json.get("local_path").is_none());
        assert_eq!(json["asset_id"], 81);
    }

    #[test]
    fn asset_bytes_are_revisioned_content_without_a_local_path() {
        let content = AssetBytes {
            asset: AssetRef {
                project_id: "project-001".into(),
                asset_id: 81,
                revision: Revision(5),
                kind: "image".into(),
            },
            media_type: "application/x-neon-rgba8".into(),
            width: Some(1),
            height: Some(1),
            bytes: vec![1, 2, 3, 4],
        };
        let value = serde_json::to_value(content).unwrap();
        assert!(value.get("path").is_none());
        assert!(value.get("local_path").is_none());
        assert_eq!(value["asset"]["revision"], 5);
    }

    #[test]
    fn ai_generation_command_contains_labels_and_parameters_but_no_gpu_handle() {
        let command = AiTerrainGenerateCommand {
            condition: AiTerrainCondition {
                sub: Some(6),
                parent: Some(1),
                relief: Some(3),
                texture: Some(2),
                water: Some(2),
            },
            guidance: 7.0,
            steps: 2,
            seed: 42,
            size: 32,
            target_id: "ai.terrain.preview".into(),
        };
        let value = serde_json::to_value(command).unwrap();
        assert_eq!(value["condition"]["parent"], 1);
        assert_eq!(value["target_id"], "ai.terrain.preview");
        for forbidden in ["texture", "buffer", "handle", "path"] {
            assert!(value.get(forbidden).is_none());
        }
    }

    fn event_publish() -> EventPublish {
        EventPublish {
            protocol: EVENT_PROTOCOL.into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("req-pub-1".into()),
            publisher: ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "ui-runtime-1".into(),
                pid: 1234,
                origin: "test".into(),
            },
            name: "nui.variable.changed".into(),
            schema_version: 1,
            payload: serde_json::json!({
                "module": "terrain_workbench",
                "surface": "surface.editor.terrain",
                "variable_key": "brush_size",
                "kind": "i32",
                "old_value": 4,
                "new_value": 8,
            }),
            idempotency_key: Some("evt-key-1".into()),
        }
    }

    #[test]
    fn event_publish_round_trips_and_uses_event_protocol() {
        let publish = event_publish();
        let value = serde_json::to_value(&publish).unwrap();
        assert_eq!(value["protocol"], "neon3.event");
        assert_eq!(value["name"], "nui.variable.changed");
        assert_eq!(value["payload"]["new_value"], 8);
        let decoded: EventPublish = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, publish);
    }

    #[test]
    fn event_publish_rejects_unknown_fields_and_extra_protocol() {
        let mut value = serde_json::to_value(event_publish()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("unexpected".into(), serde_json::json!(true));
        assert!(serde_json::from_value::<EventPublish>(value).is_err());

        let mut value = serde_json::to_value(event_publish()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("protocol".into(), "neon3.rpc".into());
        // A different protocol string is still structurally valid; caller must dispatch on it.
        assert!(serde_json::from_value::<EventPublish>(value).is_ok());
    }

    #[test]
    fn event_envelope_carries_epoch_sequence_and_no_gpu_handles() {
        let envelope = EventEnvelope {
            protocol: EVENT_PROTOCOL.into(),
            version: PROTOCOL_VERSION,
            event_id: EventId("event-001".into()),
            name: "nui.variable.changed".into(),
            schema_version: 1,
            epoch: 7,
            sequence: 10042,
            timestamp_unix_ms: 0,
            publisher: ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: "ui-runtime-1".into(),
                pid: 1234,
                origin: "test".into(),
            },
            payload: serde_json::json!({"variable_key": "brush_size", "new_value": 8}),
        };
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["epoch"], 7);
        assert_eq!(value["sequence"], 10042);
        for forbidden in [
            "texture",
            "buffer",
            "handle",
            "path",
            "hit_id",
            "element_id",
        ] {
            assert!(value.get(forbidden).is_none());
        }
        let decoded: EventEnvelope = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn event_subscribe_filter_round_trips() {
        let subscribe = EventSubscribe {
            protocol: EVENT_PROTOCOL.into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("req-sub-1".into()),
            client: ClientIdentity {
                kind: ClientKind::Cli,
                instance_id: "cli-1".into(),
                pid: 1,
                origin: "test".into(),
            },
            filters: vec![
                EventFilter {
                    name: None,
                    name_prefix: Some("nui.variable.".into()),
                    publisher_kinds: None,
                },
                EventFilter {
                    name: Some("project.opened".into()),
                    name_prefix: None,
                    publisher_kinds: Some(vec!["projectd".into()]),
                },
            ],
            replay_from_sequence: Some(9000),
            max_rate_hz: None,
        };
        let value = serde_json::to_value(&subscribe).unwrap();
        assert_eq!(value["filters"][0]["name_prefix"], "nui.variable.");
        assert_eq!(value["replay_from_sequence"], 9000);
        let decoded: EventSubscribe = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, subscribe);
    }

    #[test]
    fn event_ack_rejects_with_stable_error_code_and_ids() {
        let ack = EventAck {
            protocol: EVENT_PROTOCOL.into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("req-001".into()),
            status: EventAckStatus::Rejected,
            event_id: None,
            epoch: None,
            sequence: None,
            current_sequence: None,
            error: Some(EventError {
                code: "event_unknown_name".into(),
                message: "事件名未注册".into(),
                event_id: Some(EventId("event-001".into())),
                sequence: None,
            }),
        };
        let value = serde_json::to_value(&ack).unwrap();
        assert_eq!(value["status"], "rejected");
        assert_eq!(value["error"]["code"], "event_unknown_name");
        let decoded: EventAck = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, ack);
    }

    #[test]
    fn event_frames_and_responses_are_tagged() {
        let publish = EventFrame::Publish(event_publish());
        let value = serde_json::to_value(&publish).unwrap();
        assert_eq!(value["kind"], "publish");
        assert_eq!(value["name"], "nui.variable.changed");
        let decoded: EventFrame = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, publish);

        let subscribe = EventFrame::Subscribe(EventSubscribe {
            protocol: EVENT_PROTOCOL.into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("req-sub-1".into()),
            client: ClientIdentity {
                kind: ClientKind::Cli,
                instance_id: "cli-1".into(),
                pid: 1,
                origin: "test".into(),
            },
            filters: vec![EventFilter {
                name: None,
                name_prefix: Some("nui.variable.".into()),
                publisher_kinds: None,
            }],
            replay_from_sequence: Some(0),
            max_rate_hz: None,
        });
        let value = serde_json::to_value(&subscribe).unwrap();
        assert_eq!(value["kind"], "subscribe");
        let decoded: EventFrame = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, subscribe);

        assert_eq!(
            serde_json::to_value(&EventFrame::Unsubscribe).unwrap()["kind"],
            "unsubscribe"
        );
        assert_eq!(
            serde_json::to_value(&EventFrame::Heartbeat).unwrap()["kind"],
            "heartbeat"
        );

        let delivery = EventResponse::Delivery(EventDelivery {
            protocol: EVENT_PROTOCOL.into(),
            version: PROTOCOL_VERSION,
            event: EventEnvelope {
                protocol: EVENT_PROTOCOL.into(),
                version: PROTOCOL_VERSION,
                event_id: EventId("event-003".into()),
                name: "nui.variable.changed".into(),
                schema_version: 1,
                epoch: 2,
                sequence: 6,
                timestamp_unix_ms: 0,
                publisher: ClientIdentity {
                    kind: ClientKind::UiRuntime,
                    instance_id: "ui-1".into(),
                    pid: 1,
                    origin: "test".into(),
                },
                payload: serde_json::json!({"new_value": 8}),
            },
        });
        let value = serde_json::to_value(&delivery).unwrap();
        assert_eq!(value["kind"], "delivery");
        let decoded: EventResponse = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, delivery);

        let ack = EventResponse::Ack(EventAck {
            protocol: EVENT_PROTOCOL.into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("req-001".into()),
            status: EventAckStatus::Accepted,
            event_id: Some(EventId("event-003".into())),
            epoch: Some(2),
            sequence: Some(6),
            current_sequence: Some(6),
            error: None,
        });
        let value = serde_json::to_value(&ack).unwrap();
        assert_eq!(value["kind"], "ack");
        let decoded: EventResponse = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, ack);
    }

    #[test]
    fn event_delivery_and_snapshot_round_trip() {
        let delivery = EventDelivery {
            protocol: EVENT_PROTOCOL.into(),
            version: PROTOCOL_VERSION,
            event: EventEnvelope {
                protocol: EVENT_PROTOCOL.into(),
                version: PROTOCOL_VERSION,
                event_id: EventId("event-002".into()),
                name: "project.opened".into(),
                schema_version: 1,
                epoch: 2,
                sequence: 5,
                timestamp_unix_ms: 0,
                publisher: ClientIdentity {
                    kind: ClientKind::Projectd,
                    instance_id: "projectd-1".into(),
                    pid: 99,
                    origin: "test".into(),
                },
                payload: serde_json::json!({"project_id": "project-001"}),
            },
        };
        let value = serde_json::to_value(&delivery).unwrap();
        assert_eq!(value["event"]["name"], "project.opened");
        assert_eq!(value["event"]["epoch"], 2);
        let decoded: EventDelivery = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, delivery);

        let snapshot = EventSnapshot {
            epoch: 2,
            current_sequence: 5,
            registered_namespaces: vec!["nui.variable".into(), "project".into()],
        };
        let value = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(value["current_sequence"], 5);
        let decoded: EventSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, snapshot);

        let retention = EventRetention {
            capacity: 4096,
            retained: 3,
        };
        let value = serde_json::to_value(&retention).unwrap();
        assert_eq!(value["capacity"], 4096);
        let decoded: EventRetention = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, retention);
    }

    #[test]
    fn external_render_contract_round_trips_without_native_handles() {
        let negotiation = RenderBackendNegotiation {
            session_id: "host-session-001".into(),
            preferred_backends: vec![RenderBackend::Dx12],
            required_features: vec!["shared_texture".into(), "shared_fence".into()],
            host: RenderHostIdentity {
                kind: HostEngineKind::Godot,
                pid: 12345,
                adapter: RenderAdapterIdentity {
                    vendor_id: Some(4318),
                    device_id: Some(1234),
                    luid: Some("adapter-luid".into()),
                    name: Some("test adapter".into()),
                },
                plugin_version: "neon3-godot-adapter-1".into(),
            },
        };
        let value = serde_json::to_value(&negotiation).unwrap();
        assert_eq!(value["preferred_backends"][0], "dx12");
        assert!(value.get("handle").is_none());
        assert!(value.get("texture_handle").is_none());
        assert_eq!(
            serde_json::from_value::<RenderBackendNegotiation>(value).unwrap(),
            negotiation
        );

        let surface = RenderSurfaceDescriptor {
            session_id: "host-session-001".into(),
            surface_id: "surface.quest-panel".into(),
            generation: 2,
            transport: RenderTransport::D3d12SharedTextureV1,
            adapter_luid: Some("adapter-luid".into()),
            texture: SharedTextureDescriptor {
                format: "rgba8unorm_srgb".into(),
                size: RenderSurfaceSize {
                    width: 1280,
                    height: 720,
                },
                mip_levels: 1,
                buffer_index: 1,
                broker_token: "broker-token".into(),
            },
            fence: SharedFenceDescriptor {
                kind: "d3d12_shared_fence".into(),
                broker_token: "broker-fence-token".into(),
                initial_value: 20,
            },
        };
        let value = serde_json::to_value(&surface).unwrap();
        assert!(value["texture"].get("broker_token").is_some());
        assert!(value["texture"].get("handle").is_none());
        assert_eq!(
            serde_json::from_value::<RenderSurfaceDescriptor>(value).unwrap(),
            surface
        );
    }

    #[test]
    fn external_render_frame_requires_generation_and_fence_value() {
        let frame = RenderFrameReady {
            session_id: "host-session-001".into(),
            surface_id: "surface.quest-panel".into(),
            generation: 3,
            frame_sequence: 184,
            buffer_index: 2,
            fence_value: 527,
        };
        let value = serde_json::to_value(&frame).unwrap();
        assert_eq!(value["generation"], 3);
        assert_eq!(value["buffer_index"], 2);
        assert_eq!(value["fence_value"], 527);

        let mut missing_fence = value;
        missing_fence.as_object_mut().unwrap().remove("fence_value");
        assert!(serde_json::from_value::<RenderFrameReady>(missing_fence).is_err());
    }

    #[test]
    fn external_surface_open_round_trips_color_and_id_targets() {
        let open = RenderSurfaceOpen {
            session_id: "bevy-session".into(),
            surface_id: "bevy.screen".into(),
            kind: RenderSurfaceKind::ScreenUi,
            size: RenderSurfaceSize {
                width: 1280,
                height: 720,
            },
            format: "rgba8unorm".into(),
            color_space: RenderSurfaceColorSpace::Srgb,
            depth: false,
            buffer_count: 1,
            placement: None,
            targets: vec![
                RenderSurfaceTarget {
                    target_id: "bevy.screen.color".into(),
                    kind: RenderSurfaceTargetKind::Color,
                    format: "rgba8unorm".into(),
                },
                RenderSurfaceTarget {
                    target_id: "bevy.screen.id".into(),
                    kind: RenderSurfaceTargetKind::Id,
                    format: "r32uint".into(),
                },
            ],
        };
        let value = serde_json::to_value(&open).unwrap();
        assert_eq!(value["targets"][1]["kind"], "id");
        assert_eq!(
            serde_json::from_value::<RenderSurfaceOpen>(value).unwrap(),
            open
        );
    }
}
