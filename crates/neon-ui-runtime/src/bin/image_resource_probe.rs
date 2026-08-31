use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, Revision, RpcRequest, RpcResponse,
    ServiceName, UiImageSource, UiImageUploadRequest,
};
use serde_json::{Value, json};

const POLL_TIMEOUT: Duration = Duration::from_secs(5);

fn free_endpoint() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("probe can bind a local port")
        .local_addr()
        .expect("probe can read local port")
}

fn client(kind: ClientKind, origin: &str) -> ClientIdentity {
    ClientIdentity {
        kind,
        instance_id: "image-resource-probe".into(),
        pid: std::process::id(),
        origin: origin.into(),
    }
}

fn request(
    id: &str,
    target: &str,
    method: &str,
    params: Value,
    expected_revision: Option<Revision>,
    idempotency_key: Option<&str>,
    kind: ClientKind,
) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: PROTOCOL_VERSION,
        request_id: RequestId(id.into()),
        client: client(kind, "neon-image-resource-probe"),
        target: ServiceName(target.into()),
        method: method.into(),
        params,
        expected_revision,
        idempotency_key: idempotency_key.map(str::to_owned),
    }
}

fn call(endpoint: SocketAddr, request: &RpcRequest) -> Result<RpcResponse, String> {
    RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(request))
        .map_err(|error| error.to_string())
}

fn wait_health(endpoint: SocketAddr, target: &str, kind: ClientKind) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < POLL_TIMEOUT {
        let request = request(
            "health",
            target,
            "service.health",
            json!({}),
            None,
            None,
            kind.clone(),
        );
        if let Ok(response) = call(endpoint, &request)
            && response.status == neon_protocol::RpcStatus::Accepted
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(format!("{target} did not become healthy before timeout"))
}

fn binary_path(current: PathBuf, name: &str) -> PathBuf {
    let mut path = current;
    path.set_file_name(format!("{name}.exe"));
    path
}

fn spawn_services(
    wgpu_endpoint: SocketAddr,
    ui_endpoint: SocketAddr,
) -> Result<(Child, Child), String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let wgpu_path = binary_path(current.clone(), "neon-wgpu-runtime");
    let ui_path = binary_path(current, "neon-ui-runtime");
    let wgpu = Command::new(wgpu_path)
        .args(["--window-server", &wgpu_endpoint.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn wgpu-runtime: {error}"))?;
    let ui = Command::new(ui_path)
        .args([
            "--forward-server",
            &ui_endpoint.to_string(),
            &wgpu_endpoint.to_string(),
            "127.0.0.1:9",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn ui-runtime: {error}"))?;
    Ok((wgpu, ui))
}

fn image_flow_source() -> &'static str {
    "version 1\nsurface external-image-flow revision 1\nbudget nodes=8 bindings=0 instances=8 text=8 glyphs=64 events=0 clips=8\nresource engine-image-01 image\nsurface external-image-flow column w 96 h 96\n  panel external-panel frame engine-image-01 w 96 h 96 nine_slice 1 1 1 1 border 12 12 12 12 mode stretch fill_center true\n"
}

fn emit(step: &str, input: Value, response: Option<&RpcResponse>, pass: bool) {
    println!(
        "{}",
        serde_json::to_string(&json!({
            "step": step,
            "input": input,
            "response": response,
            "pass": pass,
        }))
        .expect("probe callback serializes")
    );
}

fn stop_child(mut child: Child) {
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

fn run_probe() -> Result<(), String> {
    let wgpu_endpoint = free_endpoint();
    let ui_endpoint = free_endpoint();
    let (wgpu, ui) = spawn_services(wgpu_endpoint, ui_endpoint)?;
    let result = (|| {
        wait_health(wgpu_endpoint, "wgpu-runtime", ClientKind::Cli)?;
        emit(
            "wgpu.health",
            json!({"endpoint": wgpu_endpoint}),
            None,
            true,
        );
        let gpu_started = Instant::now();
        while gpu_started.elapsed() < POLL_TIMEOUT {
            let ready = call(
                wgpu_endpoint,
                &request(
                    "gpu-ready",
                    "wgpu-runtime",
                    "debug.window.images",
                    json!({}),
                    None,
                    None,
                    ClientKind::Cli,
                ),
            )
            .ok()
            .and_then(|response| response.result)
            .is_some_and(|result| result.get("atlas_ready").is_some());
            if ready {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        wait_health(ui_endpoint, "ui-runtime", ClientKind::Cli)?;
        emit("ui.health", json!({"endpoint": ui_endpoint}), None, true);

        let mut bytes = Vec::with_capacity(4 * 4 * 4);
        for y in 0..4 {
            for x in 0..4 {
                let color = match (x, y) {
                    (0, 0) => [255, 0, 0, 255],
                    (3, 0) => [0, 255, 0, 255],
                    (0, 3) => [0, 0, 255, 255],
                    (3, 3) => [255, 255, 0, 255],
                    _ => [32, 32, 32, 255],
                };
                bytes.extend_from_slice(&color);
            }
        }
        let upload = UiImageUploadRequest {
            source: UiImageSource {
                image_id: "engine-image-01".into(),
                media_type: "application/x-neon-rgba8".into(),
                width: 4,
                height: 4,
                bytes: bytes.clone(),
            },
        };
        let upload_request = request(
            "image-upload-01",
            "ui-runtime",
            "ui.image.upload",
            json!(upload),
            None,
            Some("image-upload-01"),
            ClientKind::ExternalHost,
        );
        let upload_response = call(ui_endpoint, &upload_request)?;
        let upload_pass = upload_response.status == neon_protocol::RpcStatus::Accepted
            && upload_response.result.as_ref().is_some_and(|result| {
                result["gpu_owner"] == "neon-wgpu-runtime-window"
                    && result["texture"]["image_id"] == "engine-image-01"
            });
        emit(
            "ui.image.upload",
            json!({"image_id": "engine-image-01", "width": 4, "height": 4, "bytes": bytes.len()}),
            Some(&upload_response),
            upload_pass,
        );
        if !upload_pass {
            return Err("UI image upload was not accepted with a texture reference".into());
        }

        let flow_request = request(
            "image-flow-01",
            "ui-runtime",
            "ui.flow.submit",
            json!({"source": image_flow_source()}),
            None,
            Some("image-flow-01"),
            ClientKind::ExternalHost,
        );
        let fragment_response = call(ui_endpoint, &flow_request)?;
        let fragment_pass = fragment_response.status == neon_protocol::RpcStatus::Accepted
            && fragment_response
                .result
                .as_ref()
                .is_some_and(|result| result["surface_id"] == "external-image-flow");
        emit(
            "ui.image.flow.binding",
            json!({"node_id": "external-panel", "image_id": "engine-image-01", "structure": "panel.frame", "source": image_flow_source()}),
            Some(&fragment_response),
            fragment_pass,
        );
        if !fragment_pass {
            return Err("external Image Flow submission was rejected".into());
        }

        let inspect_request = request(
            "image-inspect-01",
            "wgpu-runtime",
            "debug.window.images",
            json!({}),
            None,
            None,
            ClientKind::Cli,
        );
        let inspect_response = call(wgpu_endpoint, &inspect_request)?;
        let image = inspect_response
            .result
            .as_ref()
            .and_then(|result| result["external_images"].as_array())
            .and_then(|images| {
                images
                    .iter()
                    .find(|image| image["image_id"] == "engine-image-01")
            });
        let inspect_pass = inspect_response.status == neon_protocol::RpcStatus::Accepted
            && image.is_some_and(|image| {
                image["texture_index"].is_u64()
                    && image["generation"].is_u64()
                    && image["region"]["x"] == 1
                    && image["region"]["y"] == 1
                    && image["region"]["width"] == 4
                    && image["region"]["height"] == 4
                    && image["uv"].as_array().is_some_and(|uv| uv.len() == 4)
                    && image["resident"] == true
            });
        emit(
            "wgpu.image.inspect",
            json!({"image_id": "engine-image-01", "expected_region": {"width": 4, "height": 4}}),
            Some(&inspect_response),
            inspect_pass,
        );
        if !inspect_pass {
            return Err("WGPU image residency did not expose a valid slot/region".into());
        }
        let render_response = call(
            wgpu_endpoint,
            &request(
                "nine-slice-render-01",
                "wgpu-runtime",
                "wgpu.render.target.capture",
                json!({"target": "ui.color.v1", "path": "D:\\Neon3\\artifacts\\nine-slice-probe.png", "redraw": true}),
                None,
                None,
                ClientKind::Cli,
            ),
        )?;
        let render_pass = render_response.status == neon_protocol::RpcStatus::Accepted
            && render_response
                .result
                .as_ref()
                .and_then(|result| result.get("frame_sequence"))
                .and_then(Value::as_u64)
                .is_some_and(|sequence| sequence > 0);
        emit(
            "wgpu.nine_slice.render",
            json!({"frame_pair": "image-upload-01 -> nine-slice-render-01", "expected_target_size": [96, 96], "capture_path": "D:\\Neon3\\artifacts\\nine-slice-probe.png"}),
            Some(&render_response),
            render_pass,
        );
        if !render_pass {
            return Err("nine-slice render did not advance a composed frame".into());
        }
        println!(
            "{}",
            json!({"scenario": "ui.nine-slice.external-image.v1", "status": "passed"})
        );
        Ok(())
    })();
    let _ = call(
        ui_endpoint,
        &request(
            "shutdown-ui",
            "ui-runtime",
            "service.shutdown",
            json!({}),
            None,
            None,
            ClientKind::Cli,
        ),
    );
    let _ = call(
        wgpu_endpoint,
        &request(
            "shutdown-wgpu",
            "wgpu-runtime",
            "service.shutdown",
            json!({}),
            None,
            None,
            ClientKind::Cli,
        ),
    );
    stop_child(ui);
    stop_child(wgpu);
    result
}

fn main() {
    match run_probe() {
        Ok(()) => {}
        Err(error) => {
            println!(
                "{}",
                json!({"scenario": "ui.nine-slice.external-image.v1", "status": "failed", "error": error})
            );
            std::process::exit(1);
        }
    }
}
