//! Deterministic JSONL acceptance probe for the persisted NUI point/line Canvas.
//! It launches the real headless WGPU service, submits a fixed Canvas fragment
//! through public RPC, and checks the accepted composition diagnostics.

use std::{
    io,
    net::SocketAddr,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcStatus,
    ServiceName,
};
use neon_ui_schema::{
    UiBounds, UiCanvasData, UiCanvasLine, UiCanvasPoint, UiCommand, UiFragment, UiFragmentId,
    UiFragmentSubmission, UiNode, UiNodeId, UiNodeKind, UiStyle,
};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39241";
const TIMEOUT: Duration = Duration::from_secs(10);

fn request(method: &str, sequence: u64, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("canvas-panel-probe-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "canvas-panel-probe".into(),
            pid: std::process::id(),
            origin: "canvas-panel-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("canvas-panel-probe-{sequence}")),
    }
}

fn call(
    endpoint: SocketAddr,
    method: &str,
    sequence: u64,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let response = RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(&request(method, sequence, params)))
        .map_err(|error| error.to_string())?;
    if response.status != RpcStatus::Accepted {
        return Err(format!("{method} rejected: {:?}", response.error));
    }
    Ok(response.result.unwrap_or_else(|| json!({})))
}

fn canvas_fragment() -> UiFragment {
    UiFragment {
        fragment_id: UiFragmentId("canvas-panel-probe".into()),
        revision: Revision(1),
        root: UiNode {
            node_id: UiNodeId("canvas".into()),
            kind: UiNodeKind::Canvas,
            bounds: UiBounds {
                x: 16.0,
                y: 16.0,
                width: 320.0,
                height: 180.0,
            },
            layout: None,
            visible: true,
            enabled: false,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [0.05, 0.07, 0.12, 1.0],
                ..UiStyle::default()
            },
            enter_transition: None,
            world_depth: None,
            world_scale: None,
            children: Vec::new(),
        },
        effects: vec![neon_ui_schema::UiEffect::CanvasData {
            node_id: UiNodeId("canvas".into()),
            data: UiCanvasData {
                version: 1,
                points: vec![UiCanvasPoint {
                    id: "corner".into(),
                    position: [80.0, 48.0],
                    radius: 6.0,
                    color: [1.0, 0.25, 0.2, 1.0],
                }],
                lines: vec![
                    UiCanvasLine {
                        id: "vertical".into(),
                        start: [80.0, 16.0],
                        end: [80.0, 150.0],
                        width: 2.0,
                        color: [0.2, 0.9, 1.0, 1.0],
                    },
                    UiCanvasLine {
                        id: "horizontal".into(),
                        start: [24.0, 48.0],
                        end: [280.0, 48.0],
                        width: 2.0,
                        color: [0.2, 0.9, 1.0, 1.0],
                    },
                ],
            },
        }],
    }
}

fn launch() -> io::Result<Child> {
    let binary = std::env::current_exe()?.with_file_name("neon-wgpu-runtime.exe");
    Command::new(binary)
        .args(["--headless-server", ENDPOINT])
        .spawn()
}

fn main() -> io::Result<()> {
    let endpoint: SocketAddr = ENDPOINT.parse().expect("fixed endpoint");
    let mut service = launch()?;
    let started = Instant::now();
    let health = loop {
        match call(endpoint, "service.health", 1, json!({})) {
            Ok(value) => break value,
            Err(error) if started.elapsed() < TIMEOUT => {
                thread::sleep(Duration::from_millis(100));
                if service.try_wait()?.is_some() {
                    return Err(io::Error::other(format!(
                        "service exited while waiting: {error}"
                    )));
                }
            }
            Err(error) => return Err(io::Error::other(format!("service health timeout: {error}"))),
        }
    };
    println!(
        "{}",
        json!({"callback":"canvas.health","endpoint":ENDPOINT,"result":"passed","data":health})
    );
    let fragment = canvas_fragment();
    let submitted = call(
        endpoint,
        "wgpu.ui.submit_fragment",
        2,
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment)
        }),
    )
    .map_err(io::Error::other)?;
    println!(
        "{}",
        json!({"callback":"canvas.submit","frame_sequence":1,"point_count":1,"line_count":2,"result":"passed","data":submitted})
    );
    let diagnostics =
        call(endpoint, "wgpu.render.diagnostics", 3, json!({})).map_err(io::Error::other)?;
    let capture = call(
        endpoint,
        "wgpu.render.target.capture",
        4,
        json!({"target":"ui.color.v1"}),
    )
    .map_err(io::Error::other)?;
    let passed = diagnostics
        .get("fragment_count")
        .and_then(serde_json::Value::as_u64)
        == Some(1)
        && capture.get("target").and_then(serde_json::Value::as_str) == Some("ui.color.v1");
    println!(
        "{}",
        json!({"callback":"canvas.result","frame_sequence":1,"producer":{"canvas_data_version":1,"point_count":1,"line_count":2},"consumer":{"fragment_count":diagnostics.get("fragment_count"),"graph_revision":diagnostics.get("graph_revision"),"capture_target":capture.get("target")},"result":if passed {"passed"} else {"failed"}})
    );
    let _ = call(endpoint, "service.shutdown", 5, json!({}));
    let deadline = Instant::now() + Duration::from_secs(2);
    while service.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    if service.try_wait()?.is_none() {
        service.kill()?;
        let _ = service.wait();
    }
    if passed {
        Ok(())
    } else {
        Err(io::Error::other(
            "canvas diagnostics or capture assertion failed",
        ))
    }
}
