//! Headless command handling for Neon3's sole future window and GPU owner.
//! No other Neon3 crate may initialize window or GPU objects.

use std::collections::HashMap;

use neon_observability::{
    CommandJournal, CommandReceipt, CommandState, DebugSnapshot, JournalFilter, TraceLevel,
    TraceRecord, EVENT_COMMAND_ACCEPTED, EVENT_COMMAND_RECEIVED, EVENT_COMMAND_REJECTED,
};
use neon_protocol::{
    HealthStatus, RequestId, Revision, RpcError, RpcRequest, RpcResponse, RpcStatus,
    ServiceDescription, ServiceHealth, ServiceName, PROTOCOL_VERSION,
};
use neon_ui_schema::{UiCommand, UiFragment, UiFragmentId};
use serde_json::{json, Value};

pub const SERVICE_NAME: &str = "wgpu-runtime";
pub const CAPABILITY_UI_FRAGMENT: &str = "wgpu.ui.fragment.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderDiagnostics {
    pub graph_revision: Revision,
    pub fragment_count: usize,
    pub mode: RenderMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderMode {
    Headless,
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
            capabilities: vec![CAPABILITY_UI_FRAGMENT.into(), "wgpu.render.diagnostics".into()],
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

        if matches!(request.method.as_str(), "wgpu.ui.submit_fragment" | "wgpu.ui.remove_fragment") {
            let Some(idempotency_key) = request.idempotency_key.as_ref() else {
                return self.reject(request_id, "invalid_request", "idempotency_key is required", None);
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
            "wgpu.render.diagnostics" => self.accept(request_id, diagnostics_value(self.diagnostics())),
            "debug.snapshot.get" => self.accept(request_id, json!(self.debug_snapshot())),
            "debug.command.get" => self.command_get(request_id, request.params),
            "debug.trace.query" => self.trace_query(request_id, request.params),
            "wgpu.ui.submit_fragment" => self.submit_fragment(request_id, request.params),
            "wgpu.ui.remove_fragment" => self.remove_fragment(request_id, request.params),
            _ => self.reject(request_id, "unsupported_method", "method is not supported", None),
        };
        if matches!(request.method.as_str(), "wgpu.ui.submit_fragment" | "wgpu.ui.remove_fragment")
            && response.status == RpcStatus::Accepted
            && let Some(idempotency_key) = request.idempotency_key
        {
            self.idempotent_responses.insert(idempotency_key, response.clone());
        }
        response
    }

    fn command_get(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let Some(target_id) = params.get("request_id").and_then(Value::as_str) else {
            return self.reject(request_id, "invalid_request", "request_id is required", None);
        };
        match self.command_receipt(&RequestId(target_id.into())) {
            Some(receipt) => self.accept(request_id, json!(receipt)),
            None => self.reject(request_id, "not_found", "command receipt was not found", None),
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
            Ok(_) => return self.reject(request_id, "invalid_request", "expected submit_fragment command", None),
            Err(_) => return self.reject(request_id, "invalid_request", "invalid UI command", None),
        };
        let UiCommand::SubmitFragment { fragment } = command else { unreachable!() };
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
        self.fragments.insert(fragment.fragment_id.clone(), fragment);
        self.graph_revision = Revision(self.graph_revision.0 + 1);
        self.accept(request_id, diagnostics_value(self.diagnostics()))
    }

    fn remove_fragment(&mut self, request_id: RequestId, params: Value) -> RpcResponse {
        let command: UiCommand = match serde_json::from_value(params) {
            Ok(command @ UiCommand::RemoveFragment { .. }) => command,
            Ok(_) => return self.reject(request_id, "invalid_request", "expected remove_fragment command", None),
            Err(_) => return self.reject(request_id, "invalid_request", "invalid UI command", None),
        };
        let UiCommand::RemoveFragment { fragment_id, revision } = command else { unreachable!() };
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

    fn record_receipt(&mut self, request_id: &RequestId, state: CommandState, error_code: Option<String>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion};
    use neon_ui_schema::{UiBounds, UiEffect, UiNode, UiNodeId, UiNodeKind};

    fn request(id: &str, method: &str, params: Value) -> RpcRequest {
        RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId(id.into()),
            client: ClientIdentity { kind: ClientKind::Cli, instance_id: "test".into(), pid: 1, origin: "test".into() },
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
                bounds: UiBounds { x: 0.0, y: 0.0, width: 10.0, height: 10.0 },
                visible: true,
                enabled: true,
                text_key: None,
                children: Vec::new(),
            },
            effects: vec![UiEffect::SemanticAction { action: "ui.static.ready".into() }],
        }
    }

    fn submit(id: &str, revision: u64) -> RpcRequest {
        let mut request = request(id, "wgpu.ui.submit_fragment", json!(UiCommand::SubmitFragment { fragment: fragment(revision) }));
        request.idempotency_key = Some(format!("key-{id}"));
        request
    }

    #[test]
    fn headless_health_and_describe_are_available() {
        let mut runtime = WgpuRuntime::headless(7);
        let health = runtime.handle(request("health", "service.health", json!({})));
        let describe = runtime.handle(request("describe", "service.describe", json!({})));
        assert_eq!(health.status, RpcStatus::Accepted);
        assert_eq!(health.result.unwrap()["status"], "healthy");
        assert_eq!(describe.result.unwrap()["epoch"], 7);
    }

    #[test]
    fn submit_updates_headless_composition_registry() {
        let mut runtime = WgpuRuntime::headless(1);
        let response = runtime.handle(submit("submit", 1));
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.diagnostics().fragment_count, 1);
        assert_eq!(runtime.diagnostics().graph_revision, Revision(1));
        assert_eq!(runtime.command_receipt(&RequestId("submit".into())).unwrap().state, CommandState::Accepted);
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
        let response = runtime.handle(request("lookup", "debug.command.get", json!({"request_id": "submit"})));
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.result.unwrap()["state"], "accepted");
    }

    #[test]
    fn ui_runtime_and_cli_source_do_not_claim_renderer_ownership() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for crate_name in ["neon-ui-runtime", "neon-cli"] {
            let source = std::fs::read_to_string(workspace.join("crates").join(crate_name).join("src/main.rs"))
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
        let (_device, _queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("neon3-headless-acceptance"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
        }, None))
        .expect("the selected headless adapter must create a device and queue");
    }
}
