//! Local Neon3 session supervisor. It owns child-process lifecycle only; it
//! does not own a window, GPU object, domain state, or UI declaration.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::fs::{self, OpenOptions};
use std::io::BufReader;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use neon_ipc::RpcClient;
use neon_protocol::{
    AssetRef, ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest,
    RpcStatus, ServiceName,
};
use neon_ui_runtime::{lower_nui_flow_effects, parse_nui_flow};
use neon_ui_schema::{
    UiCommand, UiControlPresentation, UiDropPlacement, UiEffect, UiFragment, UiFragmentId,
    UiFragmentRevision, UiFragmentSubmission, UiInputValue, UiIntent, UiProgramSemanticEvent,
    UiProgramSemanticEventKind, UiSemanticEvent, UiSemanticEventType,
    UiSemanticInteractionMetadata, UiSemanticPayloadValue,
};
use serde_json::json;

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const HELP: &str = "neon-dev case <kanban-reparent|asset-review|component-gallery|data-grid|scroll-view|virtual-list> [--show-logs]\nneon-dev status [manifest-path]\nneon-dev scenario <drag-card02-before|component-gallery-interactions|component-gallery-window-input>\nneon-dev capture-window <wgpu-loopback-endpoint> [path]\nneon-dev debug-interaction <wgpu-loopback-endpoint> <interaction-id>\nneon-dev inspect-window <wgpu-loopback-endpoint>\nneon-dev probe-window <wgpu-loopback-endpoint> <x> <y>";
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("neon-dev: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let [command] = args.as_slice()
        && command == "status"
    {
        let path = workspace_root()?.join("target/neon-dev/latest.json");
        return print_status(&path);
    }
    if let [command, path] = args.as_slice()
        && command == "status"
    {
        return print_status(Path::new(path));
    }
    if matches!(args.as_slice(), [command, scenario] if command == "scenario" && scenario == "drag-card02-before")
    {
        return run_drag_card02_before_scenario();
    }
    if matches!(args.as_slice(), [command, scenario] if command == "scenario" && scenario == "component-gallery-interactions")
    {
        return run_component_gallery_scenario();
    }
    if matches!(args.as_slice(), [command, scenario] if command == "scenario" && scenario == "component-gallery-window-input")
    {
        return run_component_gallery_window_input_scenario();
    }
    if let [command, endpoint] = args.as_slice()
        && command == "capture-window"
    {
        return capture_window(endpoint, None);
    }
    if let [command, endpoint, path] = args.as_slice()
        && command == "capture-window"
    {
        return capture_window(endpoint, Some(path));
    }
    if let [command, endpoint] = args.as_slice()
        && command == "inspect-window"
    {
        return inspect_window_input(endpoint);
    }
    if let [command, endpoint, x, y] = args.as_slice()
        && command == "probe-window"
    {
        return probe_window_input(endpoint, x, y);
    }
    if let [command, endpoint, interaction_id] = args.as_slice()
        && command == "debug-interaction"
    {
        return debug_interaction(endpoint, interaction_id);
    }
    let case = match args.as_slice() {
        [command, case]
            if command == "case"
                && matches!(
                    case.as_str(),
                    "kanban-reparent"
                        | "asset-review"
                        | "component-gallery"
                        | "data-grid"
                        | "scroll-view"
                        | "virtual-list"
                ) =>
        {
            case
        }
        [command, case, flag]
            if command == "case"
                && flag == "--show-logs"
                && matches!(
                    case.as_str(),
                    "kanban-reparent"
                        | "asset-review"
                        | "component-gallery"
                        | "data-grid"
                        | "scroll-view"
                        | "virtual-list"
                ) =>
        {
            case
        }
        [flag] if matches!(flag.as_str(), "--help" | "-h" | "help") => {
            println!("{HELP}");
            return Ok(());
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("usage:\n{HELP}"),
            ));
        }
    };
    let show_logs = args.iter().any(|argument| argument == "--show-logs");
    run_case(case, show_logs)
}

fn run_case(case: &str, show_logs: bool) -> io::Result<()> {
    let workspace = workspace_root()?;
    let wgpu_endpoint = reserve_loopback_endpoint()?;
    let ui_endpoint = reserve_loopback_endpoint()?;
    let host_endpoint = reserve_loopback_endpoint()?;
    let job = ProcessJob::new()?;
    let mut children = ChildSession::default();
    let mut manifest =
        LiveSessionManifest::start(&workspace, case, wgpu_endpoint, ui_endpoint, host_endpoint)?;

    let result = run_case_session(
        &workspace,
        case,
        show_logs,
        wgpu_endpoint,
        ui_endpoint,
        host_endpoint,
        &job,
        &mut children,
        &mut manifest,
    );
    match result {
        Ok(()) => {
            children.stop_all();
            manifest.stop()
        }
        Err(error) => {
            children.stop_all();
            let _ = manifest.fail(&error);
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_case_session(
    workspace: &Path,
    case: &str,
    show_logs: bool,
    wgpu_endpoint: SocketAddr,
    ui_endpoint: SocketAddr,
    host_endpoint: SocketAddr,
    job: &ProcessJob,
    children: &mut ChildSession,
    manifest: &mut LiveSessionManifest,
) -> io::Result<()> {
    let projectd_endpoint = if case == "component-gallery" {
        Some(reserve_loopback_endpoint()?)
    } else {
        None
    };
    if let Some(endpoint) = projectd_endpoint {
        let projectd = spawn_service(
            executable(workspace, "neon-projectd"),
            &["--server", &endpoint.to_string()],
            show_logs,
        )?;
        job.assign(&projectd)?;
        children.push(projectd);
        wait_for_endpoint(endpoint)?;
    }
    let wgpu_endpoint_text = wgpu_endpoint.to_string();
    let ui_endpoint_text = ui_endpoint.to_string();
    let projectd_endpoint_text = projectd_endpoint.map(|endpoint| endpoint.to_string());
    let gallery_image_json = projectd_endpoint
        .map(component_gallery_image_asset)
        .transpose()?
        .map(|asset| serde_json::to_string(&asset).expect("AssetRef serializes"));
    let mut wgpu_args = vec![
        "--window-server",
        wgpu_endpoint_text.as_str(),
        ui_endpoint_text.as_str(),
    ];
    if let Some(endpoint) = projectd_endpoint_text.as_deref() {
        wgpu_args.push(endpoint);
    }
    let wgpu = spawn_service(
        executable(workspace, "neon-wgpu-runtime"),
        &wgpu_args,
        show_logs,
    )?;
    job.assign(&wgpu)?;
    let wgpu_pid = wgpu.id();
    children.push(wgpu);
    manifest.spawned(ManifestService::Wgpu, wgpu_pid)?;
    wait_for_endpoint(wgpu_endpoint)?;
    wait_for_window_gpu(wgpu_endpoint)?;
    manifest.ready(
        ManifestService::Wgpu,
        query_process_epoch(wgpu_endpoint, "wgpu-runtime"),
    )?;

    let host_endpoint_text = host_endpoint.to_string();
    let domain_args = if case == "component-gallery" {
        vec![
            host_endpoint_text.as_str(),
            "--component-gallery",
            "--gallery-image",
            gallery_image_json
                .as_deref()
                .expect("component gallery has projectd asset"),
        ]
    } else {
        vec![host_endpoint_text.as_str()]
    };
    let host = spawn_service(
        executable(workspace, "demo_domain_controller"),
        &domain_args,
        show_logs,
    )?;
    job.assign(&host)?;
    let host_pid = host.id();
    children.push(host);
    manifest.spawned(ManifestService::Host, host_pid)?;
    wait_for_endpoint(host_endpoint)?;
    manifest.ready(
        ManifestService::Host,
        query_process_epoch(host_endpoint, "ui-host"),
    )?;

    let ui = {
        let ui_endpoint_text = ui_endpoint.to_string();
        let wgpu_endpoint_text = wgpu_endpoint.to_string();
        let host_endpoint_text = host_endpoint.to_string();
        spawn_service(
            executable(workspace, "neon-ui-runtime"),
            &[
                "--forward-server",
                &ui_endpoint_text,
                &wgpu_endpoint_text,
                &host_endpoint_text,
            ],
            show_logs,
        )?
    };
    job.assign(&ui)?;
    let ui_pid = ui.id();
    children.push(ui);
    manifest.spawned(ManifestService::Ui, ui_pid)?;
    wait_for_endpoint(ui_endpoint)?;
    manifest.ready(
        ManifestService::Ui,
        query_process_epoch(ui_endpoint, "ui-runtime"),
    )?;

    let mut submitter = Command::new(executable(workspace, "nui_flow_demo"));
    submitter
        .args([case, &ui_endpoint.to_string()])
        .current_dir(workspace);
    if let Some(asset) = gallery_image_json.as_deref() {
        submitter.arg(asset);
    }
    hide_console_window(&mut submitter);
    let demo = submitter.spawn()?;
    job.assign(&demo)?;
    let status = demo.wait_with_output()?;
    if !status.status.success() {
        return Err(io::Error::other(format!(
            "case submitter exited with {}",
            status.status
        )));
    }

    manifest.record_window_viewport(query_window_viewport(wgpu_endpoint)?)?;
    manifest.session_ready()?;
    println!("{}", manifest.value());
    wait_for_session_end(children);
    Ok(())
}

fn run_component_gallery_scenario() -> io::Result<()> {
    let result = run_component_gallery_scenario_inner();
    match result {
        Ok(value) => {
            println!("{}", value);
            Ok(())
        }
        Err(error) => {
            println!(
                "{}",
                json!({"scenario": "component-gallery-interactions", "status": "failed", "steps": [], "error": {"code": "scenario_failed", "message": error.to_string()}})
            );
            Err(error)
        }
    }
}

fn run_component_gallery_window_input_scenario() -> io::Result<()> {
    let result = run_component_gallery_window_input_scenario_inner();
    match result {
        Ok(value) => {
            println!("{value}");
            Ok(())
        }
        Err(error) => {
            println!(
                "{}",
                json!({"scenario": "component-gallery-window-input", "status": "failed", "steps": [], "error": {"code": "scenario_failed", "message": error.to_string()}})
            );
            Err(error)
        }
    }
}

fn run_component_gallery_window_input_scenario_inner() -> io::Result<serde_json::Value> {
    const SCENARIO: &str = "component-gallery-window-input";
    const FRAGMENT_ID: &str = "nui-flow-case-component-gallery";
    let workspace = workspace_root()?;
    let wgpu_endpoint = reserve_loopback_endpoint()?;
    let ui_endpoint = reserve_loopback_endpoint()?;
    let domain_endpoint = reserve_loopback_endpoint()?;
    let projectd_endpoint = reserve_loopback_endpoint()?;
    let job = ProcessJob::new()?;
    let mut children = ChildSession::default();

    let projectd = spawn_service(
        executable(&workspace, "neon-projectd"),
        &["--server", &projectd_endpoint.to_string()],
        false,
    )?;
    job.assign(&projectd)?;
    children.push(projectd);
    wait_for_endpoint(projectd_endpoint)?;
    let gallery_image = component_gallery_image_asset(projectd_endpoint)?;
    let gallery_image_json = serde_json::to_string(&gallery_image).expect("AssetRef serializes");
    let wgpu_endpoint_text = wgpu_endpoint.to_string();
    let ui_endpoint_text = ui_endpoint.to_string();
    let projectd_endpoint_text = projectd_endpoint.to_string();
    let wgpu = spawn_service(
        executable(&workspace, "neon-wgpu-runtime"),
        &[
            "--window-server",
            &wgpu_endpoint_text,
            &ui_endpoint_text,
            &projectd_endpoint_text,
        ],
        false,
    )?;
    job.assign(&wgpu)?;
    children.push(wgpu);
    wait_for_endpoint(wgpu_endpoint)?;
    wait_for_window_gpu(wgpu_endpoint)?;
    let domain = spawn_service(
        executable(&workspace, "component_gallery_domain_controller"),
        &[&domain_endpoint.to_string(), &gallery_image_json],
        false,
    )?;
    job.assign(&domain)?;
    children.push(domain);
    wait_for_endpoint(domain_endpoint)?;
    let ui = spawn_service(
        executable(&workspace, "neon-ui-runtime"),
        &[
            "--forward-server",
            &ui_endpoint.to_string(),
            &wgpu_endpoint.to_string(),
            &domain_endpoint.to_string(),
        ],
        false,
    )?;
    job.assign(&ui)?;
    children.push(ui);
    wait_for_endpoint(ui_endpoint)?;

    let gallery_image = component_gallery_image_asset(projectd_endpoint)?;
    let (document, program) =
        neon_ui_runtime::demo_domain::component_gallery_program(gallery_image)
            .map_err(io::Error::other)?;
    let effects = lower_nui_flow_effects(&document);
    let mut fragment = UiFragment {
        fragment_id: UiFragmentId(FRAGMENT_ID.into()),
        revision: Revision(1),
        root: document.ir.root,
        effects,
    };
    let initial_domain =
        neon_ui_runtime::demo_domain::DemoInputDomain::new(program, document.input_schema)
            .map_err(|error| io::Error::other(error.message))?;
    neon_ui_runtime::demo_domain::apply_visible_status_to_fragment(
        &mut fragment,
        &initial_domain.snapshot(),
    );
    assert_accepted(
        call(
            ui_endpoint,
            &rpc_request(
                "window-input-submit",
                "ui-runtime",
                "ui.fragment.submit",
                json!(UiCommand::SubmitFragment {
                    submission: UiFragmentSubmission::new(fragment)
                }),
                None,
                Some("window-input-submit"),
            ),
        )?,
        "window Gallery fragment submission",
    )?;

    let mut steps = Vec::new();
    let action_before = window_graph_revision(wgpu_endpoint)?;
    let action = debug_window_target_command(
        wgpu_endpoint,
        "window-input-action-button",
        "debug.window.input.activate_target",
        &format!("{FRAGMENT_ID}/action-button"),
    )?;
    let action_delivery = wait_for_pointer_delivery(wgpu_endpoint, Duration::from_secs(5))?;
    if action_delivery
        .get("state")
        .and_then(serde_json::Value::as_str)
        != Some("accepted")
    {
        return Err(io::Error::other(format!(
            "gallery action button delivery was not accepted: {action_delivery}"
        )));
    }
    let _action_after =
        wait_for_graph_revision_after(wgpu_endpoint, action_before, Duration::from_secs(5))?;
    let action_snapshot = call(
        wgpu_endpoint,
        &rpc_request(
            "window-input-action-button-snapshot",
            "wgpu-runtime",
            "wgpu.ui.fragment.snapshot",
            json!({"fragment_id": FRAGMENT_ID}),
            None,
            None,
        ),
    )?;
    assert_accepted(
        action_snapshot.clone(),
        "accepted gallery action button update",
    )?;
    let action_fragment: UiFragment = serde_json::from_value(
        action_snapshot
            .result
            .ok_or_else(|| io::Error::other("action button snapshot omitted result"))?["fragment"]
            .clone(),
    )
    .map_err(io::Error::other)?;
    let action_toggled = action_fragment.effects.iter().any(|effect| {
        matches!(
            effect,
            UiEffect::ControlPresentation {
                node_id,
                state: UiControlPresentation::Toggle { selected: false }
            } if node_id.0 == "feature-toggle"
        )
    });
    if !action_toggled {
        return Err(io::Error::other(format!(
            "action button did not publish the feature toggle presentation"
        )));
    }
    steps.push(json!({"step": "action_button_toggles_feature", "gesture": action, "delivery": action_delivery, "feature_enabled": false}));
    let drag_before = window_graph_revision(wgpu_endpoint)?;
    let drag = debug_window_drag_gesture(
        wgpu_endpoint,
        "window-input-compass-equipment",
        "backpack-compass",
        "equipment-zone",
    )?;
    let drag_delivery = wait_for_pointer_delivery(wgpu_endpoint, Duration::from_secs(5))?;
    if drag_delivery
        .get("state")
        .and_then(serde_json::Value::as_str)
        != Some("accepted")
    {
        return Err(io::Error::other(format!(
            "declared compass drag delivery was not accepted: {drag_delivery}"
        )));
    }
    let drag_after =
        wait_for_graph_revision_after(wgpu_endpoint, drag_before, Duration::from_secs(5))?;
    let drag_snapshot = call(
        wgpu_endpoint,
        &rpc_request(
            "window-input-compass-equipment-snapshot",
            "wgpu-runtime",
            "wgpu.ui.fragment.snapshot",
            json!({"fragment_id": FRAGMENT_ID}),
            None,
            None,
        ),
    )?;
    assert_accepted(
        drag_snapshot.clone(),
        "accepted backpack presentation update",
    )?;
    let drag_fragment: UiFragment = serde_json::from_value(drag_snapshot.result.ok_or_else(|| io::Error::other("drag presentation snapshot omitted result"))?["fragment"].clone()).map_err(io::Error::other)?;
    if drag_fragment.revision != Revision(action_fragment.revision.0 + 1)
        || find_node(&drag_fragment.root, "backpack-compass").is_some()
    {
        return Err(io::Error::other(
            "declared compass drop did not publish the accepted presentation update",
        ));
    }
    let registry = call(
        wgpu_endpoint,
        &rpc_request(
            "window-input-drag-registry",
            "wgpu-runtime",
            "wgpu.render.diagnostics",
            json!({}),
            None,
            None,
        ),
    )?;
    assert_accepted(registry.clone(), "post-drag WGPU registry diagnostics")?;
    if registry
        .result
        .as_ref()
        .and_then(|value| value.get("fragment_count"))
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(io::Error::other(format!(
            "drag presentation replacement left more than one WGPU fragment: {registry:?}"
        )));
    }
    steps.push(json!({"step": "compass_to_equipment_drag", "gesture": drag, "delivery": drag_delivery, "accepted_presentation": true, "fragment_registry": {"count": 1, "fragment_id": drag_fragment.fragment_id, "fragment_revision": drag_fragment.revision}, "composition_revision": {"before": drag_before, "after": drag_after}}));

    let grid_path = format!("{FRAGMENT_ID}/asset-grid");
    let tail_control_path =
        format!("{FRAGMENT_ID}/asset-grid/data-grid-row-virtual-row-9999/cell-owner");

    for (id, node_key, input_key, target_fraction, expected) in [
        (
            "slider",
            "exposure-slider",
            "slider_value",
            0.8,
            ExpectedNumeric::F32(0.8),
        ),
        (
            "drag-value",
            "count-drag",
            "drag_value",
            0.75,
            ExpectedNumeric::I32(18),
        ),
        (
            "scrollbar",
            "gallery-scroll",
            "scroll_position",
            0.65,
            ExpectedNumeric::F32(0.65),
        ),
    ] {
        let node_path = format!("{FRAGMENT_ID}/{node_key}");
        let before = window_graph_revision(wgpu_endpoint)?;
        let gesture = debug_window_value_gesture(
            wgpu_endpoint,
            &format!("window-input-{id}-gesture"),
            &node_path,
            target_fraction,
        )?;
        let delivery = wait_for_pointer_delivery(wgpu_endpoint, Duration::from_secs(5))?;
        if delivery.get("state").and_then(serde_json::Value::as_str) != Some("accepted") {
            return Err(io::Error::other(format!(
                "{id} gesture delivery was not accepted: {delivery}"
            )));
        }
        let after = wait_for_graph_revision_after(wgpu_endpoint, before, Duration::from_secs(5))?;
        let trace = wait_for_accepted_delivery_trace(
            wgpu_endpoint,
            activation_interaction_id(&gesture)?,
            Duration::from_secs(5),
        )?;
        let committed = assert_numeric_commit(
            ui_endpoint,
            wgpu_endpoint,
            FRAGMENT_ID,
            node_key,
            input_key,
            expected,
        )?;
        if id == "slider" {
            let capture_path = workspace.join("target/neon-dev").join(format!(
                "neon3-component-gallery-immediate-grid-{}-{}.png",
                std::process::id(),
                TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let capture = capture_final_target(wgpu_endpoint, &capture_path)?;
            if capture
                .get("composition_revision")
                .and_then(serde_json::Value::as_u64)
                != Some(after)
            {
                return Err(io::Error::other(format!(
                    "immediate final target did not contain scalar composition {after}: {capture}"
                )));
            }
            let nonblank_pixels = assert_nonblank_data_grid_region(&capture_path)?;
            steps.push(json!({
                "step": "immediate_scalar_data_grid_render",
                "capture": capture,
                "nonblank_data_grid_header_pixels": nonblank_pixels,
                "scroll": "not_performed",
            }));
        }
        steps.push(json!({
            "step": format!("{id}_drag_commit"),
            "gesture": gesture,
            "delivery": delivery,
            "trace": trace,
            "scalar_snapshot": committed.0,
            "rendered_presentation": committed.1,
            "composition_revision": {"before": before, "after": after},
        }));
    }

    let grid_before = window_graph_revision(wgpu_endpoint)?;
    let grid_scroll = debug_window_target_command(
        wgpu_endpoint,
        "window-input-grid-scroll",
        "debug.window.input.scroll_to_max",
        &grid_path,
    )?;
    assert_max_scroll(&grid_scroll, "asset grid")?;
    if grid_scroll
        .get("scheduled_window_requests")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(io::Error::other(format!(
            "asset grid maximum scroll did not schedule exactly one window request: {grid_scroll}"
        )));
    }
    let final_window =
        wait_for_final_grid_window(wgpu_endpoint, FRAGMENT_ID, Duration::from_secs(5))?;
    let grid_after =
        wait_for_graph_revision_after(wgpu_endpoint, grid_before, Duration::from_secs(5))?;
    if grid_after <= grid_before {
        return Err(io::Error::other(format!(
            "grid window publication did not advance composition ({grid_before} -> {grid_after})"
        )));
    }
    let tail_before = grid_after;
    let tail_activation = debug_window_target_command(
        wgpu_endpoint,
        "window-input-tail-control",
        "debug.window.input.activate_target",
        &tail_control_path,
    )?;
    let tail_delivery = wait_for_pointer_delivery(wgpu_endpoint, Duration::from_secs(5))?;
    let tail_after =
        wait_for_graph_revision_after(wgpu_endpoint, tail_before, Duration::from_secs(5))?;
    let tail_trace = wait_for_accepted_delivery_trace(
        wgpu_endpoint,
        activation_interaction_id(&tail_activation)?,
        Duration::from_secs(5),
    )?;
    steps.push(json!({"step": "grid_scroll_tail_window", "scroll": grid_scroll, "final_window": final_window, "tail_control": {"semantic_node_path": tail_control_path, "activation": tail_activation, "delivery": tail_delivery, "trace": tail_trace}, "composition_revision": {"before": grid_before, "after_window": grid_after, "after_tail_activation": tail_after}}));
    Ok(
        json!({"scenario": SCENARIO, "status": "passed", "acceptance_level": "wgpu-rendered", "steps": steps}),
    )
}

fn capture_final_target(endpoint: SocketAddr, path: &Path) -> io::Result<serde_json::Value> {
    let response = call(
        endpoint,
        &rpc_request(
            "component-gallery-immediate-grid-capture",
            "wgpu-runtime",
            "wgpu.render.target.capture",
            json!({"target": "ui.color.v1", "path": path.to_string_lossy(), "redraw": false}),
            None,
            None,
        ),
    )?;
    assert_accepted(response.clone(), "immediate final-target capture")?;
    response
        .result
        .ok_or_else(|| io::Error::other("immediate final-target capture omitted result"))
}

fn assert_nonblank_data_grid_region(path: &Path) -> io::Result<usize> {
    let file = std::fs::File::open(path)?;
    let decoder = png::Decoder::new(BufReader::new(file));
    let mut reader = decoder.read_info().map_err(io::Error::other)?;
    let mut pixels = vec![
        0;
        reader.output_buffer_size().ok_or_else(|| io::Error::other(
            "PNG output buffer size overflowed"
        ))?
    ];
    let info = reader.next_frame(&mut pixels).map_err(io::Error::other)?;
    if info.color_type != png::ColorType::Rgba {
        return Err(io::Error::other(format!(
            "final target capture must be RGBA, got {:?}",
            info.color_type
        )));
    }
    let width = info.width as usize;
    let height = info.height as usize;
    // At the gallery's minimum viewport the center pane occupies x=384..984
    // and the DataGrid header occupies y=14..38. Percentages also work at
    // non-unit physical scale factors.
    let x_start = width * 24 / 100;
    let x_end = width * 58 / 100;
    let y_start = height * 2 / 100;
    let y_end = height * 4 / 100;
    let mut nonblank = 0usize;
    for y in y_start..y_end {
        for x in x_start..x_end {
            let pixel = &pixels[(y * width + x) * 4..][..4];
            if pixel[0].abs_diff(6) > 8 || pixel[1].abs_diff(7) > 8 || pixel[2].abs_diff(9) > 8 {
                nonblank += 1;
            }
        }
    }
    let sample_pixels = (x_end - x_start) * (y_end - y_start);
    if nonblank < sample_pixels / 2 {
        return Err(io::Error::other(format!(
            "immediate DataGrid header was blank in {path:?}: {nonblank}/{sample_pixels} nonblank pixels"
        )));
    }
    Ok(nonblank)
}

fn debug_window_target_command(
    endpoint: SocketAddr,
    id: &str,
    method: &str,
    semantic_node_path: &str,
) -> io::Result<serde_json::Value> {
    let response = call(
        endpoint,
        &rpc_request(
            id,
            "wgpu-runtime",
            method,
            json!({"semantic_node_path": semantic_node_path}),
            None,
            None,
        ),
    )?;
    assert_accepted(response.clone(), method)?;
    response
        .result
        .ok_or_else(|| io::Error::other(format!("{method} omitted its result")))
}

fn debug_window_value_gesture(
    endpoint: SocketAddr,
    id: &str,
    semantic_node_path: &str,
    target_fraction: f64,
) -> io::Result<serde_json::Value> {
    let response = call(
        endpoint,
        &rpc_request(
            id,
            "wgpu-runtime",
            "debug.window.input.value_gesture",
            json!({
                "semantic_node_path": semantic_node_path,
                "target_fraction": target_fraction,
            }),
            None,
            None,
        ),
    )?;
    assert_accepted(response.clone(), "debug.window.input.value_gesture")?;
    response
        .result
        .ok_or_else(|| io::Error::other("debug value gesture omitted its result"))
}

fn debug_window_drag_gesture(
    endpoint: SocketAddr,
    id: &str,
    source_node_key: &str,
    target_node_key: &str,
) -> io::Result<serde_json::Value> {
    let response = call(
        endpoint,
        &rpc_request(
            id,
            "wgpu-runtime",
            "debug.window.input.drag_gesture",
            json!({
                "source_node_key": source_node_key,
                "target_node_key": target_node_key,
            }),
            None,
            None,
        ),
    )?;
    assert_accepted(response.clone(), "debug.window.input.drag_gesture")?;
    response
        .result
        .ok_or_else(|| io::Error::other("debug drag gesture omitted its result"))
}

#[derive(Clone, Copy)]
enum ExpectedNumeric {
    I32(i32),
    F32(f32),
}

fn assert_numeric_commit(
    ui_endpoint: SocketAddr,
    wgpu_endpoint: SocketAddr,
    fragment_id: &str,
    node_key: &str,
    input_key: &str,
    expected: ExpectedNumeric,
) -> io::Result<(serde_json::Value, serde_json::Value)> {
    let host = call(
        ui_endpoint,
        &rpc_request(
            &format!("window-input-{input_key}-snapshot"),
            "ui-runtime",
            "debug.ui.host.snapshot",
            json!({}),
            None,
            None,
        ),
    )?;
    assert_accepted(host.clone(), "UI host scalar snapshot")?;
    let snapshot: neon_ui_schema::UiProgramInputSnapshot = serde_json::from_value(
        host.result
            .ok_or_else(|| io::Error::other("UI host scalar snapshot is not active"))?,
    )
    .map_err(io::Error::other)?;
    let scalar = snapshot
        .scalar_inputs
        .values
        .get(input_key)
        .ok_or_else(|| io::Error::other(format!("UI host snapshot omitted {input_key}")))?;
    let scalar_matches = match (expected, &scalar.value) {
        (ExpectedNumeric::I32(expected), UiInputValue::I32 { value }) => *value == expected,
        (ExpectedNumeric::F32(expected), UiInputValue::F32 { value }) => {
            (*value - expected).abs() < 0.001
        }
        _ => false,
    };
    if !scalar_matches {
        return Err(io::Error::other(format!(
            "UI host scalar {input_key} did not retain the committed value: {:?}",
            scalar.value
        )));
    }

    let rendered = call(
        wgpu_endpoint,
        &rpc_request(
            &format!("window-input-{input_key}-rendered"),
            "wgpu-runtime",
            "wgpu.ui.fragment.snapshot",
            json!({"fragment_id": fragment_id}),
            None,
            None,
        ),
    )?;
    assert_accepted(rendered.clone(), "rendered fragment snapshot")?;
    let fragment: UiFragment = serde_json::from_value(rendered.result.ok_or_else(|| io::Error::other("rendered fragment snapshot omitted result"))?["fragment"].clone()).map_err(io::Error::other)?;
    let presentation = fragment
        .effects
        .iter()
        .find_map(|effect| match effect {
            UiEffect::ControlPresentation { node_id, state } if node_id.0 == node_key => {
                Some(state)
            }
            _ => None,
        })
        .ok_or_else(|| {
            io::Error::other(format!(
                "rendered fragment omitted presentation for {node_key}"
            ))
        })?;
    let rendered_matches = match (expected, presentation) {
        (ExpectedNumeric::I32(expected), UiControlPresentation::Numeric { value, min, max }) => {
            (*value - expected as f32).abs() < 0.001 && *min == 0.0 && *max == 24.0
        }
        (ExpectedNumeric::F32(expected), UiControlPresentation::Numeric { value, min, max }) => {
            (*value - expected).abs() < 0.001 && *min == 0.0 && *max == 1.0
        }
        (ExpectedNumeric::F32(expected), UiControlPresentation::Scroll { position }) => {
            (*position - expected).abs() < 0.001
        }
        _ => false,
    };
    if !rendered_matches {
        return Err(io::Error::other(format!(
            "rendered presentation for {node_key} reset after commit: {presentation:?}"
        )));
    }
    Ok((
        json!({
            "input_revision": snapshot.scalar_inputs.input_revision,
            "input_key": input_key,
            "value": scalar.value,
            "source": scalar.source,
        }),
        serde_json::to_value(presentation).map_err(io::Error::other)?,
    ))
}

fn assert_max_scroll(result: &serde_json::Value, label: &str) -> io::Result<()> {
    let scroll = result.get("scroll").unwrap_or(result);
    let offset = scroll
        .pointer("/offset/y")
        .and_then(serde_json::Value::as_f64);
    let maximum = scroll
        .pointer("/max_offset/y")
        .and_then(serde_json::Value::as_f64);
    if offset.is_none() || maximum.is_none() || offset != maximum || maximum == Some(0.0) {
        return Err(io::Error::other(format!(
            "{label} did not resolve to a nonzero maximum scroll offset: {result}"
        )));
    }
    Ok(())
}

fn activation_interaction_id(result: &serde_json::Value) -> io::Result<&str> {
    result
        .get("interaction_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| io::Error::other("debug activation omitted interaction_id"))
}

fn wait_for_graph_revision_after(
    endpoint: SocketAddr,
    before: u64,
    timeout: Duration,
) -> io::Result<u64> {
    let deadline = Instant::now() + timeout;
    loop {
        let current = window_graph_revision(endpoint)?;
        if current > before {
            return Ok(current);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("composition revision did not advance after {before}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_final_grid_window(
    endpoint: SocketAddr,
    fragment_id: &str,
    timeout: Duration,
) -> io::Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    let mut last_frame = serde_json::Value::Null;
    loop {
        let response = call(
            endpoint,
            &rpc_request(
                "window-input-final-grid",
                "wgpu-runtime",
                "wgpu.ui.fragment.snapshot",
                json!({"fragment_id": fragment_id}),
                None,
                None,
            ),
        )?;
        assert_accepted(response.clone(), "grid fragment snapshot")?;
        let fragment: UiFragment = serde_json::from_value(response.result.ok_or_else(|| io::Error::other("grid fragment snapshot omitted result"))?["fragment"].clone()).map_err(io::Error::other)?;
        if let Some(UiEffect::DataGridFrame { declaration, frame }) = fragment
            .effects
            .iter()
            .find(|effect| matches!(effect, UiEffect::DataGridFrame { .. }))
            && frame.first_row
                == frame
                    .total_rows
                    .saturating_sub(u64::from(declaration.max_window_rows))
            && frame.window_rows.len() == declaration.max_window_rows as usize
            && frame
                .window_rows
                .last()
                .is_some_and(|row| row.stable_row_key == "virtual-row-9999")
        {
            return Ok(
                json!({"list_revision": frame.list_revision.0, "first_row": frame.first_row, "window_rows": frame.window_rows.len(), "last_row_key": frame.window_rows.last().map(|row| &row.stable_row_key)}),
            );
        } else if let Some(UiEffect::DataGridFrame { frame, .. }) = fragment
            .effects
            .iter()
            .find(|effect| matches!(effect, UiEffect::DataGridFrame { .. }))
        {
            last_frame = json!({"list_revision": frame.list_revision.0, "first_row": frame.first_row, "window_rows": frame.window_rows.len(), "last_row_key": frame.window_rows.last().map(|row| &row.stable_row_key)});
        }
        if Instant::now() >= deadline {
            let delivery = call(
                endpoint,
                &rpc_request(
                    "window-input-grid-delivery",
                    "wgpu-runtime",
                    "debug.window.input.snapshot",
                    json!({}),
                    None,
                    None,
                ),
            )?
            .result
            .and_then(|value| value.get("data_grid_window_delivery").cloned())
            .unwrap_or(serde_json::Value::Null);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "final DataGrid window was not published; last frame: {last_frame}; delivery: {delivery}"
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_accepted_delivery_trace(
    endpoint: SocketAddr,
    interaction_id: &str,
    timeout: Duration,
) -> io::Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let response = call(
            endpoint,
            &rpc_request(
                "window-input-trace",
                "wgpu-runtime",
                "debug.interaction.get",
                json!({"interaction_id": interaction_id}),
                None,
                None,
            ),
        )?;
        if response.status == RpcStatus::Accepted {
            let records = response
                .result
                .as_ref()
                .and_then(|value| value.get("records"))
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            let accepted = records.iter().any(|record| {
                record.get("stage").and_then(serde_json::Value::as_str) == Some("delivery_accepted")
                    && record.get("outcome").and_then(serde_json::Value::as_str) == Some("accepted")
            });
            if accepted {
                return Ok(json!({"interaction_id": interaction_id, "records": records}));
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("interaction trace {interaction_id} did not reach accepted delivery"),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn window_graph_revision(endpoint: SocketAddr) -> io::Result<u64> {
    let response = call(
        endpoint,
        &rpc_request(
            "window-input-diagnostics",
            "wgpu-runtime",
            "wgpu.render.diagnostics",
            json!({}),
            None,
            None,
        ),
    )?;
    assert_accepted(response.clone(), "WGPU diagnostics")?;
    response
        .result
        .and_then(|value| {
            value
                .get("graph_revision")
                .and_then(serde_json::Value::as_u64)
        })
        .ok_or_else(|| io::Error::other("WGPU diagnostics omitted graph_revision"))
}

fn wait_for_pointer_delivery(
    endpoint: SocketAddr,
    timeout: Duration,
) -> io::Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let response = call(
            endpoint,
            &rpc_request(
                "window-input-snapshot",
                "wgpu-runtime",
                "debug.window.input.snapshot",
                json!({}),
                None,
                None,
            ),
        )?;
        assert_accepted(response.clone(), "window input snapshot")?;
        let delivery = response
            .result
            .and_then(|value| value.get("pointer_delivery").cloned())
            .unwrap_or_else(|| json!({"state": "missing"}));
        match delivery.get("state").and_then(serde_json::Value::as_str) {
            Some("accepted") | Some("rejected") | Some("transport_failed") | Some("not_sent") => {
                return Ok(delivery);
            }
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "UI host did not respond to pointer delivery",
                ));
            }
        }
    }
}

fn wait_for_window_gpu(endpoint: SocketAddr) -> io::Result<()> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let response = call(
            endpoint,
            &rpc_request(
                "window-input-ready",
                "wgpu-runtime",
                "debug.window.input.snapshot",
                json!({}),
                None,
                None,
            ),
        );
        if let Ok(response) = response
            && response.status == RpcStatus::Accepted
            && response
                .result
                .as_ref()
                .and_then(|value| value.get("state"))
                .and_then(serde_json::Value::as_str)
                != Some("uninitialized")
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "window compositor did not initialize",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn inspect_window_input(endpoint: &str) -> io::Result<()> {
    let endpoint = endpoint.parse::<SocketAddr>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid WGPU loopback endpoint: {error}"),
        )
    })?;
    let response = call(
        endpoint,
        &rpc_request(
            "window-image-inspect",
            "wgpu-runtime",
            "debug.window.images",
            json!({}),
            None,
            None,
        ),
    )?;
    println!(
        "{}",
        json!({
            "status": if response.status == RpcStatus::Accepted { "passed" } else { "rejected" },
            "snapshot": response.result,
            "error": response.error,
        })
    );
    if response.status == RpcStatus::Accepted {
        Ok(())
    } else {
        Err(io::Error::other("window input inspection was rejected"))
    }
}

fn capture_window(endpoint: &str, path: Option<&String>) -> io::Result<()> {
    let endpoint = endpoint.parse::<SocketAddr>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid WGPU loopback endpoint: {error}"),
        )
    })?;
    let path = path
        .map(|path| {
            let mut path = PathBuf::from(path);
            if path.extension().is_none() {
                path.set_extension("png");
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("png") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "capture path must use the .png extension",
                ));
            }
            if path.is_relative() {
                path = std::env::current_dir()?.join(path);
            }
            Ok(path)
        })
        .transpose()?;
    let mut params = json!({"target": "ui.color.v1"});
    if let Some(path) = path {
        params["path"] = json!(path.to_string_lossy());
    }
    let request_id = format!(
        "neon-dev-capture-window-{}-{}",
        std::process::id(),
        unix_time_ms()?
    );
    let response = call(
        endpoint,
        &rpc_request(
            &request_id,
            "wgpu-runtime",
            "wgpu.render.target.capture",
            params,
            None,
            None,
        ),
    )?;
    println!(
        "{}",
        json!({
            "status": if response.status == RpcStatus::Accepted { "passed" } else { "rejected" },
            "request_id": response.request_id,
            "capture": response.result,
            "error": response.error,
        })
    );
    if response.status == RpcStatus::Accepted {
        Ok(())
    } else {
        Err(io::Error::other("window capture was rejected"))
    }
}

fn debug_interaction(endpoint: &str, interaction_id: &str) -> io::Result<()> {
    let endpoint = endpoint.parse::<SocketAddr>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid WGPU loopback endpoint: {error}"),
        )
    })?;
    let response = call(
        endpoint,
        &rpc_request(
            "neon-dev-debug-interaction",
            "wgpu-runtime",
            "debug.interaction.get",
            json!({"interaction_id": interaction_id}),
            None,
            None,
        ),
    )?;
    println!(
        "{}",
        json!({
            "endpoint": endpoint.to_string(),
            "method": "debug.interaction.get",
            "interaction_id": interaction_id,
            "response": response,
        })
    );
    if response.status == RpcStatus::Accepted {
        Ok(())
    } else {
        Err(io::Error::other("interaction trace query was rejected"))
    }
}

#[derive(Clone, Copy)]
enum ManifestService {
    Wgpu,
    Ui,
    Host,
}

impl ManifestService {
    fn index(self) -> usize {
        match self {
            Self::Wgpu => 0,
            Self::Ui => 1,
            Self::Host => 2,
        }
    }
}

struct LiveSessionManifest {
    session_id: String,
    case: String,
    state: &'static str,
    started_at_unix_ms: u64,
    stopped_at_unix_ms: Option<u64>,
    failure: Option<String>,
    supervisor_pid: u32,
    pids: [Option<u32>; 3],
    endpoints: [SocketAddr; 3],
    process_epochs: [Option<u64>; 3],
    service_ready: [bool; 3],
    window_viewport: Option<serde_json::Value>,
    session_path: PathBuf,
    latest_path: PathBuf,
}

impl LiveSessionManifest {
    fn start(
        workspace: &Path,
        case: &str,
        wgpu_endpoint: SocketAddr,
        ui_endpoint: SocketAddr,
        host_endpoint: SocketAddr,
    ) -> io::Result<Self> {
        let started_at_unix_ms = unix_time_ms()?;
        let session_id = format!("{started_at_unix_ms}-{}", std::process::id());
        let root = workspace.join("target/neon-dev");
        let mut manifest = Self {
            session_path: root.join("sessions").join(format!("{session_id}.json")),
            latest_path: root.join("latest.json"),
            session_id,
            case: case.into(),
            state: "starting",
            started_at_unix_ms,
            stopped_at_unix_ms: None,
            failure: None,
            supervisor_pid: std::process::id(),
            pids: [None; 3],
            endpoints: [wgpu_endpoint, ui_endpoint, host_endpoint],
            process_epochs: [None; 3],
            service_ready: [false; 3],
            window_viewport: None,
        };
        manifest.persist_session()?;
        manifest.publish_as_latest()?;
        Ok(manifest)
    }

    fn spawned(&mut self, service: ManifestService, pid: u32) -> io::Result<()> {
        self.pids[service.index()] = Some(pid);
        self.persist()
    }

    fn ready(&mut self, service: ManifestService, process_epoch: Option<u64>) -> io::Result<()> {
        self.service_ready[service.index()] = true;
        self.process_epochs[service.index()] = process_epoch;
        self.persist()
    }

    fn session_ready(&mut self) -> io::Result<()> {
        self.state = "ready";
        self.persist()
    }

    fn record_window_viewport(&mut self, viewport: serde_json::Value) -> io::Result<()> {
        self.window_viewport = Some(viewport);
        self.persist()
    }

    fn stop(&mut self) -> io::Result<()> {
        self.state = "stopped";
        self.stopped_at_unix_ms = Some(unix_time_ms()?);
        self.persist()
    }

    fn fail(&mut self, error: &io::Error) -> io::Result<()> {
        self.state = "failed";
        self.stopped_at_unix_ms = Some(unix_time_ms()?);
        self.failure = Some(error.to_string());
        self.persist()
    }

    fn persist(&self) -> io::Result<()> {
        self.persist_session()?;
        if self.is_latest_session()? {
            atomic_write_json(&self.latest_path, &self.value())?;
        }
        Ok(())
    }

    fn persist_session(&self) -> io::Result<()> {
        atomic_write_json(&self.session_path, &self.value())
    }

    fn publish_as_latest(&mut self) -> io::Result<()> {
        let should_publish = match read_manifest(&self.latest_path) {
            Ok(current) => current
                .get("started_at_unix_ms")
                .and_then(serde_json::Value::as_u64)
                .is_none_or(|started| started <= self.started_at_unix_ms),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                ) =>
            {
                true
            }
            Err(error) => return Err(error),
        };
        if should_publish {
            atomic_write_json(&self.latest_path, &self.value())?;
        }
        Ok(())
    }

    fn is_latest_session(&self) -> io::Result<bool> {
        let latest = read_manifest(&self.latest_path)?;
        Ok(latest.get("session_id").and_then(serde_json::Value::as_str)
            == Some(self.session_id.as_str()))
    }

    fn value(&self) -> serde_json::Value {
        let service_state = |index: usize| {
            if self.service_ready[index] {
                "ready"
            } else if self.pids[index].is_some() {
                "starting"
            } else {
                "pending"
            }
        };
        json!({
            "kind": "neon3.session",
            "version": 2,
            "session_id": self.session_id,
            "case": self.case,
            "state": self.state,
            "started_at_unix_ms": self.started_at_unix_ms,
            "stopped_at_unix_ms": self.stopped_at_unix_ms,
            "failure": self.failure,
            "window_mode": "windowed",
            "window": {
                "viewport": self.window_viewport,
            },
            "pids": {
                "neon_dev": self.supervisor_pid,
                "wgpu": self.pids[0],
                "ui": self.pids[1],
                "host": self.pids[2],
            },
            "endpoints": {
                "wgpu": self.endpoints[0].to_string(),
                "ui": self.endpoints[1].to_string(),
                "host": self.endpoints[2].to_string(),
            },
            "process_epoch": {
                "wgpu": self.process_epochs[0],
                "ui": self.process_epochs[1],
                "host": self.process_epochs[2],
            },
            "services": [
                {"name": "wgpu-runtime", "pid": self.pids[0], "endpoint": self.endpoints[0].to_string(), "process_epoch": self.process_epochs[0], "state": service_state(0)},
                {"name": "ui-runtime", "pid": self.pids[1], "endpoint": self.endpoints[1].to_string(), "process_epoch": self.process_epochs[1], "state": service_state(1)},
                {"name": "ui-host", "pid": self.pids[2], "endpoint": self.endpoints[2].to_string(), "process_epoch": self.process_epochs[2], "state": service_state(2)},
            ],
            "debug": {
                "snapshot": {"endpoint": self.endpoints[0].to_string(), "method": "debug.snapshot.get"},
                "window_input": {"endpoint": self.endpoints[0].to_string(), "method": "debug.window.input.snapshot"},
                "window_capture": {"endpoint": self.endpoints[0].to_string(), "method": "wgpu.render.target.capture", "params": {"target": "ui.color.v1"}},
                "interaction_get": {"endpoint": self.endpoints[0].to_string(), "method": "debug.interaction.get", "params": {"interaction_id": "<interaction-id>"}},
                "interaction_query": {"endpoint": self.endpoints[0].to_string(), "method": "debug.interaction.query", "params": {"filters": {}, "limit": 100}},
            },
        })
    }
}

fn query_window_viewport(endpoint: SocketAddr) -> io::Result<serde_json::Value> {
    let response = call(
        endpoint,
        &rpc_request(
            "neon-dev-manifest-window-viewport",
            "wgpu-runtime",
            "debug.snapshot.get",
            json!({}),
            None,
            None,
        ),
    )?;
    assert_accepted(response.clone(), "window viewport snapshot")?;
    response
        .result
        .and_then(|snapshot| snapshot.pointer("/window/viewport").cloned())
        .ok_or_else(|| io::Error::other("window debug snapshot omitted the accepted viewport"))
}

fn query_process_epoch(endpoint: SocketAddr, target: &str) -> Option<u64> {
    let response = call(
        endpoint,
        &rpc_request(
            "neon-dev-manifest-describe",
            target,
            "service.describe",
            json!({}),
            None,
            None,
        ),
    )
    .ok()?;
    if response.status != RpcStatus::Accepted {
        return None;
    }
    response
        .result?
        .get("epoch")
        .and_then(serde_json::Value::as_u64)
}

fn print_status(path: &Path) -> io::Result<()> {
    let manifest = read_manifest(path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&manifest).map_err(io::Error::other)?
    );
    Ok(())
}

fn read_manifest(path: &Path) -> io::Result<serde_json::Value> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid session manifest {}: {error}", path.display()),
        )
    })
}

fn atomic_write_json(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "manifest path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid manifest file name"))?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn unix_time_ms() -> io::Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    u64::try_from(millis).map_err(io::Error::other)
}

fn probe_window_input(endpoint: &str, x: &str, y: &str) -> io::Result<()> {
    let endpoint = endpoint.parse::<SocketAddr>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid WGPU loopback endpoint: {error}"),
        )
    })?;
    let x = x.parse::<f64>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid x coordinate: {error}"),
        )
    })?;
    let y = y.parse::<f64>().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid y coordinate: {error}"),
        )
    })?;
    if !x.is_finite() || !y.is_finite() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "coordinates must be finite",
        ));
    }
    let response = call(
        endpoint,
        &rpc_request(
            "window-input-probe",
            "wgpu-runtime",
            "debug.window.input.probe",
            json!({"logical_position": {"x": x, "y": y}}),
            None,
            None,
        ),
    )?;
    println!(
        "{}",
        json!({"status": if response.status == RpcStatus::Accepted { "passed" } else { "rejected" }, "probe": response.result, "error": response.error})
    );
    if response.status == RpcStatus::Accepted {
        Ok(())
    } else {
        Err(io::Error::other("window input probe was rejected"))
    }
}

fn run_component_gallery_scenario_inner() -> io::Result<serde_json::Value> {
    const SCENARIO: &str = "component-gallery-interactions";
    const FRAGMENT_ID: &str = "component-gallery-scenario";
    let workspace = workspace_root()?;
    let wgpu_endpoint = reserve_loopback_endpoint()?;
    let ui_endpoint = reserve_loopback_endpoint()?;
    let domain_endpoint = reserve_loopback_endpoint()?;
    let projectd_endpoint = reserve_loopback_endpoint()?;
    let job = ProcessJob::new()?;
    let mut children = ChildSession::default();

    let projectd = spawn_service(
        executable(&workspace, "neon-projectd"),
        &["--server", &projectd_endpoint.to_string()],
        false,
    )?;
    job.assign(&projectd)?;
    children.push(projectd);
    wait_for_endpoint(projectd_endpoint)?;
    let gallery_image = component_gallery_image_asset(projectd_endpoint)?;
    let gallery_image_json = serde_json::to_string(&gallery_image).expect("AssetRef serializes");

    let wgpu = spawn_service(
        executable(&workspace, "neon-wgpu-runtime"),
        &["--headless-server", &wgpu_endpoint.to_string()],
        false,
    )?;
    job.assign(&wgpu)?;
    children.push(wgpu);
    wait_for_endpoint(wgpu_endpoint)?;
    let domain = spawn_service(
        executable(&workspace, "component_gallery_domain_controller"),
        &[&domain_endpoint.to_string(), &gallery_image_json],
        false,
    )?;
    job.assign(&domain)?;
    children.push(domain);
    wait_for_endpoint(domain_endpoint)?;
    let ui = spawn_service(
        executable(&workspace, "neon-ui-runtime"),
        &[
            "--forward-server",
            &ui_endpoint.to_string(),
            &wgpu_endpoint.to_string(),
            &domain_endpoint.to_string(),
        ],
        false,
    )?;
    job.assign(&ui)?;
    children.push(ui);
    wait_for_endpoint(ui_endpoint)?;

    let (document, program) =
        neon_ui_runtime::demo_domain::component_gallery_program(gallery_image)
            .map_err(io::Error::other)?;
    let mut fragment = UiFragment {
        fragment_id: UiFragmentId(FRAGMENT_ID.into()),
        revision: Revision(1),
        root: document.ir.root.clone(),
        effects: lower_nui_flow_effects(&document),
    };
    let submit = rpc_request(
        "gallery-submit-1",
        "ui-runtime",
        "ui.fragment.submit",
        json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment.clone())
        }),
        None,
        Some("gallery-submit-1"),
    );
    let response = call(ui_endpoint, &submit)?;
    assert_accepted(response, "initial Gallery fragment submission")?;

    let controls = [
        ("feature-toggle", "Checkbox"),
        ("mode-radio", "RadioButton"),
        ("exposure-slider", "Slider"),
        ("count-drag", "DragValue"),
        ("mode-combo", "Combo"),
        ("mode-dropdown", "Dropdown"),
        ("item-selectable", "Selectable"),
        ("item-list", "ListBox"),
        ("gallery-scroll", "Scrollbar"),
    ];
    let mut steps: Vec<serde_json::Value> = Vec::new();
    for (index, (node_key, control)) in controls.into_iter().enumerate() {
        let declaration = program
            .event_records
            .iter()
            .find(|event| event.node_key == node_key)
            .ok_or_else(|| {
                io::Error::other(format!("{control} declaration missing for {node_key}"))
            })?;
        let intent = fragment.effects.iter().find_map(|effect| match effect {
            UiEffect::SemanticIntent { intent } | UiEffect::BoundSemanticIntent { intent, .. }
                if matches!(intent, UiIntent::Invoke { action, .. } if action == &declaration.intent) => Some(intent.clone()),
            _ => None,
        }).ok_or_else(|| io::Error::other(format!("{control} semantic binding is missing")))?;
        let domain_inputs = if index == 0 {
            initial_inputs(&document.input_schema, &program.revision)?
        } else {
            let previous = steps
                .last()
                .ok_or_else(|| io::Error::other("missing previous Gallery step"))?;
            serde_json::from_value(previous["domain_snapshot"]["inputs"].clone())
                .map_err(|e| io::Error::other(e.to_string()))?
        };
        let payload = declaration
            .bound_input_keys
            .iter()
            .map(|key| {
                domain_inputs
                    .values
                    .get(key)
                    .ok_or_else(|| io::Error::other(format!("missing input {key}")))
                    .map(|value| (key.clone(), input_payload(value)))
            })
            .collect::<io::Result<std::collections::BTreeMap<_, _>>>()?;
        let kind = if matches!(
            node_key,
            "feature-toggle"
                | "mode-radio"
                | "mode-combo"
                | "mode-dropdown"
                | "item-selectable"
                | "item-list"
        ) {
            UiProgramSemanticEventKind::SelectionChanged
        } else {
            UiProgramSemanticEventKind::ValueCommit
        };
        let event = UiProgramSemanticEvent {
            event_id: format!("gallery-event-{index}"),
            kind,
            intent: declaration.intent.clone(),
            source_node_key: node_key.into(),
            payload,
            program_revision: program.revision.clone(),
            input_revision: domain_inputs.input_revision,
            request_id: format!("gallery-request-{index}"),
            idempotency_key: format!("gallery-key-{index}"),
            requested_value: None,
            interaction: UiSemanticInteractionMetadata {
                interaction_id: format!("gallery-interaction-{index}"),
                sequence: index as u64 + 1,
                renderer_epoch: 1,
            },
        };
        let hit_request = rpc_request(
            &format!("gallery-hit-{index}"),
            "wgpu-runtime",
            "test.ui.hit_sample.request",
            json!({"pointer_id": 7, "sequence": index as u64 + 1}),
            None,
            None,
        );
        let hit = call(wgpu_endpoint, &hit_request)?;
        assert_accepted(hit, "Gallery hit sample")?;
        let hit_complete = rpc_request(
            &format!("gallery-hit-complete-{index}"),
            "wgpu-runtime",
            "test.ui.hit_sample.complete",
            json!({"pointer_id": 7, "test_hit_id": 0}),
            None,
            None,
        );
        assert_accepted(
            call(wgpu_endpoint, &hit_complete)?,
            "Gallery hit completion",
        )?;
        let legacy_event = UiSemanticEvent {
            event: match kind {
                UiProgramSemanticEventKind::ValueTentative => UiSemanticEventType::ValuePreview,
                UiProgramSemanticEventKind::SelectionChanged => {
                    UiSemanticEventType::SelectionChanged
                }
                _ => UiSemanticEventType::ValueCommit,
            },
            event_id: format!("gallery-ui-event-{index}"),
            renderer_epoch: 1,
            composition_revision: Revision(1),
            fragment: UiFragmentRevision {
                id: fragment.fragment_id.clone(),
                revision: fragment.revision,
            },
            intent,
            pointer: None,
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: None,
        };
        let ui_validation = call(
            ui_endpoint,
            &rpc_request(
                &format!("gallery-ui-validation-{index}"),
                "ui-runtime",
                "ui.input.event",
                json!(legacy_event),
                Some(fragment.revision),
                Some(&format!("gallery-ui-key-{index}")),
            ),
        )?;
        assert_accepted(ui_validation.clone(), "UiRuntime Gallery event validation")?;
        let host_snapshot = call(
            ui_endpoint,
            &rpc_request(
                &format!("gallery-host-snapshot-{index}"),
                "ui-runtime",
                "debug.ui.host.snapshot",
                json!({}),
                None,
                None,
            ),
        )?;
        assert_accepted(host_snapshot.clone(), "Gallery host input snapshot")?;
        let host_snapshot: neon_ui_schema::UiProgramInputSnapshot = serde_json::from_value(
            host_snapshot
                .result
                .ok_or_else(|| io::Error::other("Gallery host snapshot is not active"))?,
        )
        .map_err(io::Error::other)?;
        let visible = call(
            wgpu_endpoint,
            &rpc_request(
                &format!("gallery-visible-{index}"),
                "wgpu-runtime",
                "wgpu.ui.fragment.snapshot",
                json!({"fragment_id": FRAGMENT_ID}),
                None,
                None,
            ),
        )?;
        assert_accepted(visible.clone(), "Gallery visible snapshot")?;
        fragment = serde_json::from_value(
            visible
                .result
                .ok_or_else(|| io::Error::other("visible snapshot has no result"))?["fragment"]
                .clone(),
        )
        .map_err(|e| io::Error::other(e.to_string()))?;
        let status_key = format!("status-{}", declaration.bound_input_keys[0]);
        let status_text = find_node(&fragment.root, &status_key)
            .and_then(first_literal)
            .ok_or_else(|| io::Error::other(format!("{control} visible status is missing")))?;
        let input_key = &declaration.bound_input_keys[0];
        let domain_snapshot = neon_ui_runtime::demo_domain::DemoInputDomainSnapshot {
            inputs: host_snapshot.scalar_inputs,
            visible_status: std::collections::BTreeMap::from([(
                status_key.clone(),
                status_text.clone(),
            )]),
        };
        steps.push(json!({"step": index + 1, "control": control, "node_key": node_key, "status": "passed", "hit": {"status": "accepted", "pointer_id": 7}, "binding": {"status": "validated", "input_keys": declaration.bound_input_keys}, "event": {"ui_runtime_status": ui_validation.status, "domain_status": "accepted", "request_id": event.request_id, "error": serde_json::Value::Null}, "input_revision": domain_snapshot.inputs.input_revision, "value": domain_snapshot.inputs.values[input_key].value, "visible_status": {"node": status_key, "text": status_text}, "domain_snapshot": domain_snapshot}));
    }
    Ok(json!({
        "scenario": SCENARIO,
        "status": "passed",
        "controls": 9,
        "interactive_controls": 9,
        "display_only_controls": [],
        "steps": steps,
        "errors": []
    }))
}

fn rpc_request(
    id: &str,
    target: &str,
    method: &str,
    params: serde_json::Value,
    expected_revision: Option<Revision>,
    idempotency_key: Option<&str>,
) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(id.into()),
        client: scenario_client(),
        target: ServiceName(target.into()),
        method: method.into(),
        params,
        expected_revision,
        idempotency_key: idempotency_key.map(str::to_owned),
    }
}

fn initial_inputs(
    schema: &neon_ui_schema::UiInputSchema,
    revision: &neon_ui_schema::UiProgramRevision,
) -> io::Result<neon_ui_schema::UiResolvedInputs> {
    neon_ui_runtime::UiInputStore::activate(revision.clone(), schema.clone())
        .map(|store| store.snapshot())
        .map_err(|error| io::Error::other(error.message))
}

fn input_payload(
    value: &neon_ui_schema::UiResolvedInputValue,
) -> neon_ui_schema::UiSemanticPayloadValue {
    match &value.value {
        UiInputValue::Bool { value } => UiSemanticPayloadValue::Bool { value: *value },
        UiInputValue::I32 { value } => UiSemanticPayloadValue::I32 { value: *value },
        UiInputValue::F32 { value } => UiSemanticPayloadValue::F32 { value: *value },
        UiInputValue::Enum { value } => UiSemanticPayloadValue::Enum {
            value: value.clone(),
        },
        _ => panic!("Gallery fixture contains an unsupported semantic input kind"),
    }
}

fn first_literal(node: &neon_ui_schema::UiNode) -> Option<String> {
    match &node.text {
        Some(neon_ui_schema::TextRef::Literal { value }) => Some(value.clone()),
        _ => node.children.iter().find_map(first_literal),
    }
}

fn run_drag_card02_before_scenario() -> io::Result<()> {
    const SCENARIO: &str = "drag-card02-before";
    const FRAGMENT_ID: &str = "scenario-drag-card02-before";
    let workspace = workspace_root()?;
    let wgpu_endpoint = reserve_loopback_endpoint()?;
    let ui_endpoint = reserve_loopback_endpoint()?;
    let domain_endpoint = reserve_loopback_endpoint()?;
    let job = ProcessJob::new()?;
    let mut children = ChildSession::default();

    let wgpu = spawn_service(
        executable(&workspace, "neon-wgpu-runtime"),
        &["--headless-server", &wgpu_endpoint.to_string()],
        false,
    )?;
    job.assign(&wgpu)?;
    children.push(wgpu);
    wait_for_endpoint(wgpu_endpoint)?;

    let domain = spawn_service(
        executable(&workspace, "demo_domain_controller"),
        &[&domain_endpoint.to_string()],
        false,
    )?;
    job.assign(&domain)?;
    children.push(domain);
    wait_for_endpoint(domain_endpoint)?;

    let ui = spawn_service(
        executable(&workspace, "neon-ui-runtime"),
        &[
            "--forward-server",
            &ui_endpoint.to_string(),
            &wgpu_endpoint.to_string(),
            &domain_endpoint.to_string(),
        ],
        false,
    )?;
    job.assign(&ui)?;
    children.push(ui);
    wait_for_endpoint(ui_endpoint)?;

    let document = parse_nui_flow(include_str!(
        "../../../tests/fixtures/ui/kanban-reparent-workbench.nui"
    ))
    .map_err(|error| io::Error::other(format!("scenario fixture is invalid: {error:?}")))?;
    let effects = lower_nui_flow_effects(&document);
    let intent = effects
        .iter()
        .find_map(|effect| match effect {
            UiEffect::DropBinding { binding } if binding.key == "progress-audit-drop" => {
                Some(binding.intent.clone())
            }
            _ => None,
        })
        .ok_or_else(|| io::Error::other("scenario drop binding is missing"))?;
    let fragment = UiFragment {
        fragment_id: UiFragmentId(FRAGMENT_ID.into()),
        revision: Revision(1),
        root: document.ir.root,
        effects,
    };
    let client = scenario_client();
    let submit = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId("scenario-card02-submit".into()),
        client: client.clone(),
        target: ServiceName("ui-runtime".into()),
        method: "ui.fragment.submit".into(),
        params: json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment.clone())
        }),
        expected_revision: None,
        idempotency_key: Some("scenario-card02-submit-v1".into()),
    };
    assert_accepted(call(ui_endpoint, &submit)?, "initial fragment submission")?;

    let event = UiSemanticEvent {
        event: UiSemanticEventType::DragDrop,
        event_id: "scenario-card02-before".into(),
        renderer_epoch: 1,
        composition_revision: Revision(1),
        fragment: UiFragmentRevision {
            id: fragment.fragment_id.clone(),
            revision: Revision(1),
        },
        intent,
        pointer: None,
        focus: None,
        data_grid_cell: None,
        text: None,
        control_value: None,
        drag_drop: Some(neon_ui_schema::UiDragDropPayload {
            source_key: "backlog-card-02".into(),
            target_key: "in-progress-panel".into(),
            placement: UiDropPlacement::Before,
            presentation_template_key: Some("progress-template".into()),
        }),
    };
    let apply = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId("scenario-card02-before".into()),
        client: client.clone(),
        target: ServiceName("ui-runtime".into()),
        method: "ui.input.event".into(),
        params: json!(event),
        expected_revision: Some(Revision(1)),
        idempotency_key: Some("scenario-card02-before-v1".into()),
    };
    let response = call(ui_endpoint, &apply)?;
    assert_accepted(response, "drag/drop application")?;

    let snapshot = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId("scenario-card02-snapshot".into()),
        client,
        target: ServiceName("wgpu-runtime".into()),
        method: "wgpu.ui.fragment.snapshot".into(),
        params: json!({"fragment_id": FRAGMENT_ID}),
        expected_revision: None,
        idempotency_key: None,
    };
    let response = call(wgpu_endpoint, &snapshot)?;
    assert_accepted(response.clone(), "accepted fragment snapshot")?;
    let accepted: UiFragment = serde_json::from_value(
        response
            .result
            .ok_or_else(|| io::Error::other("fragment snapshot has no result"))?["fragment"]
            .clone(),
    )
    .map_err(|error| io::Error::other(format!("fragment snapshot is invalid: {error}")))?;
    let board_columns = find_node(&accepted.root, "board-columns")
        .ok_or_else(|| io::Error::other("board columns missing"))?;
    let progress_index = board_columns
        .children
        .iter()
        .position(|node| node.node_id.0 == "in-progress-panel")
        .ok_or_else(|| io::Error::other("drop target missing"))?;
    let representation = board_columns
        .children
        .get(
            progress_index
                .checked_sub(1)
                .ok_or_else(|| io::Error::other("target has no preceding sibling"))?,
        )
        .ok_or_else(|| io::Error::other("accepted representation missing"))?;
    accepted
        .validate()
        .map_err(|error| io::Error::other(format!("accepted fragment is invalid: {error:?}")))?;
    let prototype = find_node(&accepted.root, "progress-template")
        .ok_or_else(|| io::Error::other("target template prototype missing"))?;
    if accepted.revision != Revision(2)
        || find_node(&accepted.root, "backlog-card-02").is_some()
        || representation.node_id.0 != "progress-template-backlog-card-02-r2-progress-template"
        || prototype.visible
        || prototype.enabled
        || !representation.visible
        || !representation.enabled
    {
        return Err(io::Error::other(
            "accepted fragment did not preserve the requested source, target, placement, and template semantics",
        ));
    }
    println!(
        "{}",
        json!({
            "scenario": SCENARIO, "status": "passed", "revision": accepted.revision.0,
            "source": "backlog-card-02", "target": "in-progress-panel",
            "placement": "before", "template": "progress-template",
            "prototype_hidden": true, "representation_visible": true,
            "fragment_placement": {"parent": "board-columns", "immediately_before": "in-progress-panel", "node": representation.node_id.0}
        })
    );
    Ok(())
}

fn scenario_client() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "neon-dev-scenario".into(),
        pid: std::process::id(),
        origin: "neon-dev".into(),
    }
}

fn call(endpoint: SocketAddr, request: &RpcRequest) -> io::Result<neon_protocol::RpcResponse> {
    RpcClient::connect(endpoint)
        .and_then(|mut client| client.call(request))
        .map_err(|error| io::Error::other(error.to_string()))
}

fn component_gallery_image_asset(projectd_endpoint: SocketAddr) -> io::Result<AssetRef> {
    let response = call(
        projectd_endpoint,
        &rpc_request(
            "component-gallery-assets",
            "projectd",
            "asset.list",
            json!({}),
            None,
            None,
        ),
    )?;
    assert_accepted(response.clone(), "component gallery asset snapshot")?;
    let assets: Vec<AssetRef> = serde_json::from_value(
        response
            .result
            .ok_or_else(|| io::Error::other("projectd asset snapshot has no result"))?,
    )
    .map_err(io::Error::other)?;
    assets
        .into_iter()
        .find(|asset| asset.kind == "image")
        .ok_or_else(|| io::Error::other("projectd snapshot has no image AssetRef"))
}

fn assert_accepted(response: neon_protocol::RpcResponse, operation: &str) -> io::Result<()> {
    if response.status == RpcStatus::Accepted {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{operation} rejected: {:?}",
        response.error
    )))
}

fn find_node<'a>(
    node: &'a neon_ui_schema::UiNode,
    key: &str,
) -> Option<&'a neon_ui_schema::UiNode> {
    (node.node_id.0 == key)
        .then_some(node)
        .or_else(|| node.children.iter().find_map(|child| find_node(child, key)))
}

fn workspace_root() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    executable
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("cannot determine workspace root"))
}

fn executable(workspace: &Path, name: &str) -> PathBuf {
    workspace
        .join("target")
        .join("debug")
        .join(format!("{name}.exe"))
}

fn reserve_loopback_endpoint() -> io::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = listener.local_addr()?;
    drop(listener);
    Ok(endpoint)
}

fn spawn_service(executable: PathBuf, arguments: &[&str], show_logs: bool) -> io::Result<Child> {
    let mut command = Command::new(executable);
    command.args(arguments).stdin(Stdio::null());
    if show_logs {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    hide_console_window(&mut command);
    command.spawn()
}

fn wait_for_endpoint(endpoint: SocketAddr) -> io::Result<()> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        match TcpStream::connect_timeout(&endpoint, Duration::from_millis(100)) {
            Ok(_) => return Ok(()),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("service did not bind {endpoint}"),
                ));
            }
        }
    }
}

fn wait_for_session_end(children: &mut ChildSession) {
    loop {
        if children
            .children
            .iter_mut()
            .any(|child| child.try_wait().ok().flatten().is_some())
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_: &mut Command) {}

#[derive(Default)]
struct ChildSession {
    children: Vec<Child>,
}

#[cfg(windows)]
struct ProcessJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl ProcessJob {
    fn new() -> io::Result<Self> {
        use std::mem::size_of;
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let error = io::Error::last_os_error();
                windows_sys::Win32::Foundation::CloseHandle(handle);
                return Err(error);
            }
            Ok(Self(handle))
        }
    }

    fn assign(&self, child: &Child) -> io::Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as _) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(not(windows))]
struct ProcessJob;

#[cfg(not(windows))]
impl ProcessJob {
    fn new() -> io::Result<Self> {
        Ok(Self)
    }
    fn assign(&self, _: &Child) -> io::Result<()> {
        Ok(())
    }
}

impl ChildSession {
    fn push(&mut self, child: Child) {
        self.children.push(child);
    }

    fn stop_all(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.children.clear();
    }
}

impl Drop for ChildSession {
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_manifest_is_atomically_updated_through_clean_stop() {
        let root = test_root("lifecycle");
        fs::create_dir_all(&root).unwrap();
        let mut manifest = LiveSessionManifest::start(
            &root,
            "component-gallery",
            "127.0.0.1:4101".parse().unwrap(),
            "127.0.0.1:4102".parse().unwrap(),
            "127.0.0.1:4103".parse().unwrap(),
        )
        .unwrap();
        manifest.spawned(ManifestService::Wgpu, 101).unwrap();
        manifest.ready(ManifestService::Wgpu, Some(7)).unwrap();
        manifest.spawned(ManifestService::Host, 102).unwrap();
        manifest.ready(ManifestService::Host, None).unwrap();
        manifest.spawned(ManifestService::Ui, 103).unwrap();
        manifest.ready(ManifestService::Ui, Some(8)).unwrap();
        manifest
            .record_window_viewport(json!({
                "physical_size": {"width": 2502, "height": 1350},
                "logical_size": {"width": 1668.0, "height": 900.0},
                "scale_factor": 1.5,
            }))
            .unwrap();
        manifest.session_ready().unwrap();

        let latest = read_manifest(&root.join("target/neon-dev/latest.json")).unwrap();
        let session = read_manifest(&manifest.session_path).unwrap();
        assert_eq!(latest, session);
        assert_eq!(latest["state"], "ready");
        assert_eq!(latest["pids"]["wgpu"], 101);
        assert_eq!(latest["endpoints"]["host"], "127.0.0.1:4103");
        assert_eq!(latest["process_epoch"]["wgpu"], 7);
        assert_eq!(latest["process_epoch"]["ui"], 8);
        assert!(latest["process_epoch"]["host"].is_null());
        assert_eq!(latest["window"]["viewport"]["physical_size"]["width"], 2502);
        assert_eq!(
            latest["window"]["viewport"]["logical_size"]["height"],
            900.0
        );

        manifest.stop().unwrap();
        let stopped = read_manifest(&manifest.session_path).unwrap();
        assert_eq!(stopped["state"], "stopped");
        assert!(stopped["stopped_at_unix_ms"].as_u64().is_some());
        assert_eq!(
            stopped,
            read_manifest(&root.join("target/neon-dev/latest.json")).unwrap()
        );
        assert!(
            fs::read_dir(manifest.session_path.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp"))
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_exposes_live_window_query_contract() {
        let root = test_root("contract");
        fs::create_dir_all(&root).unwrap();
        let manifest = LiveSessionManifest::start(
            &root,
            "component-gallery",
            "127.0.0.1:4101".parse().unwrap(),
            "127.0.0.1:4102".parse().unwrap(),
            "127.0.0.1:4103".parse().unwrap(),
        )
        .unwrap();
        let manifest = manifest.value();
        assert_eq!(manifest["kind"], "neon3.session");
        assert_eq!(manifest["window_mode"], "windowed");
        assert_eq!(manifest["services"].as_array().unwrap().len(), 3);
        assert_eq!(
            manifest["debug"]["interaction_get"]["method"],
            "debug.interaction.get"
        );
        assert_eq!(
            manifest["debug"]["window_input"]["method"],
            "debug.window.input.snapshot"
        );
        assert!(manifest.to_string().contains("127.0.0.1:4101"));

        fs::remove_dir_all(root).unwrap();
    }

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "neon-dev-manifest-{label}-{}-{}",
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }
}
