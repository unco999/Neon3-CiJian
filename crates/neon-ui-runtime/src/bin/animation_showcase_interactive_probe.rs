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
    UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME, UI_TIMELINE_ANIMATION_CAPABILITY_NAME,
    UiHostInbound, UiHostPublication, UiInputChange,
    UiInputFrame, UiInputSchema, UiInputValue, UiProgram, UiProgramCapability,
    UiProgramCapabilityOwner, UiProgramCapabilityStatus, UiProgramRevision,
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
    expected_motion: &'static str,
    expected_easing: &'static str,
    numeric_node: Option<&'static str>,
    expected_numeric: Option<f64>,
    expect_transform: bool,
    expected_transform_from_identity: Option<bool>,
    expected_transform_target_identity: Option<bool>,
    expect_timeline: bool,
}

impl Step {
    fn visual(
        button: &'static str,
        machine: &'static str,
        expected_state: &'static str,
        panel: &'static str,
        expected_motion: &'static str,
        expected_easing: &'static str,
    ) -> Self {
        Self {
            button,
            machine,
            expected_state,
            panel,
            expected_motion,
            expected_easing,
            numeric_node: None,
            expected_numeric: None,
            expect_transform: false,
            expected_transform_from_identity: None,
            expected_transform_target_identity: None,
            expect_timeline: false,
        }
    }

    fn numeric(
        button: &'static str,
        machine: &'static str,
        expected_state: &'static str,
        panel: &'static str,
        expected_motion: &'static str,
        expected_numeric: f64,
    ) -> Self {
        Self {
            button,
            machine,
            expected_state,
            panel,
            expected_motion,
            expected_easing: "linear",
            numeric_node: Some("anim-progress-i"),
            expected_numeric: Some(expected_numeric),
            expect_transform: false,
            expected_transform_from_identity: None,
            expected_transform_target_identity: None,
            expect_timeline: false,
        }
    }

    fn transform(
        button: &'static str,
        machine: &'static str,
        expected_state: &'static str,
        panel: &'static str,
        from_identity: bool,
        target_identity: bool,
    ) -> Self {
        Self {
            button,
            machine,
            expected_state,
            panel,
            expected_motion: "transform-enter",
            expected_easing: "spring",
            numeric_node: None,
            expected_numeric: None,
            expect_transform: true,
            expected_transform_from_identity: Some(from_identity),
            expected_transform_target_identity: Some(target_identity),
            expect_timeline: false,
        }
    }

    fn timeline(
        button: &'static str,
        machine: &'static str,
        expected_state: &'static str,
        panel: &'static str,
    ) -> Self {
        Self {
            button,
            machine,
            expected_state,
            panel,
            expected_motion: "timeline-pulse",
            expected_easing: "bounce",
            numeric_node: None,
            expected_numeric: None,
            expect_transform: false,
            expected_transform_from_identity: None,
            expected_transform_target_identity: None,
            expect_timeline: true,
        }
    }
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
            UI_TIMELINE_ANIMATION_CAPABILITY_NAME,
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
        let mut input_revision = Revision(0);
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
                if let Some(observation) = &observation {
                    let _ = host_events.send(observation.clone());
                }
                let changes = match observation.as_ref().map(|event| event.action.as_str()) {
                    Some("anim.i.fill") => vec![UiInputChange {
                        key: "fill_progress".into(),
                        value: UiInputValue::F32 { value: 100.0 },
                    }],
                    Some("anim.i.empty") => vec![UiInputChange {
                        key: "fill_progress".into(),
                        value: UiInputValue::F32 { value: 0.0 },
                    }],
                    Some("anim.exit.unmount") => vec![UiInputChange {
                        key: "show_exit".into(),
                        value: UiInputValue::Bool { value: false },
                    }],
                    Some("anim.exit.mount") => vec![UiInputChange {
                        key: "show_exit".into(),
                        value: UiInputValue::Bool { value: true },
                    }],
                    _ => Vec::new(),
                };
                let expected_input_revision = input_revision;
                if !changes.is_empty() {
                    input_revision = Revision(input_revision.0.saturating_add(1));
                }
                let publication = UiHostPublication {
                    scalar_frame: UiInputFrame {
                        program_revision: host_program.revision.clone(),
                        expected_input_revision,
                        request_id: request.request_id.0.clone(),
                        idempotency_key: request
                            .idempotency_key
                            .clone()
                            .unwrap_or_else(|| request.request_id.0.clone()),
                        changes,
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

fn animation_control(
    method: &str,
    sequence: u64,
    node_path: &str,
    progress: Option<f64>,
) -> Result<Value, String> {
    let mut params = json!({"node_path": node_path});
    if let Some(progress) = progress {
        params["progress"] = json!(progress);
    }
    call_result(
        WGPU_ENDPOINT.parse().expect("fixed WGPU endpoint"),
        "wgpu-runtime",
        method,
        60_000 + sequence,
        params,
    )
}

fn run_animation_controls(child: &mut Child, sequence: u64) -> Result<(), String> {
    let node_path = "surface.anim-showcase/anim-panel-m";
    let pause = animation_control("wgpu.ui.animation.pause", sequence, node_path, None)?;
    let paused_snapshot = wgpu_snapshot(sequence + 1)?;
    thread::sleep(Duration::from_millis(60));
    let paused_again = wgpu_snapshot(sequence + 2)?;
    let paused_visual = frame_visual(&paused_snapshot, node_path);
    let paused_again_visual = frame_visual(&paused_again, node_path);
    let frozen = paused_visual
        .zip(paused_again_visual)
        .is_some_and(|(left, right)| {
            close_enough(number_at(left, "opacity"), number_at(right, "opacity"))
                && close_enough(
                    number_at(left, "bounds.width"),
                    number_at(right, "bounds.width"),
                )
        });
    let resume = animation_control("wgpu.ui.animation.resume", sequence + 3, node_path, None)?;
    let seek_half = animation_control(
        "wgpu.ui.animation.seek",
        sequence + 4,
        node_path,
        Some(0.5),
    )?;
    let seek_snapshot = wgpu_snapshot(sequence + 5)?;
    let seek_progress = active_transition(&seek_snapshot, node_path)
        .and_then(|transition| transition.get("progress"))
        .and_then(Value::as_f64);
    let complete = animation_control(
        "wgpu.ui.animation.seek",
        sequence + 6,
        node_path,
        Some(1.0),
    )?;
    let completed_snapshot = wgpu_snapshot(sequence + 7)?;
    let cleaned = active_transition(&completed_snapshot, node_path).is_none();
    let process_alive = child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none();
    let pass = pause.get("state").and_then(Value::as_str) == Some("paused")
        && frozen
        && resume.get("state").and_then(Value::as_str) == Some("running")
        && seek_half.get("progress").and_then(Value::as_f64).is_some_and(|value| {
            (value - 0.5).abs() <= 0.001
        })
        && seek_progress.is_some_and(|value| (value - 0.5).abs() <= 0.12)
        && complete.get("state").and_then(Value::as_str) == Some("completed")
        && cleaned
        && process_alive;
    emit(json!({
        "probe": "animation-showcase.control.v1",
        "sequence": sequence,
        "input": {"node_path": node_path, "actions": ["pause", "resume", "seek(0.5)", "seek(1.0)"]},
        "producer": {"pause": pause, "resume": resume, "seek_half": seek_half, "complete": complete},
        "consumer": {"paused_snapshot": paused_snapshot, "paused_again": paused_again, "seek_snapshot": seek_snapshot, "completed_snapshot": completed_snapshot, "seek_progress": seek_progress, "frozen": frozen, "cleaned": cleaned},
        "pairing": {"status": "matched"},
        "result": if pass { "passed" } else { "failed" },
        "pass_result": pass,
    }));
    if pass {
        Ok(())
    } else {
        Err(format!(
            "animation controls failed: frozen={frozen}, seek_progress={seek_progress:?}, cleaned={cleaned}, pause={pause}, resume={resume}, complete={complete}"
        ))
    }
}

fn observe_timeline_cubic_segment(child: &mut Child, sequence: u64) -> Result<(), String> {
    let node_path = "surface.anim-showcase/anim-panel-m";
    let deadline = Instant::now() + Duration::from_millis(600);
    let mut snapshot = Value::Null;
    let mut transition = None;
    while Instant::now() < deadline {
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("WGPU process exited while observing cubic-bezier segment".into());
        }
        snapshot = wgpu_snapshot(sequence)?;
        transition = active_transition(&snapshot, node_path);
        if transition.is_some_and(|transition| {
            transition.get("easing").and_then(Value::as_str) == Some("cubic_bezier")
                && transition
                    .get("timeline")
                    .and_then(|timeline| timeline.get("segment_index"))
                    .and_then(Value::as_u64)
                    == Some(1)
        }) {
            break;
        }
        thread::sleep(Duration::from_millis(12));
    }
    let pass = transition.is_some_and(|transition| {
        transition.get("easing").and_then(Value::as_str) == Some("cubic_bezier")
            && transition
                .get("timeline")
                .and_then(|timeline| timeline.get("segment_index"))
                .and_then(Value::as_u64)
                == Some(1)
    });
    emit(json!({
        "probe": "animation-showcase.curve.v1",
        "sequence": sequence,
        "input": {"node_path": node_path, "expected_segment": 1, "expected_easing": "cubic_bezier"},
        "producer": {"timeline_transition": transition},
        "consumer": {"snapshot_frame": window_snapshot(&snapshot).get("active_transitions").and_then(|active| active.get("frame")).and_then(|frame| frame.get("frame_sequence"))},
        "pairing": {"status": "matched"},
        "result": if pass { "passed" } else { "failed" },
        "pass_result": pass,
    }));
    if pass {
        Ok(())
    } else {
        Err(format!("cubic-bezier segment was not observed: {transition:?}"))
    }
}

fn wait_for_renderer_idle(child: &mut Child, sequence: u64) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut snapshot = Value::Null;
    while Instant::now() < deadline {
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("WGPU process exited while waiting for initial animation settle".into());
        }
        snapshot = wgpu_snapshot(sequence).unwrap_or(Value::Null);
        let active_count = window_snapshot(&snapshot)
            .get("active_transitions")
            .and_then(|value| value.get("count"))
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        if active_count == 0 {
            return Ok(snapshot);
        }
        thread::sleep(Duration::from_millis(20));
    }
    let active = window_snapshot(&snapshot)
        .get("active_transitions")
        .cloned()
        .unwrap_or(Value::Null);
    let active_summary = active
        .get("transitions")
        .and_then(Value::as_array)
        .map(|transitions| {
            transitions
                .iter()
                .map(|transition| {
                    json!({
                        "node_key": transition.get("node_key"),
                        "motion_key": transition.get("motion_key"),
                        "reason": transition.get("reason"),
                        "transition_id": transition.get("transition_id"),
                        "current_matches_target": transition.get("current_matches_target"),
                        "elapsed_ms": transition.get("elapsed_ms"),
                        "duration_ms": transition.get("duration_ms"),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let history_summary = active
        .get("history")
        .and_then(Value::as_array)
        .map(|history| {
            history
                .iter()
                .rev()
                .take(20)
                .map(|entry| {
                    json!({
                        "node_key": entry.get("node_key"),
                        "motion_key": entry.get("motion_key"),
                        "reason": entry.get("reason"),
                        "status": entry.get("status"),
                        "transition_id": entry.get("transition_id"),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Err(format!(
        "initial animations did not settle: count={}, transitions={active_summary:?}, history={history_summary:?}",
        active.get("count").and_then(Value::as_u64).unwrap_or(u64::MAX),
    ))
}

fn frame_visual<'a>(snapshot: &'a Value, node_path: &str) -> Option<&'a Value> {
    window_snapshot(snapshot)
        .get("active_transitions")?
        .get("frame")?
        .get("nodes")?
        .as_array()?
        .iter()
        .find(|node| node.get("node_id").and_then(Value::as_str) == Some(node_path))
        .and_then(|node| node.get("visual"))
}

fn number_at(value: &Value, path: &str) -> Option<f64> {
    let mut current = value;
    for segment in path.split('.') {
        current = if let Ok(index) = segment.parse::<usize>() {
            current.get(index)?
        } else {
            current.get(segment)?
        };
    }
    current.as_f64()
}

fn close_enough(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => (left - right).abs() <= 0.0001,
        _ => false,
    }
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
    previous_numeric_transition_id: u64,
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
        "btn-i-fill" => "anim.i.fill",
        "btn-i-empty" => "anim.i.empty",
        "btn-j-spring" => "anim.j.spring",
        "btn-j-reset" => "anim.j.reset",
        "btn-k-show" => "anim.k.show",
        "btn-k-reset" => "anim.k.reset",
        "btn-m-play" => "anim.m.play",
        "btn-m-reset" => "anim.m.reset",
        other => return Err(format!("unknown probe button {other}")),
    };
    let button_path = format!("surface.anim-showcase/{}", step.button);
    let panel_path = format!("surface.anim-showcase/{}", step.panel);
    let timeline_child_path = format!("surface.anim-showcase/{}", "m-core");
    let numeric_path = step
        .numeric_node
        .map(|node| format!("surface.anim-showcase/{node}"));
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
    let mut observed_wgpu = Value::Null;
    let mut observed_timeline_child = Value::Null;
    let mut observed_transition_id = previous_transition_id;
    let mut render_seen = false;
    let mut timeline_panel_seen = false;
    let mut timeline_child_seen = false;
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
        if current_transition_id > observed_transition_id
            && active_transition(&wgpu, &panel_path).is_some()
        {
            observed_transition_id = current_transition_id;
            observed_wgpu = wgpu.clone();
        }
        render_seen = observed_transition_id > previous_transition_id;
        if step.expect_timeline {
            let panel_timeline = active_transition(&wgpu, &panel_path)
                .and_then(|transition| transition.get("timeline"))
                .is_some_and(|timeline| {
                    timeline.get("segment_count").and_then(Value::as_u64) == Some(2)
                        && timeline.get("repeat").and_then(Value::as_str) == Some("Count(2)")
                        && timeline.get("group").and_then(Value::as_str) == Some("Sequence")
                });
            timeline_panel_seen |= panel_timeline;
            let staggered_child = active_transition(&wgpu, &timeline_child_path)
                .and_then(|transition| transition.get("timeline"))
                .and_then(|timeline| timeline.get("stagger_delay_ms"))
                .and_then(Value::as_u64)
                == Some(20);
            if staggered_child {
                timeline_child_seen = true;
                observed_timeline_child = wgpu.clone();
            }
            if window_snapshot(&wgpu)
                .get("active_transitions")
                .and_then(|active| active.get("history"))
                .and_then(Value::as_array)
                .is_some_and(|history| {
                    history.iter().any(|entry| {
                        entry.get("node_key").and_then(Value::as_str)
                            == Some(timeline_child_path.as_str())
                            && entry
                                .get("timeline")
                                .and_then(|timeline| timeline.get("stagger_ms"))
                                .and_then(Value::as_u64)
                                == Some(20)
                    })
                })
            {
                timeline_child_seen = true;
            }
        }
        let timeline_seen = timeline_panel_seen && timeline_child_seen;
        let numeric_render_seen = numeric_path.as_deref().is_none_or(|path| {
            transition_id(&wgpu, path) > previous_numeric_transition_id
                && active_transition(&wgpu, path).is_some()
        });
        if state_matches && render_seen && numeric_render_seen && (!step.expect_timeline || timeline_seen) {
            break;
        }
        thread::sleep(Duration::from_millis(12));
    }
    let mut spring_overshoot = true;
    if step.expected_easing == "spring" && step.expected_state == "bounced" {
        spring_overshoot = false;
        let overshoot_deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < overshoot_deadline {
            let snapshot = wgpu_snapshot(sequence + 2).unwrap_or(Value::Null);
            if let Some(transition) = active_transition(&snapshot, &panel_path) {
                let sampled_width = number_at(transition, "sampled.bounds.width");
                let target_width = number_at(transition, "target.bounds.width");
                if sampled_width.zip(target_width).is_some_and(|(sampled, target)| {
                    sampled > target + 0.01
                }) {
                    spring_overshoot = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(12));
        }
    }
    let static_target_matches = if step.expected_transform_target_identity == Some(false) {
        let settle_deadline = Instant::now() + Duration::from_secs(2);
        let mut matched = false;
        while Instant::now() < settle_deadline {
            let snapshot = wgpu_snapshot(sequence + 3).unwrap_or(Value::Null);
            if active_transition(&snapshot, &panel_path).is_none()
                && let Some(visual) = frame_visual(&snapshot, &panel_path)
            {
                let translation_x = number_at(visual, "transform.translation.0");
                let scale_x = number_at(visual, "transform.scale.0");
                let rotation = number_at(visual, "transform.rotation_degrees");
                matched = translation_x.is_some_and(|value| value.abs() > 0.01)
                    && scale_x.is_some_and(|value| (value - 1.0).abs() > 0.01)
                    && rotation.is_some_and(|value| value.abs() > 0.01);
                if matched {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        matched
    } else {
        true
    };
    let selected = last_transition(&ui, step.machine);
    let active = active_transition(&wgpu, &panel_path);
    let timeline_child_transition = active_transition(&observed_timeline_child, &timeline_child_path)
        .or_else(|| active_transition(&wgpu, &timeline_child_path));
    let observed_active = active_transition(&observed_wgpu, &panel_path);
    let renderer_transition = active.or(observed_active);
    let numeric_active = numeric_path
        .as_deref()
        .and_then(|path| active_transition(&wgpu, path));
    let evidence_wgpu = if observed_wgpu != Value::Null {
        &observed_wgpu
    } else {
        &wgpu
    };
    let (producer_frame, consumer_frame) = frame_pair(evidence_wgpu, &panel_path);
    let (numeric_producer_frame, numeric_consumer_frame) = numeric_path
        .as_deref()
        .map(|path| frame_pair(&wgpu, path))
        .unwrap_or((None, None));
    let frame_matched = producer_frame
        .zip(consumer_frame)
        .is_some_and(|(producer, consumer)| consumer >= producer);
    let numeric_frame_matched = if step.numeric_node.is_some() {
        numeric_producer_frame
            .zip(numeric_consumer_frame)
            .is_some_and(|(producer, consumer)| consumer >= producer)
    } else {
        true
    };
    let transition_matches = selected.is_some_and(|transition| {
        transition.get("target_state").and_then(Value::as_str) == Some(step.expected_state)
            && transition.get("motion_key").and_then(Value::as_str) == Some(step.expected_motion)
    });
    let renderer_transition_matches = renderer_transition.is_some_and(|transition| {
        transition.get("motion_key").and_then(Value::as_str) == Some(step.expected_motion)
            && transition.get("easing").and_then(Value::as_str) == Some(step.expected_easing)
    });
    let timeline_matches = if step.expect_timeline {
        timeline_panel_seen
            && timeline_child_seen
            && renderer_transition.is_some_and(|transition| {
                transition
                    .get("timeline")
                    .and_then(|timeline| timeline.get("segment_count"))
                    .and_then(Value::as_u64)
                    == Some(2)
            })
    } else {
        true
    };
    let numeric_target = numeric_active.and_then(|transition| {
        number_at(transition, "target.numeric_value.value")
    });
    let numeric_sampled = numeric_active.and_then(|transition| {
        number_at(transition, "sampled.numeric_value.value")
    });
    let numeric_matches = match step.expected_numeric {
        Some(expected) => {
            numeric_active.is_some()
                && close_enough(numeric_target, Some(expected))
                && numeric_sampled.is_some()
        }
        None => true,
    };
    let transform_matches = if step.expect_transform {
        renderer_transition.is_some_and(|transition| {
            let translation_x = number_at(transition, "from_transform.translation.0");
            let scale_x = number_at(transition, "from_transform.scale.0");
            let rotation = number_at(transition, "from_transform.rotation_degrees");
            let target_translation_x = number_at(transition, "target_transform.translation.0");
            let target_scale_x = number_at(transition, "target_transform.scale.0");
            let target_rotation = number_at(transition, "target_transform.rotation_degrees");
            let from_identity = translation_x.is_some_and(|value| value.abs() <= 0.01)
                && number_at(transition, "from_transform.translation.1")
                    .is_some_and(|value| value.abs() <= 0.01)
                && scale_x.is_some_and(|value| (value - 1.0).abs() <= 0.01)
                && number_at(transition, "from_transform.scale.1")
                    .is_some_and(|value| (value - 1.0).abs() <= 0.01)
                && rotation.is_some_and(|value| value.abs() <= 0.01);
            let target_identity = target_translation_x.is_some_and(|value| value.abs() <= 0.01)
                && number_at(transition, "target_transform.translation.1")
                    .is_some_and(|value| value.abs() <= 0.01)
                && target_scale_x.is_some_and(|value| (value - 1.0).abs() <= 0.01)
                && number_at(transition, "target_transform.scale.1")
                    .is_some_and(|value| (value - 1.0).abs() <= 0.01)
                && target_rotation.is_some_and(|value| value.abs() <= 0.01);
            let source_expectation = step
                .expected_transform_from_identity
                .is_none_or(|expected| expected == from_identity);
            let target_expectation = step
                .expected_transform_target_identity
                .is_none_or(|expected| expected == target_identity);
            source_expectation && target_expectation
        })
    } else {
        true
    };
    let process_alive = child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none();
    let pass = activate != Value::Null
        && state(&ui, step.machine) == Some(step.expected_state)
        && renderer_transition.is_some()
        && render_seen
        && transition_matches
        && renderer_transition_matches
        && timeline_matches
        && numeric_matches
        && spring_overshoot
        && transform_matches
        && static_target_matches
        && frame_matched
        && numeric_frame_matched
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
            "expected_motion": step.expected_motion,
            "expected_easing": step.expected_easing,
            "numeric_node": step.numeric_node,
            "expected_numeric": step.expected_numeric,
            "expect_transform": step.expect_transform,
            "transform_matches": transform_matches,
            "static_target_matches": static_target_matches,
            "timeline_matches": timeline_matches,
            "spring_overshoot": spring_overshoot,
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
            "renderer_transition": renderer_transition,
            "timeline_child_transition": timeline_child_transition,
            "numeric_transition": numeric_active,
            "numeric_target": numeric_target,
            "numeric_sampled": numeric_sampled,
            "render_status": if pass { "accepted" } else { "rejected" },
            "active_transition_id": transition_id(evidence_wgpu, &panel_path),
        },
        "pairing": {
            "producer_frame": producer_frame,
            "consumer_frame": consumer_frame,
            "numeric_producer_frame": numeric_producer_frame,
            "numeric_consumer_frame": numeric_consumer_frame,
            "status": if frame_matched && numeric_frame_matched { "matched" } else { "mismatch" },
        },
        "diagnostic": {
            "previous_transition_id": previous_transition_id,
            "observed_transition_id": observed_transition_id,
            "final_transition_id": transition_id(&wgpu, &panel_path),
            "history": window_snapshot(&wgpu)
                .get("active_transitions")
                .and_then(|value| value.get("history")),
        },
        "result": if pass { "passed" } else { "failed" },
        "pass_result": pass,
    }));
    if pass {
        Ok((ui, wgpu, host))
    } else {
        Err(format!(
            "step {} failed: state={:?}, selected={selected:?}, active={active:?}, observed_transition_id={observed_transition_id}, numeric={numeric_active:?}, frame={producer_frame:?}->{consumer_frame:?}, numeric_frame={numeric_producer_frame:?}->{numeric_consumer_frame:?}",
            step.button,
            state(&ui, step.machine),
        ))
    }
}

fn wait_for_exit_unmount(
    host_events: &Receiver<HostObservation>,
    child: &mut Child,
    sequence: u64,
) -> Result<(), String> {
    let node_path = "surface.anim-showcase/anim-panel-l";
    let before_snapshot = wgpu_snapshot(sequence).unwrap_or(Value::Null);
    let activate = call_result(
        WGPU_ENDPOINT.parse().expect("fixed WGPU endpoint"),
        "wgpu-runtime",
        "debug.window.input.activate_target",
        40_000 + sequence,
        json!({"semantic_node_path": "surface.anim-showcase/btn-l-unmount"}),
    )?;
    let deadline = Instant::now() + STEP_TIMEOUT;
    let host = host_event_for(host_events, "anim.exit.unmount", deadline)
        .ok_or_else(|| "host did not receive anim.exit.unmount".to_string())?;
    let mut active_snapshot = Value::Null;
    let mut observed_snapshot = Value::Null;
    let mut active = None;
    let mut observed_active = None;
    while Instant::now() < deadline {
        if child.try_wait().map_err(|error| error.to_string())?.is_some() {
            return Err("WGPU process exited during exit/unmount".into());
        }
        active_snapshot = wgpu_snapshot(sequence + 1).unwrap_or(Value::Null);
        active = active_transition(&active_snapshot, node_path);
        if active.is_some_and(|transition| {
            transition.get("reason").and_then(Value::as_str) == Some("exit")
        }) {
            observed_snapshot = active_snapshot.clone();
            observed_active = active_transition(&observed_snapshot, node_path);
            break;
        }
        thread::sleep(Duration::from_millis(12));
    }
    let evidence_snapshot = if observed_snapshot != Value::Null {
        &observed_snapshot
    } else {
        &active_snapshot
    };
    let evidence_active = observed_active.or(active);
    let mut removed = false;
    let remove_deadline = Instant::now() + STEP_TIMEOUT;
    let mut final_snapshot = active_snapshot.clone();
    while Instant::now() < remove_deadline {
        final_snapshot = wgpu_snapshot(sequence + 2).unwrap_or(Value::Null);
        removed = active_transition(&final_snapshot, node_path).is_none()
            && frame_visual(&final_snapshot, node_path).is_none();
        if removed {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let final_exit_diagnostic = final_snapshot
        .get("window")
        .and_then(|window| window.get("active_transitions"))
        .and_then(|active| active.get("last_exit_reconciliation"))
        .filter(|diagnostic| diagnostic.get("result").and_then(Value::as_str) == Some("exit_started"))
        .cloned();
    let exit_evidence = evidence_active
        .cloned()
        .or(final_exit_diagnostic);
    let (observed_producer_frame, observed_consumer_frame) = frame_pair(evidence_snapshot, node_path);
    let producer_frame = observed_producer_frame.or_else(|| {
        exit_evidence
            .as_ref()
            .and_then(|evidence| evidence.get("source_frame_sequence"))
            .and_then(Value::as_u64)
    });
    let consumer_frame = observed_consumer_frame.or_else(|| {
        final_snapshot
            .get("window")
            .and_then(|window| window.get("active_transitions"))
            .and_then(|active| active.get("frame"))
            .and_then(|frame| frame.get("frame_sequence"))
            .and_then(Value::as_u64)
    });
    let frame_matched = producer_frame
        .zip(consumer_frame)
        .is_some_and(|(producer, consumer)| consumer >= producer);
    let exit_track = exit_evidence.as_ref().is_some_and(|transition| {
        transition.get("motion_key").and_then(Value::as_str) == Some("exit-fade")
            && transition.get("easing").and_then(Value::as_str) == Some("ease_in")
            && (transition.get("reason").and_then(Value::as_str) == Some("exit")
                || transition.get("result").and_then(Value::as_str) == Some("exit_started"))
    });
    let process_alive = child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none();
    let pass = activate != Value::Null && exit_track && frame_matched && removed && process_alive;
    emit(json!({
        "probe": "animation-showcase.exit.v1",
        "sequence": sequence,
        "input": {"button": "btn-l-unmount", "semantic_intent": "anim.exit.unmount", "node_path": node_path},
        "producer": {"host_request_id": host.request_id, "interaction_id": host.interaction_id, "interaction_sequence": host.sequence, "ui_runtime": ui_animation_snapshot(sequence + 3).unwrap_or(Value::Null)},
        "consumer": {"activation": activate, "before_snapshot": before_snapshot, "exit_transition": exit_evidence, "final_snapshot": final_snapshot, "removed": removed},
        "pairing": {"producer_frame": producer_frame, "consumer_frame": consumer_frame, "status": if frame_matched { "matched" } else { "mismatch" }},
        "result": if pass { "passed" } else { "failed" },
        "pass_result": pass,
    }));
    if pass {
        Ok(())
    } else {
        Err(format!("exit/unmount failed: exit_track={exit_track}, frame={producer_frame:?}->{consumer_frame:?}, removed={removed}, observed={evidence_active:?}, snapshot={final_snapshot}"))
    }
}

fn wait_for_exit_mount(
    host_events: &Receiver<HostObservation>,
    child: &mut Child,
    sequence: u64,
) -> Result<(), String> {
    let node_path = "surface.anim-showcase/anim-panel-l";
    let activate = call_result(
        WGPU_ENDPOINT.parse().expect("fixed WGPU endpoint"),
        "wgpu-runtime",
        "debug.window.input.activate_target",
        41_000 + sequence,
        json!({"semantic_node_path": "surface.anim-showcase/btn-l-mount"}),
    )?;
    let deadline = Instant::now() + STEP_TIMEOUT;
    let host = host_event_for(host_events, "anim.exit.mount", deadline)
        .ok_or_else(|| "host did not receive anim.exit.mount".to_string())?;
    let mut snapshot = Value::Null;
    let mut mounted = false;
    while Instant::now() < deadline {
        snapshot = wgpu_snapshot(sequence + 1).unwrap_or(Value::Null);
        mounted = frame_visual(&snapshot, node_path)
            .and_then(|visual| visual.get("opacity"))
            .is_some_and(|opacity| opacity.as_f64().is_some_and(|value| value > 0.99));
        if mounted {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let process_alive = child
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none();
    let pass = activate != Value::Null && mounted && process_alive;
    emit(json!({
        "probe": "animation-showcase.exit.v1",
        "sequence": sequence,
        "input": {"button": "btn-l-mount", "semantic_intent": "anim.exit.mount", "node_path": node_path},
        "producer": {"host_request_id": host.request_id, "interaction_id": host.interaction_id, "interaction_sequence": host.sequence},
        "consumer": {"activation": activate, "mounted_visual": frame_visual(&snapshot, node_path), "mounted": mounted},
        "pairing": {"status": "matched"},
        "result": if pass { "passed" } else { "failed" },
        "pass_result": pass,
    }));
    if pass { Ok(()) } else { Err(format!("exit/mount failed: mounted={mounted}, snapshot={snapshot}")) }
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
            "input": {"surface_id": surface_id, "buttons": document.ir.events.len(), "case": "animation-showcase"},
            "producer": {"ui_runtime": flow_result, "program_revision": 1},
            "consumer": {"renderer": "flow forwarded to WGPU"},
            "pairing": {"status": "ready"},
            "result": "passed",
            "pass_result": true,
        }));
        if manual {
            emit(json!({
                "probe": "animation-showcase.interactive.v1",
                "sequence": 4,
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

        let idle_snapshot = wait_for_renderer_idle(&mut child, 3)?;
        emit(json!({
            "probe": "animation-showcase.interactive.v1",
            "sequence": 3,
            "input": {"phase": "initial_animation_settle"},
            "producer": {"ui_runtime": ui_animation_snapshot(3).ok()},
            "consumer": {"active_count": window_snapshot(&idle_snapshot)
                .get("active_transitions")
                .and_then(|value| value.get("count"))},
            "pairing": {"status": "matched"},
            "result": "passed",
            "pass_result": true,
        }));

        let steps = [
            Step::visual("btn-a-toggle", "panel-a", "expanded", "anim-panel-a", "expand", "ease_out"),
            Step::visual("btn-a-collapse", "panel-a", "compact", "anim-panel-a", "collapse", "ease_in"),
            Step::visual("btn-a-toggle", "panel-a", "expanded", "anim-panel-a", "expand", "ease_out"),
            Step::visual("btn-a-toggle", "panel-a", "compact", "anim-panel-a", "collapse", "ease_in"),
            Step::visual("btn-b-toggle", "panel-b", "hidden", "anim-panel-b", "fade-out", "ease_in"),
            Step::visual("btn-b-show", "panel-b", "visible", "anim-panel-b", "fade-in", "ease_out"),
            Step::visual("btn-b-toggle", "panel-b", "hidden", "anim-panel-b", "fade-out", "ease_in"),
            Step::visual("btn-b-toggle", "panel-b", "visible", "anim-panel-b", "fade-in", "ease_out"),
            Step::visual("btn-c-warn", "panel-c", "warning", "anim-panel-c", "color-shift", "ease_in_out"),
            Step::visual("btn-c-error", "panel-c", "error", "anim-panel-c", "color-shift", "ease_in_out"),
            Step::visual("btn-c-normal", "panel-c", "normal", "anim-panel-c", "color-shift", "ease_in_out"),
            Step::visual("btn-d-toggle", "panel-d", "right", "anim-panel-d", "slide", "ease_in_out"),
            Step::visual("btn-d-left", "panel-d", "left", "anim-panel-d", "slide", "ease_in_out"),
            Step::visual("btn-e-toggle", "panel-e", "expanded", "anim-panel-e", "expand", "ease_out"),
            Step::visual("btn-e-collapse", "panel-e", "collapsed", "anim-panel-e", "collapse", "ease_in"),
            Step::visual("btn-e-toggle", "panel-e", "expanded", "anim-panel-e", "expand", "ease_out"),
            Step::visual("btn-e-toggle", "panel-e", "collapsed", "anim-panel-e", "collapse", "ease_in"),
            Step::visual("btn-f-next", "panel-f", "medium", "anim-panel-f", "expand", "ease_out"),
            Step::visual("btn-f-next", "panel-f", "large", "anim-panel-f", "expand", "ease_out"),
            Step::visual("btn-f-next", "panel-f", "small", "anim-panel-f", "collapse", "ease_in"),
            Step::visual("btn-f-large", "panel-f", "large", "anim-panel-f", "expand", "ease_out"),
            Step::visual("btn-f-next", "panel-f", "small", "anim-panel-f", "collapse", "ease_in"),
            Step::visual("btn-g-toggle", "panel-g", "emphasized", "anim-panel-g", "edge-glow", "ease_out"),
            Step::visual("btn-g-quiet", "panel-g", "quiet", "anim-panel-g", "edge-glow", "ease_out"),
            Step::visual("btn-g-toggle", "panel-g", "emphasized", "anim-panel-g", "edge-glow", "ease_out"),
            Step::visual("btn-g-quiet", "panel-g", "quiet", "anim-panel-g", "edge-glow", "ease_out"),
            Step::visual("btn-h-toggle", "panel-h", "shown", "anim-panel-h", "delayed-reveal", "ease_out"),
            Step::visual("btn-h-again", "panel-h", "hidden", "anim-panel-h", "fade-out", "ease_in"),
            Step::numeric("btn-i-fill", "panel-i", "full", "anim-panel-i", "linear-fill", 100.0),
            Step::numeric("btn-i-empty", "panel-i", "empty", "anim-panel-i", "linear-fill", 0.0),
            Step::visual("btn-j-spring", "panel-j", "bounced", "anim-panel-j", "spring-expand", "spring"),
            Step::visual("btn-j-reset", "panel-j", "rest", "anim-panel-j", "spring-expand", "spring"),
            Step::transform("btn-k-show", "panel-k", "settled", "anim-panel-k", true, false),
            Step::transform("btn-k-reset", "panel-k", "rest", "anim-panel-k", false, true),
            Step::timeline("btn-m-play", "panel-m", "peak", "anim-panel-m"),
            Step::timeline("btn-m-reset", "panel-m", "rest", "anim-panel-m"),
        ];
        let mut previous_transition_ids = std::collections::HashMap::<&str, u64>::new();
        for (index, step) in steps.iter().enumerate() {
            let baseline = wgpu_snapshot(100 + index as u64).ok();
            let previous = previous_transition_ids
                .get(step.panel)
                .copied()
                .or_else(|| {
                    baseline.as_ref().map(|snapshot| {
                        transition_id(snapshot, &format!("surface.anim-showcase/{}", step.panel))
                    })
                });
            let previous = previous.unwrap_or(0);
            let previous_numeric = step
                .numeric_node
                .and_then(|node| previous_transition_ids.get(node).copied())
                .or_else(|| {
                    baseline.as_ref().and_then(|snapshot| {
                        step.numeric_node.map(|node| {
                            transition_id(snapshot, &format!("surface.anim-showcase/{node}"))
                        })
                    })
                })
                .unwrap_or(0);
            let (_, wgpu, _) = wait_for_step(
                step,
                previous,
                previous_numeric,
                &host_events_rx,
                &mut child,
                100 + index as u64,
            )?;
            previous_transition_ids.insert(
                step.panel,
                transition_id(&wgpu, &format!("surface.anim-showcase/{}", step.panel)),
            );
            if let Some(node) = step.numeric_node {
                previous_transition_ids.insert(
                    node,
                    transition_id(&wgpu, &format!("surface.anim-showcase/{node}")),
                );
            }
            if step.button == "btn-m-play" {
                observe_timeline_cubic_segment(&mut child, 70_000 + index as u64)?;
                run_animation_controls(&mut child, 70_000 + index as u64)?;
            }
            thread::sleep(UPDATE_SETTLE);
        }

        wait_for_exit_unmount(&host_events_rx, &mut child, 200)?;
        wait_for_exit_mount(&host_events_rx, &mut child, 201)?;

        // Wait for the final frame to settle. This proves the renderer commits
        // the exact target after the button-driven transitions, rather than
        // merely reporting an accepted command while an old track remains.
        let final_deadline = Instant::now() + STEP_TIMEOUT;
        let mut final_ui = Value::Null;
        let mut final_wgpu = Value::Null;
        let mut final_active_count = u64::MAX;
        let mut final_h_opacity = None;
        let mut final_i_value = None;
        let mut final_k_identity = false;
        let mut final_l_mounted = false;
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
            final_i_value = window_snapshot(&final_wgpu)
                .get("active_transitions")
                .and_then(|value| value.get("frame"))
                .and_then(|frame| frame.get("nodes"))
                .and_then(Value::as_array)
                .and_then(|nodes| {
                    nodes.iter().find(|node| {
                        node.get("node_id").and_then(Value::as_str)
                            == Some("surface.anim-showcase/anim-progress-i")
                    })
                })
                .and_then(|node| number_at(node, "visual.numeric_value.value"));
            final_k_identity = frame_visual(
                &final_wgpu,
                "surface.anim-showcase/anim-panel-k",
            )
            .is_some_and(|visual| {
                number_at(visual, "transform.translation.0")
                    .is_some_and(|value| value.abs() <= 0.01)
                    && number_at(visual, "transform.translation.1")
                        .is_some_and(|value| value.abs() <= 0.01)
                    && number_at(visual, "transform.scale.0")
                        .is_some_and(|value| (value - 1.0).abs() <= 0.01)
                    && number_at(visual, "transform.scale.1")
                        .is_some_and(|value| (value - 1.0).abs() <= 0.01)
                    && number_at(visual, "transform.rotation_degrees")
                        .is_some_and(|value| value.abs() <= 0.01)
            });
            final_l_mounted = frame_visual(
                &final_wgpu,
                "surface.anim-showcase/anim-panel-l",
            )
            .and_then(|visual| visual.get("opacity"))
            .and_then(Value::as_f64)
            .is_some_and(|opacity| opacity > 0.99);
            if active == 0
                && state(&final_ui, "panel-a") == Some("compact")
                && state(&final_ui, "panel-b") == Some("visible")
                && state(&final_ui, "panel-c") == Some("normal")
                && state(&final_ui, "panel-d") == Some("left")
                && state(&final_ui, "panel-e") == Some("collapsed")
                && state(&final_ui, "panel-f") == Some("small")
                && state(&final_ui, "panel-g") == Some("quiet")
                && state(&final_ui, "panel-h") == Some("hidden")
                && state(&final_ui, "panel-i") == Some("empty")
                && state(&final_ui, "panel-j") == Some("rest")
                && state(&final_ui, "panel-k") == Some("rest")
                && state(&final_ui, "panel-m") == Some("rest")
                && final_h_opacity.is_some_and(|opacity| opacity <= 0.0001)
                && close_enough(final_i_value, Some(0.0))
                && final_k_identity
                && final_l_mounted
            {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let health = call_result(wgpu_endpoint, "wgpu-runtime", "service.health", 50_000, json!({}))?;
        let final_pass = final_active_count == 0
            && final_h_opacity.is_some_and(|opacity| opacity <= 0.0001)
            && close_enough(final_i_value, Some(0.0))
            && final_k_identity
            && final_l_mounted
            && state(&final_ui, "panel-a") == Some("compact")
            && state(&final_ui, "panel-b") == Some("visible")
            && state(&final_ui, "panel-c") == Some("normal")
            && state(&final_ui, "panel-d") == Some("left")
            && state(&final_ui, "panel-e") == Some("collapsed")
            && state(&final_ui, "panel-f") == Some("small")
            && state(&final_ui, "panel-g") == Some("quiet")
            && state(&final_ui, "panel-h") == Some("hidden")
            && state(&final_ui, "panel-i") == Some("empty")
            && state(&final_ui, "panel-j") == Some("rest")
            && state(&final_ui, "panel-k") == Some("rest")
            && state(&final_ui, "panel-m") == Some("rest");
        emit(json!({
            "probe": "animation-showcase.interactive.v1",
            "sequence": 50_000,
            "input": {"final_health_check": true, "expected_active_count": 0, "expected_numeric": 0.0},
            "producer": {"ui_runtime": final_ui, "state_revision": final_ui.get("state_revision")},
            "consumer": {"health": health, "active_count": final_active_count, "active_tracks": window_snapshot(&final_wgpu)
                .get("active_transitions")
                .and_then(|active| active.get("transitions")), "panel_h_opacity": final_h_opacity, "panel_i_numeric_value": final_i_value, "panel_k_identity": final_k_identity, "panel_l_mounted": final_l_mounted},
            "pairing": {"status": "matched"},
            "result": if final_pass { "passed" } else { "failed" },
            "pass_result": final_pass,
        }));
        if final_pass {
            Ok(())
        } else {
            Err(format!("final state did not settle: active={final_active_count}, h_opacity={final_h_opacity:?}, i_value={final_i_value:?}, ui={final_ui}, wgpu={final_wgpu}"))
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
