//! JSONL acceptance probe for the Windows Acrylic backdrop path.
//! Launches the real windowed renderer, then reads its public debug snapshot.

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
use serde_json::{Value, json};

const DEFAULT_ENDPOINT: &str = "127.0.0.1:39261";
const TIMEOUT: Duration = Duration::from_secs(12);

fn request(method: &str, sequence: u64) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("window-backdrop-probe-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "window-backdrop-probe".into(),
            pid: std::process::id(),
            origin: "window-backdrop-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params: json!({}),
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("window-backdrop-probe-{sequence}")),
    }
}

fn call(endpoint: SocketAddr, method: &str, sequence: u64) -> Result<Value, String> {
    let response = RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(&request(method, sequence)))
        .map_err(|error| error.to_string())?;
    if response.status != RpcStatus::Accepted {
        return Err(format!("{method} rejected: {:?}", response.error));
    }
    Ok(response.result.unwrap_or_else(|| json!({})))
}

fn launch(endpoint: SocketAddr) -> io::Result<Child> {
    let binary = std::env::current_exe()?.with_file_name("neon-wgpu-runtime.exe");
    Command::new(binary)
        .env("NEON_WINDOW_CHROME", "borderless")
        .env("NEON_WINDOW_BACKDROP", "acrylic")
        .args(["--window-server", &endpoint.to_string()])
        .spawn()
}

fn main() -> io::Result<()> {
    let endpoint: SocketAddr = std::env::var("NEON_PROBE_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_ENDPOINT.into())
        .parse()
        .map_err(|error| io::Error::other(format!("invalid NEON_PROBE_ENDPOINT: {error}")))?;
    let mut service = launch(endpoint)?;
    let started = Instant::now();
    let health = loop {
        match call(endpoint, "service.health", 1) {
            Ok(value) => break value,
            Err(error) if started.elapsed() < TIMEOUT => {
                thread::sleep(Duration::from_millis(100));
                if service.try_wait()?.is_some() {
                    println!("{}", json!({"probe":"window-backdrop","stage":"error","error":error,"pass":false}));
                    return Err(io::Error::other("renderer exited before health check"));
                }
            }
            Err(error) => return Err(io::Error::other(format!("health timeout: {error}"))),
        }
    };
    let snapshot = call(endpoint, "debug.window.input.snapshot", 2).map_err(io::Error::other)?;
    let backdrop = snapshot.get("window_backdrop").cloned().unwrap_or_else(|| json!({}));
    let active = backdrop.get("active").and_then(Value::as_str);
    let alpha_mode = backdrop.get("surface_alpha_mode").and_then(Value::as_str);
    let shell = snapshot.get("shell_frame").cloned().unwrap_or(Value::Null);
    let shell_status = shell.get("status").and_then(Value::as_str);
    let producer = shell.get("producer_bounds_logical").and_then(Value::as_array);
    let consumer = shell.get("consumer_region_physical").and_then(Value::as_array);
    let frame = shell.get("frame").and_then(Value::as_u64);
    let scale = shell.get("scale_factor").and_then(Value::as_f64);
    let geometry_pass = shell_status == Some("paired")
        && producer.is_some_and(|bounds| bounds.len() == 4 && bounds.iter().all(Value::is_number))
        && consumer.is_some_and(|region| region.len() == 8)
        && frame.is_some_and(|value| value > 0)
        && scale.is_some_and(|value| value > 0.0);
    let pass = matches!(active, Some("native-accent-glass") | Some("acrylic"))
        && alpha_mode == Some("PreMultiplied")
        && geometry_pass;
    println!(
        "{}",
        json!({
            "probe":"window-backdrop",
            "stage":"result",
            "endpoint":endpoint,
            "input":{"NEON_WINDOW_CHROME":"borderless","NEON_WINDOW_BACKDROP":"acrylic"},
            "producer":{"health":health},
            "consumer":{"window_backdrop":backdrop},
            "geometry":{"shell_frame":shell,"production_bounds_present":producer.is_some(),"consumer_region_present":consumer.is_some(),"frame_pair":frame.is_some(),"scale":scale,"status":if geometry_pass { "paired" } else { "missing-or-stale-or-coordinate-mismatch" }},
            "pass":pass,
        })
    );
    let _ = call(endpoint, "service.shutdown", 3);
    let deadline = Instant::now() + Duration::from_secs(2);
    while service.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    if service.try_wait()?.is_none() {
        let _ = service.kill();
    }
    if pass { Ok(()) } else { Err(io::Error::other("Acrylic backdrop was not active")) }
}
