//! End-to-end NUI Flow Canvas acceptance probe.
//!
//! Exercises: Flow source -> UI Runtime compiler/host adapter -> revisioned
//! canvas_data UiInputFrame -> WGPU window compositor -> final PNG pixels.

use std::{
    fs::File,
    io::{self, BufReader},
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_schema::{
    UI_CANVAS_POINTS_LINES_CAPABILITY_NAME, UI_NINE_SLICE_CAPABILITY_NAME,
    UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_CAPABILITY_NAME,
    UI_PROGRAM_SCHEMA_VERSION, UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
    UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME, UiCanvasData, UiCanvasLine, UiCanvasPoint,
    UiInputChange, UiInputFrame, UiInputValue, UiProgramCapability, UiProgramCapabilityOwner,
    UiProgramCapabilityStatus, UiProgramRevision,
};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(15);

fn endpoint() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn identity() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::ExternalHost,
        instance_id: "nui-canvas-window-probe".into(),
        pid: std::process::id(),
        origin: "nui-canvas-window-probe".into(),
    }
}
fn request(
    id: &str,
    target: &str,
    method: &str,
    params: Value,
    expected: Option<Revision>,
    key: Option<&str>,
) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: PROTOCOL_VERSION,
        request_id: RequestId(id.into()),
        client: identity(),
        target: ServiceName(target.into()),
        method: method.into(),
        params,
        expected_revision: expected,
        idempotency_key: key.map(str::to_owned),
    }
}
fn call(endpoint: SocketAddr, request: RpcRequest) -> Result<RpcResponse, String> {
    RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(&request))
        .map_err(|error| error.to_string())
}
fn health(endpoint: SocketAddr, target: &str) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < TIMEOUT {
        if call(
            endpoint,
            request("health", target, "service.health", json!({}), None, None),
        )
        .is_ok_and(|response| response.status == RpcStatus::Accepted)
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!("{target} health timeout"))
}
fn binary(name: &str) -> io::Result<std::path::PathBuf> {
    let mut path = std::env::current_exe()?;
    path.set_file_name(format!("{name}.exe"));
    Ok(path)
}
fn stop(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}
fn flow() -> &'static str {
    "version 1\nsurface canvas-window revision 1\nbudget nodes=8 bindings=1 instances=8 text=8 glyphs=64 events=0 clips=8\ninput guides canvas_data default canvas:empty\nsurface canvas-window column w 360 h 220\n  panel canvas_host w 360 h 220\n    canvas guides data $guides w 320 h 180\n"
}
fn program() -> UiProgramRevision {
    UiProgramRevision {
        program_id: "canvas-window".into(),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: [
            UI_PROGRAM_CAPABILITY_NAME,
            UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
            UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
            UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
            UI_NINE_SLICE_CAPABILITY_NAME,
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
fn canvas() -> UiCanvasData {
    UiCanvasData {
        version: 1,
        points: vec![UiCanvasPoint {
            id: "detected.corner".into(),
            position: [80.0, 64.0],
            radius: 8.0,
            color: [1.0, 0.15, 0.12, 1.0],
        }],
        lines: vec![
            UiCanvasLine {
                id: "detected.diagonal".into(),
                start: [20.0, 20.0],
                end: [286.0, 148.0],
                width: 4.0,
                color: [0.0, 0.9, 1.0, 1.0],
            },
            UiCanvasLine {
                id: "detected.gap".into(),
                start: [24.0, 128.0],
                end: [290.0, 128.0],
                width: 2.0,
                color: [0.0, 0.9, 1.0, 1.0],
            },
        ],
    }
}
fn pixels(path: &str) -> io::Result<(u64, u64)> {
    let mut reader = png::Decoder::new(BufReader::new(File::open(path)?))
        .read_info()
        .map_err(io::Error::other)?;
    let mut bytes = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut bytes).map_err(io::Error::other)?;
    let mut red = 0;
    let mut cyan = 0;
    for p in bytes[..info.buffer_size()].chunks_exact(4) {
        if p[3] > 8
            && p[0] > 120
            && p[0] > p[1].saturating_add(35)
            && p[0] > p[2].saturating_add(35)
        {
            red += 1;
        }
        if p[3] > 8
            && p[1] > 100
            && p[2] > 100
            && p[0].saturating_add(30) < p[1]
            && p[0].saturating_add(30) < p[2]
        {
            cyan += 1;
        }
    }
    Ok((red, cyan))
}
fn emit(callback: &str, value: Value) {
    println!("{}", json!({"callback":callback,"input":value}));
}
fn run() -> Result<(), String> {
    let wgpu_endpoint = endpoint();
    let ui_endpoint = endpoint();
    let wgpu = Command::new(binary("neon-wgpu-runtime").map_err(|e| e.to_string())?)
        .args(["--window-server", &wgpu_endpoint.to_string()])
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let ui = Command::new(binary("neon-ui-runtime").map_err(|e| e.to_string())?)
        .args([
            "--forward-server",
            &ui_endpoint.to_string(),
            &wgpu_endpoint.to_string(),
            "127.0.0.1:9",
        ])
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let result = (|| {
        health(wgpu_endpoint, "wgpu-runtime")?;
        health(ui_endpoint, "ui-runtime")?;
        let submitted = call(
            ui_endpoint,
            request(
                "canvas-flow",
                "ui-runtime",
                "ui.flow.submit",
                json!({"source":flow()}),
                None,
                Some("canvas-flow"),
            ),
        )?;
        if submitted.status != RpcStatus::Accepted {
            return Err(format!("flow rejected: {:?}", submitted.error));
        }
        emit(
            "canvas.flow",
            json!({"surface":"canvas-window","status":"accepted"}),
        );
        let frame = UiInputFrame {
            program_revision: program(),
            expected_input_revision: Revision(0),
            request_id: "canvas-data-frame".into(),
            idempotency_key: "canvas-data-frame".into(),
            changes: vec![UiInputChange {
                key: "guides".into(),
                value: UiInputValue::CanvasData { value: canvas() },
            }],
        };
        let updated = call(
            ui_endpoint,
            request(
                "canvas-input",
                "ui-runtime",
                "ui.input.frame",
                json!(frame),
                Some(Revision(1)),
                Some("canvas-input"),
            ),
        )?;
        if updated.status != RpcStatus::Accepted {
            return Err(format!("input rejected: {:?}", updated.error));
        }
        emit(
            "canvas.data",
            json!({"point_count":1,"line_count":2,"revision":updated.revision}),
        );
        let ui_snapshot = call(
            ui_endpoint,
            request(
                "canvas-snapshot",
                "ui-runtime",
                "debug.ui.host.snapshot",
                json!({}),
                None,
                None,
            ),
        )?;
        let snapshot_value = ui_snapshot.result.clone().unwrap_or(Value::Null);
        let snapshot_pass = snapshot_value
            .get("scalar_inputs")
            .and_then(|value| value.get("values"))
            .and_then(|value| value.get("guides"))
            .and_then(|value| value.get("value"))
            .and_then(|value| value.get("value"))
            .is_some();
        emit(
            "canvas.snapshot",
            json!({"input_key":"guides","snapshot_contains_canvas_data":snapshot_pass,"response_revision":ui_snapshot.revision}),
        );
        if !snapshot_pass {
            return Err("UI Runtime snapshot did not contain persisted canvas_data".into());
        }
        let path = std::env::temp_dir().join("neon3-nui-canvas-window.png");
        let capture = call(
            wgpu_endpoint,
            request(
                "canvas-capture",
                "wgpu-runtime",
                "wgpu.render.target.capture",
                json!({"target":"ui.color.v1","path":path.to_string_lossy(),"redraw":true}),
                None,
                None,
            ),
        )?;
        let artifact = capture
            .result
            .as_ref()
            .and_then(|r| r.get("artifact_path"))
            .and_then(Value::as_str)
            .ok_or("capture missing artifact")?;
        let (red, cyan) = pixels(artifact).map_err(|e| e.to_string())?;
        let pass = capture.status == RpcStatus::Accepted && red > 20 && cyan > 300;
        println!(
            "{}",
            json!({"callback":"canvas.result","producer":{"flow":"canvas-window","input_revision":1,"point_count":1,"line_count":2},"consumer":{"window_target":"ui.color.v1","artifact_path":artifact,"red_pixels":red,"cyan_pixels":cyan,"capture":capture.result},"result":if pass{"passed"}else{"failed"}})
        );
        if pass {
            Ok(())
        } else {
            Err("window canvas pixels missing".into())
        }
    })();
    let _ = call(
        ui_endpoint,
        request(
            "shutdown-ui",
            "ui-runtime",
            "service.shutdown",
            json!({}),
            None,
            None,
        ),
    );
    let _ = call(
        wgpu_endpoint,
        request(
            "shutdown-wgpu",
            "wgpu-runtime",
            "service.shutdown",
            json!({}),
            None,
            None,
        ),
    );
    stop(ui);
    stop(wgpu);
    result
}
fn main() {
    if let Err(error) = run() {
        println!(
            "{}",
            json!({"callback":"canvas.result","result":"failed","error":error})
        );
        std::process::exit(1)
    }
}
