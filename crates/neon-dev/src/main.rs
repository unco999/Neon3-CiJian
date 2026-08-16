//! Local Neon3 session supervisor. It owns child-process lifecycle only; it
//! does not own a window, GPU object, domain state, or UI declaration.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcStatus,
    ServiceName,
};
use neon_ui_runtime::{lower_nui_flow_effects, parse_nui_flow};
use neon_ui_schema::{
    UiCommand, UiDropPlacement, UiEffect, UiFragment, UiFragmentId, UiFragmentRevision,
    UiFragmentSubmission, UiProgramSemanticEvent, UiProgramSemanticEventKind,
    UiSemanticInteractionMetadata, UiSemanticPayloadValue, UiInputValue, UiIntent,
    UiSemanticEvent, UiSemanticEventType,
};
use serde_json::json;

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

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
    if matches!(args.as_slice(), [command, scenario] if command == "scenario" && scenario == "drag-card02-before") {
        return run_drag_card02_before_scenario();
    }
    if matches!(args.as_slice(), [command, scenario] if command == "scenario" && scenario == "component-gallery-interactions") {
        return run_component_gallery_scenario();
    }
    if matches!(args.as_slice(), [command, scenario] if command == "scenario" && scenario == "component-gallery-window-input") {
        return run_component_gallery_window_input_scenario();
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
            if command == "case" && matches!(case.as_str(), "kanban-reparent" | "asset-review" | "component-gallery" | "data-grid" | "scroll-view" | "virtual-list") =>
        {
            case
        }
        [command, case, flag]
            if command == "case"
                && flag == "--show-logs"
                && matches!(case.as_str(), "kanban-reparent" | "asset-review" | "component-gallery" | "data-grid" | "scroll-view" | "virtual-list") =>
        {
            case
        }
        [flag] if flag == "--help" => {
            println!("neon-dev case <kanban-reparent|asset-review|component-gallery|data-grid|scroll-view|virtual-list> [--show-logs]\nneon-dev scenario <drag-card02-before|component-gallery-interactions|component-gallery-window-input>\nneon-dev debug-interaction <wgpu-loopback-endpoint> <interaction-id>\nneon-dev inspect-window <wgpu-loopback-endpoint>\nneon-dev probe-window <wgpu-loopback-endpoint> <x> <y>");
            return Ok(());
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: neon-dev case <kanban-reparent|asset-review|component-gallery|data-grid|scroll-view|virtual-list> [--show-logs] | neon-dev scenario <drag-card02-before|component-gallery-interactions|component-gallery-window-input> | neon-dev debug-interaction <wgpu-loopback-endpoint> <interaction-id> | neon-dev inspect-window <wgpu-loopback-endpoint> | neon-dev probe-window <wgpu-loopback-endpoint> <x> <y>",
            ));
        }
    };
    let show_logs = args.iter().any(|argument| argument == "--show-logs");
    let workspace = workspace_root()?;
    let wgpu_endpoint = reserve_loopback_endpoint()?;
    let ui_endpoint = reserve_loopback_endpoint()?;
    let domain_endpoint = reserve_loopback_endpoint()?;
    let job = ProcessJob::new()?;
    let mut children = ChildSession::default();

    let wgpu = spawn_service(
        executable(&workspace, "neon-wgpu-runtime"),
        &[
            "--window-server".as_ref(),
            &wgpu_endpoint.to_string(),
            &ui_endpoint.to_string(),
        ],
        show_logs,
    )?;
    job.assign(&wgpu)?;
    children.push(wgpu);
    wait_for_endpoint(wgpu_endpoint)?;

    let domain_endpoint_text = domain_endpoint.to_string();
    let domain_args = if case == "component-gallery" {
        vec![domain_endpoint_text.as_str(), "--component-gallery"]
    } else {
        vec![domain_endpoint_text.as_str()]
    };
    let domain = spawn_service(
        executable(&workspace, "demo_domain_controller"),
        &domain_args,
        show_logs,
    )?;
    job.assign(&domain)?;
    children.push(domain);
    wait_for_endpoint(domain_endpoint)?;

    let ui = {
        let ui_endpoint_text = ui_endpoint.to_string();
        let wgpu_endpoint_text = wgpu_endpoint.to_string();
        let domain_endpoint_text = domain_endpoint.to_string();
        spawn_service(
            executable(&workspace, "neon-ui-runtime"),
            &[
                "--forward-server",
                &ui_endpoint_text,
                &wgpu_endpoint_text,
                &domain_endpoint_text,
            ],
            show_logs,
        )?
    };
    job.assign(&ui)?;
    children.push(ui);
    wait_for_endpoint(ui_endpoint)?;

    let mut submitter = Command::new(executable(&workspace, "nui_flow_demo"));
    submitter
        .args([case, &ui_endpoint.to_string()])
        .current_dir(&workspace);
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

    println!("{}", session_manifest(case, wgpu_endpoint, ui_endpoint, domain_endpoint));
    wait_for_session_end(&mut children);
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
            println!("{}", json!({"scenario": "component-gallery-interactions", "status": "failed", "steps": [], "error": {"code": "scenario_failed", "message": error.to_string()}}));
            Err(error)
        }
    }
}

fn run_component_gallery_window_input_scenario() -> io::Result<()> {
    let result = run_component_gallery_window_input_scenario_inner();
    match result {
        Ok(value) => { println!("{value}"); Ok(()) }
        Err(error) => {
            println!("{}", json!({"scenario": "component-gallery-window-input", "status": "failed", "steps": [], "error": {"code": "scenario_failed", "message": error.to_string()}}));
            Err(error)
        }
    }
}

fn run_component_gallery_window_input_scenario_inner() -> io::Result<serde_json::Value> {
    const SCENARIO: &str = "component-gallery-window-input";
    const FRAGMENT_ID: &str = "component-gallery-window-input";
    // These are stable logical points in the declared 1280x680 gallery surface.
    const LEFT_CHECKBOX: (f64, f64) = (48.0, 70.0);
    const RIGHT_OWNER_CELL: (f64, f64) = (900.0, 54.0);
    let workspace = workspace_root()?;
    let wgpu_endpoint = reserve_loopback_endpoint()?;
    let ui_endpoint = reserve_loopback_endpoint()?;
    let domain_endpoint = reserve_loopback_endpoint()?;
    let job = ProcessJob::new()?;
    let mut children = ChildSession::default();
    let wgpu = spawn_service(executable(&workspace, "neon-wgpu-runtime"), &["--window-server", &wgpu_endpoint.to_string(), &ui_endpoint.to_string()], false)?;
    job.assign(&wgpu)?; children.push(wgpu); wait_for_endpoint(wgpu_endpoint)?;
    wait_for_window_gpu(wgpu_endpoint)?;
    let domain = spawn_service(executable(&workspace, "component_gallery_domain_controller"), &[&domain_endpoint.to_string()], false)?;
    job.assign(&domain)?; children.push(domain); wait_for_endpoint(domain_endpoint)?;
    let ui = spawn_service(executable(&workspace, "neon-ui-runtime"), &["--forward-server", &ui_endpoint.to_string(), &wgpu_endpoint.to_string(), &domain_endpoint.to_string()], false)?;
    job.assign(&ui)?; children.push(ui); wait_for_endpoint(ui_endpoint)?;

    let (document, _) = neon_ui_runtime::demo_domain::component_gallery_program().map_err(io::Error::other)?;
    let effects = lower_nui_flow_effects(&document);
    let fragment = UiFragment { fragment_id: UiFragmentId(FRAGMENT_ID.into()), revision: Revision(1), root: document.ir.root, effects };
    assert_accepted(call(ui_endpoint, &rpc_request("window-input-submit", "ui-runtime", "ui.fragment.submit", json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }), None, Some("window-input-submit")))?, "window Gallery fragment submission")?;

    let mut steps = Vec::new();
    for (index, (control, point, expected_path)) in [
        ("left-checkbox", LEFT_CHECKBOX, "feature-toggle"),
        ("right-owner-cell", RIGHT_OWNER_CELL, "asset-grid/data-grid-row-virtual-row-0/cell-owner"),
    ].into_iter().enumerate() {
        let before = window_graph_revision(wgpu_endpoint)?;
        let probe = call(wgpu_endpoint, &rpc_request(&format!("window-input-probe-{index}"), "wgpu-runtime", "debug.window.input.probe", json!({"logical_position": {"x": point.0, "y": point.1}}), None, None))?;
        assert_accepted(probe.clone(), "logical input probe")?;
        let path = probe.result.as_ref().and_then(|value| value.pointer("/fallback_hit/semantic_node_path")).and_then(serde_json::Value::as_str).unwrap_or_default();
        if !path.ends_with(expected_path) { return Err(io::Error::other(format!("{control} probe resolved '{path}', expected declared path ending '{expected_path}'"))); }
        let activation = call(wgpu_endpoint, &rpc_request(&format!("window-input-activate-{index}"), "wgpu-runtime", "debug.window.input.activate", json!({"logical_position": {"x": point.0, "y": point.1}}), None, None))?;
        assert_accepted(activation.clone(), "debug semantic activation")?;
        let delivery = wait_for_pointer_delivery(wgpu_endpoint, Duration::from_secs(5))?;
        if delivery.get("state").and_then(serde_json::Value::as_str) != Some("accepted") { return Err(io::Error::other(format!("{control} pointer delivery did not receive UI host acceptance: {delivery}"))); }
        let after = window_graph_revision(wgpu_endpoint)?;
        if after <= before { return Err(io::Error::other(format!("{control} did not advance the WGPU composition revision ({before} -> {after})"))); }
        steps.push(json!({"control": control, "logical_point": {"x": point.0, "y": point.1}, "probe_path": path, "pointer_delivery": delivery, "composition_revision": {"before": before, "after": after}}));
    }
    Ok(json!({"scenario": SCENARIO, "status": "passed", "acceptance_level": "composition-ready", "steps": steps}))
}

fn window_graph_revision(endpoint: SocketAddr) -> io::Result<u64> {
    let response = call(endpoint, &rpc_request("window-input-diagnostics", "wgpu-runtime", "wgpu.render.diagnostics", json!({}), None, None))?;
    assert_accepted(response.clone(), "WGPU diagnostics")?;
    response.result.and_then(|value| value.get("graph_revision").and_then(serde_json::Value::as_u64)).ok_or_else(|| io::Error::other("WGPU diagnostics omitted graph_revision"))
}

fn wait_for_pointer_delivery(endpoint: SocketAddr, timeout: Duration) -> io::Result<serde_json::Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let response = call(endpoint, &rpc_request("window-input-snapshot", "wgpu-runtime", "debug.window.input.snapshot", json!({}), None, None))?;
        assert_accepted(response.clone(), "window input snapshot")?;
        let delivery = response.result.and_then(|value| value.get("pointer_delivery").cloned()).unwrap_or_else(|| json!({"state": "missing"}));
        match delivery.get("state").and_then(serde_json::Value::as_str) {
            Some("accepted") | Some("rejected") | Some("transport_failed") | Some("not_sent") => return Ok(delivery),
            _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => return Err(io::Error::new(io::ErrorKind::TimedOut, "UI host did not respond to pointer delivery")),
        }
    }
}

fn wait_for_window_gpu(endpoint: SocketAddr) -> io::Result<()> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let response = call(endpoint, &rpc_request("window-input-ready", "wgpu-runtime", "debug.window.input.snapshot", json!({}), None, None));
        if let Ok(response) = response
            && response.status == RpcStatus::Accepted
            && response.result.as_ref().and_then(|value| value.get("state")).and_then(serde_json::Value::as_str) != Some("uninitialized") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "window compositor did not initialize"));
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
            "window-input-inspect",
            "wgpu-runtime",
            "debug.window.input.snapshot",
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

fn session_manifest(case: &str, wgpu_endpoint: SocketAddr, ui_endpoint: SocketAddr, domain_endpoint: SocketAddr) -> serde_json::Value {
    json!({
        "kind": "neon3.session",
        "version": 1,
        "case": case,
        "services": [
            {"name": "wgpu-runtime", "endpoint": wgpu_endpoint.to_string()},
            {"name": "ui-runtime", "endpoint": ui_endpoint.to_string()},
            {"name": "demo-domain", "endpoint": domain_endpoint.to_string()},
        ],
        "debug": {
            "snapshot": {"endpoint": wgpu_endpoint.to_string(), "method": "debug.snapshot.get"},
            "interaction_get": {"endpoint": wgpu_endpoint.to_string(), "method": "debug.interaction.get", "params": {"interaction_id": "<interaction-id>"}},
            "interaction_query": {"endpoint": wgpu_endpoint.to_string(), "method": "debug.interaction.query", "params": {"filters": {}, "limit": 100}},
        },
    })
}

fn probe_window_input(endpoint: &str, x: &str, y: &str) -> io::Result<()> {
    let endpoint = endpoint.parse::<SocketAddr>().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid WGPU loopback endpoint: {error}")))?;
    let x = x.parse::<f64>().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid x coordinate: {error}")))?;
    let y = y.parse::<f64>().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid y coordinate: {error}")))?;
    if !x.is_finite() || !y.is_finite() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "coordinates must be finite")); }
    let response = call(endpoint, &rpc_request("window-input-probe", "wgpu-runtime", "debug.window.input.probe", json!({"logical_position": {"x": x, "y": y}}), None, None))?;
    println!("{}", json!({"status": if response.status == RpcStatus::Accepted { "passed" } else { "rejected" }, "probe": response.result, "error": response.error}));
    if response.status == RpcStatus::Accepted { Ok(()) } else { Err(io::Error::other("window input probe was rejected")) }
}

fn run_component_gallery_scenario_inner() -> io::Result<serde_json::Value> {
    const SCENARIO: &str = "component-gallery-interactions";
    const FRAGMENT_ID: &str = "component-gallery-scenario";
    let workspace = workspace_root()?;
    let wgpu_endpoint = reserve_loopback_endpoint()?;
    let ui_endpoint = reserve_loopback_endpoint()?;
    let domain_endpoint = reserve_loopback_endpoint()?;
    let job = ProcessJob::new()?;
    let mut children = ChildSession::default();

    let wgpu = spawn_service(executable(&workspace, "neon-wgpu-runtime"), &["--headless-server", &wgpu_endpoint.to_string()], false)?;
    job.assign(&wgpu)?; children.push(wgpu); wait_for_endpoint(wgpu_endpoint)?;
    let domain = spawn_service(executable(&workspace, "component_gallery_domain_controller"), &[&domain_endpoint.to_string()], false)?;
    job.assign(&domain)?; children.push(domain); wait_for_endpoint(domain_endpoint)?;
    let ui = spawn_service(executable(&workspace, "neon-ui-runtime"), &["--forward-server", &ui_endpoint.to_string(), &wgpu_endpoint.to_string(), &domain_endpoint.to_string()], false)?;
    job.assign(&ui)?; children.push(ui); wait_for_endpoint(ui_endpoint)?;

    let (document, program) = neon_ui_runtime::demo_domain::component_gallery_program().map_err(io::Error::other)?;
    let mut fragment = UiFragment { fragment_id: UiFragmentId(FRAGMENT_ID.into()), revision: Revision(1), root: document.ir.root.clone(), effects: lower_nui_flow_effects(&document) };
    let submit = rpc_request("gallery-submit-1", "ui-runtime", "ui.fragment.submit", json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment.clone()) }), None, Some("gallery-submit-1"));
    let response = call(ui_endpoint, &submit)?;
    assert_accepted(response, "initial Gallery fragment submission")?;

    let controls = [
        ("feature-toggle", "Checkbox"), ("mode-radio", "RadioButton"),
        ("exposure-slider", "Slider"), ("count-drag", "DragValue"),
        ("mode-combo", "Combo"), ("mode-dropdown", "Dropdown"),
        ("item-selectable", "Selectable"), ("item-list", "ListBox"),
        ("gallery-scroll", "Scrollbar"),
    ];
    let mut steps: Vec<serde_json::Value> = Vec::new();
    for (index, (node_key, control)) in controls.into_iter().enumerate() {
        let declaration = program.event_records.iter().find(|event| event.node_key == node_key)
            .ok_or_else(|| io::Error::other(format!("{control} declaration missing for {node_key}")))?;
        let intent = fragment.effects.iter().find_map(|effect| match effect {
            UiEffect::SemanticIntent { intent } | UiEffect::BoundSemanticIntent { intent, .. }
                if matches!(intent, UiIntent::Invoke { action, .. } if action == &declaration.intent) => Some(intent.clone()),
            _ => None,
        }).ok_or_else(|| io::Error::other(format!("{control} semantic binding is missing")))?;
        let domain_inputs = if index == 0 {
            initial_inputs(&document.input_schema, &program.revision)?
        } else {
            let previous = steps.last().ok_or_else(|| io::Error::other("missing previous Gallery step"))?;
            serde_json::from_value(previous["domain_snapshot"]["inputs"].clone()).map_err(|e| io::Error::other(e.to_string()))?
        };
        let payload = declaration.bound_input_keys.iter().map(|key| {
            domain_inputs.values.get(key)
                    .ok_or_else(|| io::Error::other(format!("missing input {key}")))
                    .map(|value| (key.clone(), input_payload(value)))
        }).collect::<io::Result<std::collections::BTreeMap<_, _>>>()?;
        let kind = if matches!(node_key, "feature-toggle" | "mode-radio" | "mode-combo" | "mode-dropdown" | "item-selectable" | "item-list") { UiProgramSemanticEventKind::SelectionChanged }
            else { UiProgramSemanticEventKind::ValueCommit };
        let event = UiProgramSemanticEvent {
            event_id: format!("gallery-event-{index}"), kind, intent: declaration.intent.clone(),
            source_node_key: node_key.into(), payload, program_revision: program.revision.clone(),
            input_revision: domain_inputs.input_revision, request_id: format!("gallery-request-{index}"),
            idempotency_key: format!("gallery-key-{index}"), requested_value: None, interaction: UiSemanticInteractionMetadata { interaction_id: format!("gallery-interaction-{index}"), sequence: index as u64 + 1, renderer_epoch: 1 },
        };
        let hit_request = rpc_request(&format!("gallery-hit-{index}"), "wgpu-runtime", "test.ui.hit_sample.request", json!({"pointer_id": 7, "sequence": index as u64 + 1}), None, None);
        let hit = call(wgpu_endpoint, &hit_request)?;
        assert_accepted(hit, "Gallery hit sample")?;
        let hit_complete = rpc_request(&format!("gallery-hit-complete-{index}"), "wgpu-runtime", "test.ui.hit_sample.complete", json!({"pointer_id": 7, "test_hit_id": 0}), None, None);
        assert_accepted(call(wgpu_endpoint, &hit_complete)?, "Gallery hit completion")?;
        let legacy_event = UiSemanticEvent {
            event: match kind {
                UiProgramSemanticEventKind::ValueTentative => UiSemanticEventType::ValuePreview,
                UiProgramSemanticEventKind::SelectionChanged => UiSemanticEventType::SelectionChanged,
                _ => UiSemanticEventType::ValueCommit,
            },
            event_id: format!("gallery-ui-event-{index}"), renderer_epoch: 1,
            composition_revision: Revision(1),
            fragment: UiFragmentRevision { id: fragment.fragment_id.clone(), revision: fragment.revision },
            intent, pointer: None, focus: None, data_grid_cell: None, text: None, control_value: None, drag_drop: None,
        };
        let ui_validation = call(ui_endpoint, &rpc_request(&format!("gallery-ui-validation-{index}"), "ui-runtime", "ui.input.event", json!(legacy_event), Some(fragment.revision), Some(&format!("gallery-ui-key-{index}"))) )?;
        assert_accepted(ui_validation.clone(), "UiRuntime Gallery event validation")?;
        let response = call(domain_endpoint, &rpc_request(&format!("gallery-event-rpc-{index}"), "demo-domain", "ui.program.event", json!(event), Some(domain_inputs.input_revision), Some(&format!("gallery-key-{index}"))) )?;
        if response.status != RpcStatus::Accepted {
            return Err(io::Error::other(format!("{control} rejected: {:?}", response.error)));
        }
        let result = response.result.clone().ok_or_else(|| io::Error::other("domain accepted without result"))?;
        let validation = result["validation"].clone();
        if validation["status"] != "accepted" { return Err(io::Error::other(format!("{control} semantic validation failed: {validation}"))); }
        let domain_snapshot: neon_ui_runtime::demo_domain::DemoInputDomainSnapshot = serde_json::from_value(result["snapshot"].clone()).map_err(|e| io::Error::other(e.to_string()))?;
        fragment.revision = Revision(index as u64 + 2);
        neon_ui_runtime::demo_domain::apply_visible_status_to_fragment(&mut fragment, &domain_snapshot);
        let submit = rpc_request(&format!("gallery-submit-{}", index + 2), "ui-runtime", "ui.fragment.submit", json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment.clone()) }), Some(Revision(index as u64 + 1)), Some(&format!("gallery-submit-{}", index + 2)));
        assert_accepted(call(ui_endpoint, &submit)?, "Gallery visible status submission")?;
        let visible = call(wgpu_endpoint, &rpc_request(&format!("gallery-visible-{index}"), "wgpu-runtime", "wgpu.ui.fragment.snapshot", json!({"fragment_id": FRAGMENT_ID}), None, None))?;
        assert_accepted(visible.clone(), "Gallery visible snapshot")?;
        let accepted_fragment: UiFragment = serde_json::from_value(visible.result.ok_or_else(|| io::Error::other("visible snapshot has no result"))?["fragment"].clone()).map_err(|e| io::Error::other(e.to_string()))?;
        let status_key = format!("status-{}", declaration.bound_input_keys[0]);
        let status_text = find_node(&accepted_fragment.root, &status_key).and_then(first_literal).ok_or_else(|| io::Error::other(format!("{control} visible status is missing")))?;
        let input_key = &declaration.bound_input_keys[0];
        steps.push(json!({"step": index + 1, "control": control, "node_key": node_key, "status": "passed", "hit": {"status": "accepted", "pointer_id": 7}, "binding": {"status": "validated", "input_keys": declaration.bound_input_keys}, "event": {"ui_runtime_status": ui_validation.status, "domain_status": validation["status"], "request_id": event.request_id, "error": serde_json::Value::Null}, "input_revision": domain_snapshot.inputs.input_revision, "value": domain_snapshot.inputs.values[input_key].value, "visible_status": {"node": status_key, "text": status_text}, "domain_snapshot": domain_snapshot}));
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

fn rpc_request(id: &str, target: &str, method: &str, params: serde_json::Value, expected_revision: Option<Revision>, idempotency_key: Option<&str>) -> RpcRequest {
    RpcRequest { protocol: "neon3.rpc".into(), version: ProtocolVersion { major: 1, minor: 0 }, request_id: RequestId(id.into()), client: scenario_client(), target: ServiceName(target.into()), method: method.into(), params, expected_revision, idempotency_key: idempotency_key.map(str::to_owned) }
}

fn initial_inputs(schema: &neon_ui_schema::UiInputSchema, revision: &neon_ui_schema::UiProgramRevision) -> io::Result<neon_ui_schema::UiResolvedInputs> {
    neon_ui_runtime::UiInputStore::activate(revision.clone(), schema.clone()).map(|store| store.snapshot()).map_err(|error| io::Error::other(error.message))
}

fn input_payload(value: &neon_ui_schema::UiResolvedInputValue) -> neon_ui_schema::UiSemanticPayloadValue {
    match &value.value {
        UiInputValue::Bool { value } => UiSemanticPayloadValue::Bool { value: *value },
        UiInputValue::I32 { value } => UiSemanticPayloadValue::I32 { value: *value },
        UiInputValue::F32 { value } => UiSemanticPayloadValue::F32 { value: *value },
        UiInputValue::Enum { value } => UiSemanticPayloadValue::Enum { value: value.clone() },
        _ => panic!("Gallery fixture contains an unsupported semantic input kind"),
    }
}

fn first_literal(node: &neon_ui_schema::UiNode) -> Option<String> {
    match &node.text { Some(neon_ui_schema::TextRef::Literal { value }) => Some(value.clone()), _ => node.children.iter().find_map(first_literal) }
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
    if accepted.revision != Revision(2)
        || find_node(&accepted.root, "backlog-card-02").is_some()
        || representation.node_id.0 != "progress-template-backlog-card-02-r2-progress-template"
    {
        return Err(io::Error::other("accepted fragment did not preserve the requested source, target, placement, and template semantics"));
    }
    println!(
        "{}",
        json!({
            "scenario": SCENARIO, "status": "passed", "revision": accepted.revision.0,
            "source": "backlog-card-02", "target": "in-progress-panel",
            "placement": "before", "template": "progress-template",
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
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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
}

impl Drop for ChildSession {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_manifest_exposes_only_live_service_and_read_only_debug_endpoints() {
        let manifest = session_manifest(
            "component-gallery",
            "127.0.0.1:4101".parse().unwrap(),
            "127.0.0.1:4102".parse().unwrap(),
            "127.0.0.1:4103".parse().unwrap(),
        );
        assert_eq!(manifest["kind"], "neon3.session");
        assert_eq!(manifest["services"].as_array().unwrap().len(), 3);
        assert_eq!(manifest["debug"]["interaction_get"]["method"], "debug.interaction.get");
        assert!(manifest.to_string().contains("127.0.0.1:4101"));
    }
}
