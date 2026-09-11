//! Component showcase probe.
//!
//! Loads cases/component-showcase/showcase.nui, parses it, lowers it to a
//! UiFragment, and submits it to a persistent neon-wgpu-runtime window.
//! Used to visually verify all new components (skins, bindings, Splitter,
//! ContextMenu, TreeView, scrollable containers).

use std::{
    net::SocketAddr,
    process::{Child, Command},
    thread,
    time::Duration,
};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcStatus,
    ServiceName,
};
use neon_ui_schema::{UiCommand, UiEffect, UiFragment, UiFragmentId, UiFragmentSubmission, UiNode};
use neon_ui_runtime::nui_flow::{lower_nui_flow, lower_nui_flow_effects, parse_nui_flow};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39254";
const NUI_PATH: &str = r"D:\Neon3\cases\component-showcase\showcase.nui";

fn request(method: &str, sequence: u64, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("showcase-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "showcase-probe".into(),
            pid: std::process::id(),
            origin: "showcase-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("showcase-{sequence}")),
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

fn load_nui_fragment() -> Result<(UiNode, Vec<UiEffect>), String> {
    let source = std::fs::read_to_string(NUI_PATH)
        .map_err(|error| format!("read nui failed: {error}"))?;
    let document = parse_nui_flow(&source)
        .map_err(|error| format!("parse nui failed: {error:?}"))?;
    let ir = lower_nui_flow(&document);
    let effects = lower_nui_flow_effects(&document);
    Ok((ir.root, effects))
}

fn main() -> Result<(), String> {
    println!("=== Component Showcase Probe ===");
    println!("Loading NUI from: {NUI_PATH}");

    let (root, effects) = load_nui_fragment()?;
    println!(
        "Parsed OK: root={}, children={}, effects={}",
        root.node_id.0,
        root.children.len(),
        effects.len()
    );

    println!("Launching neon-wgpu-runtime...");
    let mut child = launch().map_err(|error| format!("launch failed: {error}"))?;
    thread::sleep(Duration::from_secs(2));

    let endpoint: SocketAddr = ENDPOINT.parse().unwrap();

    // Force surface to None so it renders as a normal screen UI container
    let mut root = root;
    root.surface = None;
    println!("Root node: kind={:?}, bounds={:?}, style.bg={:?}, surface={:?}, children={}",
        root.kind, root.bounds, root.style.background_color, root.surface, root.children.len());

    // Submit fragment directly (like grid_pulse_probe) - build AFTER modifying root
    let fragment = UiFragment {
        fragment_id: UiFragmentId("showcase".into()),
        revision: Revision(1),
        root: root.clone(),
        effects: effects.clone(),
    };
    let command = UiCommand::SubmitFragment {
        submission: UiFragmentSubmission::new(fragment),
    };
    call(endpoint, "wgpu.ui.submit_fragment", 1, serde_json::to_value(&command).unwrap())?;
    println!("Fragment submitted: showcase (wgpu.ui.submit_fragment)");

    println!("\nWindow is now displaying the component showcase.");
    println!("Press Ctrl+C to exit (window will close).");
    println!("\nComponents visible:");
    println!("  - 9 skinned components (Button/Panel/Slider/Scrollbar/ProgressBar/Checkbox/RadioButton/TextInput/Tooltip)");
    println!("  - 6 property bindings (opacity/scroll_offset/checked/value/numeric/scroll)");
    println!("  - 3 new components (Splitter/ContextMenu/TreeView)");
    println!("  - Scrollable container with 10 items");
    println!("  - Right-click support (context_menu_requested)");

    // Capture a screenshot after 3 seconds to verify rendering
    thread::sleep(Duration::from_secs(3));
    std::fs::create_dir_all(r"D:\Neon3\shots").ok();
    match call(endpoint, "wgpu.render.target.capture", 999, json!({"target":"ui.color.v1", "path": r"D:\Neon3\shots\showcase-probe.png", "redraw": true})) {
        Ok(resp) => println!("Capture result: {}", resp),
        Err(e) => println!("Capture failed: {}", e),
    }

    // Keep the UI alive by periodically resubmitting the fragment
    let mut revision = 2u64;
    loop {
        thread::sleep(Duration::from_millis(500));
        if child.try_wait().ok().flatten().is_some() {
            println!("Runtime exited unexpectedly");
            break;
        }
        let fragment = UiFragment {
            fragment_id: UiFragmentId("showcase".into()),
            revision: Revision(revision),
            root: root.clone(),
            effects: effects.clone(),
        };
        let command = UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment),
        };
        if let Err(error) = call(endpoint, "wgpu.ui.submit_fragment", revision, serde_json::to_value(&command).unwrap()) {
            eprintln!("heartbeat submit failed: {error}");
        }
        revision += 1;
    }

    let _ = child.kill();
    Ok(())
}
