//! Transport-independent public protocol types belong here.
//! This crate must not create GPU or window objects.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RPC_PROTOCOL: &str = "neon3.rpc";

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
            asset: AssetRef { project_id: "project-001".into(), asset_id: 81, revision: Revision(5), kind: "image".into() },
            media_type: "application/x-neon-rgba8".into(), width: Some(1), height: Some(1), bytes: vec![1, 2, 3, 4],
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
}
