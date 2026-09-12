//! Component showcase probe with full interactivity.
//!
//! Loads cases/component-showcase/showcase.nui and runs a persistent window
//! with a minimal UI-host RPC server. All component interactions are wired:
//! Button, Checkbox, Radio, Scroll buttons, ContextMenu, TreeView toggle.

use std::{
    net::SocketAddr,
    process::{Child, Command},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_schema::{
    UiCommand, UiControlPresentation, UiEffect, UiFragment, UiFragmentId, UiFragmentSubmission,
    UiNode, UiNodeId, UiSemanticEvent, TextRef,
};
use neon_ui_runtime::nui_flow::{lower_nui_flow, lower_nui_flow_effects, parse_nui_flow};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39254";
const UI_ENDPOINT: &str = "127.0.0.1:39255";
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
        .args(["--window-server", ENDPOINT, UI_ENDPOINT])
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

// === Application state ===

struct TreeState {
    project_expanded: bool,
    crates_expanded: bool,
}

impl TreeState {
    fn new() -> Self { Self { project_expanded: true, crates_expanded: true } }
    fn toggle_project(&mut self) { self.project_expanded = !self.project_expanded; }
    fn toggle_crates(&mut self) { self.crates_expanded = !self.crates_expanded; }
    fn apply(&self, node: &mut UiNode) {
        match node.node_id.0.as_str() {
            "tree-root" => {
                if let Some(text) = &mut node.text {
                    *text = TextRef::Literal {
                        value: if self.project_expanded { "▼ project/" } else { "▶ project/" }.into(),
                    };
                }
            }
            "tree-crate-1" => {
                node.visible = self.project_expanded;
                if let Some(text) = &mut node.text {
                    *text = TextRef::Literal {
                        value: if self.crates_expanded { "▼ crates/" } else { "▶ crates/" }.into(),
                    };
                }
            }
            "tree-crate-2" | "tree-crate-3" | "tree-crate-4" => {
                node.visible = self.project_expanded && self.crates_expanded;
            }
            "tree-docs" => {
                node.visible = self.project_expanded;
                node.bounds.y = if self.crates_expanded { 148.0 } else { 64.0 };
            }
            "tree-cases" => {
                node.visible = self.project_expanded;
                node.bounds.y = if self.crates_expanded { 176.0 } else { 92.0 };
            }
            _ => {}
        }
        for child in &mut node.children {
            self.apply(child);
        }
    }
}

struct AppState {
    checkbox_state: bool,
    radio_state: bool,
    slider_val: f32,
    scroll_pos: f32,
    progress_val: f32,
    click_count: u32,
    context_menu_visible: bool,
    splitter_mode: u8, // 0=50/50, 1=30/70, 2=70/30
    tree: TreeState,
}

impl AppState {
    fn new() -> Self {
        Self {
            checkbox_state: true,
            radio_state: false,
            slider_val: 42.0,
            scroll_pos: 0.3,
            progress_val: 0.65,
            click_count: 0,
            context_menu_visible: false,
            splitter_mode: 0,
            tree: TreeState::new(),
        }
    }

    /// Build ControlPresentation effects from current state.
    fn presentation_effects(&self) -> Vec<UiEffect> {
        vec![
            UiEffect::ControlPresentation {
                node_id: UiNodeId("check-demo".into()),
                state: UiControlPresentation::Toggle { selected: self.checkbox_state },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("radio-demo".into()),
                state: UiControlPresentation::Toggle { selected: self.radio_state },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("slider-demo".into()),
                state: UiControlPresentation::Numeric { value: self.slider_val, min: 0.0, max: 100.0 },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("progress-demo".into()),
                state: UiControlPresentation::Numeric { value: self.progress_val, min: 0.0, max: 1.0 },
            },
            UiEffect::ControlPresentation {
                node_id: UiNodeId("scroll-demo".into()),
                state: UiControlPresentation::Scroll { position: self.scroll_pos },
            },
        ]
    }

    /// Apply visibility and text changes to the root node.
    fn apply(&self, node: &mut UiNode) {
        self.tree.apply(node);
        // Context menu visibility
        if node.node_id.0 == "demo-context" {
            node.visible = self.context_menu_visible;
        }
        // Button text shows click count
        if node.node_id.0 == "btn-demo" && self.click_count > 0 {
            if let Some(text) = &mut node.text {
                *text = TextRef::Literal {
                    value: format!("Clicked {} times", self.click_count),
                };
            }
        }
        for child in &mut node.children {
            self.apply(child);
        }
    }

    fn handle_action(&mut self, action: &str) {
        match action {
            "demo.button.click" => {
                self.click_count += 1;
                self.progress_val = (self.progress_val + 0.05).min(1.0);
                println!("[button] clicked {} times, progress={:.2}", self.click_count, self.progress_val);
            }
            "demo.check.toggle" => {
                self.checkbox_state = !self.checkbox_state;
                println!("[checkbox] -> {}", self.checkbox_state);
            }
            "demo.radio.toggle" => {
                self.radio_state = !self.radio_state;
                println!("[radio] -> {}", self.radio_state);
            }
            "demo.scroll.up" => {
                self.scroll_pos = (self.scroll_pos - 0.1).max(0.0);
                println!("[scroll] up -> {:.2}", self.scroll_pos);
            }
            "demo.scroll.down" => {
                self.scroll_pos = (self.scroll_pos + 0.1).min(1.0);
                println!("[scroll] down -> {:.2}", self.scroll_pos);
            }
            "demo.ctx.show" => {
                self.context_menu_visible = !self.context_menu_visible;
                println!("[ctx] menu visible={}", self.context_menu_visible);
            }
            "demo.ctx.copy" | "demo.ctx.paste" | "demo.ctx.delete" => {
                println!("[ctx] item: {action}");
                self.context_menu_visible = false;
            }
            "demo.splitter.toggle" => {
                self.splitter_mode = (self.splitter_mode + 1) % 3;
                let labels = ["50/50", "30/70", "70/30"];
                println!("[splitter] mode={}", labels[self.splitter_mode as usize]);
            }
            "demo.tree.root" => { self.tree.toggle_project(); println!("[tree] project={}", self.tree.project_expanded); }
            "demo.tree.crates" => { self.tree.toggle_crates(); println!("[tree] crates={}", self.tree.crates_expanded); }
            "demo.tree.docs" | "demo.tree.cases" | "demo.tree.schema" | "demo.tree.runtime" | "demo.tree.wgpu" => {
                println!("[tree] leaf: {action}");
            }
            _ => println!("[unknown] {action}"),
        }
    }
}

/// Starts a minimal UI-host RPC server that receives semantic events.
fn start_ui_host_server(click_queue: Arc<Mutex<Vec<String>>>) {
    let endpoint: SocketAddr = UI_ENDPOINT.parse().unwrap();
    let server = match RpcServer::bind(endpoint) {
        Ok(server) => server,
        Err(error) => { eprintln!("[ui-host] bind failed: {error}"); return; }
    };
    println!("[ui-host] listening on {UI_ENDPOINT}");
    thread::spawn(move || {
        let _ = server.serve_until(move |req: RpcRequest| {
            if req.method == "service.shutdown" {
                return (RpcResponse {
                    request_id: req.request_id.clone(), status: RpcStatus::Accepted,
                    revision: None, result: Some(json!({"state":"accepted"})),
                    error: None, snapshot: None,
                }, false);
            }
            if req.method == "ui.host.inbound" {
                if let Some(action) = serde_json::from_value::<UiSemanticEvent>(req.params.clone())
                    .ok()
                    .and_then(|event| {
                        let neon_ui_schema::UiIntent::Invoke { action, .. } = event.intent;
                        Some(action)
                    })
                {
                    println!("[ui-host] click: {action}");
                    if let Ok(mut q) = click_queue.lock() { q.push(action); }
                }
            }
            (RpcResponse {
                request_id: req.request_id.clone(), status: RpcStatus::Accepted,
                revision: None, result: Some(json!({"state":"accepted"})),
                error: None, snapshot: None,
            }, true)
        });
    });
}

fn main() -> Result<(), String> {
    println!("=== Component Showcase Probe (interactive) ===");
    let (root, base_effects) = load_nui_fragment()?;
    println!("Parsed: root={}, effects={}", root.node_id.0, base_effects.len());

    let click_queue = Arc::new(Mutex::new(Vec::<String>::new()));
    start_ui_host_server(click_queue.clone());

    let mut child = launch().map_err(|e| format!("launch failed: {e}"))?;
    thread::sleep(Duration::from_secs(2));
    let endpoint: SocketAddr = ENDPOINT.parse().unwrap();

    let mut root = root;
    root.surface = None;

    let mut state = AppState::new();
    let mut revision = 1u64;

    // Initial submit
    let mut display_root = root.clone();
    state.apply(&mut display_root);
    let mut effects = base_effects.clone();
    effects.extend(state.presentation_effects());
    let fragment = UiFragment {
        fragment_id: UiFragmentId("showcase".into()),
        revision: Revision(revision),
        root: display_root,
        effects,
    };
    call(endpoint, "wgpu.ui.submit_fragment", revision,
        serde_json::to_value(&UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }).unwrap())?;
    println!("Initial fragment submitted. Click around!");

    loop {
        thread::sleep(Duration::from_millis(200));
        if child.try_wait().ok().flatten().is_some() {
            println!("Runtime exited"); break;
        }

        let clicks: Vec<String> = {
            let mut q = click_queue.lock().unwrap();
            std::mem::take(&mut *q)
        };
        let mut changed = !clicks.is_empty();
        for action in &clicks {
            state.handle_action(action);
        }

        revision += 1;
        let mut display_root = root.clone();
        state.apply(&mut display_root);
        let mut effects = base_effects.clone();
        effects.extend(state.presentation_effects());
        let fragment = UiFragment {
            fragment_id: UiFragmentId("showcase".into()),
            revision: Revision(revision),
            root: display_root,
            effects,
        };
        if let Err(e) = call(endpoint, "wgpu.ui.submit_fragment", revision,
            serde_json::to_value(&UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }).unwrap())
        {
            eprintln!("heartbeat failed: {e}");
        }
        let _ = changed;
    }

    let _ = child.kill();
    Ok(())
}
