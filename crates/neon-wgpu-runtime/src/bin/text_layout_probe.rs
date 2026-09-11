//! Text layout rendering probe.
//!
//! Launches the real windowed WGPU runtime, submits a fragment with canvas
//! baseline guides + multiple text scenarios, and captures a PNG for visual
//! inspection of line spacing, baseline alignment, and CJK/Latin mixing.
//!
//! Usage:
//!   cargo run --release --bin text_layout_probe
//! Output: D:\Neon3\shots\text-layout-probe.png

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
    UiBounds, UiCanvasData, UiCanvasLine, UiCommand, UiFragment, UiFragmentId,
    UiFragmentSubmission, UiNode, UiNodeId, UiNodeKind, UiRichTextSpan, UiStyle, TextRef,
};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39251";
const TIMEOUT: Duration = Duration::from_secs(15);
const CAPTURE_PATH: &str = r"D:\Neon3\shots\text-layout-probe.png";

fn request(method: &str, sequence: u64, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("text-layout-probe-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "text-layout-probe".into(),
            pid: std::process::id(),
            origin: "text-layout-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("text-layout-probe-{sequence}")),
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

fn label_node(id: &str, x: f32, y: f32, w: f32, h: f32, text: TextRef) -> UiNode {
    UiNode {
        node_id: UiNodeId(id.into()),
        kind: UiNodeKind::Label,
        bounds: UiBounds { x, y, width: w, height: h },
        layout: None,
        visible: true,
        enabled: true,
        text_key: None,
        text: Some(text),
        image: None,
        surface: None,
        style: UiStyle::default(),
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        children: Vec::new(),
    }
}

fn hline(id: &str, x: f32, y: f32, w: f32, color: [f32; 4]) -> UiCanvasLine {
    UiCanvasLine {
        id: id.into(),
        start: [x, y],
        end: [x + w, y],
        width: 1.0,
        color,
    }
}

fn test_fragment() -> UiFragment {
    let guide_red: [f32; 4] = [1.0, 0.3, 0.3, 0.6];
    let guide_blue: [f32; 4] = [0.3, 0.6, 1.0, 0.6];
    let guide_green: [f32; 4] = [0.3, 1.0, 0.5, 0.6];

    // Build canvas guide lines: horizontal lines every 24px (approx line height)
    // across the full width, plus vertical section dividers.
    let mut lines = Vec::new();
    for i in 0..30 {
        let y = 40.0 + i as f32 * 24.0;
        lines.push(hline(&format!("guide-h-{i}"), 20.0, y, 860.0, guide_blue));
    }
    // Section dividers
    for (i, x) in [300.0, 580.0].iter().enumerate() {
        lines.push(UiCanvasLine {
            id: format!("guide-v-{i}"),
            start: [*x, 20.0],
            end: [*x, 680.0],
            width: 1.0,
            color: guide_green,
        });
    }
    // Per-section baseline references (red = expected baseline at top+ascent)
    // Actual font metrics: ascent=15.488, line_height=19.344 (FONT_RASTER_SIZE=16)
    // But layout_text vertically centers: top = y + (height - block_height)*0.5
    // For single-line with height≈line_height: top ≈ y + (h-19.344)*0.5
    // We draw reference lines at the EXPECTED baseline after centering.
    let ascent = 15.488_f32;
    let line_h = 19.344_f32;
    let baseline1 = |y: f32, h: f32, lines: f32| -> f32 {
        let block = line_h * lines;
        let top = y + (h - block).max(0.0) * 0.5;
        top + ascent
    };
    lines.push(hline("s1-baseline", 30.0, baseline1(52.0, 120.0, 3.0), 250.0, guide_red));
    lines.push(hline("s2-baseline", 310.0, baseline1(52.0, 120.0, 3.0), 250.0, guide_red));
    lines.push(hline("s3-baseline", 590.0, baseline1(52.0, 120.0, 3.0), 280.0, guide_red));
    // Rich text: max_scale=2.0, line_height=19.344*2=38.688, baseline = top + ascent*2
    {
        let block = line_h * 2.0;
        let top = 228.0 + (60.0 - block).max(0.0) * 0.5;
        lines.push(hline("s4-baseline", 30.0, top + ascent * 2.0, 840.0, guide_red));
    }
    lines.push(hline("s5-baseline1", 30.0, baseline1(350.0, 24.0, 1.0), 200.0, guide_red));
    lines.push(hline("s5-baseline2", 250.0, baseline1(350.0, 24.0, 1.0), 200.0, guide_red));
    lines.push(hline("s5-baseline3", 470.0, baseline1(350.0, 24.0, 1.0), 200.0, guide_red));
    lines.push(hline("s6-baseline", 30.0, baseline1(428.0, 80.0, 2.0), 150.0, guide_red));
    lines.push(hline("s7-baseline", 200.0, baseline1(428.0, 80.0, 4.0), 300.0, guide_red));

    // Section titles (drawn as canvas lines won't show text; use Label nodes)
    let mut children: Vec<UiNode> = vec![
        // ── Section 1: Multi-line English wrapping ──
        label_node(
            "s1-title", 30.0, 24.0, 250.0, 20.0,
            TextRef::Literal { value: "1. English wrap".into() },
        ),
        label_node(
            "s1-body", 30.0, 52.0, 250.0, 120.0,
            TextRef::Literal {
                value: "The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs.".into(),
            },
        ),

        // ── Section 2: Multi-line CJK ──
        label_node(
            "s2-title", 310.0, 24.0, 250.0, 20.0,
            TextRef::Literal { value: "2. 中文换行测试".into() },
        ),
        label_node(
            "s2-body", 310.0, 52.0, 250.0, 120.0,
            TextRef::Literal {
                value: "天地玄黄宇宙洪荒日月盈昃辰宿列张寒来暑往秋收冬藏闰余成岁律吕调阳".into(),
            },
        ),

        // ── Section 3: CJK + Latin mixed ──
        label_node(
            "s3-title", 590.0, 24.0, 280.0, 20.0,
            TextRef::Literal { value: "3. 中英混排 CJK+Latin".into() },
        ),
        label_node(
            "s3-body", 590.0, 52.0, 280.0, 120.0,
            TextRef::Literal {
                value: "Neon3 是一个 UI 框架，支持 Rust 和 TypeScript SDK，版本 v0.2.7 修复了文字排版。".into(),
            },
        ),

        // ── Section 4: Rich text with mixed scales ──
        label_node(
            "s4-title", 30.0, 200.0, 250.0, 20.0,
            TextRef::Literal { value: "4. Rich text scales".into() },
        ),
        label_node(
            "s4-body", 30.0, 228.0, 840.0, 60.0,
            TextRef::Rich {
                spans: vec![
                    UiRichTextSpan { value: "Small ".into(), color: [0.9, 0.95, 0.98, 1.0], scale: 0.6 },
                    UiRichTextSpan { value: "Normal ".into(), color: [0.9, 0.95, 0.98, 1.0], scale: 1.0 },
                    UiRichTextSpan { value: "Large ".into(), color: [1.0, 0.8, 0.4, 1.0], scale: 1.5 },
                    UiRichTextSpan { value: "Huge ".into(), color: [1.0, 0.5, 0.5, 1.0], scale: 2.0 },
                    UiRichTextSpan { value: "back to normal baseline".into(), color: [0.9, 0.95, 0.98, 1.0], scale: 1.0 },
                ],
            },
        ),

        // ── Section 5: Single-line precision test ──
        label_node(
            "s5-title", 30.0, 320.0, 400.0, 20.0,
            TextRef::Literal { value: "5. Baseline precision (red line = expected baseline)".into() },
        ),
        label_node(
            "s5-aaa", 30.0, 350.0, 200.0, 24.0,
            TextRef::Literal { value: "AAAA bbbb 1234".into() },
        ),
        label_node(
            "s5-cjk", 250.0, 350.0, 200.0, 24.0,
            TextRef::Literal { value: "汉字测试 对齐".into() },
        ),
        label_node(
            "s5-mixed", 470.0, 350.0, 200.0, 24.0,
            TextRef::Literal { value: "Test 测试 xX".into() },
        ),

        // ── Section 6: Long word overflow ──
        label_node(
            "s6-title", 30.0, 400.0, 400.0, 20.0,
            TextRef::Literal { value: "6. Long word overflow".into() },
        ),
        label_node(
            "s6-body", 30.0, 428.0, 150.0, 80.0,
            TextRef::Literal {
                value: "Supercalifragilisticexpialidocious".into(),
            },
        ),

        // ── Section 7: Explicit newlines ──
        label_node(
            "s7-title", 200.0, 400.0, 300.0, 20.0,
            TextRef::Literal { value: "7. Explicit newlines".into() },
        ),
        label_node(
            "s7-body", 200.0, 428.0, 300.0, 80.0,
            TextRef::Literal {
                value: "Line one\nLine two\n\nLine four (blank above)".into(),
            },
        ),
    ];

    // Canvas node with guide lines
    children.push(UiNode {
        node_id: UiNodeId("guides".into()),
        kind: UiNodeKind::Canvas,
        bounds: UiBounds { x: 0.0, y: 0.0, width: 900.0, height: 700.0 },
        layout: None,
        visible: true,
        enabled: false,
        text_key: None,
        text: None,
        image: None,
        surface: None,
        style: UiStyle::default(),
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        children: Vec::new(),
    });

    UiFragment {
        fragment_id: UiFragmentId("text-layout-probe".into()),
        revision: Revision(1),
        root: UiNode {
            node_id: UiNodeId("root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 900.0, height: 700.0 },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [0.06, 0.08, 0.12, 1.0],
                ..UiStyle::default()
            },
            enter_transition: None,
            world_depth: None,
            world_scale: None,
            children,
        },
        effects: vec![neon_ui_schema::UiEffect::CanvasData {
            node_id: UiNodeId("guides".into()),
            data: UiCanvasData {
                version: 1,
                points: vec![],
                lines,
            },
        }],
    }
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
                    return Err(std::io::Error::other(format!(
                        "service exited while waiting: {error}"
                    )));
                }
            }
            Err(error) => return Err(std::io::Error::other(format!("health timeout: {error}"))),
        }
    }
    println!("[text-layout-probe] service healthy");

    // Submit fragment
    let fragment = test_fragment();
    let submitted = call(
        endpoint,
        "wgpu.ui.submit_fragment",
        2,
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment)
        }),
    )
    .map_err(std::io::Error::other)?;
    println!("[text-layout-probe] fragment submitted: {submitted}");

    // Wait a frame for render
    thread::sleep(Duration::from_millis(500));

    // Capture
    std::fs::create_dir_all(r"D:\Neon3\shots").ok();
    let capture = call(
        endpoint,
        "wgpu.render.target.capture",
        3,
        json!({"target":"ui.color.v1", "path": CAPTURE_PATH, "redraw": true}),
    )
    .map_err(std::io::Error::other)?;
    println!("[text-layout-probe] capture result: {capture}");

    // Diagnostics
    let diagnostics =
        call(endpoint, "wgpu.render.diagnostics", 4, json!({})).map_err(std::io::Error::other)?;
    println!("[text-layout-probe] diagnostics: {diagnostics}");

    // Shutdown
    let _ = call(endpoint, "service.shutdown", 5, json!({}));
    let deadline = Instant::now() + Duration::from_secs(2);
    while service.try_wait()?.is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    if service.try_wait()?.is_none() {
        service.kill()?;
        let _ = service.wait();
    }

    println!("[text-layout-probe] done. PNG saved to {CAPTURE_PATH}");
    Ok(())
}
