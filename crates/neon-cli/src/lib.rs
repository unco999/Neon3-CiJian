//! Public protocol client helpers. This crate must not create windows or GPU objects.

use std::net::SocketAddr;

use neon_ipc::{RpcClient, TransportError};
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_schema::{
    UiBounds, UiCommand, UiEffect, UiFragment, UiFragmentId, UiFragmentRevision, UiFragmentSubmission, UiIntent, UiNode, UiNodeId, UiNodeKind, UiPointerMetadata, UiSemanticEvent, UiSemanticEventType, UiStyle, TextRef,
};
use serde_json::{Value, json};

pub const SCENARIO_ID: &str = "ui.static-fragment.submit.v1";
pub const DETAIL_TOGGLE_SCENARIO_ID: &str = "ui.detail-toggle.v1";

pub fn run_headless_scenario(endpoint: SocketAddr) -> Result<Value, TransportError> {
    let mut steps = Vec::new();
    let health = call(endpoint, "health-1", "service.health", json!({}), None)?;
    record_step(&mut steps, "service.health", &health);
    if health.status != RpcStatus::Accepted
        || health.result.as_ref().and_then(|value| value.get("status")) != Some(&json!("healthy"))
    {
        return Ok(failed(steps, &health));
    }

    let describe = call(endpoint, "describe-1", "service.describe", json!({}), None)?;
    record_step(&mut steps, "service.describe", &describe);
    if describe.status != RpcStatus::Accepted {
        return Ok(failed(steps, &describe));
    }

    let command = UiCommand::SubmitFragment {
        submission: UiFragmentSubmission::new(static_fragment(Revision(1))),
    };
    let submit = call(
        endpoint,
        "submit-1",
        "wgpu.ui.submit_fragment",
        json!(command),
        Some("submit-key-1"),
    )?;
    record_step(&mut steps, "wgpu.ui.submit_fragment", &submit);
    if submit.status != RpcStatus::Accepted {
        return Ok(failed(steps, &submit));
    }

    let duplicate = call(
        endpoint,
        "submit-duplicate-1",
        "wgpu.ui.submit_fragment",
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(static_fragment(Revision(2)))
        }),
        Some("submit-key-1"),
    )?;
    record_step(&mut steps, "wgpu.ui.submit_fragment.retry", &duplicate);
    if duplicate.status != RpcStatus::Accepted {
        return Ok(failed(steps, &duplicate));
    }

    let diagnostics = call(
        endpoint,
        "diagnostics-1",
        "wgpu.render.diagnostics",
        json!({}),
        None,
    )?;
    record_step(&mut steps, "wgpu.render.diagnostics", &diagnostics);
    if diagnostics.status != RpcStatus::Accepted
        || diagnostics
            .result
            .as_ref()
            .and_then(|value| value.get("fragment_count"))
            != Some(&json!(1))
    {
        return Ok(failed(steps, &diagnostics));
    }

    let receipt = call(
        endpoint,
        "receipt-1",
        "debug.command.get",
        json!({"request_id": "submit-1"}),
        None,
    )?;
    record_step(&mut steps, "debug.command.get", &receipt);
    if receipt.status != RpcStatus::Accepted {
        return Ok(failed(steps, &receipt));
    }

    let traces = call(
        endpoint,
        "traces-1",
        "debug.trace.query",
        json!({"request_id": "submit-1"}),
        None,
    )?;
    record_step(&mut steps, "debug.trace.query", &traces);
    if traces.status != RpcStatus::Accepted
        || traces
            .result
            .as_ref()
            .is_none_or(|records| records.as_array().is_none_or(Vec::is_empty))
    {
        return Ok(failed(steps, &traces));
    }

    Ok(json!({
        "scenario": SCENARIO_ID,
        "status": "passed",
        "steps": steps,
        "request_ids": ["health-1", "describe-1", "submit-1", "submit-duplicate-1", "diagnostics-1", "receipt-1", "traces-1"],
        "trace_records": traces.result,
        "diagnostics": diagnostics.result,
    }))
}

/// Headless service-level acceptance scenario for a declarative button-driven content update.
/// The CLI submits semantic data only; it never supplies a render hit ID or screen coordinate.
pub fn run_detail_toggle_scenario(endpoint: SocketAddr) -> Result<Value, TransportError> {
    let mut steps = Vec::new();
    let health = call(endpoint, "detail-health-1", "service.health", json!({}), None)?;
    record_step(&mut steps, "service.health", &health);
    if health.status != RpcStatus::Accepted { return Ok(failed_detail(steps, &health)); }
    let initial = call(endpoint, "detail-submit-1", "wgpu.ui.submit_fragment", json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(detail_fragment(Revision(1), false)) }), Some("detail-submit-key-1"))?;
    record_step(&mut steps, "wgpu.ui.submit_fragment.initial", &initial);
    if initial.status != RpcStatus::Accepted { return Ok(failed_detail(steps, &initial)); }
    let intent = UiIntent::Invoke { action: "ui.detail.toggle".into(), params: json!({"section": "inspector"}) };
    let event = UiSemanticEvent {
        event: UiSemanticEventType::PointerClick,
        event_id: "detail-toggle-event-1".into(),
        renderer_epoch: 1,
        composition_revision: initial.revision.unwrap_or(Revision(0)),
        fragment: UiFragmentRevision { id: UiFragmentId("cli-detail-toggle".into()), revision: Revision(1) },
        intent,
        pointer: Some(UiPointerMetadata { id: 0, sequence: 1, logical_position: [208.0, 32.0] }),
        focus: None,
    };
    let validated = call(endpoint, "detail-event-1", "wgpu.ui.semantic_event.validate", json!(event), None)?;
    record_step(&mut steps, "wgpu.ui.semantic_event.validate", &validated);
    if validated.status != RpcStatus::Accepted { return Ok(failed_detail(steps, &validated)); }
    let updated = call(endpoint, "detail-submit-2", "wgpu.ui.submit_fragment", json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(detail_fragment(Revision(2), true)) }), Some("detail-submit-key-2"))?;
    record_step(&mut steps, "wgpu.ui.submit_fragment.updated", &updated);
    if updated.status != RpcStatus::Accepted { return Ok(failed_detail(steps, &updated)); }
    let diagnostics = call(endpoint, "detail-diagnostics-1", "wgpu.render.diagnostics", json!({}), None)?;
    record_step(&mut steps, "wgpu.render.diagnostics", &diagnostics);
    if diagnostics.status != RpcStatus::Accepted || diagnostics.result.as_ref().and_then(|value| value.get("fragment_count")) != Some(&json!(1)) { return Ok(failed_detail(steps, &diagnostics)); }
    Ok(json!({
        "scenario": DETAIL_TOGGLE_SCENARIO_ID,
        "status": "passed",
        "acceptance_level": "service-ready",
        "steps": steps,
        "request_ids": ["detail-health-1", "detail-submit-1", "detail-event-1", "detail-submit-2", "detail-diagnostics-1"],
        "transition": {"from_fragment_revision": 1, "to_fragment_revision": 2, "intent": "ui.detail.toggle", "lower_content": "Inspector details are now visible."},
        "diagnostics": diagnostics.result,
    }))
}

pub fn detail_fragment(revision: Revision, detail_visible: bool) -> UiFragment {
    let lower_text = if detail_visible { "Inspector details are now visible." } else { "Select Show details to inspect this item." };
    UiFragment {
        fragment_id: UiFragmentId("cli-detail-toggle".into()), revision,
        root: UiNode { node_id: UiNodeId("editor-shell".into()), kind: UiNodeKind::Panel, bounds: UiBounds { x: 24.0, y: 24.0, width: 420.0, height: 240.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, style: UiStyle::default(), enter_transition: None,
            children: vec![
                UiNode { node_id: UiNodeId("title".into()), kind: UiNodeKind::Label, bounds: UiBounds { x: 20.0, y: 18.0, width: 220.0, height: 28.0 }, layout: None, visible: true, enabled: true, text_key: None, text: Some(TextRef::Literal { value: "Terrain Inspector".into() }), image: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
                UiNode { node_id: UiNodeId("show-details".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 250.0, y: 16.0, width: 145.0, height: 34.0 }, layout: None, visible: true, enabled: true, text_key: None, text: Some(TextRef::Literal { value: "Show details".into() }), image: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
                UiNode { node_id: UiNodeId("detail-region".into()), kind: UiNodeKind::Label, bounds: UiBounds { x: 20.0, y: 82.0, width: 376.0, height: 120.0 }, layout: None, visible: true, enabled: true, text_key: None, text: Some(TextRef::Literal { value: lower_text.into() }), image: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
            ] },
        effects: vec![UiEffect::SemanticIntent { intent: UiIntent::Invoke { action: "ui.detail.toggle".into(), params: json!({"section": "inspector"}) } }],
    }
}

pub fn static_fragment(revision: Revision) -> UiFragment {
    UiFragment {
        fragment_id: UiFragmentId("cli-static-fragment".into()),
        revision,
        root: UiNode {
            node_id: UiNodeId("cli-root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            style: UiStyle::default(),
            enter_transition: None,
            children: Vec::new(),
        },
        effects: vec![UiEffect::SemanticAction {
            action: "ui.static.ready".into(),
        }],
    }
}

fn call(
    endpoint: SocketAddr,
    request_id: &str,
    method: &str,
    params: Value,
    idempotency_key: Option<&str>,
) -> Result<RpcResponse, TransportError> {
    let request = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(request_id.into()),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "neon-cli-headless".into(),
            pid: std::process::id(),
            origin: "neon-cli".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: idempotency_key.map(str::to_owned),
    };
    let mut client = RpcClient::connect(endpoint)?;
    client.call(&request)
}

fn record_step(steps: &mut Vec<Value>, method: &str, response: &RpcResponse) {
    steps.push(json!({
        "method": method,
        "target": "wgpu-runtime",
        "status": response.status,
        "request_id": response.request_id,
        "revision": response.revision,
        "error": response.error.as_ref().map(|error| &error.code),
    }));
}

fn failed(steps: Vec<Value>, response: &RpcResponse) -> Value {
    json!({
        "scenario": SCENARIO_ID,
        "status": "failed",
        "steps": steps,
        "error": response
            .error
            .as_ref()
            .map_or("unexpected_response", |error| error.code.as_str()),
    })
}

fn failed_detail(steps: Vec<Value>, response: &RpcResponse) -> Value {
    json!({"scenario": DETAIL_TOGGLE_SCENARIO_ID, "status": "failed", "steps": steps, "error": response.error.as_ref().map_or("unexpected_response", |error| error.code.as_str())})
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_ipc::RpcServer;
    use neon_protocol::RpcError;
    use std::thread;

    fn response(request: RpcRequest) -> RpcResponse {
        let result = match request.method.as_str() {
            "service.health" => json!({"status": "healthy"}),
            "service.describe" => json!({"epoch": 1, "capabilities": ["wgpu.ui.fragment.v1"]}),
            "wgpu.ui.submit_fragment" => json!({"fragment_count": 1}),
            "wgpu.ui.semantic_event.validate" => request.params,
            "wgpu.render.diagnostics" => json!({"fragment_count": 1, "mode": "headless"}),
            "debug.command.get" => json!({"state": "accepted"}),
            "debug.trace.query" => json!([{ "event": "command.accepted" }]),
            _ => json!({}),
        };
        RpcResponse {
            request_id: request.request_id,
            status: RpcStatus::Accepted,
            revision: Some(Revision(1)),
            result: Some(result),
            snapshot: None,
            error: None,
        }
    }

    #[test]
    fn scenario_outputs_parseable_success_json() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || {
            for _ in 0..7 {
                server.serve_one(response).unwrap();
            }
        });
        let outcome = run_headless_scenario(endpoint).unwrap();
        assert_eq!(outcome["status"], "passed");
        assert_eq!(outcome["steps"].as_array().unwrap().len(), 7);
        serde_json::from_value::<Value>(outcome).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn server_rejection_surfaces_stable_error_code() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || {
            server
                .serve_one(|request| RpcResponse {
                    request_id: request.request_id,
                    status: RpcStatus::Rejected,
                    revision: Some(Revision(1)),
                    result: None,
                    snapshot: None,
                    error: Some(RpcError {
                        code: "revision_conflict".into(),
                        message: "stale".into(),
                        current_revision: Some(Revision(1)),
                        object_id: None,
                    }),
                })
                .unwrap();
        });
        let outcome = run_headless_scenario(endpoint).unwrap();
        assert_eq!(outcome["status"], "failed");
        assert_eq!(outcome["error"], "revision_conflict");
        thread.join().unwrap();
    }

    #[test]
    fn detail_toggle_scenario_outputs_revisioned_content_transition() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || for _ in 0..5 { server.serve_one(response).unwrap(); });
        let outcome = run_detail_toggle_scenario(endpoint).unwrap();
        assert_eq!(outcome["status"], "passed");
        assert_eq!(outcome["transition"]["from_fragment_revision"], 1);
        assert_eq!(outcome["transition"]["to_fragment_revision"], 2);
        assert_eq!(detail_fragment(Revision(2), true).root.children[2].text, Some(TextRef::Literal { value: "Inspector details are now visible.".into() }));
        thread.join().unwrap();
    }
}
