//! Grid pulse rendering probe.
//!
//! Renders a 6x6 grid of panels with random visibility (matching the
//! grid-pulse.nui case structure) and captures a PNG.

use std::{
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
    UiBounds, UiCommand, UiEffect, UiFragment, UiFragmentId, UiFragmentSubmission, UiNode,
    UiNodeId, UiNodeKind, UiStyle,
};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39252";
const TIMEOUT: Duration = Duration::from_secs(15);
const CAPTURE_PATH: &str = r"D:\Neon3\shots\grid-pulse-probe.png";

fn request(method: &str, sequence: u64, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("grid-pulse-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "grid-pulse-probe".into(),
            pid: std::process::id(),
            origin: "grid-pulse-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("grid-pulse-{sequence}")),
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

fn launch() -> std::io::Result<Child> {
    let binary = std::env::current_exe()?.with_file_name("neon-wgpu-runtime.exe");
    Command::new(binary)
        .args(["--window-server", ENDPOINT])
        .spawn()
}

fn panel(id: &str, x: f32, y: f32, w: f32, h: f32, color: [f32; 4], visible: bool) -> UiNode {
    UiNode {
        node_id: UiNodeId(id.into()),
        kind: UiNodeKind::Panel,
        bounds: UiBounds { x, y, width: w, height: h },
        layout: None,
        visible,
        enabled: true,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: UiStyle {
            background_color: color,
            ..UiStyle::default()
        },
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        children: Vec::new(),
    }
}

fn grid_fragment() -> UiFragment {
    const COLS: usize = 6;
    const ROWS: usize = 6;
    const CELL: f32 = 44.0;
    const GAP: f32 = 6.0;
    const PADDING: f32 = 16.0;
    let grid_w = COLS as f32 * CELL + (COLS as f32 - 1.0) * GAP;
    let grid_h = ROWS as f32 * CELL + (ROWS as f32 - 1.0) * GAP;

    // Simple LCG random
    let mut rng = 42u32;
    let mut next = || {
        rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
        ((rng >> 16) & 0x7fff) as f32 / 32767.0
    };

    let mut cells = Vec::new();
    let mut hot_count = 0;
    for r in 0..ROWS {
        for c in 0..COLS {
            let i = r * COLS + c;
            let v = next();
            let visible = v > 0.5;
            if visible { hot_count += 1; }
            let x = PADDING + c as f32 * (CELL + GAP);
            let y = PADDING + 32.0 + r as f32 * (CELL + GAP);
            // Blue for normal, red tint for "hot" (>0.75)
            let color = if v > 0.75 {
                [1.0, 0.4, 0.4, 1.0]
            } else {
                [0.3, 0.65, 1.0, 1.0]
            };
            cells.push(panel(&format!("cell_{i}"), x, y, CELL, CELL, color, visible));
        }
    }

    let root = UiNode {
        node_id: UiNodeId("root".into()),
        kind: UiNodeKind::Panel,
        bounds: UiBounds { x: 0.0, y: 0.0, width: PADDING * 2.0 + grid_w, height: PADDING * 2.0 + grid_h + 32.0 },
        layout: None,
        visible: true,
        enabled: true,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: UiStyle {
            background_color: [0.1, 0.1, 0.14, 1.0],
            ..UiStyle::default()
        },
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        children: cells,
    };

    println!("[grid-pulse] {hot_count}/36 cells visible");
    UiFragment {
        fragment_id: UiFragmentId("grid-pulse".into()),
        revision: Revision(1),
        root,
        effects: Vec::<UiEffect>::new(),
    }
}

fn main() -> std::io::Result<()> {
    let endpoint: SocketAddr = ENDPOINT.parse().expect("fixed endpoint");
    let mut service = launch()?;
    let started = Instant::now();

    loop {
        match call(endpoint, "service.health", 1, json!({})) {
            Ok(_) => break,
            Err(error) if started.elapsed() < TIMEOUT => {
                thread::sleep(Duration::from_millis(100));
                if service.try_wait()?.is_some() {
                    return Err(std::io::Error::other(format!("service exited: {error}")));
                }
            }
            Err(error) => return Err(std::io::Error::other(format!("health timeout: {error}"))),
        }
    }
    println!("[grid-pulse] service healthy");

    let fragment = grid_fragment();
    let submitted = call(
        endpoint,
        "wgpu.ui.submit_fragment",
        2,
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment)
        }),
    )
    .map_err(std::io::Error::other)?;
    println!("[grid-pulse] fragment submitted: {submitted}");

    thread::sleep(Duration::from_millis(800));

    std::fs::create_dir_all(r"D:\Neon3\shots").ok();
    let capture = call(
        endpoint,
        "wgpu.render.target.capture",
        3,
        json!({"target":"ui.color.v1", "path": CAPTURE_PATH, "redraw": true}),
    )
    .map_err(std::io::Error::other)?;
    println!("[grid-pulse] capture result: {capture}");

    let _ = call(endpoint, "service.shutdown", 4, json!({}));
    let deadline = Instant::now() + Duration::from_secs(2);
    while service.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    if service.try_wait()?.is_none() {
        service.kill()?;
    }
    println!("[grid-pulse] done -> {CAPTURE_PATH}");
    Ok(())
}
