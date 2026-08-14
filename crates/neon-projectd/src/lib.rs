//! Headless project asset authority. This crate owns project asset bytes and never creates GPU objects.

use std::{collections::{HashMap, VecDeque}, net::SocketAddr};

use neon_ipc::{RpcServer, TransportError};
use neon_observability::{CommandJournal, CommandReceipt, CommandState, DebugSnapshot, JournalFilter, TraceLevel, EVENT_COMMAND_ACCEPTED, EVENT_COMMAND_RECEIVED, EVENT_COMMAND_REJECTED};
use neon_protocol::{AssetBytes, AssetRef, HealthStatus, PROTOCOL_VERSION, ProjectSnapshot, ProjectSummary, RequestId, Revision, RpcError, RpcRequest, RpcResponse, RpcStatus, ServiceDescription, ServiceEvent, ServiceHealth, ServiceName, SubscriptionRequest, SubscriptionResponse};
use serde_json::{json, Value};

pub const SERVICE_NAME: &str = "projectd";
pub const CAPABILITY_ASSET_BYTES: &str = "project.asset_bytes.v1";
pub const CAPABILITY_PROJECT_SUBSCRIBE: &str = "project.subscribe.poll.v1";
const FIXTURE_SARASA_UI_SC_LIGHT_TTF: &[u8] = include_bytes!("../../../assets/fonts/SarasaUiSC-Light.ttf");

pub struct Projectd {
    epoch: u64,
    revision: Revision,
    project_id: String,
    endpoint: String,
    assets: HashMap<(String, u64, u64, String), AssetBytes>,
    events: VecDeque<ServiceEvent>,
    next_event_sequence: u64,
    receipts: HashMap<RequestId, CommandReceipt>,
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
        // The fixture is a workspace-owned, licensed font; only the renderer parses its bytes.
        let font = AssetBytes { asset: font_asset.clone(), media_type: "font/ttf".into(), width: None, height: None, bytes: FIXTURE_SARASA_UI_SC_LIGHT_TTF.to_vec() };
        let mut assets = HashMap::new();
        assets.insert(key(&image_asset), image);
        assets.insert(key(&font_asset), font);
        let mut service = Self {
            epoch,
            revision: Revision(5),
            project_id: "fixture-project".into(),
            endpoint: "headless://projectd".into(),
            assets,
            events: VecDeque::with_capacity(128),
            next_event_sequence: 1,
            receipts: HashMap::new(),
            journal: CommandJournal::new(ServiceName(SERVICE_NAME.into()), epoch, 128),
        };
        service.publish_snapshot();
        service
    }

    pub fn service_description(&self) -> ServiceDescription {
        ServiceDescription { service: ServiceName(SERVICE_NAME.into()), protocol_version: PROTOCOL_VERSION, endpoint: self.endpoint.clone(), epoch: self.epoch, capabilities: vec!["project.summary.v1".into(), "asset.list.v1".into(), CAPABILITY_ASSET_BYTES.into(), CAPABILITY_PROJECT_SUBSCRIBE.into(), "debug.trace.query.v1".into()] }
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        DebugSnapshot { service: ServiceName(SERVICE_NAME.into()), epoch: self.epoch, revision: self.revision, health: HealthStatus::Healthy, capabilities: self.service_description().capabilities, active_jobs: Vec::new() }
    }

    pub fn handle(&mut self, request: RpcRequest) -> RpcResponse {
        let request_id = request.request_id.clone();
        let revision_before = self.revision;
        self.journal.append(TraceLevel::Info, EVENT_COMMAND_RECEIVED, Some(request_id.clone()), None, None, None, Some(revision_before), None, json!({"method": request.method}));
        let result = match request.method.as_str() {
            "service.health" => Ok(json!(ServiceHealth { service: ServiceName(SERVICE_NAME.into()), status: HealthStatus::Healthy, epoch: self.epoch })),
            "service.describe" => Ok(json!(self.service_description())),
            "service.shutdown" => Ok(json!({"state": "accepted"})),
            "debug.snapshot.get" => Ok(json!(self.debug_snapshot())),
            "debug.health.check" => Ok(json!(ServiceHealth { service: ServiceName(SERVICE_NAME.into()), status: HealthStatus::Healthy, epoch: self.epoch })),
            "debug.diagnostics.get" => Ok(json!({"project_id": self.project_id, "revision": self.revision, "asset_count": self.assets.len(), "event_sequence": self.current_sequence(), "journal_records": self.journal.records().len()})),
            "debug.command.get" => self.command_receipt(request.params),
            "debug.trace.query" | "debug.journal.query" => Ok(json!(self.trace_query(request.params))),
            "project.summary" => Ok(json!(self.project_summary())),
            "asset.list" => Ok(json!(self.assets_sorted())),
            "asset.get_bytes" => self.asset_bytes(request.params),
            "service.subscribe" | "project.subscribe" => self.subscribe(request.params),
            _ => Err(("unsupported_method", "method is not supported")),
        };
        match result {
            Ok(result) => {
                self.receipts.insert(request_id.clone(), CommandReceipt { request_id: request_id.clone(), state: CommandState::Accepted, revision_before: Some(revision_before), revision_after: Some(self.revision), error_code: None });
                self.journal.append(TraceLevel::Info, EVENT_COMMAND_ACCEPTED, Some(request_id.clone()), None, None, None, Some(revision_before), Some(self.revision), json!({"method": request.method}));
                RpcResponse { request_id, status: RpcStatus::Accepted, revision: Some(self.revision), result: Some(result), snapshot: Some(json!(self.project_snapshot())), error: None }
            }
            Err((code, message)) => {
                self.receipts.insert(request_id.clone(), CommandReceipt { request_id: request_id.clone(), state: CommandState::Rejected, revision_before: Some(revision_before), revision_after: Some(self.revision), error_code: Some(code.into()) });
                self.journal.append(TraceLevel::Warn, EVENT_COMMAND_REJECTED, Some(request_id.clone()), None, None, None, Some(revision_before), Some(self.revision), json!({"method": request.method, "code": code}));
                RpcResponse { request_id, status: RpcStatus::Rejected, revision: Some(self.revision), result: None, snapshot: Some(json!(self.project_snapshot())), error: Some(RpcError { code: code.into(), message: message.into(), current_revision: Some(self.revision), object_id: None }) }
            }
        }
    }

    pub fn set_endpoint(&mut self, endpoint: SocketAddr) {
        self.endpoint = format!("tcp://{endpoint}");
    }

    fn current_sequence(&self) -> u64 {
        self.next_event_sequence.saturating_sub(1)
    }

    fn assets_sorted(&self) -> Vec<AssetRef> {
        let mut assets = self.assets.values().map(|value| value.asset.clone()).collect::<Vec<_>>();
        assets.sort_by_key(|asset| (asset.project_id.clone(), asset.asset_id, asset.revision, asset.kind.clone()));
        assets
    }

    fn project_summary(&self) -> ProjectSummary {
        ProjectSummary { project_id: self.project_id.clone(), revision: self.revision, epoch: self.epoch, sequence: self.current_sequence(), asset_count: self.assets.len() }
    }

    fn project_snapshot(&self) -> ProjectSnapshot {
        ProjectSnapshot { summary: self.project_summary(), assets: self.assets_sorted() }
    }

    fn publish_snapshot(&mut self) {
        let sequence = self.next_event_sequence;
        let summary = ProjectSummary { project_id: self.project_id.clone(), revision: self.revision, epoch: self.epoch, sequence, asset_count: self.assets.len() };
        let snapshot = ProjectSnapshot { summary, assets: self.assets_sorted() };
        self.events.push_back(ServiceEvent { epoch: self.epoch, sequence, payload: json!({"event": "project.snapshot.published", "snapshot": snapshot}) });
        self.next_event_sequence += 1;
    }

    fn subscribe(&self, params: Value) -> Result<Value, (&'static str, &'static str)> {
        let cursor: SubscriptionRequest = serde_json::from_value(params).map_err(|_| ("invalid_request", "a subscription epoch and cursor are required"))?;
        if cursor.epoch != self.epoch { return Err(("epoch_mismatch", "subscription cursor belongs to a previous service epoch")); }
        if let (Some(from_sequence), Some(first)) = (cursor.from_sequence, self.events.front()) {
            if from_sequence.saturating_add(1) < first.sequence { return Err(("subscription_cursor_expired", "subscription cursor is older than retained events")); }
        }
        let from_sequence = cursor.from_sequence.unwrap_or(0);
        Ok(json!(SubscriptionResponse { epoch: self.epoch, next_sequence: self.next_event_sequence, events: self.events.iter().filter(|event| event.sequence > from_sequence).cloned().collect() }))
    }

    fn command_receipt(&self, params: Value) -> Result<Value, (&'static str, &'static str)> {
        let request_id = params.get("request_id").and_then(Value::as_str).ok_or(("invalid_request", "request_id is required"))?;
        self.receipts.get(&RequestId(request_id.into())).map(|receipt| json!(receipt)).ok_or(("command_not_found", "command receipt is not retained"))
    }

    fn trace_query(&self, params: Value) -> Vec<neon_observability::TraceRecord> {
        let filter = JournalFilter {
            request_id: params.get("request_id").and_then(Value::as_str).map(|value| RequestId(value.into())),
            revision: params.get("revision").and_then(Value::as_u64).map(Revision),
            ..JournalFilter::default()
        };
        self.journal.query(&filter)
    }

    fn asset_bytes(&self, params: Value) -> Result<Value, (&'static str, &'static str)> {
        let asset: AssetRef = serde_json::from_value(params).map_err(|_| ("invalid_request", "a stable AssetRef is required"))?;
        self.assets.get(&key(&asset)).map(|content| json!(content)).ok_or(("asset_revision_not_found", "asset bytes are not available at this revision"))
    }
}

pub fn serve(endpoint: SocketAddr, epoch: u64) -> Result<(), TransportError> {
    let server = RpcServer::bind(endpoint)?;
    let mut service = Projectd::fixture(epoch);
    service.set_endpoint(server.local_addr()?);
    server.serve_until(|request| {
        let shutdown = request.method == "service.shutdown";
        (service.handle(request), !shutdown)
    })
}

fn key(asset: &AssetRef) -> (String, u64, u64, String) { (asset.project_id.clone(), asset.asset_id, asset.revision.0, asset.kind.clone()) }

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion, RequestId};
    use std::{net::TcpListener, thread, time::Duration};

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

    #[test]
    fn snapshot_assets_subscription_and_receipts_are_revisioned() {
        let mut service = Projectd::fixture(3);
        let summary = service.handle(request("summary", "project.summary", json!({})));
        assert_eq!(summary.status, RpcStatus::Accepted);
        let summary: ProjectSummary = serde_json::from_value(summary.result.unwrap()).unwrap();
        assert_eq!(summary.project_id, "fixture-project");
        assert_eq!(summary.revision, Revision(5));
        assert_eq!(summary.epoch, 3);
        assert_eq!(summary.sequence, 1);
        assert_eq!(summary.asset_count, 2);

        let assets = service.handle(request("assets", "asset.list", json!({})));
        let assets: Vec<AssetRef> = serde_json::from_value(assets.result.unwrap()).unwrap();
        assert_eq!(assets.iter().map(|asset| asset.asset_id).collect::<Vec<_>>(), vec![81, 82]);

        let subscription = service.handle(request("subscribe", "project.subscribe", json!(SubscriptionRequest { epoch: 3, from_sequence: Some(0) })));
        let subscription: SubscriptionResponse = serde_json::from_value(subscription.result.unwrap()).unwrap();
        assert_eq!(subscription.next_sequence, 2);
        assert_eq!(subscription.events.len(), 1);
        assert_eq!(subscription.events[0].payload["event"], "project.snapshot.published");

        let receipt = service.handle(request("receipt", "debug.command.get", json!({"request_id": "summary"})));
        let receipt: CommandReceipt = serde_json::from_value(receipt.result.unwrap()).unwrap();
        assert_eq!(receipt.state, CommandState::Accepted);
        assert_eq!(receipt.revision_after, Some(Revision(5)));

        let stale = service.handle(request("stale-subscribe", "service.subscribe", json!(SubscriptionRequest { epoch: 2, from_sequence: None })));
        assert_eq!(stale.error.unwrap().code, "epoch_mismatch");
    }

    #[test]
    fn loopback_server_exposes_the_project_control_plane() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        drop(listener);
        let server = thread::spawn(move || serve(endpoint, 7));
        let call = |request: RpcRequest| {
            let mut client = loop {
                match neon_ipc::RpcClient::connect(endpoint) {
                    Ok(client) => break client,
                    Err(_) => thread::sleep(Duration::from_millis(5)),
                }
            };
            client.call(&request).unwrap()
        };
        let health = call(request("health", "service.health", json!({})));
        assert_eq!(health.status, RpcStatus::Accepted);
        let description = call(request("describe", "service.describe", json!({})));
        let description: ServiceDescription = serde_json::from_value(description.result.unwrap()).unwrap();
        assert_eq!(description.endpoint, format!("tcp://{endpoint}"));
        let summary = call(request("summary", "project.summary", json!({})));
        assert_eq!(summary.snapshot.unwrap()["summary"]["epoch"], 7);
        let assets = call(request("assets", "asset.list", json!({})));
        assert_eq!(assets.result.unwrap().as_array().unwrap().len(), 2);
        let subscription = call(request("subscribe", "service.subscribe", json!(SubscriptionRequest { epoch: 7, from_sequence: None })));
        assert_eq!(subscription.result.unwrap()["events"].as_array().unwrap().len(), 1);
        let shutdown = call(request("shutdown", "service.shutdown", json!({})));
        assert_eq!(shutdown.status, RpcStatus::Accepted);
        server.join().unwrap().unwrap();
    }
}
