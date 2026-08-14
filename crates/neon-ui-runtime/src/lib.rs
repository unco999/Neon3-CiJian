//! Headless UI declaration runtime. It must not create windows or GPU objects.

use std::{collections::HashMap, net::SocketAddr};

use neon_ipc::{RpcClient, RpcServer, TransportError};
use neon_observability::{
    CommandJournal, CommandReceipt, CommandState, DebugSnapshot, EVENT_COMMAND_ACCEPTED,
    EVENT_COMMAND_RECEIVED, EVENT_COMMAND_REJECTED, JournalFilter, TraceLevel, TraceRecord,
};
use neon_protocol::{
    ClientIdentity, ClientKind, HealthStatus, PROTOCOL_VERSION, ProtocolVersion, RequestId,
    Revision, RpcError, RpcRequest, RpcResponse, RpcStatus, ServiceDescription, ServiceHealth, ServiceName,
};
use neon_ui_schema::{
    UiBounds, UiCommand, UiEffect, UiFragment, UiFragmentId, UiFragmentSubmission, UiNode, UiNodeId, UiNodeKind, UiStyle,
    UiDiagnosticsState, UiInspectorState, UiInspectorTab, UiSurfaceEvent, UiSurfaceEventKind, UiSurfaceEventRequest, UiSurfaceSnapshot, UiSurfaceState,
    UiSemanticEvent, UiTransition, UiTransitionState, ERROR_FRAGMENT_REVISION_STALE,
    ERROR_INPUT_SEQUENCE_STALE, ERROR_INTENT_NOT_BOUND, ERROR_RENDERER_EPOCH_MISMATCH,
};
use serde_json::{Value, json};

pub const SERVICE_NAME: &str = "ui-runtime";

#[derive(Clone, Debug, PartialEq)]
pub struct UiSurfaceTransition {
    pub snapshot: UiSurfaceSnapshot,
    pub effects: Vec<UiEffect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiSurfaceMachine { revision: Revision, state: UiSurfaceState }

impl UiSurfaceMachine {
    fn new() -> Self {
        Self { revision: Revision(0), state: UiSurfaceState { diagnostics: UiDiagnosticsState::Collapsed, inspector: UiInspectorState { tab: UiInspectorTab::Overview } } }
    }

    fn snapshot(&self) -> UiSurfaceSnapshot {
        UiSurfaceSnapshot { revision: self.revision, value: self.state.clone(), available_events: vec![UiSurfaceEventKind::DiagnosticsToggle, UiSurfaceEventKind::InspectorTabSelect] }
    }

    fn transition(&mut self, event: UiSurfaceEvent) -> Result<UiSurfaceTransition, (&'static str, &'static str)> {
        match event {
            UiSurfaceEvent::DiagnosticsToggle => {
                self.state.diagnostics = match self.state.diagnostics { UiDiagnosticsState::Collapsed => UiDiagnosticsState::Expanded, UiDiagnosticsState::Expanded => UiDiagnosticsState::Collapsed };
            }
            UiSurfaceEvent::InspectorTabSelect { tab } => {
                if self.state.inspector.tab == tab { return Err(("ui_guard_rejected", "inspector tab is already selected")); }
                self.state.inspector.tab = tab;
            }
        }
        self.revision = Revision(self.revision.0 + 1);
        Ok(UiSurfaceTransition { snapshot: self.snapshot(), effects: Vec::new() })
    }
}

pub struct UiRuntime {
    epoch: u64,
    client: ClientIdentity,
    cached_fragment: Option<UiFragment>,
    journal: CommandJournal,
    receipts: HashMap<RequestId, CommandReceipt>,
    last_input_sequence: HashMap<u64, u64>,
    idempotent_responses: HashMap<String, RpcResponse>,
    surface: UiSurfaceMachine,
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
            receipts: HashMap::new(),
            last_input_sequence: HashMap::new(),
            idempotent_responses: HashMap::new(),
            surface: UiSurfaceMachine::new(),
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
            capabilities: vec![
                "ui.static_fragment.submit.v1".into(),
                "ui.fragment.submit.v1".into(),
                "ui.semantic_input.v1".into(),
                "ui.intent_dispatch.v1".into(),
                "ui.surface.machine.v1".into(),
            ],
        }
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        DebugSnapshot {
            service: ServiceName(SERVICE_NAME.into()),
            epoch: self.epoch,
            revision: self
                .cached_fragment
                .as_ref()
                .map_or(Revision(0), |fragment| fragment.revision),
            health: HealthStatus::Healthy,
            capabilities: self.service_description().capabilities,
            active_jobs: Vec::new(),
        }
    }

    pub fn handle_service_request(&mut self, request: RpcRequest) -> RpcResponse {
        let result = match request.method.as_str() {
            "service.health" => Some(json!(self.service_health())),
            "service.describe" => Some(json!(self.service_description())),
            "service.shutdown" => Some(json!({"state": "accepted"})),
            "debug.snapshot.get" => Some(json!(self.debug_snapshot())),
            "ui.fragment.submit" => return self.handle_fragment_submit(request),
            "ui.surface.snapshot.get" => Some(self.surface_value()),
            "ui.surface.event" => return self.handle_surface_event(request),
            "ui.input.event" => return self.handle_input_event(request),
            "ui.intent.dispatch" => return self.handle_intent_dispatch(request),
            _ => None,
        };
        match result {
            Some(result) => RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Accepted,
                revision: Some(
                    self.cached_fragment
                        .as_ref()
                        .map_or(Revision(0), |fragment| fragment.revision),
                ),
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

    fn handle_surface_event(&mut self, request: RpcRequest) -> RpcResponse {
        let Some(key) = request.idempotency_key.clone() else { return self.rejected(request.request_id, "invalid_request", "idempotency_key is required"); };
        if let Some(response) = self.idempotent_responses.get(&key) { let mut response = response.clone(); response.request_id = request.request_id; return response; }
        if request.expected_revision != Some(self.surface.revision) { return self.rejected(request.request_id, "revision_conflict", "UI surface revision is stale"); }
        let event = match self.parse_surface_event(&request.params) { Ok(event) => event, Err((code, message)) => return self.rejected(request.request_id, code, message) };
        let transition = match self.surface.transition(event) { Ok(transition) => transition, Err((code, message)) => return self.rejected(request.request_id, code, message) };
        self.record_receipt(&request.request_id, CommandState::Accepted, None);
        let response = RpcResponse { request_id: request.request_id, status: RpcStatus::Accepted, revision: Some(transition.snapshot.revision), result: Some(json!(transition.snapshot)), snapshot: None, error: None };
        self.idempotent_responses.insert(key, response.clone());
        response
    }

    fn parse_surface_event(&self, params: &Value) -> Result<UiSurfaceEvent, (&'static str, &'static str)> {
        serde_json::from_value::<UiSurfaceEventRequest>(params.clone())
            .map(|request| request.event)
            .map_err(|_| ("invalid_request", "a supported UI surface event is required"))
    }

    fn surface_value(&self) -> Value {
        json!(self.surface.snapshot())
    }

    /// Runs the UI declaration control plane. A React client can only submit to this
    /// service; the service forwards a validated declaration to the sole renderer.
    pub fn serve_forwarder(
        endpoint: SocketAddr,
        wgpu_endpoint: SocketAddr,
        epoch: u64,
    ) -> Result<(), TransportError> {
        let server = RpcServer::bind(endpoint)?;
        let mut runtime = Self::new(epoch, "ui-runtime-forwarder");
        server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            let request_id = request.request_id.clone();
            let response = if request.method == "ui.fragment.submit" {
                runtime.forward_fragment(wgpu_endpoint, request).unwrap_or_else(|error| {
                    runtime.rejected(request_id, "service_unavailable", &error.to_string())
                })
            } else {
                runtime.handle_service_request(request)
            };
            (response, !shutdown)
        })
    }

    /// The cache is published only after the renderer accepted the exact declaration.
    pub fn forward_fragment(
        &mut self,
        wgpu_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let request_id = request.request_id.clone();
        let Some(idempotency_key) = request.idempotency_key.clone() else {
            return Ok(self.rejected(request_id, "invalid_request", "idempotency_key is required"));
        };
        if let Some(cached) = self.idempotent_responses.get(&idempotency_key) {
            let mut response = cached.clone();
            response.request_id = request_id;
            return Ok(response);
        }
        let fragment = match self.validate_fragment_submission(&request) {
            Ok(fragment) => fragment,
            Err((code, message)) => return Ok(self.rejected(request_id, code, message)),
        };
        self.record_receipt(&request.request_id, CommandState::Received, None);
        self.journal.append(TraceLevel::Info, EVENT_COMMAND_RECEIVED, Some(request.request_id.clone()), None, None, None, Some(self.debug_snapshot().revision), None, json!({"method": "ui.fragment.submit", "fragment_id": fragment.fragment_id.0, "fragment_revision": fragment.revision.0}));
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: request.request_id.clone(),
            client: self.client.clone(), target: ServiceName("wgpu-runtime".into()), method: "wgpu.ui.submit_fragment".into(),
            params: request.params, expected_revision: None, idempotency_key: Some(idempotency_key.clone()),
        };
        let response = RpcClient::connect(wgpu_endpoint)?.call(&forwarded)?;
        if response.status == RpcStatus::Accepted {
            self.cached_fragment = Some(fragment);
            self.record_receipt(&request.request_id, CommandState::Accepted, None);
            self.journal.append(TraceLevel::Info, EVENT_COMMAND_ACCEPTED, Some(request.request_id.clone()), None, None, None, response.revision, response.revision, json!({"target": "wgpu-runtime", "state": "accepted"}));
            self.idempotent_responses.insert(idempotency_key, response.clone());
        } else {
            self.record_receipt(&request.request_id, CommandState::Rejected, response.error.as_ref().map(|error| error.code.clone()));
            self.journal.append(TraceLevel::Warn, EVENT_COMMAND_REJECTED, Some(request.request_id.clone()), None, None, None, Some(self.debug_snapshot().revision), response.revision, json!({"target": "wgpu-runtime", "state": "rejected", "code": response.error.as_ref().map(|error| error.code.clone())}));
        }
        Ok(response)
    }

    fn handle_fragment_submit(&mut self, request: RpcRequest) -> RpcResponse {
        let fragment = match self.validate_fragment_submission(&request) {
            Ok(fragment) => fragment,
            Err((code, message)) => return self.rejected(request.request_id, code, message),
        };
        let revision = fragment.revision;
        self.cached_fragment = Some(fragment);
        self.accepted(request.request_id, json!({"fragment_revision": revision, "state": "accepted"}))
    }

    fn validate_fragment_submission(&self, request: &RpcRequest) -> Result<UiFragment, (&'static str, &'static str)> {
        let command: UiCommand = serde_json::from_value(request.params.clone())
            .map_err(|_| ("invalid_request", "invalid UI command"))?;
        let UiCommand::SubmitFragment { submission } = command else {
            return Err(("invalid_request", "expected submit_fragment command"));
        };
        if submission.validate().is_err() {
            return Err(("invalid_request", "invalid UI fragment submission"));
        }
        if request.expected_revision.is_some_and(|expected| expected != self.debug_snapshot().revision) {
            return Err(("revision_conflict", "UI fragment revision is stale"));
        }
        if self.cached_fragment.as_ref().is_some_and(|current| submission.fragment.revision <= current.revision) {
            return Err(("revision_conflict", "UI fragment revision is stale"));
        }
        Ok(submission.fragment)
    }

    /// Forwards a renderer-resolved semantic event as a typed domain RPC.
    /// Render IDs and fragment-local node IDs are not accepted or emitted here.
    pub fn dispatch_semantic_event(
        &mut self,
        terrain_endpoint: SocketAddr,
        event: UiSemanticEvent,
        request_id: RequestId,
        idempotency_key: String,
    ) -> Result<RpcResponse, TransportError> {
        self.validate_semantic_event(&event)
            .map_err(|code| TransportError::Io(std::io::Error::other(code)))?;
        let neon_ui_schema::UiIntent::Invoke { action, params } = event.intent.clone();
        let request = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: request_id.clone(),
            client: self.client.clone(), target: ServiceName("terrain-runtime".into()), method: action,
            params, expected_revision: Some(event.fragment.revision), idempotency_key: Some(idempotency_key),
        };
        self.record_receipt(&request_id, CommandState::Received, None);
        self.journal.append(TraceLevel::Info, EVENT_COMMAND_RECEIVED, Some(request_id.clone()), None, None, None, Some(event.fragment.revision), None, json!({"event_id": event.event_id, "target": "terrain-runtime", "method": request.method}));
        let response = RpcClient::connect(terrain_endpoint)?.call(&request)?;
        let accepted = response.status == RpcStatus::Accepted;
        self.record_receipt(&request_id, if accepted { CommandState::Accepted } else { CommandState::Rejected }, response.error.as_ref().map(|error| error.code.clone()));
        self.journal.append(if accepted { TraceLevel::Info } else { TraceLevel::Warn }, if accepted { EVENT_COMMAND_ACCEPTED } else { EVENT_COMMAND_REJECTED }, Some(request_id), None, None, None, Some(event.fragment.revision), response.revision, json!({"event_id": event.event_id, "renderer_epoch": event.renderer_epoch, "fragment_id": event.fragment.id.0, "target": "terrain-runtime", "method": request.method}));
        Ok(response)
    }

    pub fn command_receipt(&self, request_id: &RequestId) -> Option<&CommandReceipt> { self.receipts.get(request_id) }

    fn handle_input_event(&mut self, mut request: RpcRequest) -> RpcResponse {
        let event: UiSemanticEvent = match serde_json::from_value(request.params.clone()) {
            Ok(event) => event,
            Err(_) => return self.rejected(request.request_id, "invalid_request", "invalid UI semantic event"),
        };
        match self.validate_semantic_event(&event) {
            Ok(()) => {
                let neon_ui_schema::UiIntent::Invoke { action, params } = event.intent.clone();
                if action == "ui.surface.event" {
                    request.method = "ui.surface.event".into();
                    request.params = params;
                    request.expected_revision = Some(event.fragment.revision);
                    return self.handle_surface_event(request);
                }
                self.accepted(request.request_id, json!({"event_id": event.event_id, "intent": event.intent}))
            }
            Err(code) => self.rejected(request.request_id, code, "UI semantic event was rejected"),
        }
    }

    fn handle_intent_dispatch(&mut self, mut request: RpcRequest) -> RpcResponse {
        let intent: neon_ui_schema::UiIntent = match serde_json::from_value(request.params.clone()) {
            Ok(intent) => intent,
            Err(_) => return self.rejected(request.request_id, "invalid_request", "invalid UI intent"),
        };
        if intent.validate().is_err() { return self.rejected(request.request_id, ERROR_INTENT_NOT_BOUND, "UI intent is not bound"); }
        let neon_ui_schema::UiIntent::Invoke { action, params } = intent.clone();
        if action == "ui.surface.event" {
            request.method = "ui.surface.event".into();
            request.params = params;
            return self.handle_surface_event(request);
        }
        self.accepted(request.request_id, json!({"intent": intent}))
    }

    fn validate_semantic_event(&mut self, event: &UiSemanticEvent) -> Result<(), &'static str> {
        if event.renderer_epoch != self.epoch { return Err(ERROR_RENDERER_EPOCH_MISMATCH); }
        let Some(fragment) = self.cached_fragment.as_ref() else { return Err(ERROR_INTENT_NOT_BOUND); };
        if fragment.fragment_id != event.fragment.id || fragment.revision != event.fragment.revision { return Err(ERROR_FRAGMENT_REVISION_STALE); }
        if !fragment.effects.iter().any(|effect| matches!(effect, UiEffect::SemanticIntent { intent } | UiEffect::BoundSemanticIntent { intent, .. } if intent == &event.intent)) {
            return Err(ERROR_INTENT_NOT_BOUND);
        }
        if let Some(pointer) = &event.pointer {
            if self.last_input_sequence.get(&pointer.id).is_some_and(|last| pointer.sequence <= *last) { return Err(ERROR_INPUT_SEQUENCE_STALE); }
            self.last_input_sequence.insert(pointer.id, pointer.sequence);
        }
        event.intent.validate().map_err(|_| ERROR_INTENT_NOT_BOUND)
    }

    fn accepted(&mut self, request_id: RequestId, result: Value) -> RpcResponse {
        self.record_receipt(&request_id, CommandState::Accepted, None);
        RpcResponse { request_id, status: RpcStatus::Accepted, revision: Some(self.debug_snapshot().revision), result: Some(result), snapshot: None, error: None }
    }

    fn rejected(&mut self, request_id: RequestId, code: &str, message: &str) -> RpcResponse {
        self.record_receipt(&request_id, CommandState::Rejected, Some(code.into()));
        RpcResponse { request_id, status: RpcStatus::Rejected, revision: Some(self.debug_snapshot().revision), result: None, snapshot: None, error: Some(RpcError { code: code.into(), message: message.into(), current_revision: Some(self.debug_snapshot().revision), object_id: None }) }
    }

    fn record_receipt(&mut self, request_id: &RequestId, state: CommandState, error_code: Option<String>) {
        let revision = self.debug_snapshot().revision;
        self.receipts.insert(request_id.clone(), CommandReceipt { request_id: request_id.clone(), state, revision_before: Some(revision), revision_after: Some(revision), error_code });
    }

    pub fn static_fragment(&self, revision: Revision) -> UiFragment {
        UiFragment {
            fragment_id: UiFragmentId("static-editor-shell".into()),
            revision,
            root: UiNode {
                node_id: UiNodeId("root-panel".into()),
                kind: UiNodeKind::Panel,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 320.0,
                    height: 160.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                style: UiStyle {
                    background_color: [0.055, 0.07, 0.09, 0.98],
                    border_color: [0.22, 0.76, 0.88, 0.8],
                    border_width: 1.0,
                    corner_radius: 6.0,
                    opacity: 1.0,
                },
                enter_transition: Some(UiTransition {
                    delay_ms: 0,
                    duration_ms: 220,
                    easing: neon_ui_schema::UiEasing::EaseOut,
                    from: UiTransitionState {
                        opacity: Some(0.0),
                        bounds: Some(UiBounds {
                            x: 0.0,
                            y: 12.0,
                            width: 320.0,
                            height: 160.0,
                        }),
                        ..UiTransitionState::default()
                    },
                }),
                children: vec![UiNode {
                    node_id: UiNodeId("title-label".into()),
                    kind: UiNodeKind::Label,
                    bounds: UiBounds {
                        x: 16.0,
                        y: 16.0,
                        width: 160.0,
                        height: 24.0,
                    },
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: Some("ui.static.title".into()),
                    text: None,
                    image: None,
                    style: UiStyle {
                        background_color: [0.16, 0.23, 0.28, 0.9],
                        corner_radius: 3.0,
                        ..UiStyle::default()
                    },
                    enter_transition: Some(UiTransition {
                        delay_ms: 80,
                        duration_ms: 180,
                        easing: neon_ui_schema::UiEasing::EaseOut,
                        from: UiTransitionState {
                            opacity: Some(0.0),
                            ..UiTransitionState::default()
                        },
                    }),
                    children: Vec::new(),
                }],
            },
            effects: vec![
                UiEffect::SemanticAction { action: "ui.static.ready".into() },
                UiEffect::SemanticIntent { intent: neon_ui_schema::UiIntent::Invoke {
                    action: "terrain.tool.select".into(), params: json!({"tool": "water_inject"}),
                }},
            ],
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
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment.clone())
            }),
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
            if response.status == RpcStatus::Accepted {
                TraceLevel::Info
            } else {
                TraceLevel::Warn
            },
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
        let mut runtime = UiRuntime::new(1, "ui-test");
        for method in ["service.health", "service.describe", "service.shutdown"] {
            let response = runtime.handle_service_request(RpcRequest {
                protocol: "neon3.rpc".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId(method.into()),
                client: ClientIdentity {
                    kind: ClientKind::Cli,
                    instance_id: "test".into(),
                    pid: 1,
                    origin: "test".into(),
                },
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
    fn react_fragment_submission_is_cached_and_revisioned() {
        let mut runtime = UiRuntime::new(1, "ui-react-test");
        let fragment = runtime.static_fragment(Revision(1));
        let response = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION,
            request_id: RequestId("react-fragment-1".into()),
            client: ClientIdentity { kind: ClientKind::UiReactClient, instance_id: "react-client".into(), pid: 1, origin: "neon-ui-react-client".into() },
            target: ServiceName(SERVICE_NAME.into()), method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }),
            expected_revision: None, idempotency_key: Some("react-fragment-key-1".into()),
        });
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.cached_fragment().unwrap().revision, Revision(1));
        assert_eq!(runtime.service_description().capabilities.iter().filter(|capability| *capability == "ui.fragment.submit.v1").count(), 1);
    }

    #[test]
    fn surface_actions_are_revisioned_idempotent_discrete_state() {
        let mut runtime = UiRuntime::new(1, "ui-surface-test");
        let action = |id: &str, expected_revision, key: &str, params| RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId(id.into()),
            client: ClientIdentity { kind: ClientKind::UiReactClient, instance_id: "react-client".into(), pid: 1, origin: "neon-ui-react-client".into() },
            target: ServiceName(SERVICE_NAME.into()), method: "ui.surface.event".into(), params, expected_revision: Some(Revision(expected_revision)), idempotency_key: Some(key.into()),
        };
        let first = runtime.handle_service_request(action("toggle-1", 0, "toggle-key", json!({"event": {"type": "DIAGNOSTICS_TOGGLE"}})));
        assert_eq!(first.status, RpcStatus::Accepted);
        assert_eq!(first.result.as_ref().unwrap()["value"]["diagnostics"], "expanded");
        assert_eq!(first.revision, Some(Revision(1)));
        let retry = runtime.handle_service_request(action("toggle-retry", 0, "toggle-key", json!({"event": {"type": "DIAGNOSTICS_TOGGLE"}})));
        assert_eq!(retry.status, RpcStatus::Accepted);
        assert_eq!(retry.revision, Some(Revision(1)));
        assert_eq!(runtime.surface.revision, Revision(1));
        let stale = runtime.handle_service_request(action("tab-stale", 0, "tab-key", json!({"event": {"type": "INSPECTOR_TAB_SELECT", "tab": "materials"}})));
        assert_eq!(stale.error.unwrap().code, "revision_conflict");
        let tab = runtime.handle_service_request(action("tab-1", 1, "tab-key", json!({"event": {"type": "INSPECTOR_TAB_SELECT", "tab": "materials"}})));
        assert_eq!(tab.status, RpcStatus::Accepted);
        assert_eq!(runtime.surface.state.inspector.tab, UiInspectorTab::Materials);
        assert_eq!(runtime.surface.revision, Revision(2));
        let duplicate = runtime.handle_service_request(action("tab-duplicate", 2, "tab-duplicate-key", json!({"event": {"type": "INSPECTOR_TAB_SELECT", "tab": "materials"}})));
        assert_eq!(duplicate.status, RpcStatus::Rejected);
        assert_eq!(duplicate.error.unwrap().code, "ui_guard_rejected");
    }

    #[test]
    fn local_surface_intent_dispatches_through_the_typed_machine() {
        let mut runtime = UiRuntime::new(1, "ui-surface-intent-test");
        let request = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId("surface-intent-1".into()),
            client: ClientIdentity { kind: ClientKind::UiReactClient, instance_id: "react-client".into(), pid: 1, origin: "neon-ui-react-client".into() },
            target: ServiceName(SERVICE_NAME.into()), method: "ui.intent.dispatch".into(),
            params: json!(neon_ui_schema::UiIntent::Invoke { action: "ui.surface.event".into(), params: json!({"event": {"type": "DIAGNOSTICS_TOGGLE"}}) }),
            expected_revision: Some(Revision(0)), idempotency_key: Some("surface-intent-key-1".into()),
        };
        let response = runtime.handle_service_request(request);
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.result.unwrap()["value"]["diagnostics"], "expanded");
        assert_eq!(runtime.surface.revision, Revision(1));
    }

    #[test]
    fn forwarder_caches_only_the_renderer_accepted_revision() {
        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = renderer.local_addr().unwrap();
        let receiver = thread::spawn(move || renderer.serve_one(|request| {
            assert_eq!(request.client.kind, ClientKind::UiRuntime);
            assert_eq!(request.method, "wgpu.ui.submit_fragment");
            assert_eq!(request.request_id.0, "forward-1");
            RpcResponse { request_id: request.request_id, status: RpcStatus::Accepted, revision: Some(Revision(9)), result: Some(json!({"state": "ready"})), snapshot: None, error: None }
        }));
        let mut runtime = UiRuntime::new(1, "ui-forward-test");
        let fragment = runtime.static_fragment(Revision(4));
        let request = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId("forward-1".into()),
            client: ClientIdentity { kind: ClientKind::UiReactClient, instance_id: "react-client".into(), pid: 1, origin: "neon-ui-react-client".into() },
            target: ServiceName(SERVICE_NAME.into()), method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }), expected_revision: None, idempotency_key: Some("forward-key-1".into()),
        };
        let response = runtime.forward_fragment(endpoint, request).unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.cached_fragment().unwrap().revision, Revision(4));
        assert_eq!(runtime.command_receipt(&RequestId("forward-1".into())).unwrap().state, CommandState::Accepted);
        receiver.join().unwrap().unwrap();
    }

    #[test]
    fn renderer_rejection_does_not_advance_the_ui_cache() {
        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = renderer.local_addr().unwrap();
        let receiver = thread::spawn(move || renderer.serve_one(|request| RpcResponse {
            request_id: request.request_id, status: RpcStatus::Rejected, revision: Some(Revision(9)), result: None, snapshot: None,
            error: Some(RpcError { code: "revision_conflict".into(), message: "renderer has a newer fragment".into(), current_revision: Some(Revision(9)), object_id: None }),
        }));
        let mut runtime = UiRuntime::new(1, "ui-forward-test");
        let fragment = runtime.static_fragment(Revision(4));
        let request = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId("forward-reject-1".into()),
            client: ClientIdentity { kind: ClientKind::UiReactClient, instance_id: "react-client".into(), pid: 1, origin: "neon-ui-react-client".into() },
            target: ServiceName(SERVICE_NAME.into()), method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }), expected_revision: None, idempotency_key: Some("forward-reject-key-1".into()),
        };
        let response = runtime.forward_fragment(endpoint, request).unwrap();
        assert_eq!(response.status, RpcStatus::Rejected);
        assert!(runtime.cached_fragment().is_none());
        assert_eq!(runtime.command_receipt(&RequestId("forward-reject-1".into())).unwrap().state, CommandState::Rejected);
        receiver.join().unwrap().unwrap();
    }

    #[test]
    fn sends_static_fragment_to_loopback_wgpu_server() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = thread::spawn(move || server.serve_one(accepted));
        let mut runtime = UiRuntime::new(1, "ui-test");
        let response = runtime
            .submit_static_fragment(
                endpoint,
                RequestId("submit-1".into()),
                Revision(1),
                "key-1".into(),
            )
            .unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(
            runtime.cached_fragment().unwrap().fragment_id.0,
            "static-editor-shell"
        );
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
            .submit_static_fragment(
                endpoint,
                RequestId("reject-1".into()),
                Revision(1),
                "key-2".into(),
            )
            .unwrap();
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "revision_conflict");
        assert!(runtime.cached_fragment().is_none());
        assert_eq!(
            runtime.traces(&JournalFilter::default())[0].event,
            EVENT_COMMAND_REJECTED
        );
        server_thread.join().unwrap().unwrap();
    }

    #[test]
    fn semantic_event_dispatches_the_identical_typed_terrain_command() {
        use neon_ui_schema::{UiFragmentRevision, UiIntent, UiPointerMetadata, UiSemanticEventType};

        let terrain_server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = terrain_server.local_addr().unwrap();
        let receiver = thread::spawn(move || {
            terrain_server.serve_one(|request| {
                assert_eq!(request.target.0, "terrain-runtime");
                assert_eq!(request.method, "terrain.tool.select");
                assert_eq!(request.params, json!({"tool": "water_inject"}));
                assert_eq!(request.expected_revision, Some(Revision(3)));
                assert!(request.idempotency_key.is_some());
                RpcResponse { request_id: request.request_id, status: RpcStatus::Accepted, revision: Some(Revision(4)), result: Some(json!({"mode": "water_paint"})), snapshot: None, error: None }
            })
        });
        let mut runtime = UiRuntime::new(7, "ui-test");
        runtime.cached_fragment = Some(runtime.static_fragment(Revision(3)));
        let event = UiSemanticEvent {
            event: UiSemanticEventType::PointerClick,
            event_id: "event-1".into(), renderer_epoch: 7, composition_revision: Revision(9),
            fragment: UiFragmentRevision { id: UiFragmentId("static-editor-shell".into()), revision: Revision(3) },
            intent: UiIntent::Invoke { action: "terrain.tool.select".into(), params: json!({"tool": "water_inject"}) },
            pointer: Some(UiPointerMetadata { id: 0, sequence: 1 }), focus: None,
        };
        let response = runtime.dispatch_semantic_event(endpoint, event, RequestId("terrain-request-1".into()), "terrain-key-1".into()).unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.command_receipt(&RequestId("terrain-request-1".into())).unwrap().state, CommandState::Accepted);
        assert_eq!(runtime.traces(&JournalFilter { request_id: Some(RequestId("terrain-request-1".into())), ..JournalFilter::default() }).len(), 2);
        receiver.join().unwrap().unwrap();
    }

    #[test]
    fn semantic_event_rejection_codes_are_explicit() {
        use neon_ui_schema::{UiFragmentRevision, UiIntent, UiPointerMetadata, UiSemanticEventType};
        let mut runtime = UiRuntime::new(7, "ui-test");
        runtime.cached_fragment = Some(runtime.static_fragment(Revision(3)));
        let event = |epoch, revision, action: &str, sequence| UiSemanticEvent {
            event: UiSemanticEventType::PointerClick, event_id: "event-reject".into(), renderer_epoch: epoch, composition_revision: Revision(9),
            fragment: UiFragmentRevision { id: UiFragmentId("static-editor-shell".into()), revision: Revision(revision) },
            intent: UiIntent::Invoke { action: action.into(), params: json!({"tool": "water_inject"}) },
            pointer: Some(UiPointerMetadata { id: 0, sequence }), focus: None,
        };
        assert_eq!(runtime.validate_semantic_event(&event(8, 3, "terrain.tool.select", 1)), Err(ERROR_RENDERER_EPOCH_MISMATCH));
        assert_eq!(runtime.validate_semantic_event(&event(7, 2, "terrain.tool.select", 1)), Err(ERROR_FRAGMENT_REVISION_STALE));
        assert_eq!(runtime.validate_semantic_event(&event(7, 3, "terrain.tool.invalid", 1)), Err(ERROR_INTENT_NOT_BOUND));
        runtime.validate_semantic_event(&event(7, 3, "terrain.tool.select", 2)).unwrap();
        assert_eq!(runtime.validate_semantic_event(&event(7, 3, "terrain.tool.select", 2)), Err(ERROR_INPUT_SEQUENCE_STALE));
    }
}
