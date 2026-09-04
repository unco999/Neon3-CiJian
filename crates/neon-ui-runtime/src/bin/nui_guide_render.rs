//! NUI tutorial guide image renderer.
//!
//! Renders a progressive set of NUI Flow teaching examples to PNG files by
//! driving the Neon3 headless external GPU server (same wire contract as the
//! SDKs). Each PNG becomes an illustration in `docs/nui-single-page.md`.
//!
//! Usage (from repo root):
//!   cargo run -p neon-ui-runtime --bin nui_guide_render -- <server-endpoint> <out-dir>
//! Example:
//!   cargo run -p neon-ui-runtime --bin nui_guide_render -- 127.0.0.1:43130 docs/media/nui-guide

use neon_ipc::RpcClient;
use neon_protocol::{ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, RpcRequest, RpcStatus, ServiceName};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Minimal UUIDv4 without extra dependencies (time-based rand fallback).
fn request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{nanos:016x}-{counter:016x}")
}

/// One teaching example: file name, surface id, flow source, and the surface
/// pixel size (must match the flow's authored size so the render is 1:1).
struct GuideExample {
    name: &'static str,
    width: u32,
    height: u32,
    flow: &'static str,
}

const CANVAS_W: u32 = 560;
const CANVAS_H: u32 = 300;

const fn ex(
    name: &'static str,
    flow: &'static str,
) -> GuideExample {
    GuideExample { name, width: CANVAS_W, height: CANVAS_H, flow }
}

const EXAMPLES: &[GuideExample] = &[
    // 1. Minimal document shape.
    ex(
        "01-minimal",
        "version 1\nsurface minimal column w 360 h 200 gap 8 pad 12 align stretch fill #17201E\n  text title h 24 value \"Hello NUI Flow\"\n  text subtitle h 18 value \"One surface. One panel. One text.\"\n",
    ),
    // 2. Layout primitives: row vs column + gap/pad/align.
    ex(
        "02-layout",
        "version 1\nsurface layout column w 480 h 220 gap 10 pad 12 align stretch fill #1B2530\n  panel left row w 200 h 40 gap 6 pad 6 fill #2E4255\n    text a value \"A\"\n    text b value \"B\"\n    text c value \"C\"\n  panel right row w 200 h 40 gap 6 pad 6 fill #3B5066\n    text x value \"X\"\n    text y value \"Y\"\n  text note value \"row lays children left-to-right; gap separates them\"\n",
    ),
    // 3. Typed inputs: the only way domain data reaches the UI.
    ex(
        "03-inputs",
        "version 1\ninput health f32:0..100 default 82\ninput name text default text:empty\nsurface inputs column w 400 h 200 gap 8 pad 12 align stretch fill #17201E\n  text health-label value \"Health:\"\n  progress_bar health_bar numeric $health\n  text name-label value \"Name:\"\n  text name-value value $name\n",
    ),
    // 4. Controls and semantic events.
    ex(
        "04-events",
        "version 1\ninput volume f32:0..100 default 50\nsurface events column w 420 h 230 gap 8 pad 12 align stretch fill #17201E\n  text title h 24 value \"Controls\"\n  button primary h 36 value \"Save\" event app.save\n  button danger h 36 value \"Delete\" event app.delete\n  slider volume numeric $volume\n  text hint value \"button -> semantic intent; slider drag -> value_commit\"\n",
    ),
    // 5. Scroll + overflow.
    ex(
        "05-scroll",
        "version 1\nsurface scroll column w 400 h 240 gap 8 pad 12 align stretch fill #17201E\n  text title h 24 value \"Inspector\"\n  scroll inspector column h 160 gap 4 pad 8 fill #22302D\n    text p1 value \"Material: Oak\"\n    text p2 value \"Roughness: 0.42\"\n    text p3 value \"Metallic: 0.00\"\n    text p4 value \"Opacity: 1.00\"\n    text p5 value \"Scale: 2.00 m\"\n    text p6 value \"Revision: 14\"\n    text p7 value \"Author: Studio\"\n    text p8 value \"Note: overflow scrolls\"\n",
    ),
    // 6. DataGrid with bound columns.
    ex(
        "06-datagrid",
        "version 1\ninput assets grid default grid:empty\nsurface datagrid column w 520 h 260 gap 8 pad 12 align stretch fill #17201E\n  text title h 24 value \"Assets\"\n  data_grid assets-grid h 180 source $assets capacity 4 row_height 28 overscan 1 columns \"id:80:text,name:180:edit:64:asset.name.commit,status:120:dropdown:draft|ready:asset.status.set\"\n  text hint value \"grid data arrives as a typed input frame from the domain service\"\n",
    ),
    // 7. Complete workbench: everything composed.
    ex(
        "07-workbench",
        "version 1\ninput health f32:0..100 default 64\ninput name text default text:empty\nsurface workbench column w 560 h 300 gap 8 pad 12 align stretch fill #1B2530\n  text title h 24 value \"Terrain Workbench\"\n  panel summary row w 536 h 56 gap 8 pad 8 fill #22384C\n    text terrain-name value $name\n    progress_bar hp numeric $health\n  panel tools row w 536 h 44 gap 6 pad 6 fill #2E4255\n    button t1 h 32 value \"Sculpt\" event terrain.tool.select\n    button t2 h 32 value \"Water\" event terrain.tool.select\n    button t3 h 32 value \"Material\" event terrain.tool.select\n  text status value \"mode sculpt, brush round\"\n",
    ),
];

fn request(client: &mut RpcClient, target: &str, method: &str, params: Value) -> Result<Value, String> {
    let request = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: PROTOCOL_VERSION,
        request_id: RequestId(request_id()),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "nui-guide-render".into(),
            pid: std::process::id(),
            origin: "nui-guide-render".into(),
        },
        target: ServiceName(target.into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(format!("{method}-{}", request_id())),
    };
    let response = client.call(&request).map_err(|e| e.to_string())?;
    if response.status != RpcStatus::Accepted {
        let code = response.error.as_ref().map(|e| e.code.clone()).unwrap_or_default();
        let message = response.error.as_ref().map(|e| e.message.clone()).unwrap_or_default();
        return Err(format!("{method} rejected: {code} {message}"));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: nui_guide_render <server-endpoint> <out-dir>");
        std::process::exit(2);
    }
    let endpoint: SocketAddr = args[1].parse().expect("valid socket addr");
    let out_dir = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let mut client = RpcClient::connect(endpoint)
        .expect("connect to headless server")
        .with_timeout(Duration::from_secs(30))
        .expect("set timeout");

    // Health first.
    let health = request(&mut client, "wgpu-runtime", "service.health", json!({})).expect("health");
    assert_eq!(health.get("status").and_then(Value::as_str), Some("healthy"), "server must be healthy");

    // One shared surface reused across examples; each submit replaces the
    // previous fragment so the render loop keeps producing frames (avoids the
    // static-skip that would starve freshly opened surfaces).
    let surface_id = "guide";
    let mut opened = false;
    for example in EXAMPLES {
        let submitted = request(
            &mut client,
            "wgpu-runtime",
            "ui.flow.submit",
            json!({"source": example.flow}),
        ).unwrap_or_else(|e| panic!("{} submit: {e}", example.name));
        let _declared = submitted.get("surface_id").and_then(Value::as_str).unwrap_or(surface_id);

        if !opened {
            let _opened = request(
                &mut client,
                "wgpu-runtime",
                "render.surface.open",
                json!({
                    "session_id": "nui-guide",
                    "surface_id": surface_id,
                    "kind": "screen_ui",
                    "size": {"width": example.width, "height": example.height},
                    "format": "rgba8unorm",
                    "color_space": "srgb",
                    "depth": false,
                    "buffer_count": 2,
                }),
            ).unwrap_or_else(|e| panic!("{} open: {e}", example.name));
            opened = true;
        }

        std::thread::sleep(Duration::from_millis(700));
        let png_path = out_dir.join(format!("{}.png", example.name));
        let captured = request(
            &mut client,
            "wgpu-runtime",
            "render.surface.capture_png",
            json!({"surface_id": surface_id, "path": png_path.to_string_lossy()}),
        ).unwrap_or_else(|e| panic!("{} capture: {e}", example.name));
        println!(
            "{} -> {} ({}) frame={}",
            example.name,
            captured.get("artifact_path").and_then(Value::as_str).unwrap_or("?"),
            captured.get("rgba_bytes").and_then(Value::as_u64).unwrap_or(0),
            captured.get("frame_sequence").and_then(Value::as_u64).unwrap_or(0),
        );
    }

    // Clean shutdown.
    let _ = request(&mut client, "wgpu-runtime", "service.shutdown", json!({}));
    println!("guide images rendered to {}", out_dir.display());
}
