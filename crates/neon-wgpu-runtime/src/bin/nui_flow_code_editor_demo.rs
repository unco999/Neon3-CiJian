//! nui_flow_code_editor_demo — 独立 NUI 组件演示。
//!
//! 打开一个 Neon3 窗口，把一份声明了 `code_editor` 的 NUI Flow fixture
//! lower 成携带 `UiEffect::CodeEditorDeclaration` 的 UiFragment，通过
//! `wgpu.ui.submit_fragment` 提交给统一 WGPU runtime 绘制；编辑器本地渲染
//! 文本、行号、光标、选区与补全提示，编辑语义仍由 neon-editor-core 承担，
//! 失焦 / Esc / Ctrl+S 时经 `ui.host.inbound` RPC 把 `DocumentCommit` 全文
//! 发给本地宿主（本程序内的 host stub 会打印机器可读证据）。
//!
//! 用法：`cargo run -p neon-wgpu-runtime --bin nui_flow_code_editor_demo`

use std::net::SocketAddr;
use std::time::Duration;

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcError, RpcRequest,
    RpcResponse, RpcStatus, ServiceName,
};
use neon_ui_runtime::{
    UiInputStore, UiLocalPresentationState, compile_nui_flow_program, evaluate_ui_program,
    lower_nui_flow_effects, parse_nui_flow,
};
use neon_ui_schema::{
    UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_CAPABILITY_NAME,
    UI_PROGRAM_SCHEMA_VERSION, UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
    UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME, UiBounds, UiCommand, UiCpuViewport, UiFragment,
    UiFragmentId, UiFragmentSubmission, UiIntent, UiProgramCapability, UiProgramCapabilityOwner,
    UiProgramCapabilityStatus, UiProgramResource, UiProgramResourceKind, UiProgramRevision,
    UiSemanticEvent, UiNode, UiNodeKind,
};
use serde_json::json;

const WGPU_ENDPOINT: &str = "127.0.0.1:43110";
const HOST_ENDPOINT: &str = "127.0.0.1:43111";
const FIXTURE: &str = include_str!("../../tests/fixtures/ui/code-editor-demo.nui");

/// `pulse-neon-text` — A-route bitmap neon for editor/UI text. The unified
/// text pipeline samples the glyph atlas with 1px + 2px rings and hands the
/// package `edge_ink`; the package only mixes colors (pure function, no GPU
/// bindings of its own). Tuning comes from `view.extras[0]`:
///   x: glow strength (default 1.0), y: pulse Hz (0 = steady),
///   z: hue drift speed (default 0.15), w: chroma mix (default 0.9).
const PULSE_NEON_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let tuning = view.extras[0];
    let strength = select(1.6, tuning.x, tuning.x > 0.0);
    let pulse_hz = max(0.0, tuning.y);
    let hue_speed = select(0.12, tuning.z, tuning.z > 0.0);
    let chroma = select(0.95, tuning.w, tuning.w > 0.0);
    let time = input.time_seconds;
    let ink = input.edge_ink;
    let core = smoothstep(0.08, 0.55, input.coverage);
    // Cyan -> violet -> pink travelling palette (per-glyph phase so the
    // gradient shimmers along the line instead of blinking uniformly).
    let a = vec3<f32>(0.35, 0.85, 1.0);
    let b = vec3<f32>(0.60, 0.35, 1.0);
    let c = vec3<f32>(1.0, 0.32, 0.72);
    let d = vec3<f32>(0.0, 0.4, 0.8);
    let hue = a + b * cos(6.283185307 * (c * (time * hue_speed + input.local_position.x * 0.22) + d));
    // Layered A-route glow: tight halo, wide bloom, and an inner hot core.
    let halo = pow(ink, 3.0) * strength * 1.5;
    let bloom = pow(ink, 1.35) * strength * 0.85;
    let inner = core * core * 1.9;
    let pulse = 0.75 + 0.25 * sin(time * 6.283185307 * pulse_hz);
    let base = input.base_color.rgb;
    let hue_mix = mix(base, hue, chroma * (1.0 - core * 0.45));
    let rgb = hue_mix * (inner + (halo + bloom) * pulse * (1.0 - core * 0.5));
    let alpha = clamp(max(core * 1.25, (halo + bloom) * 0.48), 0.0, 1.0);
    return vec4<f32>(rgb, alpha);
}
"#;
const TEXT_SWEEP_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let tuning = view.extras[0];
    let speed = select(1.2, tuning.x, tuning.x > 0.0);
    let block_w = select(0.035, tuning.y, tuning.y > 0.0);
    let strength = select(3.0, tuning.z, tuning.z > 0.0);
    let slope = select(0.8, tuning.w, tuning.w > 0.0);
    let time = input.time_seconds;
    let core = smoothstep(0.08, 0.6, input.coverage);
    // ONE small diagonal highlight. `p` is the block position along the line
    // (wraps via fract); `g` is each glyph's position in the same period
    // (period ~833px > line width, so at most one block is visible). Only the
    // glyph nearest the block centre lights up; the block centre's y follows
    // the block position so the highlight sweeps left -> right on a diagonal.
    let p = fract(time * speed);
    let g = fract(input.bounds.x * 0.0012 + 0.5);
    let d = fract(p - g + 0.5) - 0.5;
    let gx = exp(-pow(d / block_w, 2.0));
    let block_cy = 0.5 + (p - 0.5) * slope;
    let gy = exp(-pow((input.local_position.y - block_cy) / 0.22, 2.0));
    let block = gx * gy;
    // Very dark ink base; only the block inside glyph strokes brightens.
    let base = input.base_color.rgb * (0.22 + 0.22 * core);
    let light = vec3<f32>(0.85, 0.97, 1.0);
    let lit = base + light * block * strength * core;
    return vec4<f32>(lit, core);
}
"#;
const TEXT_DISTORT_SOURCE: &str = r#"
// Displacement hook: warps the glyph sample coordinates in UV space. The
// renderer detects this function and routes every glyph sample through it.
fn text_material_displace(uv: vec2<f32>, origin: vec2<f32>, span: vec2<f32>, pixel: vec2<f32>, local: vec2<f32>, time: f32) -> vec2<f32> {
    let atlas = vec2<f32>(textureDimensions(glyph_atlas));
    let amp = 0.55;
    let wob_x = sin(time * 3.2 + pixel.y * 0.09 + local.y * 7.0) * amp;
    let wob_y = sin(time * 2.4 + pixel.x * 0.07 + local.x * 9.0) * amp * 0.6;
    let uv_space = vec2<f32>(wob_x, wob_y) / atlas;
    // Keep the warp inside the glyph's own rect so neighbours never bleed in.
    let max_warp = min(span * 0.45, vec2<f32>(0.5) / atlas);
    return uv + clamp(uv_space, -max_warp, max_warp);
}
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let tuning = view.extras[0];
    let strength = select(1.1, tuning.x, tuning.x > 0.0);
    let time = input.time_seconds;
    let core = smoothstep(0.06, 0.55, input.coverage);
    let water = vec3<f32>(0.25, 0.8, 1.0);
    let deep = vec3<f32>(0.1, 0.4, 0.9);
    let sheen = 0.5 + 0.5 * sin(time * 3.2 + input.bounds.x * 0.02 + input.local_position.y * 6.0);
    let hue = mix(deep, water, sheen);
    let base = mix(input.base_color.rgb, hue, 0.6);
    let halo = pow(input.edge_ink, 2.2) * strength * 0.7;
    let rgb = base * (0.7 + 0.5 * core) + hue * halo * 0.8;
    let alpha = clamp(max(core, halo * 0.5), 0.0, 1.0);
    return vec4<f32>(rgb, alpha);
}
"#;
const RAINBOW_SOURCE: &str = r#"
fn hsv_to_rgb(h: f32) -> vec3<f32> {
    let r = 0.5 + 0.5 * cos(6.283185307 * h);
    let g = 0.5 + 0.5 * cos(6.283185307 * (h + 0.3333333));
    let b = 0.5 + 0.5 * cos(6.283185307 * (h + 0.6666667));
    return vec3<f32>(r, g, b);
}
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let tuning = view.extras[0];
    let speed = select(0.22, tuning.x, tuning.x > 0.0);
    let chroma = select(0.95, tuning.y, tuning.y > 0.0);
    let time = input.time_seconds;
    let core = smoothstep(0.06, 0.55, input.coverage);
    // Hue travels along the line (bounds.x) and drifts with time; the glyph
    // quad's local y adds a soft vertical gradient for a prism feel.
    let h = fract(time * speed + input.bounds.x * 0.012 + input.local_position.y * 0.35);
    let hue = hsv_to_rgb(h);
    let base = input.base_color.rgb;
    let rgb = mix(base, hue * (0.7 + 0.5 * input.coverage), chroma * (0.55 + 0.45 * input.coverage));
    let fringe = pow(input.edge_ink, 1.8) * 0.4;
    let rgb_fringe = rgb + hue * fringe;
    let alpha = clamp(max(core, fringe * 0.6), 0.0, 1.0);
    return vec4<f32>(rgb, alpha);
}
"#;
const AURORA_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let tuning = view.extras[0];
    let speed = select(0.18, tuning.x, tuning.x > 0.0);
    let strength = select(1.2, tuning.y, tuning.y > 0.0);
    let time = input.time_seconds;
    let core = smoothstep(0.06, 0.55, input.coverage);
    // Two flowing aurora bands (green-teal and violet) crossing the glyph.
    let g = sin(time * speed * 6.283185307 + input.bounds.x * 0.013 + input.local_position.y * 5.0);
    let v = sin(time * speed * 6.283185307 * 0.7 + input.bounds.x * 0.009 - input.local_position.y * 4.0 + 2.0);
    let aurora = vec3<f32>(0.15, 0.9, 0.55) * (0.55 + 0.45 * g)
               + vec3<f32>(0.55, 0.35, 1.0) * (0.45 - 0.35 * v);
    let base = mix(input.base_color.rgb, aurora, 0.6 * (1.0 - core * 0.35));
    let halo = pow(input.edge_ink, 2.5) * strength * 0.8;
    let rgb = base * (0.65 + 0.6 * core) + aurora * halo;
    let alpha = clamp(max(core, halo * 0.5), 0.0, 1.0);
    return vec4<f32>(rgb, alpha);
}
"#;
/// FNV-1a 64, identical to `neon-wgpu-runtime::shader_registry::shader_source_digest`
/// so the control-plane digest check accepts the demo package.
fn shader_digest(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn main() {
    let wgpu_endpoint: SocketAddr = WGPU_ENDPOINT.parse().expect("wgpu endpoint is valid");
    let host_endpoint: SocketAddr = HOST_ENDPOINT.parse().expect("host endpoint is valid");

    // 1. Local host stub: receives editor `DocumentCommit` events over the
    //    public `ui.host.inbound` RPC and prints machine-readable evidence.
    let host_thread = std::thread::spawn(move || {
        if let Err(error) = serve_host_stub(host_endpoint) {
            eprintln!("code-editor host stub failed: {error}");
        }
    });

    // 2. NUI Flow fixture -> UiFragment with the CodeEditorDeclaration effect.
    let fragment = build_code_editor_fragment().expect("code_editor fixture must lower");
    fragment
        .validate()
        .expect("code_editor demo fragment must validate");

    // 3. Submit through the same public protocol a CLI/AI client would use.
    //    The window event loop must live on the main thread, so the submitter
    //    retries from a helper thread until the window server is ready.
    let submitter = std::thread::spawn(move || {
        register_text_shader_package(wgpu_endpoint);
        submit_code_editor_fragment(wgpu_endpoint, fragment);
    });

    // 4. Window server: the only window + GPU owner, driven by the public
    //    neon3.rpc protocol. Fragments arrive over `wgpu.ui.submit_fragment`.
    //    The shared EditorBridge wires the renderer's input sink + the
    //    presentations slot + the fragment observer, so the editor core stays
    //    in the ui-runtime component while the renderer only draws snapshots.
    let editor_bridge =
        std::sync::Arc::new(neon_ui_runtime::editor_component::EditorBridge::new());

    // Optional headless self-check: `NEON_DEMO_AUTO_INPUT=1` injects one
    // character 8s after boot straight into the shared EditorBridge, so the
    // whole insert -> edit-fx -> presentation -> renderer material pass chain
    // can be verified without OS keyboard focus. Kept env-gated: normal runs
    // never auto-type.
    if std::env::var("NEON_DEMO_AUTO_INPUT").is_ok_and(|v| v == "1" || v == "true") {
        let bridge = editor_bridge.clone();
        let boot = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(8));
            let now = boot.elapsed().as_secs_f32();
            let event = neon_ui_schema::UiEditorInputEvent::Key {
                path: "code-editor-demo/source-view".to_string(),
                kind: neon_ui_schema::UiEditorKeyKind::Character("x".to_string()),
                text: Some("x".to_string()),
                shift: false,
                ctrl: false,
                viewport_height: 0.0,
                viewport_width: 0.0,
                row_height: 0.0,
                gutter_width: 0.0,
            };
            let _commits = bridge.handle_input(&event, now);
        });
    }
    let input_sink: Box<
        dyn FnMut(neon_ui_schema::UiEditorInputEvent, f32) -> Vec<neon_wgpu_runtime::EditorCommit>
            + Send,
    > = {
        let bridge = editor_bridge.clone();
        Box::new(move |event, now| {
            bridge
                .handle_input(&event, now)
                .into_iter()
                .map(|commit| neon_wgpu_runtime::EditorCommit {
                    node_path: commit.node_path,
                    event_action: commit.event_action,
                    document: commit.document,
                })
                .collect()
        })
    };
    let fragment_observer: Box<
        dyn FnMut(
                &std::collections::HashMap<
                    neon_ui_schema::UiFragmentId,
                    neon_ui_schema::UiFragment,
                >,
            ) + Send,
    > = {
        let bridge = editor_bridge.clone();
        Box::new(move |fragments| bridge.sync_fragments(fragments))
    };
    let handle = neon_wgpu_runtime::EditorBridgeHandle {
        input_sink: Some(input_sink),
        external_presentations: Some(editor_bridge.presentations.clone()),
        fragment_observer: Some(fragment_observer),
    };
    let window_result = neon_wgpu_runtime::WindowedRuntime::run_server_with_eventd_bridged(
        1,
        wgpu_endpoint,
        Some(host_endpoint),
        None,
        None,
        false,
        Some(handle),
    );

    let _ = submitter.join();
    let _ = host_thread.join();
    if let Err(error) = window_result {
        eprintln!("window runtime exited with error: {error}");
    }
}

/// Transient type-in: the glyph materializes with a holographic scan band,
/// a cyan energy lift, and an inner white flash, then settles into the token
/// color. The renderer expires the fx after ~420 ms, so only the leading edge
/// shows; fract() keeps the shader self-contained if a frame lingers.
const TEXT_TYPE_IN_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let t = fract(input.time_seconds * 2.4);
    let appear = smoothstep(0.0, 0.35, t);
    let core = smoothstep(0.08, 0.55, input.coverage);
    // Holographic scan band sweeping top -> bottom once across the glyph box.
    let scan_y = input.local_position.y / max(input.glyph_quad.w, 1.0);
    let scan = smoothstep(0.10, 0.0, abs(scan_y - t));
    // Energy lift: the glyph rises out of the baseline.
    let lift = (1.0 - appear) * 12.0;
    let holo = vec3<f32>(0.35, 1.0, 0.95);
    let base = input.base_color.rgb;
    let energy = (1.0 - appear) * 1.2;
    let rgb = mix(base, holo, 0.55) * (0.72 + 0.55 * scan + energy * 0.5);
    let glow = pow(input.edge_ink, 2.0) * (0.9 + scan * 1.4) * (1.0 - t * 0.4);
    let alpha = clamp(core * appear + glow * 0.55, 0.0, 1.0);
    return vec4<f32>(rgb + holo * glow * 0.9, alpha);
}
"#;

/// Transient delete: the ghost detonates into cyan energy -- a leading white
/// flash, a fast exponential boom, per-pixel particle jitter, and a flickering
/// ember tail as it fades (A-route, no SDF; the renderer snaps the ghost at the
/// pre-delete position and expires the fx after ~620 ms).
const TEXT_DELETE_FRAGMENT_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let t = fract(input.time_seconds * 1.65);
    let seed = dot(input.local_position, vec2<f32>(3.7, 11.3)) + 0.31;
    let core = smoothstep(0.08, 0.55, input.coverage);
    // Particle jitter: per-pixel phase grows with t^2 so the glyph shreds.
    let shake = (sin(seed * 7.0 + t * 44.0) + cos(seed * 5.0 - t * 31.0)) * 0.5;
    let jitter = shake * t * t * 4.0;
    // Energy boom: bright cyan flash at t=0, exponential decay.
    let boom = exp(-t * 6.5);
    let holo = vec3<f32>(0.3, 0.95, 1.0);
    let flicker = 0.5 + 0.5 * sin(input.time_seconds * 71.0 + seed * 13.0);
    let base = input.base_color.rgb;
    let rgb = mix(base, holo, 0.45 + 0.45 * boom) * (0.7 + flicker * 0.35 + boom * 1.5);
    let edge = pow(input.edge_ink, 2.0) * boom * 1.3;
    let fade = 1.0 - smoothstep(0.0, 1.0, t);
    let alpha = clamp(core * fade + edge * 0.7 * fade, 0.0, 1.0);
    return vec4<f32>(rgb + holo * edge * 1.1, alpha);
}
"#;

/// Selected-word glow: restrained cyan energy edge with a slow pulse. This is
/// the third editor shader behavior (selection highlight) and it is transient
/// by nature — the glow rides the current selection.
const SELECTION_GLOW_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let t = input.time_seconds;
    let pulse = 0.5 + 0.5 * sin(t * 3.0);
    let holo = vec3<f32>(0.45, 0.85, 1.0);
    let base = input.base_color.rgb;
    // Energy rim: glyph-edge ink picks up the cyan glow.
    let edge = smoothstep(0.15, 0.6, input.edge_ink);
    let core = smoothstep(0.05, 0.5, input.coverage);
    let rgb = mix(base, holo, 0.22 + 0.22 * pulse) + holo * edge * (0.45 + 0.45 * pulse);
    let alpha = clamp(core + edge * 0.5, 0.0, 1.0);
    return vec4<f32>(rgb, alpha);
}
"#;

fn register_text_shader_package(wgpu_endpoint: SocketAddr) {
    let packages = [
        (
            "pulse-neon-text",
            PULSE_NEON_SOURCE,
            "code-editor-demo-register-neon-text-v1",
        ),
        (
            "text-sweep",
            TEXT_SWEEP_SOURCE,
            "code-editor-demo-register-sweep-v1",
        ),
        (
            "text-distort",
            TEXT_DISTORT_SOURCE,
            "code-editor-demo-register-distort-v1",
        ),
        (
            "rainbow-chroma",
            RAINBOW_SOURCE,
            "code-editor-demo-register-rainbow-v1",
        ),
        (
            "aurora-glow",
            AURORA_SOURCE,
            "code-editor-demo-register-aurora-v1",
        ),
        (
            "text-type-in",
            TEXT_TYPE_IN_SOURCE,
            "code-editor-demo-register-type-in-v1",
        ),
        (
            "text-delete-fragment",
            TEXT_DELETE_FRAGMENT_SOURCE,
            "code-editor-demo-register-delete-fragment-v1",
        ),
        (
            "selection-glow",
            SELECTION_GLOW_SOURCE,
            "code-editor-demo-register-selection-glow-v1",
        ),
    ];
    let mut last_error = String::new();
    for (package_id, source, idem_key) in packages {
        let package = neon_ui_schema::UiShaderPackage {
            package_id: package_id.into(),
            version: 1,
            source_digest: shader_digest(source.as_bytes()),
            source_bytes: source.as_bytes().to_vec(),
            entry_point: "text_material".into(),
            fallback: "standard_text".into(),
            parameters: Vec::new(),
        };
        let registration = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: RequestId(format!("code-editor-demo-register-{package_id}")),
            client: ClientIdentity {
                kind: ClientKind::Cli,
                instance_id: "nui-flow-code-editor-demo".into(),
                pid: std::process::id(),
                origin: "nui-flow-code-editor-demo".into(),
            },
            target: ServiceName("wgpu-runtime".into()),
            method: "wgpu.shader.register".into(),
            params: json!({ "package": package }),
            expected_revision: None,
            idempotency_key: Some(idem_key.into()),
        };
        let mut registered = false;
        for attempt in 1..=40 {
            match RpcClient::connect(wgpu_endpoint)
                .and_then(|mut client| client.call(&registration))
            {
                Ok(response) if response.status == RpcStatus::Accepted => {
                    eprintln!(
                        "{{\"probe\":\"code-editor-demo\",\"stage\":\"text_shader_registered\",\"package\":\"{package_id}\",\"attempt\":{attempt}}}"
                    );
                    registered = true;
                    break;
                }
                Ok(response) => {
                    last_error = format!("{response:?}");
                }
                Err(err) => {
                    last_error = format!("{err}");
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(125));
        }
        if !registered {
            eprintln!(
                "{{\"probe\":\"code-editor-demo\",\"stage\":\"text_shader_register_failed\",\"package\":\"{package_id}\",\"error\":\"{last_error}\"}}"
            );
            std::process::exit(1);
        }
    }
}
fn submit_code_editor_fragment(wgpu_endpoint: SocketAddr, fragment: UiFragment) {
    let submission = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId("code-editor-demo-submit".into()),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "nui-flow-code-editor-demo".into(),
            pid: std::process::id(),
            origin: "nui-flow-code-editor-demo".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: "wgpu.ui.submit_fragment".into(),
        params: json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment)
        }),
        expected_revision: None,
        idempotency_key: Some("code-editor-demo-submit-v1".into()),
    };
    let mut last_error = String::new();
    let mut accepted = false;
    for attempt in 1..=40 {
        match RpcClient::connect(wgpu_endpoint).and_then(|mut client| client.call(&submission)) {
            Ok(response) if response.status == RpcStatus::Accepted => {
                eprintln!(
                    "{{\"probe\":\"code-editor-demo\",\"stage\":\"fragment_submitted\",\"attempt\":{attempt}}}"
                );
                accepted = true;
                break;
            }
            Ok(response) => {
                last_error = format!("{response:?}");
            }
            Err(error) => last_error = error.to_string(),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if !accepted {
        eprintln!(
            "{{\"probe\":\"code-editor-demo\",\"stage\":\"submit_failed\",\"error\":{}}}",
            json!(last_error)
        );
    }
}

/// Parses the fixture and evaluates initial visibility so the editor node is
/// actually present in the first composition.
fn build_code_editor_fragment() -> Result<UiFragment, String> {
    let document = parse_nui_flow(FIXTURE).map_err(|error| format!("{error:?}"))?;
    let mut compile_document = document.clone();
    declare_preview_fallbacks(
        &mut compile_document.ir.root,
        &mut compile_document.ir.resources,
    );
    let revision = demo_program_revision(&document.ir.surface_id.0);
    let program = compile_nui_flow_program(&compile_document, revision.clone())
        .map_err(|error| format!("{error:?}"))?;
    let inputs = UiInputStore::activate(revision, document.input_schema.clone())
        .map_err(|error| format!("{error:?}"))?;
    let frame = evaluate_ui_program(
        &program,
        &inputs.snapshot(),
        UiCpuViewport {
            logical_bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 1024.0,
            },
            revision: Revision(1),
        },
        &UiLocalPresentationState::default(),
    );
    let visibility = frame
        .nodes
        .into_iter()
        .map(|node| (node.node_key, node.visible))
        .collect();
    let mut root = document.ir.root.clone();
    apply_evaluated_visibility(&mut root, &visibility);
    Ok(UiFragment {
        fragment_id: UiFragmentId("code-editor-demo".into()),
        revision: Revision(1),
        root,
        effects: lower_nui_flow_effects(&document),
    })
}

fn declare_preview_fallbacks(node: &mut UiNode, resources: &mut Vec<UiProgramResource>) {
    let kind = match node.kind {
        UiNodeKind::Image => Some(UiProgramResourceKind::Image),
        UiNodeKind::RenderSurface => Some(UiProgramResourceKind::RenderSurface),
        _ => None,
    };
    if let Some(kind) = kind
        && !resources
            .iter()
            .any(|resource| resource.key == node.node_id.0)
    {
        resources.push(UiProgramResource {
            key: node.node_id.0.clone(),
            kind,
            has_fallback: true,
            asset_ref: None,
        });
    }
    for child in &mut node.children {
        declare_preview_fallbacks(child, resources);
    }
}

fn apply_evaluated_visibility(node: &mut UiNode, visibility: &std::collections::BTreeMap<String, bool>) {
    node.visible &= visibility.get(&node.node_id.0).copied().unwrap_or(false);
    for child in &mut node.children {
        apply_evaluated_visibility(child, visibility);
    }
}

fn demo_program_revision(surface_id: &str) -> UiProgramRevision {
    UiProgramRevision {
        program_id: format!("{surface_id}.demo"),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: [
            UI_PROGRAM_CAPABILITY_NAME,
            UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
            UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
            UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
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

/// Accepts `ui.host.inbound` RPCs from the window runtime. `DocumentCommit`
/// events are logged as machine-readable evidence; other inbound kinds are
/// rejected without side effects.
fn serve_host_stub(endpoint: SocketAddr) -> Result<(), neon_ipc::TransportError> {
    let server = RpcServer::bind(endpoint)?;
    server.serve_until(|request| {
        let shutdown = request.method == "service.shutdown";
        let response = match request.method.as_str() {
            "service.health" => RpcResponse {
                request_id: request.request_id.clone(),
                status: RpcStatus::Accepted,
                revision: Some(Revision(1)),
                result: Some(json!({"state": "healthy"})),
                snapshot: None,
                error: None,
            },
            "ui.host.inbound" => match serde_json::from_value::<UiSemanticEvent>(
                request.params.clone(),
            ) {
                Ok(event) => {
                    let action = match &event.intent {
                        UiIntent::Invoke { action, .. } => action.as_str(),
                    };
                    let document = event
                        .text
                        .as_ref()
                        .map(|commit| commit.value.as_str())
                        .unwrap_or_default();
                    let preview = document
                        .chars()
                        .take(120)
                        .collect::<String>()
                        .replace('"', "'");
                    eprintln!(
                        "{{\"probe\":\"editor-commit\",\"event\":\"DocumentCommit\",\"action\":\"{action}\",\"fragment\":\"{}\",\"document_chars\":{},\"document_preview\":\"{preview}\"}}",
                        event.fragment.id.0,
                        document.chars().count(),
                    );
                    RpcResponse {
                        request_id: request.request_id.clone(),
                        status: RpcStatus::Accepted,
                        revision: Some(Revision(1)),
                        result: None,
                        snapshot: None,
                        error: None,
                    }
                }
                Err(_) => RpcResponse {
                    request_id: request.request_id.clone(),
                    status: RpcStatus::Rejected,
                    revision: Some(Revision(1)),
                    result: None,
                    snapshot: None,
                    error: Some(RpcError {
                        code: "invalid_request".into(),
                        message: "host inbound payload is invalid".into(),
                        current_revision: None,
                        object_id: None,
                    }),
                },
            },
            _ => RpcResponse {
                request_id: request.request_id.clone(),
                status: RpcStatus::Rejected,
                revision: Some(Revision(1)),
                result: None,
                snapshot: None,
                error: Some(RpcError {
                    code: "unsupported_method".into(),
                    message: "host stub supports only ui.host.inbound".into(),
                    current_revision: None,
                    object_id: None,
                }),
            },
        };
        (response, !shutdown)
    })
}
