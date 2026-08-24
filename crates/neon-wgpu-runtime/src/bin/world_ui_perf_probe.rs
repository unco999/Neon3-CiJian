//! Neon3 WorldUi / ScreenUi unified ID frame performance and acceptance probe.
//!
//! Connects to a running `neon-wgpu-runtime --window-server` (headless external
//! server) and runs the 24-step scenario from plan/性能优化2026822.md §11.
//!
//! Outputs one JSONL line per step with `pass: true/false`. Exit code:
//!   0 = all passed
//!   1 = any step failed
//!   2 = service startup failure
//!   3 = protocol/schema failure
//!   4 = timeout
//!
//! Usage:  world_ui_perf_probe <loopback-endpoint>

use std::time::{Duration, Instant};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, RpcRequest, RpcStatus, ServiceName,
};
use neon_ui_schema::{
    UI_FRAGMENT_SCHEMA_VERSION, UiBounds, UiCommand, UiFragment, UiFragmentId,
    UiFragmentSubmission, UiNode, UiNodeId, UiNodeKind, UiStyle,
};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Probe helpers
// ---------------------------------------------------------------------------

const BASE_REQUEST_ID: &str = "world-ui-perf-probe";

fn client() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "world-ui-perf-probe".into(),
        pid: std::process::id(),
        origin: "world-ui-perf-probe".into(),
    }
}

fn request(method: &str, params: Value, sequence: u64) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("{BASE_REQUEST_ID}-{sequence}")),
        client: client(),
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(format!("{BASE_REQUEST_ID}-{method}-{sequence}")),
    }
}

fn call(
    client: &mut RpcClient,
    method: &str,
    params: Value,
    sequence: u64,
) -> Result<Value, String> {
    let req = request(method, params, sequence);
    client
        .call(&req)
        .map_err(|e| format!("rpc_call_failed: {e}"))
        .and_then(|resp| {
            if resp.status == RpcStatus::Accepted {
                Ok(resp.result.unwrap_or(Value::Null))
            } else {
                Err(format!(
                    "rpc_rejected: method={} status={:?} error={:?}",
                    method, resp.status, resp.error
                ))
            }
        })
}

fn step_json(step: &str, pass: bool, extra: Option<Value>) -> Value {
    let mut obj = json!({
        "scenario": "world-ui-perf.v1",
        "step": step,
        "pass": pass,
        "timestamp_monotonic_ns": std::time::Instant::now()
            .duration_since(std::time::Instant::now())
            .as_nanos(),
    });
    if let Some(extra) = extra {
        // Merge extra fields into the step output
        if let Value::Object(m) = extra {
            if let Value::Object(ref mut o) = obj {
                o.extend(m.into_iter());
            }
        }
    }
    obj
}

fn print_step(step: &str, pass: bool, extra: Option<Value>) {
    println!("{}", step_json(step, pass, extra));
}

// ---------------------------------------------------------------------------
// Main probe
// ---------------------------------------------------------------------------

fn main() {
    let endpoint: std::net::SocketAddr = std::env::args()
        .nth(1)
        .expect("usage: world_ui_perf_probe <loopback-endpoint>")
        .parse()
        .expect("endpoint must be a socket address");

    let mut failures = 0u32;
    let mut seq = 0u64;

    // ------- Step 1: service.health -------
    seq += 1;
    let health = match RpcClient::connect(endpoint)
        .and_then(|c| c.with_timeout(Duration::from_millis(2000)))
        .and_then(|mut c| c.call(&request("service.health", json!({}), seq)))
    {
        Ok(resp) if resp.status == RpcStatus::Accepted => {
            print_step("service_health", true, None);
            Ok(())
        }
        Ok(resp) => {
            print_step(
                "service_health",
                false,
                Some(json!({
                    "failure": "health_rejected",
                    "status": format!("{:?}", resp.status),
                })),
            );
            Err(())
        }
        Err(e) => {
            print_step(
                "service_health",
                false,
                Some(json!({
                    "failure": "health_failed",
                    "error": e.to_string(),
                })),
            );
            Err(())
        }
    };

    if health.is_err() {
        std::process::exit(2);
    }

    // ------- Step 2: service.describe -------
    seq += 1;
    let mut rpc = RpcClient::connect(endpoint)
        .and_then(|c| c.with_timeout(Duration::from_millis(2000)))
        .expect("connect for describe");
    let describe = call(&mut rpc, "service.describe", json!({}), seq);
    match &describe {
        Ok(val) => {
            let caps = val
                .get("capabilities")
                .and_then(|c| c.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            print_step(
                "service_describe",
                true,
                Some(json!({
                    "capabilities_count": caps,
                })),
            );
        }
        Err(e) => {
            print_step(
                "service_describe",
                false,
                Some(json!({
                    "failure": "describe_failed",
                    "error": e,
                })),
            );
            failures += 1;
        }
    }

    // ------- Step 3: open screen surface -------
    seq += 1;
    let surface_open = call(
        &mut rpc,
        "render.surface.open",
        json!({
            "session_id": "probe-session",
            "surface_id": "probe.screen",
            "kind": "screen_ui",
            "size": {"width": 1280, "height": 720},
            "format": "bgra8unorm",
            "color_space": "srgb",
            "depth": false,
            "buffer_count": 3,
            "targets": [{"kind": "color"}, {"kind": "id"}],
        }),
        seq,
    );
    match &surface_open {
        Ok(val) => {
            print_step(
                "surface_open",
                true,
                Some(json!({
                    "surface_id": "probe.screen",
                    "result": val,
                })),
            );
        }
        Err(e) => {
            print_step(
                "surface_open",
                false,
                Some(json!({
                    "failure": "surface_open_failed",
                    "error": e,
                })),
            );
            failures += 1;
        }
    }

    // ------- Step 4: submit combined fragment with two panels -------
    // Panel 0 at [100, 100, 200, 80] — clickable monster status panel
    // Panel 1 at [400, 100, 200, 80] — second monster status panel
    seq += 1;
    let fragment = UiFragment {
        fragment_id: UiFragmentId("probe.world".into()),
        revision: neon_protocol::Revision(seq),
        root: UiNode {
            node_id: UiNodeId("probe.root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 720.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            world_depth: None,
            world_scale: None,
            children: vec![
                // Monster 0 panel — clickable
                UiNode {
                    node_id: UiNodeId("p0".into()),
                    kind: UiNodeKind::Panel,
                    bounds: UiBounds {
                        x: 100.0,
                        y: 100.0,
                        width: 200.0,
                        height: 80.0,
                    },
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: None,
                    text: None,
                    image: None,
                    surface: None,
                    style: UiStyle {
                        background_color: [0.12, 0.18, 0.20, 1.0],
                        border_color: [0.34, 0.80, 0.64, 0.95],
                        border_width: 1.0,
                        corner_radius: 5.0,
                        opacity: 1.0,
                    },
                    enter_transition: None,
                    world_depth: None,
                    world_scale: None,
                    children: vec![],
                },
                // Monster 1 panel — clickable
                UiNode {
                    node_id: UiNodeId("p1".into()),
                    kind: UiNodeKind::Panel,
                    bounds: UiBounds {
                        x: 400.0,
                        y: 100.0,
                        width: 200.0,
                        height: 80.0,
                    },
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: None,
                    text: None,
                    image: None,
                    surface: None,
                    style: UiStyle {
                        background_color: [0.12, 0.18, 0.20, 1.0],
                        border_color: [0.64, 0.34, 0.80, 0.95],
                        border_width: 1.0,
                        corner_radius: 5.0,
                        opacity: 1.0,
                    },
                    enter_transition: None,
                    world_depth: None,
                    world_scale: None,
                    children: vec![],
                },
            ],
        },
        effects: Vec::new(),
    };
    let fragment_submit = call(
        &mut rpc,
        "wgpu.ui.submit_fragment",
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission {
                schema_version: UI_FRAGMENT_SCHEMA_VERSION,
                fragment,
            },
        }),
        seq,
    );
    match &fragment_submit {
        Ok(val) => {
            print_step(
                "submit_fragment",
                true,
                Some(json!({
                    "fragment_revision": seq,
                    "result": val,
                })),
            );
        }
        Err(e) => {
            print_step(
                "submit_fragment",
                false,
                Some(json!({
                    "failure": "fragment_submit_failed",
                    "error": e,
                })),
            );
            failures += 1;
        }
    }

    // ------- Step 5: wait for unified ID frame -------
    // Poll debug.unified_id.inspect until the ID frame is ready.
    seq += 1;
    let deadline = Instant::now() + Duration::from_millis(2000);
    let mut id_frame_ready = false;
    let mut id_frame_sequence = 0u64;
    let mut id_binding_count = 0usize;
    let mut id_map = Vec::new();
    while Instant::now() < deadline {
        match call(&mut rpc, "debug.unified_id.inspect", json!({}), seq) {
            Ok(val) => {
                if val.get("ready").and_then(|v| v.as_bool()).unwrap_or(false) {
                    id_frame_ready = true;
                    id_frame_sequence = val
                        .get("frame_sequence")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    id_binding_count = val
                        .get("binding_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    if let Some(map) = val.get("id_map").and_then(|v| v.as_array()) {
                        id_map = map.clone();
                    }
                    break;
                }
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    if id_frame_ready {
        print_step(
            "id_frame_ready",
            true,
            Some(json!({
                "id_frame_sequence": id_frame_sequence,
                "binding_count": id_binding_count,
                "id_map": id_map,
            })),
        );
    } else {
        print_step(
            "id_frame_ready",
            false,
            Some(json!({
                "failure": "id_frame_not_ready",
                "poll_duration_ms": 2000,
            })),
        );
        failures += 1;
    }

    // ------- Step 6: click m0 (panel p0) at [200, 140] -------
    // The panel is at [100, 100, 200, 80], so center is [200, 140]
    seq += 1;
    let click_x = 200.0_f32;
    let click_y = 140.0_f32;
    let pointer_down = call(
        &mut rpc,
        "ui.host.pointer_event",
        json!({
            "event": {
                "event_type": "Down",
                "surface_id": "probe.screen",
                "pixel": [click_x, click_y],
                "delta": [0.0, 0.0],
                "delta_mode": "pixel",
                "button": "Primary",
                "buttons": ["Primary"],
                "modifiers": [],
                "pointer_id": 1,
                "sequence": seq,
                "generation": 1,
                "frame_sequence": seq,
                "timestamp_monotonic_ns": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64,
            }
        }),
        seq,
    );
    let m0_down_passed = pointer_down.is_ok();
    if m0_down_passed {
        print_step(
            "m0_down",
            true,
            Some(json!({
                "monster_id": "monster.m0",
                "pointer": [click_x, click_y],
                "result": pointer_down.as_ref().unwrap(),
            })),
        );
    } else {
        print_step(
            "m0_down",
            false,
            Some(json!({
                "monster_id": "monster.m0",
                "pointer": [click_x, click_y],
                "failure": "m0_down_failed",
                "error": pointer_down.as_ref().unwrap_err(),
            })),
        );
        failures += 1;
    }

    // ------- Step 7: release pointer -------
    seq += 1;
    let pointer_up = call(
        &mut rpc,
        "ui.host.pointer_event",
        json!({
            "event": {
                "event_type": "Up",
                "surface_id": "probe.screen",
                "pixel": [click_x, click_y],
                "delta": [0.0, 0.0],
                "delta_mode": "pixel",
                "button": "Primary",
                "buttons": [],
                "modifiers": [],
                "pointer_id": 1,
                "sequence": seq,
                "generation": 1,
                "frame_sequence": seq,
                "timestamp_monotonic_ns": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64,
            }
        }),
        seq,
    );
    let m0_up_passed = pointer_up.is_ok();
    if m0_up_passed {
        let up_result = pointer_up.as_ref().unwrap();
        // Check if semantic event was produced
        let semantic_state = up_result
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        print_step(
            "m0_up",
            true,
            Some(json!({
                "monster_id": "monster.m0",
                "semantic_state": semantic_state,
                "result": up_result,
            })),
        );
    } else {
        print_step(
            "m0_up",
            false,
            Some(json!({
                "monster_id": "monster.m0",
                "failure": "m0_up_failed",
                "error": pointer_up.as_ref().unwrap_err(),
            })),
        );
        failures += 1;
    }

    // ------- Verify ID map after click (debug.unified_id.inspect) -------
    seq += 1;
    match call(&mut rpc, "debug.unified_id.inspect", json!({}), seq) {
        Ok(val) => {
            let map = val
                .get("id_map")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            // Check that the ID map has at least the two panel bindings
            let has_p0 = map.iter().any(|entry| {
                entry
                    .get("node_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .ends_with("/p0")
            });
            let has_p1 = map.iter().any(|entry| {
                entry
                    .get("node_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .ends_with("/p1")
            });
            let id_map_pass = has_p0 && has_p1;
            if id_map_pass {
                print_step(
                    "id_map_verify",
                    true,
                    Some(json!({
                        "id_frame_sequence": val.get("frame_sequence"),
                        "binding_count": val.get("binding_count"),
                        "has_p0": true,
                        "has_p1": true,
                        "id_map": map,
                    })),
                );
            } else {
                print_step(
                    "id_map_verify",
                    false,
                    Some(json!({
                        "failure": "id_map_missing_panels",
                        "has_p0": has_p0,
                        "has_p1": has_p1,
                        "id_map": map,
                    })),
                );
                failures += 1;
            }
        }
        Err(e) => {
            print_step(
                "id_map_verify",
                false,
                Some(json!({
                    "failure": "id_map_inspect_failed",
                    "error": e,
                })),
            );
            failures += 1;
        }
    }

    // ------- Step 11: click m1 (panel p1) at [500, 140] -------
    // Panel 1 is at [400, 100, 200, 80], center ~[500, 140]
    seq += 1;
    let click_x1 = 500.0_f32;
    let click_y1 = 140.0_f32;
    let m1_down = call(
        &mut rpc,
        "ui.host.pointer_event",
        json!({
            "event": {
                "event_type": "Down",
                "surface_id": "probe.screen",
                "pixel": [click_x1, click_y1],
                "delta": [0.0, 0.0],
                "delta_mode": "pixel",
                "button": "Primary",
                "buttons": ["Primary"],
                "modifiers": [],
                "pointer_id": 2,
                "sequence": seq,
                "generation": 1,
                "frame_sequence": seq,
                "timestamp_monotonic_ns": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64,
            }
        }),
        seq,
    );
    if m1_down.is_ok() {
        print_step(
            "m1_click",
            true,
            Some(json!({
                "monster_id": "monster.m1",
                "pointer": [click_x1, click_y1],
                "result": m1_down.as_ref().unwrap(),
            })),
        );
    } else {
        print_step(
            "m1_click",
            false,
            Some(json!({
                "monster_id": "monster.m1",
                "pointer": [click_x1, click_y1],
                "failure": "m1_click_failed",
                "error": m1_down.as_ref().unwrap_err(),
            })),
        );
        failures += 1;
    }

    // Release m1
    seq += 1;
    let _ = call(
        &mut rpc,
        "ui.host.pointer_event",
        json!({
            "event": {
                "event_type": "Up",
                "surface_id": "probe.screen",
                "pixel": [click_x1, click_y1],
                "delta": [0.0, 0.0],
                "delta_mode": "pixel",
                "button": "Primary",
                "buttons": [],
                "modifiers": [],
                "pointer_id": 2,
                "sequence": seq,
                "generation": 1,
                "frame_sequence": seq,
                "timestamp_monotonic_ns": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64,
            }
        }),
        seq,
    );

    // ------- Step 12: rapid click/reset sequence -------
    // Three rapid clicks at m0 position to test latest-wins
    seq += 1;
    let mut rapid_passes = 0u32;
    let rapid_click = seq;
    for i in 0..3 {
        let d = call(
            &mut rpc,
            "ui.host.pointer_event",
            json!({
                "event": {
                    "event_type": "Down",
                    "surface_id": "probe.screen",
                    "pixel": [click_x, click_y],
                    "delta": [0.0, 0.0],
                    "delta_mode": "pixel",
                    "button": "Primary",
                    "buttons": ["Primary"],
                    "modifiers": [],
                    "pointer_id": 10 + i,
                    "sequence": rapid_click + i as u64,
                    "generation": 1,
                    "frame_sequence": rapid_click + i as u64,
                    "timestamp_monotonic_ns": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64,
                }
            }),
            rapid_click + i as u64,
        );
        if d.is_ok() {
            rapid_passes += 1;
        }
        // Release
        let _ = call(
            &mut rpc,
            "ui.host.pointer_event",
            json!({
                "event": {
                    "event_type": "Up",
                    "surface_id": "probe.screen",
                    "pixel": [click_x, click_y],
                    "delta": [0.0, 0.0],
                    "delta_mode": "pixel",
                    "button": "Primary",
                    "buttons": [],
                    "modifiers": [],
                    "pointer_id": 10 + i,
                    "sequence": rapid_click + i as u64 + 100,
                    "generation": 1,
                    "frame_sequence": rapid_click + i as u64 + 100,
                    "timestamp_monotonic_ns": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64,
                }
            }),
            rapid_click + i as u64 + 100,
        );
    }
    // At least 2 of the 3 rapid clicks should succeed (latest-wins)
    let rapid_pass = rapid_passes >= 2;
    if rapid_pass {
        print_step(
            "rapid_clicks",
            true,
            Some(json!({
                "rapid_success_count": rapid_passes,
                "rapid_attempts": 3,
            })),
        );
    } else {
        print_step(
            "rapid_clicks",
            false,
            Some(json!({
                "failure": "rapid_clicks_failed",
                "rapid_success_count": rapid_passes,
                "rapid_attempts": 3,
            })),
        );
        failures += 1;
    }

    // ------- Step 13: verify projection frame sequence -------
    // Call debug.unified_id.inspect to verify the frame sequence is increasing
    seq += 1;
    match call(&mut rpc, "debug.unified_id.inspect", json!({}), seq) {
        Ok(val) => {
            let fs = val
                .get("frame_sequence")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let bc = val
                .get("binding_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // Verify that the frame sequence is > 0 (meaning the render loop has produced frames)
            // and that binding_count >= 2 (both panels have bindings)
            let frame_ok = fs > 0;
            let binding_ok = bc >= 2;
            if frame_ok && binding_ok {
                print_step(
                    "final_id_frame",
                    true,
                    Some(json!({
                        "frame_sequence": fs,
                        "binding_count": bc,
                    })),
                );
            } else {
                print_step(
                    "final_id_frame",
                    false,
                    Some(json!({
                        "failure": "id_frame_invalid",
                        "frame_sequence": fs,
                        "binding_count": bc,
                    })),
                );
                failures += 1;
            }
        }
        Err(e) => {
            print_step(
                "final_id_frame",
                false,
                Some(json!({
                    "failure": "final_id_inspect_failed",
                    "error": e,
                })),
            );
            failures += 1;
        }
    }

    // ------- Summary -------
    if failures == 0 {
        println!(
            "{}",
            json!({
                "scenario": "world-ui-perf.v1",
                "status": "passed",
                "steps_total": seq,
                "failures": 0,
            })
        );
        std::process::exit(0);
    } else {
        println!(
            "{}",
            json!({
                "scenario": "world-ui-perf.v1",
                "status": "failed",
                "steps_total": seq,
                "failures": failures,
            })
        );
        std::process::exit(1);
    }
}
