//! Headless UI declaration runtime. It must not create windows or GPU objects.

use std::net::SocketAddr;

use neon_ipc::{RpcClient, TransportError};
use neon_observability::{
    CommandJournal, DebugSnapshot, JournalFilter, TraceLevel, TraceRecord, EVENT_COMMAND_ACCEPTED,
    EVENT_COMMAND_REJECTED,
};
use neon_protocol::{
    ClientIdentity, ClientKind, HealthStatus, ProtocolVersion, RequestId, Revision, RpcRequest,
    RpcResponse, RpcStatus, ServiceDescription, ServiceHealth, ServiceName, PROTOCOL_VERSION,
};
use neon_ui_schema::{
    UiBounds, UiCommand, UiEffect, UiFragment, UiFragmentId, UiNode, UiNodeId, UiNodeKind,
};
use serde_json::json;

pub const SERVICE_NAME: &str = "ui-runtime";

pub struct UiRuntime {
    epoch: u64,
    client: ClientIdentity,
    cached_fragment: Option<UiFragment>,
    journal: CommandJournal,
}

impl UiRuntime {
    pub fn new(epoch: u64, instance_id: impl Into<String>) -> Self {
        Self {
            epoch,
            client: ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: instance_id.into(),
                pid: std::process::id(),
                origin: "neon-ui-runtime".into(),
            },
            cached_fragment: None,
            journal: CommandJournal::new(ServiceName(SERVICE_NAME.into()), epoch, 128),
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
            endpoint: "headless://ui-runtime".into(),
            epoch: self.epoch,
            capabilities: vec!["ui.static_fragment.submit.v1".into()],
        }
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        DebugSnapshot {
            service: ServiceName(SERVICE_NAME.into()),
            epoch: self.epoch,
            revision: self.cached_fragment.as_ref().map_or(Revision(0), |fragment| fragment.revision),
            health: HealthStatus::Healthy,
            capabilities: self.service_description().capabilities,
            active_jobs: Vec::new(),
        }
    }

    pub fn handle_service_request(&self, request: RpcRequest) -> RpcResponse {
        let result = match request.method.as_str() {
            "service.health" => Some(json!(self.service_health())),
            "service.describe" => Some(json!(self.service_description())),
            "service.shutdown" => Some(json!({"state": "accepted"})),
            "debug.snapshot.get" => Some(json!(self.debug_snapshot())),
            _ => None,
        };
        match result {
            Some(result) => RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Accepted,
                revision: Some(self.cached_fragment.as_ref().map_or(Revision(0), |fragment| fragment.revision)),
                result: Some(result),
                snapshot: None,
                error: None,
            },
            None => RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Rejected,
                revision: None,
                result: None,
                snapshot: None,
                error: Some(neon_protocol::RpcError {
                    code: "unsupported_method".into(),
                    message: "method is not supported".into(),
                    current_revision: None,
                    object_id: None,
                }),
            },
        }
    }

    pub fn static_fragment(&self, revision: Revision) -> UiFragment {
        UiFragment {
            fragment_id: UiFragmentId("static-editor-shell".into()),
            revision,
            root: UiNode {
                node_id: UiNodeId("root-panel".into()),
                kind: UiNodeKind::Panel,
                bounds: UiBounds { x: 0.0, y: 0.0, width: 320.0, height: 160.0 },
                visible: true,
                enabled: true,
                text_key: None,
                children: vec![UiNode {
                    node_id: UiNodeId("title-label".into()),
                    kind: UiNodeKind::Label,
                    bounds: UiBounds { x: 16.0, y: 16.0, width: 160.0, height: 24.0 },
                    visible: true,
                    enabled: true,
                    text_key: Some("ui.static.title".into()),
                    children: Vec::new(),
                }],
            },
            effects: vec![UiEffect::SemanticAction { action: "ui.static.ready".into() }],
        }
    }

    pub fn submit_static_fragment(
        &mut self,
        endpoint: SocketAddr,
        request_id: RequestId,
        revision: Revision,
        idempotency_key: String,
    ) -> Result<RpcResponse, TransportError> {
        let fragment = self.static_fragment(revision);
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: request_id.clone(),
            client: self.client.clone(),
            target: ServiceName("wgpu-runtime".into()),
            method: "wgpu.ui.submit_fragment".into(),
            params: json!(UiCommand::SubmitFragment { fragment: fragment.clone() }),
            expected_revision: None,
            idempotency_key: Some(idempotency_key),
        };
        let mut client = RpcClient::connect(endpoint)?;
        let response = client.call(&request)?;
        let event = match response.status {
            RpcStatus::Accepted => {
                self.cached_fragment = Some(fragment);
                EVENT_COMMAND_ACCEPTED
            }
            RpcStatus::Rejected | RpcStatus::Failed => EVENT_COMMAND_REJECTED,
        };
        self.journal.append(
            if response.status == RpcStatus::Accepted { TraceLevel::Info } else { TraceLevel::Warn },
            event,
            Some(request_id),
            None,
            None,
            None,
            None,
            response.revision,
            json!({"target": "wgpu-runtime", "status": response.status}),
        );
        Ok(response)
    }

    pub fn cached_fragment(&self) -> Option<&UiFragment> {
        self.cached_fragment.as_ref()
    }

    pub fn traces(&self, filter: &JournalFilter) -> Vec<TraceRecord> {
        self.journal.query(filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_ipc::RpcServer;
    use neon_protocol::RpcError;
    use std::thread;

    fn accepted(request: RpcRequest) -> RpcResponse {
        RpcResponse {
            request_id: request.request_id,
            status: RpcStatus::Accepted,
            revision: Some(Revision(1)),
            result: Some(json!({"fragment_count": 1})),
            snapshot: None,
            error: None,
        }
    }

    #[test]
    fn static_fragment_is_valid_ui_schema() {
        let runtime = UiRuntime::new(1, "ui-test");
        runtime.static_fragment(Revision(1)).validate().unwrap();
    }

    #[test]
    fn headless_service_methods_are_explicit() {
        let runtime = UiRuntime::new(1, "ui-test");
        for method in ["service.health", "service.describe", "service.shutdown"] {
            let response = runtime.handle_service_request(RpcRequest {
                protocol: "neon3.rpc".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId(method.into()),
                client: ClientIdentity { kind: ClientKind::Cli, instance_id: "test".into(), pid: 1, origin: "test".into() },
                target: ServiceName(SERVICE_NAME.into()),
                method: method.into(),
                params: json!({}),
                expected_revision: None,
                idempotency_key: None,
            });
            assert_eq!(response.status, RpcStatus::Accepted);
        }
    }

    #[test]
    fn sends_static_fragment_to_loopback_wgpu_server() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = thread::spawn(move || server.serve_one(accepted));
        let mut runtime = UiRuntime::new(1, "ui-test");
        let response = runtime
            .submit_static_fragment(endpoint, RequestId("submit-1".into()), Revision(1), "key-1".into())
            .unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.cached_fragment().unwrap().fragment_id.0, "static-editor-shell");
        assert_eq!(
            runtime.traces(&JournalFilter::default())[0].event,
            EVENT_COMMAND_ACCEPTED
        );
        server_thread.join().unwrap().unwrap();
    }

    #[test]
    fn rejection_is_exposed_and_not_cached_as_success() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = thread::spawn(move || {
            server.serve_one(|request| RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Rejected,
                revision: Some(Revision(2)),
                result: None,
                snapshot: None,
                error: Some(RpcError {
                    code: "revision_conflict".into(),
                    message: "fragment revision is stale".into(),
                    current_revision: Some(Revision(2)),
                    object_id: None,
                }),
            })
        });
        let mut runtime = UiRuntime::new(1, "ui-test");
        let response = runtime
            .submit_static_fragment(endpoint, RequestId("reject-1".into()), Revision(1), "key-2".into())
            .unwrap();
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "revision_conflict");
        assert!(runtime.cached_fragment().is_none());
        assert_eq!(runtime.traces(&JournalFilter::default())[0].event, EVENT_COMMAND_REJECTED);
        server_thread.join().unwrap().unwrap();
    }
}
