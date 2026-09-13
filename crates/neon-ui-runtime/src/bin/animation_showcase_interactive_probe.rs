//! End-to-end interactive animation showcase probe.
//!
//! This is intentionally a protocol client, not a second UI implementation.
//! It starts the real UI and WGPU runtimes, submits the checked-in NUI Flow
//! case, activates its declared buttons through the renderer's debug semantic
//! target endpoint, and verifies the state-machine -> motion -> renderer chain.
//! Every observation is JSONL and the process exit code reflects the result.

use std::{
    net::SocketAddr,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_runtime::{
    UiRuntime, compile_nui_flow_program, parse_nui_flow,
};
use neon_ui_schema::{
    UI_CANVAS_POINTS_LINES_CAPABILITY_NAME,
    UI_COMPONENT_SKIN_CAPABILITY_NAME, UI_NINE_SLICE_CAPABILITY_NAME,
    UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_CAPABILITY_NAME,
    UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION,
    UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME, UiHostInbound, UiHostPublication, UiInputFrame,
    UiInputSchema, UiProgram, UiProgramCapability, UiProgramCapabilityOwner,
    UiProgramCapabilityStatus, UiProgramRevision,
};
use serde_json::{Value, json};

const WGPU_ENDPOINT: &str = "127.0.0.1:39470";
const UI_ENDPOINT: &str = "127.0.0.1:39471";
const NUI_SOURCE: &str = include_str!("../../../../cases/animation-showcase/animation.nui");
const RUNTIME_BINARY: &str = "neon-wgpu-runtime.exe";
const RPC_TIMEOUT: Duration = Duration::from_secs(3);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const STEP_TIMEOUT: Duration = Duration::from_secs(4);
const UPDATE_SETTLE: Duration = Duration::from_millis(40);

#[derive(Clone, Debug)]
struct HostObservation {
    request_id: String,
    action: String,
    source_node_key: String,
    interaction_id: String,
    sequence: u64,
}

#[derive(Clone, Debug)]
struct Step {
    button: &'static str,
    machine: &'static str,
    expected_state: &'static str,
    panel: &'static str,
}

fn emit(record: Value) {
    println!("{}", serde_json::to_string(&record).expect("probe record serializes"));
}

fn identity() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "animation-showcase-interactive-probe".into(),
        pid: std::process::id(),
        origin: "animation-showcase-interactive-probe".into(),
    }
}

fn request(target: &str, method: &str, sequence: u64, params: Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("animation-showcase-{target}-{method}-{sequence}")),
        client: identity(),
        target: ServiceName(target.into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(format!("animation-showcase-{target}-{method}-{sequence}")),
    }
}

fn call(endpoint: SocketAddr, request: RpcRequest) -> Result<RpcResponse, String> {
    let mut client = RpcClient::connect(endpoint)
        .map_err(|error| format!("connect {endpoint}: {error}"))?
        .with_timeout(RPC_TIMEOUT)
        .map_err(|error| format!("configure RPC timeout: {error}"))?;
    client.call(&request).map_err(|error| error.to_string())
}

fn call_result(
    endpoint: SocketAddr,
    target: &str,
    method: &str,
    sequence: u64,
    params: Value,
) -> Result<Value, String> {
    call_result_with_timeout(endpoint, target, method, sequence, params, RPC_TIMEOUT)
}

fn call_result_with_timeout(
    endpoint: SocketAddr,
    target: &str,
    method: &str,
    sequence: u64,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let mut client = RpcClient::connect(endpoint)
        .map_err(|error| format!("connect {endpoint}: {error}"))?
        .with_timeout(timeout)
        .map_err(|error| format!("configure RPC timeout: {error}"))?;
    let response = client
        .call(&request(target, method, sequence, params))
        .map_err(|error| error.to_string())?;
    if response.status != RpcStatus::Accepted {
        return Err(format!(
            "{target}.{method} rejected: {:?}",
            response.error
        ));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn launch_runtime() -> Result<Child, String> {
    let binary = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .with_file_name(RUNTIME_BINARY);
    if !binary.is_file() {
        return Err(format!("runtime binary is missing: {}", binary.display()));
    }
    Command::new(binary)
        .args(["--window-server", WGPU_ENDPOINT, UI_ENDPOINT])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn WGPU runtime: {error}"))
}

fn program_revision(surface_id: &str) -> UiProgramRevision {
    UiProgramRevision {
        program_id: surface_id.into(),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: [
            UI_PROGRAM_CAPABILITY_NAME,
            UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
            UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
            UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
            UI_NINE_SLICE_CAPABILITY_NAME,
            UI_COMPONENT_SKIN_CAPABILITY_NAME,
            UI_CANVAS_POINTS_LINES_CAPABILITY_NAME,
        ]
        .into_iter()
        .map(|name| UiProgramCapability {
            name: name.into(),
            version: 1,
            owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        })
        .collect(),
    }
}

fn accepted(request: &RpcRequest, result: Value, revision: Revision) -> RpcResponse {
    RpcResponse {
        request_id: request.request_id.clone(),
        status: RpcStatus::Accepted,
        revision: Some(revision),
        result: Some(result),
        snapshot: None,
        error: None,
    }
}

fn start_host(
    host_events: Sender<HostObservation>,
    program: UiProgram,
    input_schema: UiInputSchema,
) -> Result<(SocketAddr, thread::JoinHandle<Result<(), String>>), String> {
    let server = RpcServer::bind("127.0.0.1:0".parse().expect("loopback address"))
        .map_err(|error| format!("bind probe host: {error}"))?;
    let endpoint = server
        .local_addr()
        .map_err(|error| format!("read probe host endpoint: {error}"))?;
    let thread = thread::spawn(move || {
        let host_program = program;
        let host_schema = input_schema;
        let result = server.serve_until(|request| {
            if request.method == "service.shutdown" {
                return (accepted(&request, json!({"state": "accepted"}), Revision(0)), false);
            }
            if request.method == "ui.host.inbound" {
                let inbound = serde_json::from_value::<UiHostInbound>(request.params.clone());
                let observation = inbound.as_ref().ok().and_then(|inbound| match inbound {
                    UiHostInbound::SemanticIntent { event } => {
                        Some(HostObservation {
                            request_id: request.request_id.0.clone(),
                            action: event.intent.clone(),
                            source_node_key: event.source_node_key.clone(),
                            interaction_id: event.interaction.interaction_id.clone(),
                            sequence: event.interaction.sequence,
                        })
                    }
                    _ => None,
                });
                if let Some(observation) = observation {
                    let _ = host_events.send(observation);
                }
                let publication = UiHostPublication {
                    scalar_frame: UiInputFrame {
                        program_revision: host_program.revision.clone(),
                        expected_input_revision: Revision(0),
                        request_id: request.request_id.0.clone(),
                        idempotency_key: request
                            .idempotency_key
                            .clone()
                            .unwrap_or_else(|| request.request_id.0.clone()),
                        changes: Vec::new(),
                    },
                    grid_inputs: Vec::new(),
                    presentation_update: None,
                };
                return (accepted(&request, json!(publication), Revision(0)), true);
            }
            if request.method == "ui.host.adapter.get" {
                // Flow submission normally activates the adapter in UI runtime;
                // this response keeps reconnection/restore probes deterministic.
                return (
                    accepted(
                        &request,
                        json!({"program": host_program.clone(), "input_schema": host_schema.clone()}),
                        Revision(0),
                    ),
                    true,
                );
            }
            (accepted(&request, json!({"state": "accepted"}), Revision(0)), true)
        });
        result.map_err(|error| error.to_string())
    });
    Ok((endpoint, thread))
}

fn wait_for_endpoint(endpoint: SocketAddr, target: &str, child: &mut Child) -> Result<(), String> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut sequence = 1;
    loop {
        match call_result(endpoint, target, "service.health", sequence, json!({})) {
            Ok(_) => return Ok(()),
            Err(error) if Instant::now() < deadline => {
                if child
                    .try_wait()
                    .map_err(|wait| wait.to_string())?
                    .is_some()
                {
                    return Err(format!("runtime exited during startup: {error}"));
                }
                sequence += 1;
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(format!("{target} health timeout: {error}")),
        }
    }
}

fn response_value(response: RpcResponse, label: &str) -> Result<Value, String> {
    if response.status != RpcStatus::Accepted {
        return Err(format!("{label} rejected: {:?}", response.error));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn ui_animation_snapshot(sequence: u64) -> Result<Value, String> {
    call_result(
        UI_ENDPOINT.parse().expect("fixed UI endpoint"),
        "ui-runtime",
        "debug.ui.animation.snapshot",
        10_000 + sequence,
        json!({}),
    )
}

fn wgpu_snapshot(sequence: u64) -> Result<Value, String> {
    call_result(
        WGPU_ENDPOINT.parse().expect("fixed WGPU endpoint"),
        "wgpu-runtime",
        "debug.snapshot.get",
        20_000 + sequence,
        json!({}),
    )
}

fn window_snapshot(snapshot: &Value) -> &Value {
    snapshot.get("window").unwrap_or(snapshot)
}

fn active_transition<'a>(snapshot: &'a Value, node_path: &str) -> Option<&'a Value> {
    window_snapshot(snapshot)
        .get("active_transitions")?
        .get("transitions")?
        .as_array()?
        .iter()
        .find(|transition| transition.get("node_key").and_then(Value::as_str) == Some(node_path))
}

fn last_transition<'a>(snapshot: &'a Value, machine: &str) -> Option<&'a Value> {
    snapshot
        .get("last_transitions")?
        .as_array()?
        .iter()
        .find(|transition| transition.get("machine_key").and_then(Value::as_str) == Some(machine))
}

fn state<'a>(snapshot: &'a Value, machine: &str) -> Option<&'a str> {
    snapshot
        .get("states")?
        .get(machine)?
        .as_str()
}

fn transition_id(snapshot: &Value, node_path: &str) -> u64 {
    active_transition(snapshot, node_path)
        .and_then(|transition| transition.get("transition_id"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn frame_pair(snapshot: &Value, node_path: &str) -> (Option<u64>, Option<u64>) {
    let transition = active_transition(snapshot, node_path);
    let producer = transition
        .and_then(|transition| transition.get("source_frame_sequence"))
        .and_then(Value::as_u64);
    let consumer = window_snapshot(snapshot)
        .get("active_transitions")
        .and_then(|active| active.get("frame"))
        .and_then(|frame| frame.get("frame_sequence"))
        .and_then(Value::as_u64);
    (producer, consumer)
}

fn host_event_for(
    receiver: &Receiver<HostObservation>,
    action: &str,
    deadline: Instant,
) -> Option<HostObservation> {
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(observation) if observation.action == action => return Some(observation),
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

fn wait_for_step(
    step: &Step,
    previous_transition_id: u64,
    host_events: &Receiver<HostObservation>,
    child: &mut Child,
    sequence: u64,
) -> Result<(Value, Value, HostObservation), String> {
    let action = match step.button {
        "btn-a-toggle" => "anim.a.toggle",
        "btn-a-collapse" => "anim.a.collapse",
        "btn-b-toggle" => "anim.b.toggle",
        "btn-b-show" => "anim.b.show",
        "btn-c-normal" => "anim.c.normal",
        "btn-c-warn" => "anim.c.warn",
        "btn-c-error" => "anim.c.error",
        "btn-d-toggle" => "anim.d.toggle",
        "btn-d-left" => "anim.d.left",
        "btn-e-toggle" => "anim.e.toggle",
        "btn-e-collapse" => "anim.e.collapse",
        "btn-f-next" => "anim.f.next",
        "btn-f-large" => "anim.f.large",
        "btn-g-toggle" => "anim.g.toggle",
        "btn-g-quiet" => "anim.g.toggle",
        "btn-h-toggle" => "anim.h.toggle",
        "btn-h-again" => "anim.h.toggle",
        other => return Err(format!("unknown probe button {other}")),
    };
    let button_path = format!("surface.anim-showcase/{}", step.button);
    let panel_path = format!("surface.anim-showcase/{}", step.panel);
    let activate = call_result(
        WGPU_ENDPOINT.parse().expect("fixed WGPU endpoint"),
        "wgpu-runtime",
        "debug.window.input.activate_target",
        30_000 + sequence,
        json!({"semantic_node_path": button_path}),
    )?;
    let deadline = Instant::now() + STEP_TIMEOUT;
    let host = host_event_for(host_events, action, deadline)
        .ok_or_else(|| format!("host did not receive {action} for {}", step.button))?;
    let mut ui = Value::Null;
    let mut wgpu = Value::Null;
    let mut render_seen = false;
    while Instant::now() < deadline {
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err(format!("WGPU process exited during {}", step.button));
        }
        ui = ui_animation_snapshot(sequence + 1).unwrap_or(Value::Null);
        wgpu = wgpu_snapshot(sequence + 1).unwrap_or(Value::Null);
        let state_matches = state(&ui, step.machine) == Some(step.expected_state);
        let current_transition_id = transition_id(&wgpu, &panel_path);
        render_seen = current_transition_id > previous_transition_id
            && active_transition(&wgpu, &panel_path).is_some();
        if state_matches && render_seen {
            break;
        }
        thread::sleep(Duration::from_millis(12));
    }
    let selected = last_transition(&ui, step.machine);
    let active = active_transition(&wgpu, &panel_path);
    let (producer_frame, consumer_frame) = frame_pair(&wgpu, &panel_path);
    let frame_matched = producer_frame
        .zip(consumer_frame)
        .is_some_and(|(producer, consumer)| consumer >= producer);
    let transition_matches = selected.is_some_and(|transition| {
        transition.get("target_state").and_then(Value::as_str) == Some(step.expected_state)
            && transition.get("motion_key").and_then(Value::as_str).is_some()
    });
    let process_alive = child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none();
    let pass = activate != Value::Null
        && state(&ui, step.machine) == Some(step.expected_state)
        && active.is_some()
        && render_seen
        && transition_matches
        && frame_matched
        && process_alive;
    emit(json!({
        "probe": "animation-showcase.interactive.v1",
        "sequence": sequence,
        "input": {
            "button": step.button,
            "semantic_intent": action,
            "machine": step.machine,
            "expected_state": step.expected_state,
            "panel": step.panel,
            "activation": activate,
        },
        "producer": {
            "ui_runtime": selected,
            "program_revision": ui.get("program_revision"),
            "state_revision": ui.get("state_revision"),
            "state": state(&ui, step.machine),
            "host_request_id": host.request_id,
            "source_node_key": host.source_node_key,
            "interaction_id": host.interaction_id,
            "interaction_sequence": host.sequence,
        },
        "consumer": {
            "renderer_transition": active,
            "render_status": if pass { "accepted" } else { "rejected" },
            "active_transition_id": transition_id(&wgpu, &panel_path),
        },
        "pairing": {
            "producer_frame": producer_frame,
            "consumer_frame": consumer_frame,
            "status": if frame_matched { "matched" } else { "mismatch" },
        },
        "result": if pass { "passed" } else { "failed" },
        "pass_result": pass,
    }));
    if pass {
        Ok((ui, wgpu, host))
    } else {
        Err(format!(
            "step {} failed: state={:?}, selected={selected:?}, active={active:?}, frame={producer_frame:?}->{consumer_frame:?}",
            step.button,
            state(&ui, step.machine),
        ))
    }
}

fn main() {
    let started = Instant::now();
    let manual = std::env::args().skip(1).any(|arg| arg == "--manual");
    let document = match parse_nui_flow(NUI_SOURCE) {
        Ok(document) => document,
        Err(error) => {
            emit(json!({
                "probe": "animation-showcase.interactive.v1",
                "sequence": 0,
                "result": "failed",
                "pass_result": false,
                "error": format!("NUI Flow parse failed: {error:?}"),
            }));
            std::process::exit(1);
        }
    };
    let surface_id = document.ir.surface_id.0.clone();
    let revision = program_revision(&surface_id);
    let program = match compile_nui_flow_program(&document, revision) {
        Ok(program) => program,
        Err(error) => {
            emit(json!({
                "probe": "animation-showcase.interactive.v1",
                "sequence": 0,
                "result": "failed",
                "pass_result": false,
                "error": format!("NUI Flow compile failed: {error:?}"),
            }));
            std::process::exit(1);
        }
    };
    let (host_events_tx, host_events_rx) = mpsc::channel();
    let (host_endpoint, host_thread) = match start_host(
        host_events_tx,
        program,
        document.input_schema.clone(),
    ) {
        Ok(value) => value,
        Err(error) => {
            emit(json!({
                "probe": "animation-showcase.interactive.v1",
                "sequence": 0,
                "result": "failed",
                "pass_result": false,
                "error": error,
            }));
            std::process::exit(1);
        }
    };

    let ui_endpoint: SocketAddr = UI_ENDPOINT.parse().expect("fixed UI endpoint");
    let wgpu_endpoint: SocketAddr = WGPU_ENDPOINT.parse().expect("fixed WGPU endpoint");
    let ui_thread = thread::spawn(move || {
        UiRuntime::serve_forwarder(ui_endpoint, wgpu_endpoint, host_endpoint, None, 1)
            .map_err(|error| error.to_string())
    });
    let mut child = match launch_runtime() {
        Ok(child) => child,
        Err(error) => {
            emit(json!({
                "probe": "animation-showcase.interactive.v1",
                "sequence": 0,
                "result": "failed",
                "pass_result": false,
                "error": error,
            }));
            let _ = call(host_endpoint, request("ui-host", "service.shutdown", 90_000, json!({})));
            let _ = host_thread.join();
            let _ = ui_thread.join();
            std::process::exit(1);
        }
    };

    let run_result = (|| -> Result<(), String> {
        wait_for_endpoint(wgpu_endpoint, "wgpu-runtime", &mut child)?;
        wait_for_endpoint(ui_endpoint, "ui-runtime", &mut child)?;
        let flow_response = call(
            ui_endpoint,
            request(
                "ui-runtime",
                "ui.flow.submit",
                1,
                json!({"source": NUI_SOURCE}),
            ),
        )?;
        let flow_result = response_value(flow_response, "ui.flow.submit")?;
        emit(json!({
            "probe": "animation-showcase.interactive.v1",
            "sequence": 1,
            "input": {"surface_id": surface_id, "buttons": 17, "case": "animation-showcase"},
            "producer": {"ui_runtime": flow_result, "program_revision": 1},
            "consumer": {"renderer": "flow forwarded to WGPU"},
            "pairing": {"status": "ready"},
            "result": "passed",
            "pass_result": true,
        }));
        if manual {
            emit(json!({
                "probe": "animation-showcase.interactive.v1",
                "sequence": 3,
                "input": {"mode": "manual"},
                "producer": {"ui_runtime": ui_animation_snapshot(3).ok()},
                "consumer": {"window": window_snapshot(&wgpu_snapshot(3)?)},
                "pairing": {"status": "ready"},
                "result": "waiting_for_manual_input",
                "pass_result": true,
            }));
            println!("Animation showcase window is ready. Click the buttons to test it manually; it will stay open for up to 10 minutes.");
            thread::sleep(Duration::from_secs(600));
            return Ok(());
        }
        let startup_deadline = Instant::now() + STARTUP_TIMEOUT;
        while Instant::now() < startup_deadline {
            if let Ok(snapshot) = wgpu_snapshot(2)
                && window_snapshot(&snapshot).get("state").and_then(Value::as_str) != Some("uninitialized")
            {
                break;
            }
            thread::sleep(Duration::from_millis(80));
        }
        let capture_path = r"D:\Neon3\target\animation-showcase-interactive.png";
        let capture = call_result_with_timeout(
            wgpu_endpoint,
            "wgpu-runtime",
            "wgpu.render.target.capture",
            2_500,
            json!({
                "target": "ui.color.v1",
                "path": capture_path,
                "redraw": true,
            }),
            Duration::from_secs(35),
        )?;
        emit(json!({
            "probe": "animation-showcase.interactive.v1",
            "sequence": 2,
            "input": {"target": "ui.color.v1", "path": capture_path},
            "producer": {"ui_runtime": ui_animation_snapshot(2).ok()},
            "consumer": {"capture": capture},
            "pairing": {"status": "matched"},
            "result": "passed",
            "pass_result": true,
        }));

        let steps = [
            Step { button: "btn-a-toggle", machine: "panel-a", expected_state: "expanded", panel: "anim-panel-a" },
            Step { button: "btn-a-toggle", machine: "panel-a", expected_state: "compact", panel: "anim-panel-a" },
            Step { button: "btn-b-toggle", machine: "panel-b", expected_state: "hidden", panel: "anim-panel-b" },
            Step { button: "btn-b-toggle", machine: "panel-b", expected_state: "visible", panel: "anim-panel-b" },
            Step { button: "btn-c-warn", machine: "panel-c", expected_state: "warning", panel: "anim-panel-c" },
            Step { button: "btn-c-error", machine: "panel-c", expected_state: "error", panel: "anim-panel-c" },
            Step { button: "btn-c-normal", machine: "panel-c", expected_state: "normal", panel: "anim-panel-c" },
            Step { button: "btn-d-toggle", machine: "panel-d", expected_state: "right", panel: "anim-panel-d" },
            Step { button: "btn-d-toggle", machine: "panel-d", expected_state: "left", panel: "anim-panel-d" },
            Step { button: "btn-e-toggle", machine: "panel-e", expected_state: "expanded", panel: "anim-panel-e" },
            Step { button: "btn-e-toggle", machine: "panel-e", expected_state: "collapsed", panel: "anim-panel-e" },
            Step { button: "btn-f-next", machine: "panel-f", expected_state: "medium", panel: "anim-panel-f" },
            Step { button: "btn-f-next", machine: "panel-f", expected_state: "large", panel: "anim-panel-f" },
            Step { button: "btn-f-next", machine: "panel-f", expected_state: "small", panel: "anim-panel-f" },
            Step { button: "btn-g-toggle", machine: "panel-g", expected_state: "emphasized", panel: "anim-panel-g" },
            Step { button: "btn-g-toggle", machine: "panel-g", expected_state: "quiet", panel: "anim-panel-g" },
            Step { button: "btn-h-toggle", machine: "panel-h", expected_state: "shown", panel: "anim-panel-h" },
            Step { button: "btn-h-again", machine: "panel-h", expected_state: "hidden", panel: "anim-panel-h" },
        ];
        let mut previous_transition_ids = std::collections::HashMap::<&str, u64>::new();
        for (index, step) in steps.iter().enumerate() {
            let previous = previous_transition_ids
                .get(step.panel)
                .copied()
                .unwrap_or_else(|| {
                    wgpu_snapshot(100 + index as u64)
                        .ok()
                        .map(|snapshot| transition_id(&snapshot, &format!("surface.anim-showcase/{}", step.panel)))
                        .unwrap_or(0)
                });
            let (_, wgpu, _) = wait_for_step(
                step,
                previous,
                &host_events_rx,
                &mut child,
                100 + index as u64,
            )?;
            previous_transition_ids.insert(step.panel, transition_id(&wgpu, &format!("surface.anim-showcase/{}", step.panel)));
            thread::sleep(UPDATE_SETTLE);
        }

        // Wait for the final frame to settle. This proves the renderer commits
        // the exact target after the button-driven transitions, rather than
        // merely reporting an accepted command while an old track remains.
        let final_deadline = Instant::now() + STEP_TIMEOUT;
        let mut final_ui = Value::Null;
        let mut final_wgpu = Value::Null;
        let mut final_active_count = u64::MAX;
        let mut final_h_opacity = None;
        while Instant::now() < final_deadline {
            final_ui = ui_animation_snapshot(50_000).unwrap_or(Value::Null);
            final_wgpu = wgpu_snapshot(50_000).unwrap_or(Value::Null);
            let active = window_snapshot(&final_wgpu)
                .get("active_transitions")
                .and_then(|value| value.get("count"))
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            final_active_count = active;
            final_h_opacity = window_snapshot(&final_wgpu)
                .get("active_transitions")
                .and_then(|value| value.get("frame"))
                .and_then(|value| value.get("nodes"))
                .and_then(Value::as_array)
                .and_then(|nodes| {
                    nodes.iter().find(|node| {
                        node.get("node_id").and_then(Value::as_str)
                            == Some("surface.anim-showcase/anim-panel-h")
                    })
                })
                .and_then(|node| node.get("visual"))
                .and_then(|visual| visual.get("opacity"))
                .and_then(Value::as_f64);
            if active == 0
                && state(&final_ui, "panel-a") == Some("compact")
                && state(&final_ui, "panel-b") == Some("visible")
                && state(&final_ui, "panel-c") == Some("normal")
                && state(&final_ui, "panel-d") == Some("left")
                && state(&final_ui, "panel-e") == Some("collapsed")
                && state(&final_ui, "panel-f") == Some("small")
                && state(&final_ui, "panel-g") == Some("quiet")
                && state(&final_ui, "panel-h") == Some("hidden")
                && final_h_opacity.is_some_and(|opacity| opacity <= 0.0001)
            {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let health = call_result(wgpu_endpoint, "wgpu-runtime", "service.health", 50_000, json!({}))?;
        let final_pass = final_active_count == 0
            && final_h_opacity.is_some_and(|opacity| opacity <= 0.0001)
            && state(&final_ui, "panel-a") == Some("compact")
            && state(&final_ui, "panel-b") == Some("visible")
            && state(&final_ui, "panel-c") == Some("normal")
            && state(&final_ui, "panel-d") == Some("left")
            && state(&final_ui, "panel-e") == Some("collapsed")
            && state(&final_ui, "panel-f") == Some("small")
            && state(&final_ui, "panel-g") == Some("quiet")
            && state(&final_ui, "panel-h") == Some("hidden");
        emit(json!({
            "probe": "animation-showcase.interactive.v1",
            "sequence": 50_000,
            "input": {"final_health_check": true, "expected_active_count": 0},
            "producer": {"ui_runtime": final_ui, "state_revision": final_ui.get("state_revision")},
            "consumer": {"health": health, "active_count": final_active_count, "panel_h_opacity": final_h_opacity},
            "pairing": {"status": "matched"},
            "result": if final_pass { "passed" } else { "failed" },
            "pass_result": final_pass,
        }));
        if final_pass {
            Ok(())
        } else {
            Err(format!("final state did not settle: active={final_active_count}, h_opacity={final_h_opacity:?}, ui={final_ui}, wgpu={final_wgpu}"))
        }
    })();

    let _ = call(
        wgpu_endpoint,
        request("wgpu-runtime", "service.shutdown", 90_001, json!({})),
    );
    let _ = call(
        ui_endpoint,
        request("ui-runtime", "service.shutdown", 90_002, json!({})),
    );
    let _ = call(
        host_endpoint,
        request("ui-host", "service.shutdown", 90_003, json!({})),
    );
    let _ = child.wait();
    let ui_result = ui_thread.join().unwrap_or_else(|_| Err("UI thread panicked".into()));
    let host_result = host_thread
        .join()
        .unwrap_or_else(|_| Err("host thread panicked".into()));

    let final_pass = run_result.is_ok() && ui_result.is_ok() && host_result.is_ok();
    emit(json!({
        "probe": "animation-showcase.interactive.v1",
        "sequence": 90_100,
        "input": {"elapsed_ms": started.elapsed().as_secs_f64() * 1000.0},
        "producer": {"ui_thread": ui_result.is_ok(), "host_thread": host_result.is_ok()},
        "consumer": {"wgpu_process": "shutdown_requested"},
        "pairing": {"status": "matched"},
        "result": if final_pass { "passed" } else { "failed" },
        "pass_result": final_pass,
        "error": run_result.err(),
    }));
    if !final_pass {
        std::process::exit(1);
    }
}
