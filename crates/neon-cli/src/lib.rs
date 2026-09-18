//! Public protocol client helpers. This crate must not create windows or GPU objects.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use neon_ipc::{EventClient, RpcClient, TransportError};
use neon_protocol::{
    ClientIdentity, ClientKind, EventFilter, EventFrame, EventResponse, EventSubscribe,
    ProtocolVersion, RequestId, Revision, RpcRequest, RpcResponse, RpcStatus, ServiceName,
};
use neon_ui_schema::{
    TextRef, UiBounds, UiClipShape, UiCommand, UiEffect, UiFragment, UiFragmentId,
    UiFragmentRevision, UiFragmentSubmission, UiIntent, UiNode, UiNodeId, UiNodeKind,
    UiPointerMetadata, UiSemanticEvent, UiSemanticEventType, UiStyle,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub const SCENARIO_ID: &str = "ui.static-fragment.submit.v1";
pub const DETAIL_TOGGLE_SCENARIO_ID: &str = "ui.detail-toggle.v1";

/// Resolve the public protocol target for a method when the caller does not
/// provide an explicit service. Keep this table transport-independent so SDK
/// wrappers can use the same routing contract as the CLI.
pub fn default_target(method: &str) -> &'static str {
    match method {
        m if m.starts_with("wgpu.") => "wgpu-runtime",
        m if m.starts_with("render.") => "wgpu-runtime",
        m if m.starts_with("debug.window.") => "wgpu-runtime",
        m if m.starts_with("debug.interaction.") => "wgpu-runtime",
        m if m == "debug.snapshot.get" => "wgpu-runtime",
        m if m.starts_with("ui.") => "ui-runtime",
        m if m.starts_with("debug.ui.") => "ui-runtime",
        m if m.starts_with("debug.trace.")
            || m.starts_with("debug.command.")
            || m.starts_with("debug.journal.")
            || m.starts_with("debug.replay.") =>
        {
            "ui-runtime"
        }
        m if m.starts_with("event.") => "eventd",
        m if m.starts_with("editor.") => "editor-runtime",
        m if m.starts_with("project.")
            || m.starts_with("asset.")
            || m.starts_with("transaction.") =>
        {
            "neon-projectd"
        }
        m if m.starts_with("terrain.") => "neon-terrain-runtime",
        m if m.starts_with("resource.") => "neon-resource-runtime",
        _ => "ui-runtime",
    }
}

/// Read-only debug RPC commands exposed by the public CLI.
#[derive(Debug, PartialEq)]
pub enum DebugCommand {
    Snapshot {
        endpoint: SocketAddr,
    },
    SnapshotAggregate {
        manifest: String,
        service: Option<String>,
    },
    SnapshotDiff {
        endpoint: SocketAddr,
        service: Option<String>,
        before: String,
    },
    WaitRevision {
        endpoint: SocketAddr,
        service: Option<String>,
        target: RevisionTarget,
        timeout: Duration,
    },
    CommandGet {
        endpoint: SocketAddr,
        request_id: String,
    },
    TraceQuery {
        endpoint: SocketAddr,
        query: Value,
    },
    InputActivateTarget {
        endpoint: SocketAddr,
        semantic_node_path: String,
    },
    InteractionGet {
        endpoint: SocketAddr,
        interaction_id: String,
    },
    InteractionQuery {
        endpoint: SocketAddr,
        query: Value,
    },
    RenderCapture {
        endpoint: SocketAddr,
        path: String,
    },
    WorldUiCapture {
        endpoint: SocketAddr,
        path: String,
        size: [u32; 2],
    },
    WorldUiCamera {
        endpoint: SocketAddr,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum RevisionTarget {
    Absolute(u64),
    Delta(u64),
}

/// Escape hatch for methods that do not have a dedicated CLI wrapper yet.
#[derive(Debug, PartialEq)]
pub struct RpcCommand {
    pub endpoint: SocketAddr,
    pub method: String,
    pub service: Option<String>,
    pub params: Value,
    pub idempotency_key: Option<String>,
}

impl RpcCommand {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        if args.len() < 3 || args[0] != "rpc" {
            return Err(rpc_usage().into());
        }
        let method = args[1].clone();
        let mut endpoint = None;
        let mut service = None;
        let mut params = json!({});
        let mut idempotency_key = None;
        let mut index = 2;
        while index < args.len() {
            let flag = args[index].as_str();
            let value = args
                .get(index + 1)
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag {
                "--endpoint" => endpoint = Some(parse_endpoint(value)?),
                "--service" => service = Some(value.clone()),
                "--params-json" => {
                    params = serde_json::from_str(value)
                        .map_err(|error| format!("params must be JSON: {error}"))?;
                }
                "--idempotency-key" => idempotency_key = Some(value.clone()),
                _ => return Err(format!("unknown rpc option '{flag}'\n{}", rpc_usage())),
            }
            index += 2;
        }
        if !params.is_object() {
            return Err("params must be a JSON object".into());
        }
        Ok(Self {
            endpoint: endpoint.ok_or("rpc requires --endpoint <host:port>")?,
            method,
            service,
            params,
            idempotency_key,
        })
    }
}

pub fn rpc_usage() -> &'static str {
    "neon-cli rpc <method> --endpoint <endpoint> [--service <service>] [--params-json '{...}'] [--idempotency-key <key>]"
}

impl DebugCommand {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        match args {
            [debug, snapshot] if debug == "debug" && snapshot == "snapshot" => {
                Ok(Self::SnapshotAggregate {
                    manifest: default_manifest_path(),
                    service: None,
                })
            }
            [debug, snapshot, flag, manifest]
                if debug == "debug" && snapshot == "snapshot" && flag == "--manifest" =>
            {
                Ok(Self::SnapshotAggregate {
                    manifest: manifest.clone(),
                    service: None,
                })
            }
            [debug, snapshot, endpoint, diff_flag, before]
                if debug == "debug" && snapshot == "snapshot" && diff_flag == "--diff" =>
            {
                Ok(Self::SnapshotDiff {
                    endpoint: parse_endpoint(endpoint)?,
                    service: None,
                    before: before.clone(),
                })
            }
            [
                debug,
                snapshot,
                endpoint,
                service_flag,
                service,
                diff_flag,
                before,
            ] if debug == "debug"
                && snapshot == "snapshot"
                && service_flag == "--service"
                && diff_flag == "--diff" =>
            {
                Ok(Self::SnapshotDiff {
                    endpoint: parse_endpoint(endpoint)?,
                    service: Some(service.clone()),
                    before: before.clone(),
                })
            }
            [debug, snapshot, flag, service]
                if debug == "debug" && snapshot == "snapshot" && flag == "--service" =>
            {
                Ok(Self::SnapshotAggregate {
                    manifest: default_manifest_path(),
                    service: Some(service.clone()),
                })
            }
            [
                debug,
                snapshot,
                manifest_flag,
                manifest,
                service_flag,
                service,
            ] if debug == "debug"
                && snapshot == "snapshot"
                && ((manifest_flag == "--manifest" && service_flag == "--service")
                    || (manifest_flag == "--service" && service_flag == "--manifest")) =>
            {
                let (manifest, service) = if manifest_flag == "--manifest" {
                    (manifest.clone(), service.clone())
                } else {
                    (service.clone(), manifest.clone())
                };
                Ok(Self::SnapshotAggregate {
                    manifest,
                    service: Some(service),
                })
            }
            [debug, snapshot, endpoint] if debug == "debug" && snapshot == "snapshot" => {
                Ok(Self::Snapshot {
                    endpoint: parse_endpoint(endpoint)?,
                })
            }
            [
                debug,
                wait,
                ep_flag,
                endpoint,
                revision_flag,
                revision,
                timeout_flag,
                timeout,
            ] if debug == "debug"
                && wait == "wait"
                && ep_flag == "--ep"
                && revision_flag == "--revision"
                && timeout_flag == "--timeout" =>
            {
                Ok(Self::WaitRevision {
                    endpoint: parse_endpoint(endpoint)?,
                    service: None,
                    target: parse_revision_target(revision)?,
                    timeout: parse_duration(timeout)?,
                })
            }
            [
                debug,
                wait,
                ep_flag,
                endpoint,
                service_flag,
                service,
                revision_flag,
                revision,
                timeout_flag,
                timeout,
            ] if debug == "debug"
                && wait == "wait"
                && ep_flag == "--ep"
                && service_flag == "--service"
                && revision_flag == "--revision"
                && timeout_flag == "--timeout" =>
            {
                Ok(Self::WaitRevision {
                    endpoint: parse_endpoint(endpoint)?,
                    service: Some(service.clone()),
                    target: parse_revision_target(revision)?,
                    timeout: parse_duration(timeout)?,
                })
            }
            [debug, command, get, endpoint, request_id]
                if debug == "debug" && command == "command" && get == "get" =>
            {
                Ok(Self::CommandGet {
                    endpoint: parse_endpoint(endpoint)?,
                    request_id: request_id.clone(),
                })
            }
            [debug, trace, query, endpoint, query_json]
                if debug == "debug" && trace == "trace" && query == "query" =>
            {
                let query: Value = serde_json::from_str(query_json)
                    .map_err(|error| format!("trace query must be JSON: {error}"))?;
                if !query.is_object() {
                    return Err("trace query must be a JSON object".into());
                }
                Ok(Self::TraceQuery {
                    endpoint: parse_endpoint(endpoint)?,
                    query,
                })
            }
            [debug, input, activate, endpoint, node_path]
                if debug == "debug" && input == "input" && activate == "activate" =>
            {
                Ok(Self::InputActivateTarget {
                    endpoint: parse_endpoint(endpoint)?,
                    semantic_node_path: node_path.clone(),
                })
            }
            [debug, interaction, get, endpoint, interaction_id]
                if debug == "debug" && interaction == "interaction" && get == "get" =>
            {
                Ok(Self::InteractionGet {
                    endpoint: parse_endpoint(endpoint)?,
                    interaction_id: interaction_id.clone(),
                })
            }
            [debug, interaction, query, endpoint]
                if debug == "debug" && interaction == "interaction" && query == "query" =>
            {
                Ok(Self::InteractionQuery {
                    endpoint: parse_endpoint(endpoint)?,
                    query: json!({}),
                })
            }
            [debug, interaction, query, endpoint, query_json]
                if debug == "debug" && interaction == "interaction" && query == "query" =>
            {
                let query: Value = serde_json::from_str(query_json)
                    .map_err(|error| format!("interaction query must be JSON: {error}"))?;
                if !query.is_object() {
                    return Err("interaction query must be a JSON object".into());
                }
                Ok(Self::InteractionQuery {
                    endpoint: parse_endpoint(endpoint)?,
                    query,
                })
            }
            [debug, render, capture, endpoint, path]
                if debug == "debug" && render == "render" && capture == "capture" =>
            {
                if !path.ends_with(".png") {
                    return Err("capture path must end with .png".into());
                }
                Ok(Self::RenderCapture {
                    endpoint: parse_endpoint(endpoint)?,
                    path: path.clone(),
                })
            }
            [debug, world, capture, endpoint, path]
                if debug == "debug" && world == "world-ui" && capture == "capture" =>
            {
                parse_world_ui_capture(endpoint, path, None)
            }
            [debug, world, capture, endpoint, path, width, height]
                if debug == "debug" && world == "world-ui" && capture == "capture" =>
            {
                let size = [
                    parse_capture_dimension(width)?,
                    parse_capture_dimension(height)?,
                ];
                parse_world_ui_capture(endpoint, path, Some(size))
            }
            [debug, world, camera, endpoint]
                if debug == "debug" && world == "world-ui" && camera == "camera" =>
            {
                Ok(Self::WorldUiCamera {
                    endpoint: parse_endpoint(endpoint)?,
                })
            }
            _ => Err(debug_usage().into()),
        }
    }

    fn endpoint(&self) -> SocketAddr {
        match self {
            Self::Snapshot { endpoint }
            | Self::InteractionGet { endpoint, .. }
            | Self::InteractionQuery { endpoint, .. }
            | Self::RenderCapture { endpoint, .. }
            | Self::WorldUiCapture { endpoint, .. }
            | Self::WorldUiCamera { endpoint } => *endpoint,
            Self::SnapshotAggregate { .. } => {
                panic!("aggregate snapshot does not have one endpoint")
            }
            Self::SnapshotDiff { endpoint, .. } | Self::WaitRevision { endpoint, .. } => *endpoint,
            Self::CommandGet { endpoint, .. }
            | Self::TraceQuery { endpoint, .. }
            | Self::InputActivateTarget { endpoint, .. } => *endpoint,
        }
    }

    fn method_and_params(&self) -> (&'static str, Value) {
        match self {
            Self::Snapshot { .. } => ("debug.snapshot.get", json!({})),
            Self::InteractionGet { interaction_id, .. } => (
                "debug.interaction.get",
                json!({"interaction_id": interaction_id}),
            ),
            Self::InteractionQuery { query, .. } => ("debug.interaction.query", query.clone()),
            Self::RenderCapture { path, .. } => (
                "wgpu.render.target.capture",
                json!({"target": "ui.color.v1", "path": path, "redraw": true}),
            ),
            Self::WorldUiCapture { path, size, .. } => (
                "wgpu.world_ui.lab.capture",
                json!({"path": path, "width": size[0], "height": size[1]}),
            ),
            Self::WorldUiCamera { .. } => ("wgpu.world_ui.lab.camera.snapshot", json!({})),
            Self::SnapshotAggregate { .. } => {
                panic!("aggregate snapshot is executed without a single RPC method")
            }
            Self::SnapshotDiff { .. } | Self::WaitRevision { .. } => {
                panic!("composite debug command has no single RPC method")
            }
            Self::CommandGet { request_id, .. } => {
                ("debug.command.get", json!({"request_id": request_id}))
            }
            Self::TraceQuery { query, .. } => ("debug.trace.query", query.clone()),
            Self::InputActivateTarget {
                semantic_node_path, ..
            } => (
                "debug.window.input.activate_target",
                json!({"semantic_node_path": semantic_node_path}),
            ),
        }
    }
}

fn default_manifest_path() -> String {
    ".neon/manifest.json".into()
}

pub fn debug_usage() -> &'static str {
    "neon-cli debug snapshot [--manifest <path>] [--service <name>]\nneon-cli debug snapshot <endpoint> [--diff <before.json>]\nneon-cli debug wait --ep <endpoint> --revision <N|+delta> --timeout <Nms|Ns>\nneon-cli debug command get <endpoint> <request-id>\nneon-cli debug trace query <endpoint> <query-json>\nneon-cli debug input activate <endpoint> <semantic-node-path>\nneon-cli debug interaction get <endpoint> <interaction-id>\nneon-cli debug interaction query <endpoint> [<query-json>]\nneon-cli debug render capture <endpoint> <output.png>\nneon-cli debug world-ui capture <endpoint> <output.png> [width height]\nneon-cli debug world-ui camera <endpoint>"
}

/// Event module commands for the dedicated `neon3.event` protocol.
#[derive(Debug, PartialEq)]
pub enum EventCommand {
    Snapshot { endpoint: SocketAddr },
    Subscribe { endpoint: SocketAddr, name: String },
}

impl EventCommand {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        match args {
            [event, snapshot, endpoint] if event == "event" && snapshot == "snapshot" => {
                Ok(Self::Snapshot {
                    endpoint: parse_endpoint(endpoint)?,
                })
            }
            [event, subscribe, endpoint, name] if event == "event" && subscribe == "subscribe" => {
                Ok(Self::Subscribe {
                    endpoint: parse_endpoint(endpoint)?,
                    name: name.clone(),
                })
            }
            _ => Err(event_usage().into()),
        }
    }

    fn endpoint(&self) -> SocketAddr {
        match self {
            Self::Snapshot { endpoint } | Self::Subscribe { endpoint, .. } => *endpoint,
        }
    }
}

pub fn event_usage() -> &'static str {
    "neon-cli event snapshot <endpoint>\nneon-cli event subscribe <endpoint> <name-or-prefix>"
}

pub fn execute_event(command: EventCommand) -> Result<String, TransportError> {
    let endpoint = command.endpoint();
    match command {
        EventCommand::Snapshot { .. } => {
            let snapshot = call_event_snapshot(endpoint)?;
            Ok(format!(
                "{}",
                serde_json::json!({
                    "endpoint": endpoint.to_string(),
                    "protocol": "neon3.event",
                    "snapshot": snapshot,
                })
            ))
        }
        EventCommand::Subscribe { name, .. } => {
            let mut client = EventClient::connect(endpoint)?;
            let subscribe = EventSubscribe {
                protocol: "neon3.event".into(),
                version: ProtocolVersion { major: 1, minor: 0 },
                request_id: RequestId(format!("neon-cli-event-{}", std::process::id())),
                client: ClientIdentity {
                    kind: ClientKind::Cli,
                    instance_id: "neon-cli-event".into(),
                    pid: std::process::id(),
                    origin: "neon-cli".into(),
                },
                filters: vec![EventFilter {
                    name: None,
                    name_prefix: Some(name.clone()),
                    publisher_kinds: None,
                }],
                replay_from_sequence: None,
                max_rate_hz: None,
            };
            client.send_value(&serde_json::to_value(EventFrame::Subscribe(subscribe))?)?;
            let response = client.recv_value()?;
            let ack: neon_protocol::EventAck = match serde_json::from_value::<EventResponse>(
                response,
            )? {
                EventResponse::Ack(ack) => ack,
                EventResponse::Delivery(_) => {
                    return Ok(format!(
                        "{}",
                        serde_json::json!({"endpoint": endpoint.to_string(), "status": "unexpected_delivery_before_ack"})
                    ));
                }
            };
            let mut events = Vec::new();
            loop {
                match client.recv_value() {
                    Ok(value) => match serde_json::from_value::<EventResponse>(value) {
                        Ok(EventResponse::Delivery(delivery)) => {
                            events.push(serde_json::json!(delivery.event));
                            if events.len() >= 8 {
                                break;
                            }
                        }
                        Ok(EventResponse::Ack(_)) => break,
                        Err(_) => break,
                    },
                    Err(TransportError::Timeout) | Err(TransportError::ConnectionClosed) => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(format!(
                "{}",
                serde_json::json!({
                    "endpoint": endpoint.to_string(),
                    "protocol": "neon3.event",
                    "subscription": {
                        "name_prefix": name,
                        "ack_status": ack.status,
                        "epoch": ack.epoch,
                        "current_sequence": ack.current_sequence,
                    },
                    "events": events,
                })
            ))
        }
    }
}

fn call_event_snapshot(endpoint: SocketAddr) -> Result<Value, TransportError> {
    // Snapshot is a control-plane RPC method on the same endpoint.
    let request = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("neon-cli-event-snapshot-{}", std::process::id())),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "neon-cli-event".into(),
            pid: std::process::id(),
            origin: "neon-cli".into(),
        },
        target: ServiceName("eventd".into()),
        method: "event.snapshot".into(),
        params: serde_json::json!({}),
        expected_revision: None,
        idempotency_key: None,
    };
    let mut rpc = RpcClient::connect(endpoint)?;
    let response = rpc.call(&request)?;
    response.result.ok_or(TransportError::ConnectionClosed)
}

fn parse_world_ui_capture(
    endpoint: &str,
    path: &str,
    size: Option<[u32; 2]>,
) -> Result<DebugCommand, String> {
    if !path.ends_with(".png") {
        return Err("capture path must end with .png".into());
    }
    Ok(DebugCommand::WorldUiCapture {
        endpoint: parse_endpoint(endpoint)?,
        path: path.into(),
        size: size.unwrap_or([1920, 1080]),
    })
}

fn parse_capture_dimension(value: &str) -> Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("capture dimension must be an integer: {value}"))
}

fn parse_revision_target(value: &str) -> Result<RevisionTarget, String> {
    if let Some(delta) = value.strip_prefix('+') {
        return delta
            .parse::<u64>()
            .map(RevisionTarget::Delta)
            .map_err(|_| format!("revision delta must be an integer: {value}"));
    }
    value
        .parse::<u64>()
        .map(RevisionTarget::Absolute)
        .map_err(|_| format!("revision must be an integer or +delta: {value}"))
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, multiplier) = if let Some(value) = value.strip_suffix("ms") {
        (value, 1)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1_000)
    } else {
        return Err(format!("duration must use ms or s suffix: {value}"));
    };
    let millis = number
        .parse::<u64>()
        .map_err(|_| format!("duration must be an integer: {value}"))?
        .saturating_mul(multiplier);
    Ok(Duration::from_millis(millis))
}

pub fn execute_debug(command: DebugCommand) -> Result<Value, TransportError> {
    match command {
        DebugCommand::SnapshotAggregate { manifest, service } => {
            return execute_snapshot_aggregate(&manifest, service.as_deref());
        }
        DebugCommand::SnapshotDiff {
            endpoint,
            service,
            before,
        } => {
            return execute_snapshot_diff(endpoint, service.as_deref(), &before);
        }
        DebugCommand::WaitRevision {
            endpoint,
            service,
            target,
            timeout,
        } => {
            return execute_wait_revision(endpoint, service.as_deref(), target, timeout);
        }
        _ => {}
    }
    let endpoint = command.endpoint();
    let (method, params) = command.method_and_params();
    let target = default_target(method);
    let response = call_rpc(endpoint, method, params, target, None)?;
    Ok(json!({
        "endpoint": endpoint.to_string(),
        "method": method,
        "target": target,
        "response": response,
    }))
}

fn request_target<'a>(service: Option<&'a str>, method: &'a str) -> &'a str {
    service.unwrap_or_else(|| default_target(method))
}

fn snapshot_for(
    endpoint: SocketAddr,
    service: Option<&str>,
) -> Result<RpcResponse, TransportError> {
    call_rpc(
        endpoint,
        "debug.snapshot.get",
        json!({}),
        request_target(service, "debug.snapshot.get"),
        None,
    )
}

pub fn execute_snapshot_diff(
    endpoint: SocketAddr,
    service: Option<&str>,
    before_path: &str,
) -> Result<Value, TransportError> {
    let before: Value = serde_json::from_str(
        &std::fs::read_to_string(Path::new(before_path)).map_err(TransportError::Io)?,
    )?;
    let response = snapshot_for(endpoint, service)?;
    let after = response.result.clone().unwrap_or(Value::Null);
    let mut changes = Vec::new();
    diff_values(&before, &after, "$", &mut changes);
    Ok(json!({
        "status": if response.status == RpcStatus::Accepted { "passed" } else { "failed" },
        "endpoint": endpoint.to_string(),
        "request_id": response.request_id,
        "diff": {
            "changed_paths": changes.iter().map(|change| change["path"].clone()).collect::<Vec<_>>(),
            "changes": changes,
        },
        "snapshot": after,
        "error": response.error,
    }))
}

fn diff_values(before: &Value, after: &Value, path: &str, changes: &mut Vec<Value>) {
    match (before, after) {
        (Value::Object(before), Value::Object(after)) => {
            let keys = before
                .keys()
                .chain(after.keys())
                .collect::<std::collections::BTreeSet<_>>();
            for key in keys {
                let child = format!("{path}.{key}");
                match (before.get(key), after.get(key)) {
                    (Some(before), Some(after)) => diff_values(before, after, &child, changes),
                    (Some(before), None) => {
                        changes.push(json!({"path": child, "from": before, "to": Value::Null}))
                    }
                    (None, Some(after)) => {
                        changes.push(json!({"path": child, "from": Value::Null, "to": after}))
                    }
                    _ => unreachable!(),
                }
            }
        }
        (Value::Array(before), Value::Array(after)) if before == after => {}
        _ if before != after => changes.push(json!({"path": path, "from": before, "to": after})),
        _ => {}
    }
}

pub fn execute_wait_revision(
    endpoint: SocketAddr,
    service: Option<&str>,
    target: RevisionTarget,
    timeout: Duration,
) -> Result<Value, TransportError> {
    let started = Instant::now();
    let initial = snapshot_for(endpoint, service)?;
    let initial_revision = initial.revision.map(|revision| revision.0).unwrap_or(0);
    let wanted = match target {
        RevisionTarget::Absolute(revision) => revision,
        RevisionTarget::Delta(delta) => initial_revision.saturating_add(delta),
    };
    let mut last = initial;
    loop {
        let current = last.revision.map(|revision| revision.0).unwrap_or(0);
        if current >= wanted {
            return Ok(json!({
                "wait": "revision",
                "condition": format!("revision >= {wanted}"),
                "matched": true,
                "timeout": false,
                "elapsed_ms": started.elapsed().as_millis(),
                "matched_revision": current,
                "final_snapshot": last.result,
            }));
        }
        if started.elapsed() >= timeout {
            return Ok(json!({
                "wait": "revision",
                "condition": format!("revision >= {wanted}"),
                "matched": false,
                "timeout": true,
                "elapsed_ms": started.elapsed().as_millis(),
                "last_revision": current,
            }));
        }
        std::thread::sleep(Duration::from_millis(20));
        last = snapshot_for(endpoint, service)?;
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ServiceManifestEntry {
    pub endpoint: SocketAddr,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub epoch: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ServiceManifest {
    #[serde(default)]
    pub services: std::collections::BTreeMap<String, ServiceManifestEntry>,
    #[serde(default)]
    pub eventd_endpoint: Option<SocketAddr>,
    #[serde(default)]
    pub ui_endpoint: Option<SocketAddr>,
    #[serde(default)]
    pub wgpu_endpoint: Option<SocketAddr>,
    #[serde(default)]
    pub editor_endpoint: Option<SocketAddr>,
    #[serde(default)]
    pub projectd_endpoint: Option<SocketAddr>,
}

impl ServiceManifest {
    pub fn entries(&self) -> Vec<(String, ServiceManifestEntry)> {
        if !self.services.is_empty() {
            return self
                .services
                .iter()
                .map(|(name, entry)| (name.clone(), entry.clone()))
                .collect();
        }
        [
            ("eventd", self.eventd_endpoint),
            ("ui-runtime", self.ui_endpoint),
            ("wgpu-runtime", self.wgpu_endpoint),
            ("editor-runtime", self.editor_endpoint),
            ("neon-projectd", self.projectd_endpoint),
        ]
        .into_iter()
        .filter_map(|(name, endpoint)| {
            endpoint.map(|endpoint| {
                (
                    name.into(),
                    ServiceManifestEntry {
                        endpoint,
                        pid: None,
                        epoch: None,
                    },
                )
            })
        })
        .collect()
    }
}

fn snapshot_method(service: &str) -> Option<&'static str> {
    match service {
        "wgpu-runtime" => Some("debug.snapshot.get"),
        "ui-runtime" => Some("debug.snapshot.get"),
        "eventd" => Some("event.snapshot"),
        "editor-runtime" => Some("editor.document.snapshot.get"),
        "neon-projectd" => Some("project.summary"),
        _ => None,
    }
}

pub fn execute_snapshot_aggregate(
    manifest_path: &str,
    requested_service: Option<&str>,
) -> Result<Value, TransportError> {
    let source = std::fs::read_to_string(Path::new(manifest_path)).map_err(TransportError::Io)?;
    let manifest: ServiceManifest = serde_json::from_str(&source)?;
    let entries = manifest
        .entries()
        .into_iter()
        .filter(|(name, _)| requested_service.is_none_or(|requested| requested == name));
    let mut services = serde_json::Map::new();
    for (name, entry) in entries {
        let mut record = json!({
            "endpoint": entry.endpoint.to_string(),
            "pid": entry.pid,
            "manifest_epoch": entry.epoch,
        });
        let health = call_rpc(entry.endpoint, "service.health", json!({}), &name, None);
        let describe = call_rpc(entry.endpoint, "service.describe", json!({}), &name, None);
        let snapshot = snapshot_method(&name).map(|method| {
            // The manifest endpoint is an explicit service selection. This
            // matters for methods such as debug.snapshot.get that have more
            // than one service implementation.
            call_rpc(entry.endpoint, method, json!({}), &name, None)
        });
        record["health"] = rpc_result_json(health);
        record["describe"] = rpc_result_json(describe);
        record["snapshot_method"] =
            snapshot_method(&name).map_or(Value::Null, |method| json!(method));
        record["snapshot"] = snapshot.map_or(Value::Null, rpc_result_json);
        services.insert(name, record);
    }
    let status = if services.is_empty()
        || services.values().any(|service| {
            service["health"]["status"] != "accepted"
                || service["describe"]["status"] != "accepted"
                || (service["snapshot_method"] != Value::Null
                    && service["snapshot"]["status"] != "accepted")
        }) {
        "failed"
    } else {
        "passed"
    };
    Ok(json!({
        "manifest": manifest_path,
        "status": status,
        "services": services,
    }))
}

fn rpc_result_json(result: Result<RpcResponse, TransportError>) -> Value {
    match result {
        Ok(response) => json!({
            "status": response.status,
            "request_id": response.request_id,
            "revision": response.revision,
            "result": response.result,
            "snapshot": response.snapshot,
            "error": response.error,
        }),
        Err(error) => json!({
            "status": "transport_failed",
            "error": {"code": "transport_failed", "message": error.to_string()},
        }),
    }
}

pub fn execute_rpc(command: RpcCommand) -> Result<Value, TransportError> {
    let target = command
        .service
        .as_deref()
        .unwrap_or_else(|| default_target(&command.method));
    let response = call_rpc(
        command.endpoint,
        &command.method,
        command.params.clone(),
        target,
        command.idempotency_key.as_deref(),
    )?;
    Ok(json!({
        "endpoint": command.endpoint.to_string(),
        "method": command.method,
        "target": target,
        "params": command.params,
        "response": response,
    }))
}

fn parse_endpoint(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|error| format!("invalid endpoint '{value}': {error}"))
}

fn call_rpc(
    endpoint: SocketAddr,
    method: &str,
    params: Value,
    target: &str,
    idempotency_key: Option<&str>,
) -> Result<RpcResponse, TransportError> {
    let request = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("neon-cli-debug-{}", std::process::id())),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "neon-cli-debug".into(),
            pid: std::process::id(),
            origin: "neon-cli".into(),
        },
        target: ServiceName(target.into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: idempotency_key.map(str::to_owned),
    };
    let mut client = RpcClient::connect(endpoint)?;
    client.call(&request)
}

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

    let snapshot = call(
        endpoint,
        "snapshot-1",
        "debug.snapshot.get",
        json!({}),
        None,
    )?;
    record_step(&mut steps, "debug.snapshot.get", &snapshot);
    if snapshot.status != RpcStatus::Accepted {
        return Ok(failed(steps, &snapshot));
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

    let fragment_snapshot = call(
        endpoint,
        "fragment-snapshot-1",
        "wgpu.ui.fragment.snapshot",
        json!({"fragment_id": "cli-static-fragment"}),
        None,
    )?;
    record_step(&mut steps, "wgpu.ui.fragment.snapshot", &fragment_snapshot);
    if fragment_snapshot.status != RpcStatus::Accepted {
        return Ok(failed(steps, &fragment_snapshot));
    }

    let graph = call(
        endpoint,
        "graph-1",
        "wgpu.render.graph.snapshot",
        json!({}),
        None,
    )?;
    record_step(&mut steps, "wgpu.render.graph.snapshot", &graph);
    if graph.status != RpcStatus::Accepted {
        return Ok(failed(steps, &graph));
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
        "request_ids": ["health-1", "describe-1", "snapshot-1", "submit-1", "submit-duplicate-1", "fragment-snapshot-1", "graph-1", "diagnostics-1", "receipt-1", "traces-1"],
        "trace_records": traces.result,
        "diagnostics": diagnostics.result,
        "fragment_snapshot": fragment_snapshot.result,
        "render_graph": graph.result,
    }))
}

/// Headless service-level acceptance scenario for a declarative button-driven content update.
/// The CLI submits semantic data only; it never supplies a render hit ID or screen coordinate.
pub fn run_detail_toggle_scenario(endpoint: SocketAddr) -> Result<Value, TransportError> {
    let mut steps = Vec::new();
    let health = call(
        endpoint,
        "detail-health-1",
        "service.health",
        json!({}),
        None,
    )?;
    record_step(&mut steps, "service.health", &health);
    if health.status != RpcStatus::Accepted {
        return Ok(failed_detail(steps, &health));
    }
    let initial = call(
        endpoint,
        "detail-submit-1",
        "wgpu.ui.submit_fragment",
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(detail_fragment(Revision(1), false))
        }),
        Some("detail-submit-key-1"),
    )?;
    record_step(&mut steps, "wgpu.ui.submit_fragment.initial", &initial);
    if initial.status != RpcStatus::Accepted {
        return Ok(failed_detail(steps, &initial));
    }
    let intent = UiIntent::Invoke {
        action: "ui.detail.toggle".into(),
        params: json!({"section": "inspector"}),
    };
    let event = UiSemanticEvent {
        event: UiSemanticEventType::PointerClick,
        event_id: "detail-toggle-event-1".into(),
        renderer_epoch: 1,
        composition_revision: initial.revision.unwrap_or(Revision(0)),
        fragment: UiFragmentRevision {
            id: UiFragmentId("cli-detail-toggle".into()),
            revision: Revision(1),
        },
        intent,
        pointer: Some(UiPointerMetadata { id: 0, sequence: 1 }),
        focus: None,
        data_grid_cell: None,
        text: None,
        control_value: None,
        drag_drop: None,
    };
    let validated = call(
        endpoint,
        "detail-event-1",
        "wgpu.ui.semantic_event.validate",
        json!(event),
        None,
    )?;
    record_step(&mut steps, "wgpu.ui.semantic_event.validate", &validated);
    if validated.status != RpcStatus::Accepted {
        return Ok(failed_detail(steps, &validated));
    }
    let updated = call(
        endpoint,
        "detail-submit-2",
        "wgpu.ui.submit_fragment",
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(detail_fragment(Revision(2), true))
        }),
        Some("detail-submit-key-2"),
    )?;
    record_step(&mut steps, "wgpu.ui.submit_fragment.updated", &updated);
    if updated.status != RpcStatus::Accepted {
        return Ok(failed_detail(steps, &updated));
    }
    let diagnostics = call(
        endpoint,
        "detail-diagnostics-1",
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
        return Ok(failed_detail(steps, &diagnostics));
    }
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
    let lower_text = if detail_visible {
        "Inspector details are now visible."
    } else {
        "Select Show details to inspect this item."
    };
    UiFragment {
        fragment_id: UiFragmentId("cli-detail-toggle".into()),
        revision,
        root: UiNode {
            node_id: UiNodeId("editor-shell".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 24.0,
                y: 24.0,
                width: 420.0,
                height: 240.0,
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
            children: vec![
                UiNode {
                    node_id: UiNodeId("title".into()),
                    kind: UiNodeKind::Label,
                    bounds: UiBounds {
                        x: 20.0,
                        y: 18.0,
                        width: 220.0,
                        height: 28.0,
                    },
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: None,
                    text: Some(TextRef::Literal {
                        value: "Terrain Inspector".into(),
                    }),
                    image: None,
                    surface: None,
                    style: UiStyle::default(),
                    enter_transition: None,
                    world_depth: None,
                    world_scale: None,
                    clip_shape: UiClipShape::default(),
                    children: Vec::new(),
                },
                UiNode {
                    node_id: UiNodeId("show-details".into()),
                    kind: UiNodeKind::Button,
                    bounds: UiBounds {
                        x: 250.0,
                        y: 16.0,
                        width: 145.0,
                        height: 34.0,
                    },
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: None,
                    text: Some(TextRef::Literal {
                        value: "Show details".into(),
                    }),
                    image: None,
                    surface: None,
                    style: UiStyle::default(),
                    enter_transition: None,
                    world_depth: None,
                    world_scale: None,
                    clip_shape: UiClipShape::default(),
                    children: Vec::new(),
                },
                UiNode {
                    node_id: UiNodeId("detail-region".into()),
                    kind: UiNodeKind::Label,
                    bounds: UiBounds {
                        x: 20.0,
                        y: 82.0,
                        width: 376.0,
                        height: 120.0,
                    },
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: None,
                    text: Some(TextRef::Literal {
                        value: lower_text.into(),
                    }),
                    image: None,
                    surface: None,
                    style: UiStyle::default(),
                    enter_transition: None,
                    world_depth: None,
                    world_scale: None,
                    clip_shape: UiClipShape::default(),
                    children: Vec::new(),
                },
            ],
        },
        effects: vec![UiEffect::SemanticIntent {
            intent: UiIntent::Invoke {
                action: "ui.detail.toggle".into(),
                params: json!({"section": "inspector"}),
            },
        }],
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
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            world_depth: None,
            world_scale: None,
            clip_shape: UiClipShape::default(),
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
    let target = default_target(method);
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
        target: ServiceName(target.into()),
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
        "target": default_target(method),
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
            "debug.snapshot.get" => json!({"epoch": 1, "revision": 0}),
            "wgpu.ui.submit_fragment" => json!({"fragment_count": 1}),
            "wgpu.ui.fragment.snapshot" => {
                json!({"epoch": 1, "sequence": 1, "fragment_revision": 1})
            }
            "wgpu.render.graph.snapshot" => json!({"graph_revision": 1, "targets": []}),
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
            for _ in 0..10 {
                server.serve_one(response).unwrap();
            }
        });
        let outcome = run_headless_scenario(endpoint).unwrap();
        assert_eq!(outcome["status"], "passed");
        assert_eq!(outcome["steps"].as_array().unwrap().len(), 10);
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
                        details: None,
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
        let thread = thread::spawn(move || {
            for _ in 0..5 {
                server.serve_one(response).unwrap();
            }
        });
        let outcome = run_detail_toggle_scenario(endpoint).unwrap();
        assert_eq!(outcome["status"], "passed");
        assert_eq!(outcome["transition"]["from_fragment_revision"], 1);
        assert_eq!(outcome["transition"]["to_fragment_revision"], 2);
        assert_eq!(
            detail_fragment(Revision(2), true).root.children[2].text,
            Some(TextRef::Literal {
                value: "Inspector details are now visible.".into()
            })
        );
        thread.join().unwrap();
    }

    #[test]
    fn debug_command_parses_only_read_only_interaction_queries() {
        assert_eq!(
            DebugCommand::parse(&[
                "debug".into(),
                "interaction".into(),
                "get".into(),
                "127.0.0.1:4010".into(),
                "wgpu-window-1-2".into(),
            ]),
            Ok(DebugCommand::InteractionGet {
                endpoint: "127.0.0.1:4010".parse().unwrap(),
                interaction_id: "wgpu-window-1-2".into(),
            })
        );
        assert_eq!(
            DebugCommand::parse(&[
                "debug".into(),
                "interaction".into(),
                "query".into(),
                "127.0.0.1:4010".into(),
                "{\"limit\":2}".into(),
            ]),
            Ok(DebugCommand::InteractionQuery {
                endpoint: "127.0.0.1:4010".parse().unwrap(),
                query: json!({"limit": 2}),
            })
        );
        assert!(
            DebugCommand::parse(&[
                "debug".into(),
                "interaction".into(),
                "query".into(),
                "127.0.0.1:4010".into(),
                "[]".into(),
            ])
            .is_err()
        );
        assert!(
            DebugCommand::parse(&[
                "debug".into(),
                "window".into(),
                "activate".into(),
                "127.0.0.1:4010".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn debug_world_ui_capture_parses_defaults_and_dimensions() {
        assert_eq!(
            DebugCommand::parse(&[
                "debug".into(),
                "world-ui".into(),
                "capture".into(),
                "127.0.0.1:4010".into(),
                "world-ui.png".into(),
            ]),
            Ok(DebugCommand::WorldUiCapture {
                endpoint: "127.0.0.1:4010".parse().unwrap(),
                path: "world-ui.png".into(),
                size: [1920, 1080],
            })
        );
        assert_eq!(
            DebugCommand::parse(&[
                "debug".into(),
                "world-ui".into(),
                "capture".into(),
                "127.0.0.1:4010".into(),
                "world-ui.png".into(),
                "800".into(),
                "600".into(),
            ]),
            Ok(DebugCommand::WorldUiCapture {
                endpoint: "127.0.0.1:4010".parse().unwrap(),
                path: "world-ui.png".into(),
                size: [800, 600],
            })
        );
    }

    #[test]
    fn event_command_parses_snapshot_and_subscribe() {
        assert_eq!(
            EventCommand::parse(&["event".into(), "snapshot".into(), "127.0.0.1:4010".into(),]),
            Ok(EventCommand::Snapshot {
                endpoint: "127.0.0.1:4010".parse().unwrap(),
            })
        );
        assert_eq!(
            EventCommand::parse(&[
                "event".into(),
                "subscribe".into(),
                "127.0.0.1:4010".into(),
                "nui.variable.".into(),
            ]),
            Ok(EventCommand::Subscribe {
                endpoint: "127.0.0.1:4010".parse().unwrap(),
                name: "nui.variable.".into(),
            })
        );
        assert!(
            EventCommand::parse(&["event".into(), "publish".into(), "127.0.0.1:4010".into(),])
                .is_err()
        );
    }

    #[test]
    fn event_snapshot_command_queries_eventd_control_plane() {
        use neon_ipc::RpcServer;
        use std::thread;
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || {
            server
                .serve_one(|request| {
                    assert_eq!(request.method, "event.snapshot");
                    assert_eq!(request.target.0, "eventd");
                    RpcResponse {
                        request_id: request.request_id,
                        status: RpcStatus::Accepted,
                        revision: None,
                        result: Some(json!({"epoch": 1, "current_sequence": 0, "registered_namespaces": ["nui.variable."]})),
                        snapshot: None,
                        error: None,
                    }
                })
                .unwrap();
        });
        let output = execute_event(EventCommand::Snapshot { endpoint }).unwrap();
        assert!(output.contains("nui.variable."));
        assert!(output.contains("\"current_sequence\":0"));
        thread.join().unwrap();
    }

    #[test]
    fn event_subscribe_command_streams_deliveries() {
        use neon_ipc::{DEFAULT_MAX_FRAME_SIZE, RpcServer, read_json_frame, write_json_frame};
        use neon_protocol::{
            EVENT_PROTOCOL, EventAckStatus, EventDelivery, EventEnvelope, EventId, EventResponse,
        };
        use std::thread;

        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || {
            let mut stream = server.accept().unwrap();
            let frame: EventFrame = read_json_frame(&mut stream, DEFAULT_MAX_FRAME_SIZE).unwrap();
            let EventFrame::Subscribe(subscribe) = frame else {
                panic!("expected subscribe frame");
            };
            assert_eq!(
                subscribe.filters[0].name_prefix.as_deref(),
                Some("nui.variable.")
            );
            write_json_frame(
                &mut stream,
                &EventResponse::Ack(neon_protocol::EventAck {
                    protocol: EVENT_PROTOCOL.into(),
                    version: ProtocolVersion { major: 1, minor: 0 },
                    request_id: subscribe.request_id,
                    status: EventAckStatus::Accepted,
                    event_id: None,
                    epoch: Some(1),
                    sequence: None,
                    current_sequence: Some(1),
                    error: None,
                }),
                DEFAULT_MAX_FRAME_SIZE,
            )
            .unwrap();
            write_json_frame(
                &mut stream,
                &EventResponse::Delivery(EventDelivery {
                    protocol: EVENT_PROTOCOL.into(),
                    version: ProtocolVersion { major: 1, minor: 0 },
                    event: EventEnvelope {
                        protocol: EVENT_PROTOCOL.into(),
                        version: ProtocolVersion { major: 1, minor: 0 },
                        event_id: EventId("evt-1-1".into()),
                        name: "nui.variable.changed".into(),
                        schema_version: 1,
                        epoch: 1,
                        sequence: 1,
                        timestamp_unix_ms: 0,
                        publisher: ClientIdentity {
                            kind: ClientKind::UiRuntime,
                            instance_id: "ui-1".into(),
                            pid: 1,
                            origin: "test".into(),
                        },
                        payload: json!({"variable_key": "brush_size", "new_value": 8}),
                    },
                }),
                DEFAULT_MAX_FRAME_SIZE,
            )
            .unwrap();
        });
        let output = execute_event(EventCommand::Subscribe {
            endpoint,
            name: "nui.variable.".into(),
        })
        .unwrap();
        assert!(output.contains("nui.variable.changed"));
        assert!(output.contains("\"sequence\":1"));
        thread.join().unwrap();
    }

    #[test]
    fn debug_interaction_get_uses_public_rpc_and_returns_json() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let thread = thread::spawn(move || {
            server
                .serve_one(|request| {
                    assert_eq!(request.method, "debug.interaction.get");
                    assert_eq!(request.params, json!({"interaction_id": "interaction-7"}));
                    response(request)
                })
                .unwrap();
        });
        let output = execute_debug(DebugCommand::InteractionGet {
            endpoint,
            interaction_id: "interaction-7".into(),
        })
        .unwrap();
        assert_eq!(output["method"], "debug.interaction.get");
        assert_eq!(output["response"]["status"], "accepted");
        thread.join().unwrap();
    }

    #[test]
    fn default_target_routes_public_method_families() {
        assert_eq!(default_target("ui.flow.compile"), "ui-runtime");
        assert_eq!(default_target("debug.snapshot.get"), "wgpu-runtime");
        assert_eq!(default_target("debug.trace.query"), "ui-runtime");
        assert_eq!(default_target("render.surface.open"), "wgpu-runtime");
        assert_eq!(default_target("event.snapshot"), "eventd");
        assert_eq!(default_target("editor.document.open"), "editor-runtime");
        assert_eq!(
            default_target("terrain.preview.begin"),
            "neon-terrain-runtime"
        );
        assert_eq!(
            default_target("resource.pick.open"),
            "neon-resource-runtime"
        );
    }

    #[test]
    fn rpc_command_accepts_explicit_service_and_json_options() {
        let command = RpcCommand::parse(&[
            "rpc".into(),
            "ui.flow.compile".into(),
            "--endpoint".into(),
            "127.0.0.1:4010".into(),
            "--service".into(),
            "ui-runtime".into(),
            "--params-json".into(),
            r#"{"source":"version 1"}"#.into(),
            "--idempotency-key".into(),
            "compile-1".into(),
        ])
        .unwrap();
        assert_eq!(command.service.as_deref(), Some("ui-runtime"));
        assert_eq!(command.params["source"], "version 1");
        assert_eq!(command.idempotency_key.as_deref(), Some("compile-1"));
    }

    #[test]
    fn manifest_accepts_legacy_endpoint_fields() {
        let manifest: ServiceManifest = serde_json::from_value(json!({
            "ui_endpoint": "127.0.0.1:39102",
            "wgpu_endpoint": "127.0.0.1:39103"
        }))
        .unwrap();
        let entries = manifest.entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "ui-runtime");
        assert_eq!(entries[1].0, "wgpu-runtime");
    }

    #[test]
    fn debug_commands_parse_wait_and_diff_forms() {
        assert_eq!(
            DebugCommand::parse(&[
                "debug".into(),
                "wait".into(),
                "--ep".into(),
                "127.0.0.1:39103".into(),
                "--revision".into(),
                "+1".into(),
                "--timeout".into(),
                "2s".into(),
            ]),
            Ok(DebugCommand::WaitRevision {
                endpoint: "127.0.0.1:39103".parse().unwrap(),
                service: None,
                target: RevisionTarget::Delta(1),
                timeout: Duration::from_secs(2),
            })
        );
        assert_eq!(
            DebugCommand::parse(&[
                "debug".into(),
                "snapshot".into(),
                "127.0.0.1:39103".into(),
                "--diff".into(),
                "before.json".into(),
            ]),
            Ok(DebugCommand::SnapshotDiff {
                endpoint: "127.0.0.1:39103".parse().unwrap(),
                service: None,
                before: "before.json".into(),
            })
        );
        assert_eq!(
            DebugCommand::parse(&[
                "debug".into(),
                "command".into(),
                "get".into(),
                "127.0.0.1:39103".into(),
                "request-1".into(),
            ]),
            Ok(DebugCommand::CommandGet {
                endpoint: "127.0.0.1:39103".parse().unwrap(),
                request_id: "request-1".into(),
            })
        );
        assert_eq!(
            DebugCommand::parse(&[
                "debug".into(),
                "input".into(),
                "activate".into(),
                "127.0.0.1:39103".into(),
                "root/save".into(),
            ]),
            Ok(DebugCommand::InputActivateTarget {
                endpoint: "127.0.0.1:39103".parse().unwrap(),
                semantic_node_path: "root/save".into(),
            })
        );
    }
}
