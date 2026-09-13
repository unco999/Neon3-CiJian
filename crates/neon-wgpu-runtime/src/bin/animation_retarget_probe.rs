// Deterministic P0 animation retarget probe.
//
// The probe talks to the real windowed WGPU runtime over the public RPC
// boundary. It deliberately sends no renderer-local IDs or pointer events:
// every update is a typed fragment target for the same stable node. The
// runtime's structured debug snapshot supplies the consumer-side sampled
// value, transition metadata, and frame pairing.

use std::{
    net::SocketAddr,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcStatus,
    ServiceName,
};
use neon_ui_schema::{
    UiBounds, UiClipShape, UiCommand, UiFragment, UiFragmentId, UiFragmentSubmission, UiNode,
    UiNodeId, UiNodeKind, UiStyle, UiTransition, UiTransitionState,
};
use serde_json::{json, Value};

const ENDPOINT: &str = "127.0.0.1:39261";
const NODE_KEY: &str = "retarget-panel";
const FRAGMENT_KEY: &str = "animation-retarget";
const NODE_PATH: &str = "animation-retarget/retarget-panel";
const UPDATE_COUNT: u64 = 100;
const UPDATE_INTERVAL: Duration = Duration::from_millis(60);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(8);
const WINDOW_STARTUP_GRACE: Duration = Duration::from_secs(8);
const OBSERVE_TIMEOUT: Duration = Duration::from_millis(450);
const FINAL_TIMEOUT: Duration = Duration::from_secs(2);
const EPSILON: f64 = 0.0001;

fn request(method: &str, sequence: u64, params: Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("animation-retarget-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "animation-retarget-probe".into(),
            pid: std::process::id(),
            origin: "animation-retarget-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("animation-retarget-{sequence}")),
    }
}

fn call(method: &str, sequence: u64, params: Value) -> Result<Value, String> {
    let endpoint: SocketAddr = ENDPOINT.parse().expect("fixed endpoint");
    let response = RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(&request(method, sequence, params)))
        .map_err(|error| error.to_string())?;
    if response.status != RpcStatus::Accepted {
        return Err(format!("{method} rejected: {:?}", response.error));
    }
    Ok(response.result.unwrap_or_else(|| json!({})))
}

fn launch() -> std::io::Result<Child> {
    let binary = std::env::current_exe()?.with_file_name("neon-wgpu-runtime.exe");
    if !binary.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("runtime binary is missing: {}", binary.display()),
        ));
    }
    Command::new(binary)
        .args(["--window-server", ENDPOINT])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn transition() -> UiTransition {
    UiTransition {
        delay_ms: 0,
        duration_ms: 400,
        easing: neon_ui_schema::UiEasing::EaseOut,
        from: UiTransitionState {
            bounds: None,
            background_color: None,
            border_color: None,
            border_width: None,
            corner_radius: None,
            opacity: None,
            numeric_value: None,
        },
        motion_key: Some("probe.retarget".into()),
    }
}

fn fragment(sequence: u64) -> (UiFragment, f32) {
    let x = if sequence % 2 == 0 { 32.0 } else { 248.0 };
    let opacity = if sequence % 2 == 0 { 0.35 } else { 0.95 };
    let panel = UiNode {
        node_id: UiNodeId(NODE_KEY.into()),
        kind: UiNodeKind::Panel,
        bounds: UiBounds {
            x,
            y: 24.0,
            width: 120.0,
            height: 52.0,
        },
        layout: None,
        visible: true,
        enabled: true,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: UiStyle {
            background_color: [0.1, 0.65, 0.72, 1.0],
            opacity,
            ..UiStyle::default()
        },
        enter_transition: Some(transition()),
        world_depth: None,
        world_scale: None,
        clip_shape: UiClipShape::default(),
        children: Vec::new(),
    };
    let root = UiNode {
        node_id: UiNodeId("root".into()),
        kind: UiNodeKind::Panel,
        bounds: UiBounds {
            x: 0.0,
            y: 0.0,
            // Match the window runtime's default client size so the probe does
            // not introduce a resize/epoch transition while measuring
            // retargeting itself.
            width: 1280.0,
            height: 1024.0,
        },
        layout: None,
        visible: true,
        enabled: true,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: UiStyle {
            background_color: [0.025, 0.04, 0.06, 1.0],
            ..UiStyle::default()
        },
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        clip_shape: UiClipShape::default(),
        children: vec![panel],
    };
    (
        UiFragment {
            fragment_id: UiFragmentId(FRAGMENT_KEY.into()),
            revision: Revision(sequence + 1),
            root,
            effects: Vec::new(),
        },
        x,
    )
}

fn submit(sequence: u64) -> Result<(), String> {
    let (fragment, _) = fragment(sequence);
    call(
        "wgpu.ui.submit_fragment",
        1000 + sequence,
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment),
        }),
    )?;
    Ok(())
}

fn health_until(deadline: Instant, child: &mut Child) -> Result<(), String> {
    let mut sequence = 1;
    loop {
        match call("service.health", sequence, json!({})) {
            Ok(_) => return Ok(()),
            Err(error) if Instant::now() < deadline => {
                if child
                    .try_wait()
                    .map_err(|wait| wait.to_string())?
                    .is_some()
                {
                    return Err(format!("runtime exited before health: {error}"));
                }
                sequence += 1;
                thread::sleep(Duration::from_millis(80));
            }
            Err(error) => return Err(format!("health timeout: {error}")),
        }
    }
}

fn active_transition(snapshot: &Value) -> Option<&Value> {
    let window = snapshot.get("window").unwrap_or(snapshot);
    window
        .get("active_transitions")?
        .get("transitions")?
        .as_array()?
        .iter()
        .find(|transition| transition.get("node_key").and_then(Value::as_str) == Some(NODE_PATH))
}

fn window_snapshot(snapshot: &Value) -> &Value {
    snapshot.get("window").unwrap_or(snapshot)
}

fn snapshot(sequence: u64) -> Result<Value, String> {
    call("debug.snapshot.get", 2000 + sequence, json!({}))
}

fn number(value: &Value, path: &str) -> Option<f64> {
    let mut cursor = value;
    for segment in path.split('.') {
        cursor = cursor.get(segment)?;
    }
    cursor.as_f64()
}

fn close_enough(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => (left - right).abs() <= EPSILON,
        _ => false,
    }
}

fn emit(record: Value) {
    println!("{}", serde_json::to_string(&record).expect("probe record serializes"));
}

fn fail(probe_start: Instant, sequence: u64, input: Value, error: String) -> ! {
    emit(json!({
        "probe": "animation-retarget.v1",
        "sequence": sequence,
        "input": input,
        "producer": Value::Null,
        "consumer": Value::Null,
        "pairing": {"status": "unavailable"},
        "result": "failed",
        "pass_result": false,
        "error": error,
        "elapsed_ms": probe_start.elapsed().as_secs_f64() * 1000.0,
    }));
    std::process::exit(1)
}

fn main() {
    let probe_start = Instant::now();
    let endpoint: SocketAddr = ENDPOINT.parse().expect("fixed endpoint");
    let mut child = match launch() {
        Ok(child) => child,
        Err(error) => fail(
            probe_start,
            0,
            json!({"endpoint": endpoint}),
            error.to_string(),
        ),
    };

    let result = (|| -> Result<(), String> {
        health_until(probe_start + HEALTH_TIMEOUT, &mut child)?;
        // `service.health` is served by the control thread before winit has
        // necessarily completed GPU/window initialization. Give the event
        // loop a bounded startup window before asking for compositor-owned
        // diagnostics; later polling remains bounded as well.
        thread::sleep(WINDOW_STARTUP_GRACE);
        emit(json!({
            "probe": "animation-retarget.v1",
            "sequence": 0,
            "input": {"endpoint": ENDPOINT, "update_count": UPDATE_COUNT},
            "producer": {"service": "neon-wgpu-runtime", "status": "healthy"},
            "consumer": Value::Null,
            "pairing": {"status": "ready"},
            "result": "passed",
            "pass_result": true,
        }));

        let mut previous_transition_id = 0_u64;
        let mut last_target_x = 0.0_f64;
        let mut snapshot_sequence = 3000_u64;
        for sequence in 0..=UPDATE_COUNT {
            let (input_fragment, target_x) = fragment(sequence);
            submit(sequence)?;
            let deadline = Instant::now() + OBSERVE_TIMEOUT;
            let mut observed = None;
            while Instant::now() < deadline {
                let current = snapshot(snapshot_sequence)?;
                snapshot_sequence += 1;
                if let Some(transition) = active_transition(&current) {
                    let transition_id = transition
                        .get("transition_id")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if transition_id > previous_transition_id {
                        observed = Some((current, transition_id));
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(8));
            }
            let Some((current, transition_id)) = observed else {
                return Err(format!(
                    "no new transition observed for update {sequence}"
                ));
            };
            let transition = active_transition(&current).expect("observed transition exists");
            let window = window_snapshot(&current);
            let frame_sequence = window
                .get("active_transitions")
                .and_then(|active| active.get("frame"))
                .and_then(|frame| frame.get("frame_sequence"))
                .and_then(Value::as_u64);
            let source_frame_sequence = transition
                .get("source_frame_sequence")
                .and_then(Value::as_u64);
            let from_x = number(transition, "from.bounds.x");
            let retarget_source_x = number(transition, "retarget_source.bounds.x");
            let sampled_x = number(transition, "sampled.bounds.x");
            let target_debug_x = number(transition, "target.bounds.x");
            let generation = transition
                .get("identity")
                .and_then(|identity| identity.get("generation"))
                .and_then(Value::as_u64);
            let monotonic = transition_id > previous_transition_id;
            let source_matches_new_from = if sequence == 0 {
                true
            } else {
                close_enough(from_x, retarget_source_x)
            };
            let frame_paired = source_frame_sequence
                .zip(frame_sequence)
                .is_some_and(|(producer, consumer)| consumer >= producer);
            let pass = monotonic
                && source_matches_new_from
                && close_enough(target_debug_x, Some(target_x as f64))
                && sampled_x.is_some()
                && generation == Some(1)
                && frame_paired
                && child.try_wait().map_err(|error| error.to_string())?.is_none();
            emit(json!({
                "probe": "animation-retarget.v1",
                "sequence": sequence + 1,
                "input": {
                    "node_id": NODE_KEY,
                    "target": {
                        "bounds": {"x": target_x, "y": input_fragment.root.children[0].bounds.y},
                        "opacity": input_fragment.root.children[0].style.opacity,
                    },
                    "reason": if sequence == 0 { "enter" } else { "retarget" },
                },
                "producer": {
                    "program_revision": transition.get("program_revision"),
                    "source_frame_sequence": source_frame_sequence,
                    "transition_id": transition_id,
                    "node_generation": generation,
                    "animation_epoch": transition.get("animation_epoch"),
                    "reason": transition.get("reason"),
                },
                "consumer": {
                    "sampled_from": transition.get("retarget_source"),
                    "new_from": transition.get("from"),
                    "sampled": transition.get("sampled"),
                    "target": transition.get("target"),
                    "render_status": if pass { "accepted" } else { "rejected" },
                },
                "pairing": {
                    "producer_frame": source_frame_sequence,
                    "consumer_frame": frame_sequence,
                    "status": if frame_paired { "matched" } else { "mismatch" },
                },
                "result": if pass { "passed" } else { "failed" },
                "pass_result": pass,
            }));
            if !pass {
                return Err(format!(
                    "retarget assertion failed at update {sequence}: transition_id={transition_id}, from_x={from_x:?}, sampled_from_x={retarget_source_x:?}, target_x={target_debug_x:?}, frame={source_frame_sequence:?}->{frame_sequence:?}"
                ));
            }
            previous_transition_id = transition_id;
            last_target_x = target_x as f64;
            thread::sleep(UPDATE_INTERVAL);
        }

        let final_deadline = Instant::now() + FINAL_TIMEOUT;
        let mut final_pass = false;
        let mut final_observation = json!({});
        while Instant::now() < final_deadline {
            let current = snapshot(snapshot_sequence)?;
            snapshot_sequence += 1;
            let window = window_snapshot(&current);
            let active_count = window
                .get("active_transitions")
                .and_then(|active| active.get("count"))
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            let frame_node = window
                .get("active_transitions")
                .and_then(|active| active.get("frame"))
                .and_then(|frame| frame.get("nodes"))
                .and_then(Value::as_array)
                .and_then(|nodes| {
                    nodes.iter().find(|node| {
                        node.get("node_id").and_then(Value::as_str) == Some(NODE_PATH)
                    })
                });
            let final_x = frame_node.and_then(|node| number(node, "visual.bounds.x"));
            final_observation = json!({
                "active_count": active_count,
                "final_x": final_x,
                "expected_x": last_target_x,
                "frame_node": frame_node,
            });
            final_pass = active_count == 0 && close_enough(final_x, Some(last_target_x));
            if final_pass {
                emit(json!({
                    "probe": "animation-retarget.v1",
                    "sequence": UPDATE_COUNT + 2,
                    "input": {"final_target_x": last_target_x},
                    "producer": {"active_count": active_count},
                    "consumer": {"final_sampled_x": final_x},
                    "pairing": {"status": "matched"},
                    "result": "passed",
                    "pass_result": true,
                }));
                break;
            }
            thread::sleep(Duration::from_millis(16));
        }
        if !final_pass {
            return Err(format!(
                "final target was not committed after transition completion: {final_observation}"
            ));
        }
        Ok(())
    })();

    let _ = child.kill();
    let _ = child.wait();
    if let Err(error) = result {
        fail(probe_start, UPDATE_COUNT + 3, json!({"endpoint": ENDPOINT}), error);
    }
}
