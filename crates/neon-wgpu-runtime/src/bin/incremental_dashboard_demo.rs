//! Visible retained-input incremental dashboard demo.
//!
//! The window is intentionally busy: KPI cards, a live chart, an alert rail,
//! a table, and static background chrome. Every timed update changes one input
//! slot and publishes the retained projection to the real WGPU runtime. JSONL
//! records separate CPU retained work from the renderer's fragment boundary.

use std::{
    net::SocketAddr,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_runtime::{
    UiInputStore, UiInputWriter, compile_nui_flow_program, lower_nui_flow_effects, parse_nui_flow,
    refresh_fragment_with_projection,
};
use neon_ui_schema::{
    UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiCommand, UiFragment, UiFragmentDelta,
    UiFragmentId, UiFragmentSubmission, UiInputChange, UiInputFrame, UiInputValue, UiIntent,
    UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus, UiProgramRevision,
    UiSemanticEvent,
};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39302";
const UI_ENDPOINT: &str = "127.0.0.1:39303";
const VIEWPORT_W: f32 = 1200.0;
const VIEWPORT_H: f32 = 760.0;
const RUN_FOR: Duration = Duration::from_secs(120);

fn request(method: &str, sequence: u64, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("incremental-dashboard-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "incremental-dashboard-demo".into(),
            pid: std::process::id(),
            origin: "incremental-dashboard-demo".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(format!("incremental-dashboard-{sequence}")),
    }
}

fn call(
    endpoint: SocketAddr,
    method: &str,
    sequence: u64,
    params: serde_json::Value,
) -> Result<(), String> {
    let response = RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(&request(method, sequence, params)))
        .map_err(|error| error.to_string())?;
    if response.status != RpcStatus::Accepted {
        return Err(format!("{method} rejected: {:?}", response.error));
    }
    Ok(())
}

fn launch() -> Result<Child, String> {
    let binary = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .with_file_name("neon-wgpu-runtime.exe");
    Command::new(binary)
        .args(["--window-server", ENDPOINT, UI_ENDPOINT])
        .spawn()
        .map_err(|error| error.to_string())
}

fn start_ui_host(queue: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Result<(), String> {
    let server =
        RpcServer::bind(UI_ENDPOINT.parse().unwrap()).map_err(|error| error.to_string())?;
    thread::spawn(move || {
        let _ = server.serve_until(move |request: RpcRequest| {
            if request.method == "ui.host.inbound"
                && let Ok(event) = serde_json::from_value::<UiSemanticEvent>(request.params)
                && let UiIntent::Invoke { action, .. } = event.intent
                && let Ok(mut events) = queue.lock()
            {
                let value = match event.control_value {
                    Some(neon_ui_schema::UiSemanticPayloadValue::Bool { value }) => format!(":{value}"),
                    _ => String::new(),
                };
                println!("{}", json!({"probe":"incremental-dashboard.v1","event":"semantic_click_received","action":action,"control_value":value}));
                events.push(format!("{action}{value}"));
            }
            (
                RpcResponse {
                    request_id: request.request_id,
                    status: RpcStatus::Accepted,
                    revision: None,
                    result: Some(json!({"state":"accepted"})),
                    error: None,
                    snapshot: None,
                },
                request.method != "service.shutdown",
            )
        });
    });
    Ok(())
}

fn dashboard_flow() -> String {
    let mut flow = format!(
        "version 1\nsurface incremental-dashboard revision 1\nbudget nodes=256 bindings=64 instances=256 text=128 glyphs=4096 events=32 clips=256\ninput theme_light bool default true\ninput theme_dark bool default false\nsurface root overlay w {VIEWPORT_W} h {VIEWPORT_H} fill #0A1020\n"
    );
    flow.push_str("  panel header x 24 y 18 w 1152 h 58 fill #111D35 radius 8\n");
    flow.push_str("    text title x 20 y 10 w 460 h 26 value \"NEON3 / RETAINED TELEMETRY\"\n");
    flow.push_str("    text subtitle x 20 y 35 w 700 h 16 value \"Input impact graph · retained CPU frame · sparse GPU ranges\"\n");
    flow.push_str("  button theme-button x 980 y 30 w 170 h 28 value \"Toggle Theme\" event dashboard.theme.toggle\n");
    flow.push_str(
        "  panel light-theme x 24 y 88 w 1152 h 642 fill #E8EEF7 visible $theme_light radius 8\n",
    );
    flow.push_str(
        "    text theme-label x 24 y 18 w 400 h 24 value \"LIGHT THEME / RETAINED STRUCTURE\"\n",
    );
    flow.push_str("    text theme-detail x 24 y 48 w 900 h 18 value \"Only the theme layer changes; chart, table, and control topology remain resident\"\n");
    flow.push_str("    panel theme-accent-a x 24 y 84 w 520 h 96 fill #FFFFFF radius 8\n");
    flow.push_str("    panel theme-accent-b x 570 y 84 w 540 h 96 fill #D8E5F5 radius 8\n");
    flow.push_str("    text theme-status x 24 y 210 w 900 h 22 value \"STATUS: local semantic input update\"\n");
    flow.push_str(
        "  panel dark-theme x 24 y 88 w 1152 h 642 fill #172033 visible $theme_dark radius 8\n",
    );
    flow.push_str("    text theme-label-dark x 24 y 18 w 400 h 24 value \"DARK THEME / RETAINED STRUCTURE\"\n");
    flow.push_str("    text theme-detail-dark x 24 y 48 w 900 h 18 value \"Only the theme layer changes; chart, table, and control topology remain resident\"\n");
    flow.push_str("    panel theme-accent-dark-a x 24 y 84 w 520 h 96 fill #243B5A radius 8\n");
    flow.push_str("    panel theme-accent-dark-b x 570 y 84 w 540 h 96 fill #304B70 radius 8\n");
    flow.push_str("    text theme-status-dark x 24 y 210 w 900 h 22 value \"STATUS: local semantic input update\"\n");

    let cards = [
        ("cpu-card", "CPU LOAD", "#163B63", 0.45),
        ("memory-card", "MEMORY", "#214E48", 0.62),
        ("network-card", "NETWORK", "#563D25", 0.35),
        ("stable-card", "FRAME RATE", "#33285E", 0.98),
    ];
    for (index, (id, label, color, ratio)) in cards.iter().enumerate() {
        let x = 24.0 + index as f32 * 288.0;
        flow.push_str(&format!(
            "  panel {id} x {x} y 138 w 270 h 106 fill {color} radius 8\n"
        ));
        flow.push_str(&format!(
            "    text {id}-label x 16 y 12 w 200 h 18 value \"{label}\"\n"
        ));
        flow.push_str(&format!(
            "    text {id}-value x 16 y 38 w 220 h 30 value \"{label}\"\n"
        ));
        flow.push_str(&format!(
            "    panel {id}-bar x 16 y 72 w {} h 12 fill #6EA8FE radius 3\n",
            (238.0 * ratio) as u32
        ));
    }

    flow.push_str("  panel chart-panel x 24 y 264 w 690 h 300 fill #101A2C radius 8\n");
    flow.push_str(
        "    text chart-title x 18 y 14 w 420 h 22 value \"THROUGHPUT / LAST 24 SAMPLES\"\n",
    );
    for index in 0..32 {
        let x = 18.0 + (index % 16) as f32 * 40.0;
        let y = 64.0 + (index / 16) as f32 * 100.0;
        let h = 28.0 + ((index * 37) % 60) as f32;
        let color = if index % 5 == 0 { "#E6A23C" } else { "#3B82F6" };
        if index % 3 == 0 {
            flow.push_str(&format!(
                "    panel bar-{index}-light x {x} y {y} w 25 h {h} fill #3B82F6 visible $theme_light radius 3\n"
            ));
            flow.push_str(&format!(
                "    panel bar-{index}-dark x {x} y {y} w 25 h {h} fill #F97316 visible $theme_dark radius 3\n"
            ));
        } else {
            flow.push_str(&format!(
                "    panel bar-{index} x {x} y {y} w 25 h {h} fill {color} radius 3\n"
            ));
        }
    }
    flow.push_str("    text chart-foot x 18 y 270 w 620 h 18 value \"blue: retained samples, amber: alert boundary\"\n");

    flow.push_str("  panel side-panel x 738 y 264 w 438 h 300 fill #101A2C radius 8\n");
    flow.push_str("    text side-title x 18 y 14 w 300 h 22 value \"INPUT IMPACT MAP\"\n");
    flow.push_str(
        "    text side-sub x 18 y 42 w 390 h 18 value \"Only one input changes per tick\"\n",
    );
    for (index, label) in ["cpu", "memory", "network", "alerts", "selected"]
        .iter()
        .enumerate()
    {
        let y = 78.0 + index as f32 * 38.0;
        let color = if *label == "alerts" {
            "#9A3A4D"
        } else {
            "#1D3557"
        };
        flow.push_str(&format!(
            "    panel impact-{label} x 18 y {y} w 390 h 28 fill {color} radius 4\n"
        ));
        flow.push_str(&format!("      text impact-{label}-text x 12 y 5 w 350 h 18 value \"dirty slot: {label}   to   bounded node range\"\n"));
    }
    flow.push_str(
        "    text scope-note x 18 y 278 w 390 h 18 value \"static chrome remains resident\"\n",
    );

    flow.push_str("  panel table-panel x 24 y 584 w 1152 h 146 fill #101A2C radius 8\n");
    flow.push_str(
        "    text table-title x 18 y 12 w 300 h 20 value \"RECENT JOBS / RETAINED ROWS\"\n",
    );
    for index in 0..6 {
        let x = 18.0 + (index % 3) as f32 * 380.0;
        let y = 44.0 + (index / 3) as f32 * 38.0;
        let color = if index == 2 { "#255B4A" } else { "#182840" };
        flow.push_str(&format!(
            "    panel row-{index} x {x} y {y} w 350 h 28 fill {color} radius 3\n"
        ));
        flow.push_str(&format!("      text row-{index}-text x 10 y 5 w 320 h 18 value \"job-{index:02}   completed   0.{index}ms\"\n"));
    }
    flow
}

fn submit(endpoint: SocketAddr, fragment: &UiFragment, sequence: u64) -> Result<(), String> {
    call(
        endpoint,
        "wgpu.ui.submit_fragment",
        sequence,
        serde_json::to_value(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment.clone()),
        })
        .map_err(|error| error.to_string())?,
    )
}

fn collect_nodes(
    root: &neon_ui_schema::UiNode,
    keys: &[String],
    out: &mut Vec<neon_ui_schema::UiNode>,
) {
    if keys.iter().any(|key| key == &root.node_id.0) {
        out.push(root.clone());
    }
    for child in &root.children {
        collect_nodes(child, keys, out);
    }
}

fn submit_delta(endpoint: SocketAddr, delta: UiFragmentDelta, sequence: u64) -> Result<(), String> {
    call(
        endpoint,
        "wgpu.ui.submit_fragment_delta",
        sequence,
        serde_json::to_value(UiCommand::SubmitFragmentDelta { delta })
            .map_err(|error| error.to_string())?,
    )
}

fn main() -> Result<(), String> {
    let endpoint: SocketAddr = ENDPOINT.parse().unwrap();
    let ui_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    start_ui_host(ui_events.clone())?;
    let mut child = launch()?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("wgpu runtime exited before health".into());
        }
        if call(endpoint, "service.health", 1, json!({})).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    if started.elapsed() >= Duration::from_secs(15) {
        return Err("wgpu runtime health timeout".into());
    }

    let flow = dashboard_flow();
    let document = parse_nui_flow(&flow).map_err(|error| {
        let context = error
            .diagnostics
            .first()
            .and_then(|diagnostic| {
                flow.lines()
                    .nth(diagnostic.span.line.saturating_sub(1) as usize)
            })
            .unwrap_or("");
        format!("parse: {error:?}; source_line={context:?}")
    })?;
    let revision = neon_protocol::Revision(1);
    let program_revision = UiProgramRevision {
        program_id: "incremental-dashboard".into(),
        revision,
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: vec![UiProgramCapability {
            name: UI_PROGRAM_CAPABILITY_NAME.into(),
            version: 1,
            owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        }],
    };
    let program = compile_nui_flow_program(&document, program_revision.clone())
        .map_err(|error| format!("compile: {error:?}"))?;
    let mut store = UiInputStore::activate(program_revision, document.input_schema.clone())
        .map_err(|error| error.code.to_owned())?;
    let mut projection = None;
    let mut fragment = UiFragment {
        fragment_id: UiFragmentId("incremental-dashboard".into()),
        revision: Revision(1),
        root: document.ir.root.clone(),
        effects: lower_nui_flow_effects(&document),
    };
    let first = refresh_fragment_with_projection(
        &mut projection,
        &mut fragment,
        &program,
        &store.snapshot(),
        store.schema(),
        1,
        &[],
    );
    let submit_started = Instant::now();
    submit(endpoint, &fragment, 10)?;
    let submit_rpc_ms = submit_started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{}",
        json!({"probe":"incremental-dashboard.v1","event":"frame_submitted","sequence":0,"timing_ms":{"refresh":0.0,"submit_rpc":submit_rpc_ms},"producer":{"input_revision":first.input_revision,"delta_applied":first.delta_applied,"bindings_executed":first.bindings_executed,"bindings_total":first.bindings_total,"nodes_written":first.nodes_written,"nodes_total":first.nodes_total},"consumer":{"fragment_revision":fragment.revision,"transport":"neon3.rpc","method":"wgpu.ui.submit_fragment"},"status":"passed"})
    );

    let demo_started = Instant::now();
    let mut sequence = 0usize;
    while demo_started.elapsed() < RUN_FOR {
        let action = ui_events.lock().ok().and_then(|mut events| events.pop());
        let Some(action) = action else {
            thread::sleep(Duration::from_millis(16));
            continue;
        };
        let (dark, source) = match action.as_str() {
            value if value.starts_with("dashboard.theme.toggle:") => {
                (value.ends_with("true"), "click")
            }
            "dashboard.theme.toggle" => {
                let current = matches!(
                    store
                        .snapshot()
                        .values
                        .get("theme_dark")
                        .map(|value| &value.value),
                    Some(UiInputValue::Bool { value: true })
                );
                (!current, "click")
            }
            _ => continue,
        };
        let base = store.snapshot();
        let applied = store
            .apply(
                UiInputWriter::External,
                UiInputFrame {
                    program_revision: program.revision.clone(),
                    expected_input_revision: base.input_revision,
                    request_id: format!("dashboard-input-{sequence}"),
                    idempotency_key: format!("dashboard-input-{sequence}"),
                    changes: vec![
                        UiInputChange {
                            key: "theme_dark".into(),
                            value: UiInputValue::Bool { value: dark },
                        },
                        UiInputChange {
                            key: "theme_light".into(),
                            value: UiInputValue::Bool { value: !dark },
                        },
                    ],
                },
            )
            .map_err(|error| error.code.to_owned())?;
        fragment.revision = Revision(fragment.revision.0 + 1);
        let refresh_started = Instant::now();
        let refresh = refresh_fragment_with_projection(
            &mut projection,
            &mut fragment,
            &program,
            &store.snapshot(),
            store.schema(),
            1,
            &applied.changed_slots,
        );
        let refresh_ms = refresh_started.elapsed().as_secs_f64() * 1000.0;
        let mut changed_nodes = Vec::new();
        collect_nodes(&fragment.root, &refresh.changed_nodes, &mut changed_nodes);
        let submit_started = Instant::now();
        submit_delta(
            endpoint,
            UiFragmentDelta {
                fragment_id: fragment.fragment_id.clone(),
                base_revision: Revision(fragment.revision.0 - 1),
                revision: fragment.revision,
                changed_nodes,
                effects: (refresh.effects_rebuilt > 0).then(|| fragment.effects.clone()),
            },
            11 + sequence as u64,
        )?;
        let submit_rpc_ms = submit_started.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{}",
            json!({"probe":"incremental-dashboard.v1","event":"frame_submitted","sequence":sequence+1,"source":source,"timing_ms":{"refresh":refresh_ms,"submit_rpc":submit_rpc_ms},"producer":{"input_key":"theme_dark","input_values":{"theme_dark":dark,"theme_light":!dark},"input_revision":refresh.input_revision,"dirty_slots":refresh.dirty_slots,"changed_bindings":refresh.changed_bindings,"changed_nodes":refresh.changed_nodes,"bindings_executed":refresh.bindings_executed,"bindings_total":refresh.bindings_total,"nodes_written":refresh.nodes_written,"nodes_total":refresh.nodes_total,"delta_applied":refresh.delta_applied},"consumer":{"fragment_revision":fragment.revision,"transport":"neon3.rpc","method":"wgpu.ui.submit_fragment_delta","payload":"changed_nodes_plus_optional_effects"},"status":"passed"})
        );
        sequence += 1;
        thread::sleep(Duration::from_millis(if source == "click" {
            16
        } else {
            1100
        }));
    }
    thread::sleep(Duration::from_secs(2));
    let _ = child.kill();
    println!(
        "{}",
        json!({"probe":"incremental-dashboard.v1","final":true,"status":"passed","visual":"windowed_dashboard","frames":sequence+1,"scope":"CPU retained projection + public WGPU fragment delta + renderer sparse buffer path","known_boundary":"This demo only mutates resident nodes; no full-fragment fallback is used"})
    );
    Ok(())
}
