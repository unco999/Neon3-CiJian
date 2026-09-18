//! Focused WGPU interaction probe for completed built-in toggle behavior.
//!
//! Usage:
//!   component_interaction_probe <loopback-endpoint>
//!
//! The probe talks to the real headless GPU runtime through neon3.rpc, submits
//! a minimal Switch fragment, sends a logical Down/Up pair, and emits JSONL
//! records containing producer input, consumer response, and pass_result.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, RpcRequest, RpcStatus, ServiceName,
};
use neon_ui_schema::{
    UiBounds, UiClipShape, UiCommand, UiControlPresentation, UiEffect, UiFragment, UiFragmentId,
    UiFragmentSubmission, UiIntent, UiNode, UiNodeId, UiNodeKind, UiPointerButton,
    UiPointerDeltaMode, UiPointerEventType, UiStyle, UiTransform,
};
use serde_json::{json, Value};

const PROBE: &str = "component-interaction.v1";

fn client() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "component-interaction-probe".into(),
        pid: std::process::id(),
        origin: "component-interaction-probe".into(),
    }
}

fn request(method: &str, sequence: u64, params: Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("component-interaction-{sequence}")),
        client: client(),
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(format!("component-interaction-{method}-{sequence}")),
    }
}

fn call(rpc: &mut RpcClient, method: &str, sequence: u64, params: Value) -> Result<Value, String> {
    let response = rpc
        .call(&request(method, sequence, params))
        .map_err(|error| error.to_string())?;
    if response.status != RpcStatus::Accepted {
        return Err(format!("{method} rejected: {:?}", response.error));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn emit(
    sequence: u64,
    method: &str,
    input: Value,
    consumer: Value,
    pass_result: bool,
    error: Option<String>,
) {
    println!(
        "{}",
        json!({
            "probe": PROBE,
            "sequence": sequence,
            "method": method,
            "input": input.clone(),
            "producer": input,
            "consumer": consumer,
            "frame_pairing": {"pointer_sequence": sequence},
            "error": error,
            "result": if pass_result { "passed" } else { "failed" },
            "pass_result": pass_result,
        })
    );
}

fn timestamp_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn switch_fragment() -> UiFragment {
    let switch = UiNode {
        node_id: UiNodeId("agent-switch".into()),
        kind: UiNodeKind::Switch,
        bounds: UiBounds {
            x: 20.0,
            y: 20.0,
            width: 220.0,
            height: 32.0,
        },
        layout: None,
        visible: true,
        enabled: true,
        text_key: None,
        text: Some(neon_ui_schema::TextRef::Literal {
            value: "Agent mode".into(),
        }),
        image: None,
        surface: None,
        style: UiStyle::default(),
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        clip_shape: UiClipShape::default(),
        children: Vec::new(),
    };
    let root = UiNode {
        node_id: UiNodeId("probe-root".into()),
        kind: UiNodeKind::Panel,
        bounds: UiBounds {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 180.0,
        },
        layout: None,
        visible: true,
        enabled: true,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: UiStyle {
            transform: UiTransform::default(),
            ..UiStyle::default()
        },
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        clip_shape: UiClipShape::default(),
        children: {
            let mut children = vec![switch, tree_node()];
            children.extend(splitter_fixture());
            children
        },
    };
    UiFragment {
        fragment_id: UiFragmentId("component-interaction".into()),
        revision: neon_protocol::Revision(1),
        root,
        effects: vec![
            UiEffect::ControlPresentation {
                node_id: UiNodeId("agent-switch".into()),
                state: UiControlPresentation::Toggle { selected: true },
            },
            UiEffect::BoundSemanticIntent {
                node_id: UiNodeId("agent-switch".into()),
                intent: UiIntent::Invoke {
                    action: "agent.mode.toggle".into(),
                    params: json!({}),
                },
            },
            UiEffect::BoundSemanticIntent {
                node_id: UiNodeId("tree-root".into()),
                intent: UiIntent::Invoke {
                    action: "tree.expand.toggle".into(),
                    params: json!({}),
                },
            },
            UiEffect::BoundSemanticIntent {
                node_id: UiNodeId("ide-splitter".into()),
                intent: UiIntent::Invoke {
                    action: "layout.splitter.commit".into(),
                    params: json!({}),
                },
            },
        ],
    }
}

fn tree_node() -> UiNode {
    let mut tree = UiNode {
        node_id: UiNodeId("agent-tree".into()),
        kind: UiNodeKind::TreeView,
        bounds: UiBounds {
            x: 250.0,
            y: 20.0,
            width: 130.0,
            height: 100.0,
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
        clip_shape: UiClipShape::default(),
        children: Vec::new(),
    };
    let child = UiNode {
        node_id: UiNodeId("tree-root".into()),
        kind: UiNodeKind::Label,
        bounds: UiBounds {
            x: 8.0,
            y: 8.0,
            width: 110.0,
            height: 24.0,
        },
        layout: None,
        visible: true,
        enabled: true,
        text_key: None,
        text: Some(neon_ui_schema::TextRef::Literal {
            value: "project/".into(),
        }),
        image: None,
        surface: None,
        style: UiStyle::default(),
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        clip_shape: UiClipShape::default(),
        children: Vec::new(),
    };
    tree.children.push(child);
    tree
}

fn splitter_fixture() -> Vec<UiNode> {
    let mut left = leaf_node("split-left", 20.0, 100.0, 160.0, 60.0);
    let mut splitter = leaf_node("ide-splitter", 180.0, 100.0, 8.0, 60.0);
    let mut right = leaf_node("split-right", 188.0, 100.0, 182.0, 60.0);
    left.kind = UiNodeKind::Panel;
    splitter.kind = UiNodeKind::Splitter;
    right.kind = UiNodeKind::Panel;
    vec![left, splitter, right]
}

fn leaf_node(id: &str, x: f32, y: f32, width: f32, height: f32) -> UiNode {
    UiNode {
        node_id: UiNodeId(id.into()),
        kind: UiNodeKind::Panel,
        bounds: UiBounds {
            x,
            y,
            width,
            height,
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
        clip_shape: UiClipShape::default(),
        children: Vec::new(),
    }
}

fn pointer_event(event_type: UiPointerEventType, sequence: u64, pixel: [f32; 2]) -> Value {
    let pressed = matches!(event_type, UiPointerEventType::Down);
    json!({
        "event": {
            "event_type": event_type,
            "surface_id": "component.screen",
            "pixel": pixel,
            "delta": [0.0, 0.0],
            "delta_mode": UiPointerDeltaMode::Pixel,
            "button": UiPointerButton::Primary,
            "buttons": if pressed { vec![UiPointerButton::Primary] } else { Vec::new() },
            "modifiers": Vec::<String>::new(),
            "pointer_id": 1,
            "sequence": sequence,
            "generation": 1,
            "frame_sequence": sequence,
            "timestamp_monotonic_ns": timestamp_ns(),
        }
    })
}

fn main() {
    let endpoint: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:43321".into())
        .parse()
        .expect("endpoint must be a loopback socket address");
    let mut failed = false;
    let mut sequence = 1_u64;
    let mut rpc = match RpcClient::connect(endpoint)
        .and_then(|client| client.with_timeout(Duration::from_secs(15)))
    {
        Ok(client) => client,
        Err(error) => {
            emit(
                sequence,
                "service.health",
                json!({"endpoint": endpoint}),
                Value::Null,
                false,
                Some(error.to_string()),
            );
            std::process::exit(2);
        }
    };

    let health = call(&mut rpc, "service.health", sequence, json!({}));
    let health_pass = health.is_ok();
    emit(
        sequence,
        "service.health",
        json!({"endpoint": endpoint}),
        health.clone().unwrap_or(Value::Null),
        health_pass,
        health.err(),
    );
    failed |= !health_pass;

    sequence += 1;
    let open = call(
        &mut rpc,
        "render.surface.open",
        sequence,
        json!({
            "session_id": "component-interaction-session",
            "surface_id": "component.screen",
            "kind": "screen_ui",
            "size": {"width": 400, "height": 180},
            "format": "rgba8unorm",
            "color_space": "srgb",
            "depth": false,
            "buffer_count": 2,
            "targets": [
                {"target_id": "component-color", "kind": "color", "format": "rgba8unorm"},
                {"target_id": "component-id", "kind": "id", "format": "r32uint"}
            ]
        }),
    );
    emit(
        sequence,
        "render.surface.open",
        json!({"surface_id": "component.screen", "size": [400, 180]}),
        open.clone().unwrap_or(Value::Null),
        open.is_ok(),
        open.clone().err(),
    );
    failed |= open.is_err();

    sequence += 1;
    let fragment = switch_fragment();
    let submit = call(
        &mut rpc,
        "wgpu.ui.submit_fragment",
        sequence,
        serde_json::to_value(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment),
        })
        .expect("fragment submission serializes"),
    );
    emit(
        sequence,
        "wgpu.ui.submit_fragment",
        json!({"fragment_id": "component-interaction", "node": "agent-switch", "selected_before": true}),
        submit.clone().unwrap_or(Value::Null),
        submit.is_ok(),
        submit.clone().err(),
    );
    failed |= submit.is_err();
    std::thread::sleep(Duration::from_millis(250));

    for (label, pixel) in [("switch", [120.0, 36.0]), ("tree", [300.0, 36.0])] {
        for event_type in [UiPointerEventType::Down, UiPointerEventType::Up] {
            sequence += 1;
            let event = pointer_event(event_type.clone(), sequence, pixel);
            let result = call(&mut rpc, "ui.host.pointer_event", sequence, event.clone());
            let pass = result.is_ok();
            let method = if matches!(event_type, UiPointerEventType::Down) {
                format!("{label}.pointer.down")
            } else {
                format!("{label}.pointer.up")
            };
            emit(
                sequence,
                &method,
                event,
                result.clone().unwrap_or(Value::Null),
                pass,
                result.err(),
            );
            failed |= !pass;
        }
    }

    for (label, event_type, pixel) in [
        (
            "splitter.pointer.down",
            UiPointerEventType::Down,
            [184.0, 130.0],
        ),
        (
            "splitter.pointer.move",
            UiPointerEventType::Move,
            [260.0, 130.0],
        ),
        (
            "splitter.pointer.up",
            UiPointerEventType::Up,
            [260.0, 130.0],
        ),
    ] {
        sequence += 1;
        let event = pointer_event(event_type, sequence, pixel);
        let result = call(&mut rpc, "ui.host.pointer_event", sequence, event.clone());
        let pass = result.as_ref().is_ok_and(|value| {
            if label.ends_with("up") {
                value
                    .get("semantic_event")
                    .and_then(|event| event.get("control_value"))
                    .and_then(|value| value.get("kind"))
                    .and_then(Value::as_str)
                    == Some("f32")
            } else {
                true
            }
        });
        emit(
            sequence,
            label,
            event,
            result.clone().unwrap_or(Value::Null),
            pass,
            if pass {
                None
            } else {
                Some(format!("unexpected splitter response: {:?}", result))
            },
        );
        failed |= !pass;
    }

    for (label, event_type, pixel) in [
        (
            "splitter.cancel.down",
            UiPointerEventType::Down,
            [184.0, 130.0],
        ),
        (
            "splitter.cancel.move",
            UiPointerEventType::Move,
            [300.0, 130.0],
        ),
        (
            "splitter.cancel.cancel",
            UiPointerEventType::Cancel,
            [300.0, 130.0],
        ),
    ] {
        sequence += 1;
        let event = pointer_event(event_type, sequence, pixel);
        let result = call(&mut rpc, "ui.host.pointer_event", sequence, event.clone());
        let pass = result.as_ref().is_ok_and(|value| {
            !label.ends_with("cancel")
                || value.get("state").and_then(Value::as_str) == Some("cancelled")
        });
        emit(
            sequence,
            label,
            event,
            result.clone().unwrap_or(Value::Null),
            pass,
            if pass {
                None
            } else {
                Some(format!("unexpected splitter cancel response: {:?}", result))
            },
        );
        failed |= !pass;
    }

    sequence += 1;
    let shutdown = call(&mut rpc, "service.shutdown", sequence, json!({}));
    emit(
        sequence,
        "service.shutdown",
        json!({}),
        shutdown.clone().unwrap_or(Value::Null),
        shutdown.is_ok(),
        shutdown.clone().err(),
    );
    failed |= shutdown.is_err();

    println!(
        "{}",
        json!({
            "probe": PROBE,
            "final": true,
            "pass_result": !failed,
            "completed_sequence": sequence,
        })
    );
    if failed {
        std::process::exit(1);
    }
}
