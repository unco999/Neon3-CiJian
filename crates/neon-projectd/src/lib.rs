//! Headless project asset authority. This crate owns project asset bytes and never creates GPU objects.

use std::collections::HashMap;

use neon_observability::{CommandJournal, DebugSnapshot, TraceLevel};
use neon_protocol::{AssetBytes, AssetRef, HealthStatus, PROTOCOL_VERSION, Revision, RpcError, RpcRequest, RpcResponse, RpcStatus, ServiceDescription, ServiceHealth, ServiceName};
use serde_json::{json, Value};

pub const SERVICE_NAME: &str = "projectd";
pub const CAPABILITY_ASSET_BYTES: &str = "project.asset_bytes.v1";

pub struct Projectd {
    epoch: u64,
    revision: Revision,
    assets: HashMap<(String, u64, u64), AssetBytes>,
    journal: CommandJournal,
}

impl Projectd {
    pub fn fixture(epoch: u64) -> Self {
        let image_asset = AssetRef { project_id: "fixture-project".into(), asset_id: 81, revision: Revision(5), kind: "image".into() };
        let image = AssetBytes {
            asset: image_asset.clone(), media_type: "application/x-neon-rgba8".into(), width: Some(2), height: Some(1),
            bytes: vec![51, 204, 102, 255, 51, 204, 102, 0],
        };
        let font_asset = AssetRef { project_id: "fixture-project".into(), asset_id: 82, revision: Revision(5), kind: "font".into() };
        // Fixture bytes establish ownership/protocol flow only; glyph parsing belongs to the renderer's later U5 text path.
        let font = AssetBytes { asset: font_asset.clone(), media_type: "font/ttf".into(), width: None, height: None, bytes: vec![0, 1, 0, 0] };
        let mut assets = HashMap::new();
        assets.insert(key(&image_asset), image);
        assets.insert(key(&font_asset), font);
        Self { epoch, revision: Revision(5), assets, journal: CommandJournal::new(ServiceName(SERVICE_NAME.into()), epoch, 128) }
    }

    pub fn service_description(&self) -> ServiceDescription {
        ServiceDescription { service: ServiceName(SERVICE_NAME.into()), protocol_version: PROTOCOL_VERSION, endpoint: "headless://projectd".into(), epoch: self.epoch, capabilities: vec!["project.summary.v1".into(), "asset.list.v1".into(), CAPABILITY_ASSET_BYTES.into()] }
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        DebugSnapshot { service: ServiceName(SERVICE_NAME.into()), epoch: self.epoch, revision: self.revision, health: HealthStatus::Healthy, capabilities: self.service_description().capabilities, active_jobs: Vec::new() }
    }

    pub fn handle(&mut self, request: RpcRequest) -> RpcResponse {
        let request_id = request.request_id.clone();
        let result = match request.method.as_str() {
            "service.health" => Ok(json!(ServiceHealth { service: ServiceName(SERVICE_NAME.into()), status: HealthStatus::Healthy, epoch: self.epoch })),
            "service.describe" => Ok(json!(self.service_description())),
            "debug.snapshot.get" => Ok(json!(self.debug_snapshot())),
            "project.summary" => Ok(json!({"project_id": "fixture-project", "revision": self.revision})),
            "asset.list" => Ok(json!(self.assets.values().map(|value| &value.asset).collect::<Vec<_>>())),
            "asset.get_bytes" => self.asset_bytes(request.params),
            _ => Err(("unsupported_method", "method is not supported")),
        };
        match result {
            Ok(result) => {
                self.journal.append(TraceLevel::Info, "project.asset.read", Some(request_id.clone()), None, None, None, Some(self.revision), Some(self.revision), json!({"method": request.method}));
                RpcResponse { request_id, status: RpcStatus::Accepted, revision: Some(self.revision), result: Some(result), snapshot: None, error: None }
            }
            Err((code, message)) => RpcResponse { request_id, status: RpcStatus::Rejected, revision: Some(self.revision), result: None, snapshot: None, error: Some(RpcError { code: code.into(), message: message.into(), current_revision: Some(self.revision), object_id: None }) },
        }
    }

    fn asset_bytes(&self, params: Value) -> Result<Value, (&'static str, &'static str)> {
        let asset: AssetRef = serde_json::from_value(params).map_err(|_| ("invalid_request", "a stable AssetRef is required"))?;
        self.assets.get(&key(&asset)).map(|content| json!(content)).ok_or(("asset_revision_not_found", "asset bytes are not available at this revision"))
    }
}

fn key(asset: &AssetRef) -> (String, u64, u64) { (asset.project_id.clone(), asset.asset_id, asset.revision.0) }

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion, RequestId};

    fn request(id: &str, method: &str, params: Value) -> RpcRequest {
        RpcRequest { protocol: "neon3.rpc".into(), version: ProtocolVersion { major: 1, minor: 0 }, request_id: RequestId(id.into()), client: ClientIdentity { kind: ClientKind::WgpuRuntime, instance_id: "renderer-test".into(), pid: 1, origin: "test".into() }, target: ServiceName(SERVICE_NAME.into()), method: method.into(), params, expected_revision: Some(Revision(5)), idempotency_key: None }
    }

    #[test]
    fn fixture_asset_bytes_require_exact_stable_revision() {
        let mut service = Projectd::fixture(3);
        let asset = AssetRef { project_id: "fixture-project".into(), asset_id: 81, revision: Revision(5), kind: "image".into() };
        let response = service.handle(request("asset", "asset.get_bytes", json!(asset)));
        assert_eq!(response.status, RpcStatus::Accepted);
        let content: AssetBytes = serde_json::from_value(response.result.unwrap()).unwrap();
        assert_eq!(content.bytes.len(), 8);
        let stale = service.handle(request("stale", "asset.get_bytes", json!(AssetRef { revision: Revision(4), ..content.asset })));
        assert_eq!(stale.error.unwrap().code, "asset_revision_not_found");
    }
}
