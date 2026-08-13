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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    #[test]
    fn only_the_wgpu_runtime_may_declare_gpu_dependencies() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifests = [
            "neon-protocol",
            "neon-ipc",
            "neon-observability",
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
}
