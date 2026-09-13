//! Grid pulse high-frequency update probe.
//!
//! Renders a 6x6 grid, then rapidly resubmits the fragment with changing
//! cell visibility/color (60 iterations ~ 1 second at 60fps target).
//! Verifies the runtime stays stable under high-frequency fragment churn.

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
    UiBounds, UiClipShape, UiCommand, UiEffect, UiFragment, UiFragmentId, UiFragmentSubmission, UiNode,
    UiNodeId, UiNodeKind, UiStyle,
};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39253";
const TIMEOUT: Duration = Duration::from_secs(15);
const CAPTURE_PATH: &str = r"D:\Neon3\shots\grid-pulse-probe.png";
const UPDATE_ITERATIONS: usize = 60;
const UPDATE_INTERVAL_MS: u64 = 16; // ~60fps

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
        clip_shape: UiClipShape::default(),
        children: Vec::new(),
    }
}

/// Build a grid fragment with the given RNG seed. Different seeds produce
/// different cell visibility/color patterns.
fn grid_fragment(seed: u32, revision: u32) -> (UiFragment, usize) {
    const COLS: usize = 6;
    const ROWS: usize = 6;
    const CELL: f32 = 44.0;
    const GAP: f32 = 6.0;
    const PADDING: f32 = 16.0;
    let grid_w = COLS as f32 * CELL + (COLS as f32 - 1.0) * GAP;
    let grid_h = ROWS as f32 * CELL + (ROWS as f32 - 1.0) * GAP;

    let mut rng = seed;
    let mut next = || {
        rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
        ((rng >> 16) & 0x7fff) as f32 / 32767.0
    };

    let mut cells = Vec::new();
    let mut visible_count = 0;
    for r in 0..ROWS {
        for c in 0..COLS {
            let i = r * COLS + c;
            let v = next();
            let visible = v > 0.5;
            if visible { visible_count += 1; }
            let x = PADDING + c as f32 * (CELL + GAP);
            let y = PADDING + 32.0 + r as f32 * (CELL + GAP);
            // Color shifts with seed to test color churn
            let hue = (seed as f32 * 0.01) % 1.0;
            let color = if v > 0.75 {
                // Hot: red-orange, brightness varies with seed
                [1.0, 0.3 + hue * 0.3, 0.2, 1.0]
            } else {
                // Normal: blue-cyan, brightness varies with seed
                [0.2 + hue * 0.2, 0.5 + hue * 0.2, 1.0, 1.0]
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
        clip_shape: UiClipShape::default(),
        children: cells,
    };

    (UiFragment {
        fragment_id: UiFragmentId("grid-pulse".into()),
        revision: Revision(revision as u64),
        root,
        effects: Vec::<UiEffect>::new(),
    }, visible_count)
}

fn main() -> std::io::Result<()> {
    let endpoint: SocketAddr = ENDPOINT.parse().expect("fixed endpoint");
    let mut service = launch()?;
    let started = Instant::now();

    // Wait for health
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

    // High-frequency update loop
    let mut failures = 0;
    let mut min_time_ms = f64::MAX;
    let mut max_time_ms = 0.0f64;
    let mut total_time_ms = 0.0f64;
    let mut last_visible = 0;

    for iter in 0..UPDATE_ITERATIONS {
        let seed = (iter as u32).wrapping_mul(2654435761).wrapping_add(12345);
        let (fragment, visible_count) = grid_fragment(seed, 1 + iter as u32);
        last_visible = visible_count;

        let t0 = Instant::now();
        let seq = 100 + iter as u64;
        let result = call(
            endpoint,
            "wgpu.ui.submit_fragment",
            seq,
            json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment)
            }),
        );
        let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
        total_time_ms += elapsed;
        min_time_ms = min_time_ms.min(elapsed);
        max_time_ms = max_time_ms.max(elapsed);

        match result {
            Ok(_) => {}
            Err(e) => {
                failures += 1;
                if failures <= 3 {
                    eprintln!("[grid-pulse] iter {iter} submit failed: {e}");
                }
            }
        }

        // Throttle to ~60fps
        if elapsed < UPDATE_INTERVAL_MS as f64 {
            thread::sleep(Duration::from_millis(UPDATE_INTERVAL_MS - elapsed as u64));
        }
    }

    println!("[grid-pulse] high-frequency loop complete: {UPDATE_ITERATIONS} iterations");
    println!("[grid-pulse]   failures: {failures}/{UPDATE_ITERATIONS}");
    println!("[grid-pulse]   submit time: min={min_time_ms:.2}ms max={max_time_ms:.2}ms avg={:.2}ms", total_time_ms / UPDATE_ITERATIONS as f64);
    println!("[grid-pulse]   last frame: {last_visible}/36 cells visible");
    println!("[grid-pulse] entering live update mode (Ctrl+C to exit)...");

    // Live update loop: keep changing the grid at ~10fps so the window stays active
    let mut live_iter = UPDATE_ITERATIONS;
    loop {
        let seed = (live_iter as u32).wrapping_mul(2654435761).wrapping_add(12345);
        let (fragment, visible_count) = grid_fragment(seed, 1 + live_iter as u32);
        let seq = 100 + live_iter as u64;
        let _ = call(
            endpoint,
            "wgpu.ui.submit_fragment",
            seq,
            json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment)
            }),
        );
        if live_iter % 30 == 0 {
            println!("[grid-pulse] live frame {live_iter}: {visible_count}/36 cells visible");
        }
        live_iter += 1;
        thread::sleep(Duration::from_millis(100)); // ~10fps for live view
    }
}
