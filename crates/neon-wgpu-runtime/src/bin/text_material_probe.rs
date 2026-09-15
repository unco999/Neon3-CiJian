//! JSONL acceptance probe for the renderer-owned text material path.
//!
//! The probe launches the real windowed WGPU runtime, registers a fixed text
//! shader through neon3.rpc, submits Latin/CJK/one-shot text, captures the
//! final composition, and checks the pixels produced by the consumer.

use std::{
    fs::File,
    io::{self, BufReader},
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
    UiBounds, UiClipShape, UiCommand, UiEffect, UiFragment, UiFragmentId, UiFragmentSubmission,
    UiNode, UiNodeId, UiNodeKind, UiShaderPackage, UiStyle, UiTextMaterialRef,
};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39261";
const TIMEOUT: Duration = Duration::from_secs(15);
const PACKAGE_ID: &str = "text-material-probe";

const SHADER_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let core = smoothstep(0.05, 0.6, input.coverage);
    let halo = pow(input.edge_ink, 4.0);
    let bloom = pow(input.edge_ink, 1.5) * 0.45;
    let alpha = max(core, (halo + bloom) * 0.25);
    return vec4<f32>(0.12, 0.85, 1.0, alpha);
}
"#;

#[derive(Clone, Copy)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn emit(callback: &str, value: serde_json::Value) {
    println!("{}", json!({"callback": callback, "data": value}));
}

fn shader_digest(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn request(method: &str, sequence: u64, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("text-material-probe-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "text-material-probe".into(),
            pid: std::process::id(),
            origin: "text-material-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("text-material-probe-{sequence}")),
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

fn launch() -> io::Result<Child> {
    let binary = std::env::current_exe()?.with_file_name("neon-wgpu-runtime.exe");
    Command::new(binary)
        .args(["--window-server", ENDPOINT])
        .spawn()
}

fn label(id: &str, text: &str, y: f32) -> UiNode {
    UiNode {
        node_id: UiNodeId(id.into()),
        kind: UiNodeKind::Label,
        bounds: UiBounds {
            x: 20.0,
            y,
            width: 520.0,
            height: 28.0,
        },
        layout: None,
        visible: true,
        enabled: true,
        text_key: None,
        text: Some(neon_ui_schema::TextRef::Literal { value: text.into() }),
        image: None,
        surface: None,
        style: UiStyle::default(),
        enter_transition: None,
        world_depth: None,
        world_scale: None,
        clip_shape: UiClipShape::default(),
        children: Vec::new(),
    }
}

fn material(duration_ms: Option<u32>) -> UiTextMaterialRef {
    UiTextMaterialRef {
        package_id: PACKAGE_ID.into(),
        version: 1,
        fallback: "standard_text".into(),
        overflow: [12.0, 10.0, 12.0, 10.0],
        parameters: Default::default(),
        duration_ms,
    }
}

fn fragment() -> UiFragment {
    UiFragment {
        fragment_id: UiFragmentId("text-material-probe".into()),
        revision: Revision(1),
        root: UiNode {
            node_id: UiNodeId("root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 640.0,
                height: 180.0,
            },
            layout: None,
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle {
                background_color: [0.035, 0.055, 0.075, 1.0],
                ..UiStyle::default()
            },
            enter_transition: None,
            world_depth: None,
            world_scale: None,
            clip_shape: UiClipShape::default(),
            children: vec![
                label("latin", "NUI Flow code_editor", 20.0),
                label("cjk", "组件演示", 65.0),
                label("oneshot", "ONE_SHOT", 110.0),
            ],
        },
        effects: vec![
            UiEffect::TextMaterial {
                node_id: UiNodeId("latin".into()),
                material: material(None),
            },
            UiEffect::TextMaterial {
                node_id: UiNodeId("cjk".into()),
                material: material(None),
            },
            UiEffect::TextMaterial {
                node_id: UiNodeId("oneshot".into()),
                material: material(Some(250)),
            },
        ],
    }
}

fn register_shader(endpoint: SocketAddr) -> Result<serde_json::Value, String> {
    let bytes = SHADER_SOURCE.as_bytes();
    let package = UiShaderPackage {
        package_id: PACKAGE_ID.into(),
        version: 1,
        source_digest: shader_digest(bytes),
        source_bytes: bytes.to_vec(),
        entry_point: "text_material".into(),
        fallback: "standard_text".into(),
        parameters: Vec::new(),
    };
    call(
        endpoint,
        "wgpu.shader.register",
        2,
        json!({"package": package}),
    )
}

fn submit(endpoint: SocketAddr, fragment: UiFragment) -> Result<serde_json::Value, String> {
    let submission = UiFragmentSubmission::new(fragment);
    submission
        .validate()
        .map_err(|error| format!("invalid fragment submission: {error:?}"))?;
    let command = UiCommand::SubmitFragment { submission };
    let encoded = serde_json::to_value(&command).map_err(|error| error.to_string())?;
    serde_json::from_value::<UiCommand>(encoded.clone())
        .map_err(|error| format!("UI command roundtrip failed: {error}"))?;
    call(endpoint, "wgpu.ui.submit_fragment", 3, encoded)
}

fn count_cyan(path: &str, region: Region) -> io::Result<u64> {
    let decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    let mut reader = decoder.read_info().map_err(io::Error::other)?;
    let mut bytes = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut bytes).map_err(io::Error::other)?;
    let pixels = &bytes[..info.buffer_size()];
    let width = info.width;
    let height = info.height;
    let x_end = region.x.saturating_add(region.width).min(width);
    let y_end = region.y.saturating_add(region.height).min(height);
    let mut count = 0;
    for y in region.y.min(height)..y_end {
        for x in region.x.min(width)..x_end {
            let offset = ((y * width + x) * 4) as usize;
            let [r, g, b, a] = pixels[offset..offset + 4]
                .try_into()
                .expect("RGBA8 capture pixel");
            if a > 16 && g > 100 && b > 120 && r.saturating_add(30) < g {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn capture(endpoint: SocketAddr, sequence: u64, path: &str) -> Result<serde_json::Value, String> {
    call(
        endpoint,
        "wgpu.render.target.capture",
        sequence,
        json!({"target":"ui.color.v1", "path":path, "redraw":true}),
    )
}

fn stop_service(endpoint: SocketAddr, service: &mut Child) {
    let _ = call(endpoint, "service.shutdown", 20, json!({}));
    let deadline = Instant::now() + Duration::from_secs(2);
    while service.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    if service.try_wait().ok().flatten().is_none() {
        let _ = service.kill();
        let _ = service.wait();
    }
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
                    return Err(io::Error::other(format!("service exited: {error}")));
                }
            }
            Err(error) => {
                stop_service(endpoint, &mut service);
                return Err(io::Error::other(format!("service health timeout: {error}")));
            }
        }
    };
    emit(
        "text_material.health",
        json!({"endpoint":ENDPOINT,"service":health,"result":"passed"}),
    );

    let registration = register_shader(endpoint).map_err(io::Error::other)?;
    let shader_state =
        call(endpoint, "wgpu.shader.state", 21, json!({})).map_err(io::Error::other)?;
    emit(
        "text_material.producer",
        json!({
            "request_id":"text-material-probe-2",
            "sequence":2,
            "package":PACKAGE_ID,
            "entry_point":"text_material",
            "atlas_sampling":"glyph-local-safe-coverage",
            "registration":registration,
            "shader_state":shader_state,
        }),
    );

    let submitted = submit(endpoint, fragment()).map_err(io::Error::other)?;
    emit(
        "text_material.submit",
        json!({"request_id":"text-material-probe-3","sequence":3,"latin":"NUI Flow code_editor","cjk":"组件演示","one_shot_duration_ms":250,"overflow":[12.0,10.0,12.0,10.0],"submission":submitted}),
    );
    thread::sleep(Duration::from_millis(400));

    let before_path = std::env::temp_dir().join("neon3-text-material-before.png");
    let before = capture(endpoint, 4, &before_path.to_string_lossy()).map_err(io::Error::other)?;
    let before_artifact = before
        .get("artifact_path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| io::Error::other("capture did not return artifact_path"))?;
    let latin_pixels = count_cyan(
        before_artifact,
        Region {
            x: 0,
            y: 5,
            width: 360,
            height: 55,
        },
    )?;
    let cjk_pixels = count_cyan(
        before_artifact,
        Region {
            x: 0,
            y: 50,
            width: 220,
            height: 55,
        },
    )?;
    let oneshot_before_pixels = count_cyan(
        before_artifact,
        Region {
            x: 0,
            y: 95,
            width: 260,
            height: 55,
        },
    )?;
    emit(
        "text_material.consumer",
        json!({
            "phase":"active",
            "frame_sequence":before.get("frame_sequence"),
            "composition_revision":before.get("composition_revision"),
            "artifact_path":before_artifact,
            "latin_cyan_pixels":latin_pixels,
            "cjk_cyan_pixels":cjk_pixels,
            "oneshot_cyan_pixels":oneshot_before_pixels,
        }),
    );

    thread::sleep(Duration::from_millis(600));
    let after_path = std::env::temp_dir().join("neon3-text-material-after.png");
    let after = capture(endpoint, 5, &after_path.to_string_lossy()).map_err(io::Error::other)?;
    let after_artifact = after
        .get("artifact_path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| io::Error::other("second capture did not return artifact_path"))?;
    let oneshot_after_pixels = count_cyan(
        after_artifact,
        Region {
            x: 0,
            y: 95,
            width: 260,
            height: 55,
        },
    )?;
    let diagnostics =
        call(endpoint, "wgpu.render.diagnostics", 6, json!({})).map_err(io::Error::other)?;
    let same_or_new_frame = after
        .get("frame_sequence")
        .and_then(serde_json::Value::as_u64)
        .zip(
            before
                .get("frame_sequence")
                .and_then(serde_json::Value::as_u64),
        )
        .is_some_and(|(after_frame, before_frame)| after_frame > before_frame);
    let passed = latin_pixels > 20
        && cjk_pixels > 20
        && oneshot_before_pixels > 20
        && oneshot_after_pixels < oneshot_before_pixels / 4
        && same_or_new_frame
        && diagnostics
            .get("fragment_count")
            .and_then(serde_json::Value::as_u64)
            == Some(1);
    emit(
        "text_material.result",
        json!({
            "result":if passed {"passed"} else {"failed"},
            "producer":{"latin":"NUI Flow code_editor","cjk":"组件演示","duration_ms":250},
            "consumer":{"active_frame":before.get("frame_sequence"),"after_frame":after.get("frame_sequence"),"latin_cyan_pixels":latin_pixels,"cjk_cyan_pixels":cjk_pixels,"oneshot_before_cyan_pixels":oneshot_before_pixels,"oneshot_after_cyan_pixels":oneshot_after_pixels,"diagnostics":diagnostics},
            "failure_class":if latin_pixels <= 20 {"latin_missing_or_sampling_mismatch"} else if oneshot_after_pixels >= oneshot_before_pixels / 4 {"one_shot_stale"} else {"none"},
        }),
    );
    stop_service(endpoint, &mut service);
    if passed {
        Ok(())
    } else {
        Err(io::Error::other("text material render assertion failed"))
    }
}
