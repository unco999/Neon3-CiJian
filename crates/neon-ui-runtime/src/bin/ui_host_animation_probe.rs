use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_runtime::{UiHostAdapterConfig, UiRuntime, compile_nui_flow_program, parse_nui_flow};
use neon_ui_schema::{
    UiCommand, UiHostInbound, UiHostPublication, UiInputFrame, UiProgramCapability,
    UiProgramCapabilityOwner, UiProgramCapabilityStatus, UiProgramRevision, UiProgramSemanticEvent,
    UiProgramSemanticEventKind, UiSemanticInteractionMetadata,
};
use serde_json::{Value, json};

const SOURCE: &str = r#"version 1
surface surface.multi-panel revision 1
flow multi-panel
motion left-expand duration 140 easing ease_out
motion right-expand duration 220 easing ease_in_out
machine left initial compact
state left expanded
on left multi.left.activate -> expanded
transition left compact -> expanded motion left-expand
style left.compact.left-panel x 0 y 0 w 240 h 120
style left.expanded.left-panel x 0 y 0 w 360 h 180
machine right initial compact
state right expanded
on right multi.right.activate -> expanded
transition right compact -> expanded motion right-expand
style right.compact.right-panel x 260 y 0 w 240 h 120
style right.expanded.right-panel x 260 y 0 w 420 h 220
surface multi-panel row gap 20
  panel left-panel w 240 h 120
    button left-button value "Left" event multi.left.activate
  panel right-panel w 240 h 120
    button right-button value "Right" event multi.right.activate
"#;

fn client_identity(kind: ClientKind, instance_id: &str) -> ClientIdentity {
    ClientIdentity {
        kind,
        instance_id: instance_id.into(),
        pid: std::process::id(),
        origin: "neon-ui-animation-probe".into(),
    }
}

fn response(request: &RpcRequest, result: Value) -> RpcResponse {
    RpcResponse {
        request_id: request.request_id.clone(),
        status: RpcStatus::Accepted,
        revision: Some(Revision(1)),
        result: Some(result),
        snapshot: None,
        error: None,
    }
}

fn request(method: &str, request_id: &str, params: Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: PROTOCOL_VERSION,
        request_id: RequestId(request_id.into()),
        client: client_identity(ClientKind::Cli, "animation-probe-client"),
        target: ServiceName("ui-runtime".into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(format!("probe:{request_id}")),
    }
}

fn capability(name: &str) -> UiProgramCapability {
    UiProgramCapability {
        name: name.into(),
        version: 1,
        owner: UiProgramCapabilityOwner::SharedContract,
        status: UiProgramCapabilityStatus::Supported,
    }
}

fn has_transition(node: &neon_ui_schema::UiNode, key: &str) -> bool {
    (node.node_id.0 == key && node.enter_transition.is_some())
        || node.children.iter().any(|child| has_transition(child, key))
}

fn call_rpc(
    endpoint: SocketAddr,
    request: &RpcRequest,
    timeout: Duration,
) -> Result<RpcResponse, Box<dyn std::error::Error>> {
    let mut client = RpcClient::connect(endpoint)?.with_timeout(timeout)?;
    Ok(client.call(request)?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let document = parse_nui_flow(SOURCE).map_err(|error| format!("parse failed: {error:?}"))?;
    let program_revision = UiProgramRevision {
        program_id: document.ir.surface_id.0.clone(),
        revision: Revision(1),
        schema_version: neon_ui_schema::UI_PROGRAM_SCHEMA_VERSION,
        capabilities: [
            neon_ui_schema::UI_PROGRAM_CAPABILITY_NAME,
            neon_ui_schema::UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
            neon_ui_schema::UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
            neon_ui_schema::UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
        ]
        .into_iter()
        .map(capability)
        .collect(),
    };
    let program = compile_nui_flow_program(&document, program_revision.clone())
        .map_err(|error| format!("compile failed: {error:?}"))?;
    let (events, receive) = mpsc::channel::<Value>();

    let renderer = RpcServer::bind("127.0.0.1:0".parse::<SocketAddr>()?)?;
    let renderer_endpoint = renderer.local_addr()?;
    let renderer_events = events.clone();
    let renderer_thread = thread::spawn(move || {
        let mut frame_sequence = 0_u64;
        let result = renderer.serve_until(|request| {
            frame_sequence += 1;
            let command = serde_json::from_value::<UiCommand>(request.params.clone());
            let (revision, left_motion, right_motion) = match command {
                Ok(UiCommand::SubmitFragment { submission }) => (
                    submission.fragment.revision,
                    has_transition(&submission.fragment.root, "left-panel"),
                    has_transition(&submission.fragment.root, "right-panel"),
                ),
                Ok(UiCommand::RemoveFragment { .. }) | Err(_) => (Revision(0), false, false),
            };
            let _ = renderer_events.send(json!({
                "kind": "renderer.fragment",
                "frame_sequence": frame_sequence,
                "request_id": request.request_id,
                "fragment_revision": revision,
                "has_left_transition": left_motion,
                "has_right_transition": right_motion,
            }));
            let keep_serving = revision != Revision(5);
            (
                response(&request, json!({"fragment_revision": revision})),
                keep_serving,
            )
        });
        result.map_err(|error| error.to_string())
    });

    let host = RpcServer::bind("127.0.0.1:0".parse::<SocketAddr>()?)?;
    let host_endpoint = host.local_addr()?;
    let host_program = program.clone();
    let host_schema = document.input_schema.clone();
    let host_events = events.clone();
    let host_thread = thread::spawn(move || {
        let mut sequence = 0_u64;
        let result = host.serve_until(|request| {
            sequence += 1;
            let _ = host_events.send(json!({
                "kind": "host.request",
                "sequence": sequence,
                "request_id": request.request_id,
                "method": request.method,
            }));
            if request.method == "ui.host.adapter.get" {
                return (
                    response(
                        &request,
                        json!(UiHostAdapterConfig {
                            program: host_program.clone(),
                            input_schema: host_schema.clone(),
                        }),
                    ),
                    true,
                );
            }
            std::thread::sleep(Duration::from_millis(250));
            let publication = UiHostPublication {
                scalar_frame: UiInputFrame {
                    program_revision: host_program.revision.clone(),
                    expected_input_revision: Revision(0),
                    request_id: "probe-publication".into(),
                    idempotency_key: "probe-publication".into(),
                    changes: Vec::new(),
                },
                grid_inputs: Vec::new(),
                presentation_update: None,
            };
            (response(&request, json!(publication)), sequence < 2)
        });
        result.map_err(|error| error.to_string())
    });

    let ui = RpcServer::bind("127.0.0.1:0".parse::<SocketAddr>()?)?;
    let ui_endpoint = ui.local_addr()?;
    drop(ui);
    let ui_thread = thread::spawn(move || {
        UiRuntime::serve_forwarder(ui_endpoint, renderer_endpoint, host_endpoint, None, 7)
            .map_err(|error| error.to_string())
    });

    let timeout = Duration::from_secs(2);
    let probe_started = Instant::now();
    let identity = client_identity(ClientKind::Cli, "animation-probe-client");
    let mut flow_request = request(
        "ui.flow.submit",
        "probe-flow-submit",
        json!({"source": SOURCE}),
    );
    flow_request.client = identity.clone();
    let flow_response = call_rpc(ui_endpoint, &flow_request, timeout)?;
    let flow_elapsed_ms = probe_started.elapsed().as_millis();
    println!(
        "{}",
        json!({
            "kind": "ui.response",
            "sequence": 1,
            "request_id": flow_response.request_id,
            "method": "ui.flow.submit",
            "status": flow_response.status,
            "revision": flow_response.revision,
            "elapsed_ms": flow_elapsed_ms,
        })
    );

    let make_event = |event_id: &str, intent: &str, source_node_key: &str, sequence: u64| {
        UiProgramSemanticEvent {
            event_id: event_id.into(),
            kind: UiProgramSemanticEventKind::Activate,
            intent: intent.into(),
            source_node_key: source_node_key.into(),
            payload: BTreeMap::new(),
            program_revision: program.revision.clone(),
            input_revision: Revision(0),
            request_id: event_id.into(),
            idempotency_key: event_id.into(),
            requested_value: None,
            interaction: UiSemanticInteractionMetadata {
                interaction_id: format!("{event_id}-interaction"),
                sequence,
                renderer_epoch: 7,
            },
        }
    };
    let first_request = request(
        "ui.host.inbound",
        "probe-left-inbound",
        json!(UiHostInbound::SemanticIntent {
            event: make_event("probe-left-event", "multi.left.activate", "left-button", 1)
        }),
    );
    let first_response = call_rpc(ui_endpoint, &first_request, timeout)
        .map_err(|error| format!("ui.host.inbound call failed: {error}"))?;
    let first_elapsed_ms = probe_started.elapsed().as_millis();
    println!(
        "{}",
        json!({
            "kind": "ui.response",
            "sequence": 2,
            "request_id": first_response.request_id,
            "method": "ui.host.inbound",
            "status": first_response.status,
            "revision": first_response.revision,
            "error": first_response.error,
            "elapsed_ms": probe_started.elapsed().as_millis(),
            "optimistic_return": true,
        })
    );

    let second_request = request(
        "ui.host.inbound",
        "probe-right-inbound",
        json!(UiHostInbound::SemanticIntent {
            event: make_event(
                "probe-right-event",
                "multi.right.activate",
                "right-button",
                2
            )
        }),
    );
    let second_response = call_rpc(ui_endpoint, &second_request, timeout)
        .map_err(|error| format!("second ui.host.inbound call failed: {error}"))?;
    let second_elapsed_ms = probe_started.elapsed().as_millis();
    println!(
        "{}",
        json!({
            "kind": "ui.response",
            "sequence": 3,
            "request_id": second_response.request_id,
            "method": "ui.host.inbound",
            "status": second_response.status,
            "revision": second_response.revision,
            "error": second_response.error,
            "elapsed_ms": probe_started.elapsed().as_millis(),
            "optimistic_return": true,
        })
    );

    let shutdown = request("service.shutdown", "probe-shutdown", json!({}));
    let shutdown_response = call_rpc(ui_endpoint, &shutdown, timeout)?;
    println!(
        "{}",
        json!({
            "kind": "ui.response",
            "sequence": 4,
            "request_id": shutdown_response.request_id,
            "method": "service.shutdown",
            "status": shutdown_response.status,
            "elapsed_ms": probe_started.elapsed().as_millis(),
        })
    );

    let ui_result = ui_thread.join().map_err(|_| "ui thread panicked")?;
    let renderer_result = renderer_thread
        .join()
        .map_err(|_| "renderer thread panicked")?;
    let host_result = host_thread.join().map_err(|_| "host thread panicked")?;
    let mut records = Vec::new();
    while let Ok(record) = receive.try_recv() {
        println!("{}", record);
        records.push(record);
    }

    let renderer_revisions = records
        .iter()
        .filter(|record| record["kind"] == "renderer.fragment")
        .filter_map(|record| record["fragment_revision"].as_u64())
        .collect::<Vec<_>>();
    let final_pass = ui_result.is_ok()
        && renderer_result.is_ok()
        && host_result.is_ok()
        && second_elapsed_ms < 250
        && renderer_revisions == vec![1, 2, 3, 4, 5]
        && records.iter().any(|record| {
            record["kind"] == "renderer.fragment"
                && record["fragment_revision"] == 2
                && record["has_left_transition"] == true
        })
        && records.iter().any(|record| {
            record["kind"] == "renderer.fragment"
                && record["fragment_revision"] == 4
                && record["has_right_transition"] == true
        });
    println!(
        "{}",
        json!({
            "kind": "probe.result",
            "input": {"flow": "multi-panel-sequential-inline", "intents": ["multi.left.activate", "multi.right.activate"], "host_delay_ms": 250},
            "second_response_elapsed_ms": second_elapsed_ms,
            "first_response_elapsed_ms": first_elapsed_ms,
            "frame_sequence": renderer_revisions,
            "status": if final_pass { "passed" } else { "failed" },
        })
    );
    if final_pass {
        Ok(())
    } else {
        Err("animation probe failed".into())
    }
}
