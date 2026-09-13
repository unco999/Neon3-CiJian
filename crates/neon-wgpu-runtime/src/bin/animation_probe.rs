//! Animation showcase probe - 动效综合测试案例
//!
//! 验证: enter_transition 驱动的 bounds/opacity/color/position 插值动画
//! 每个动画节点都有 enter_transition，样式变化时自动触发插值。

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
    UiCommand, UiEffect, UiEasing, UiFragment, UiFragmentId, UiFragmentSubmission, UiNode,
    UiNodeId, UiSemanticEvent, UiTransition, UiTransitionState,
};
use neon_ui_runtime::nui_flow::{lower_nui_flow, lower_nui_flow_effects, parse_nui_flow};
use serde_json::json;

const ENDPOINT: &str = "127.0.0.1:39254";
const UI_ENDPOINT: &str = "127.0.0.1:39255";
const NUI_PATH: &str = r"D:\Neon3\cases\animation-showcase\animation.nui";

fn request(method: &str, sequence: u64, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("anim-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "animation-probe".into(),
            pid: std::process::id(),
            origin: "animation-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: method.into(),
        params,
        expected_revision: Some(Revision(0)),
        idempotency_key: Some(format!("anim-{sequence}")),
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
    Ok(response.result.unwrap_or_default())
}

fn launch() -> Result<Child, String> {
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("no exe dir")?
        .join("neon-wgpu-runtime.exe");
    let log_file = std::fs::File::create("D:\\Neon3\\runtime_log.txt")
        .map_err(|e| format!("create log file failed: {e}"))?;
    Command::new(&exe)
        .args(["--window-server", ENDPOINT, UI_ENDPOINT])
        .stdout(std::process::Stdio::from(log_file.try_clone().unwrap()))
        .stderr(std::process::Stdio::from(log_file))
        .spawn()
        .map_err(|e| format!("spawn runtime failed: {e}"))
}

fn make_transition(duration_ms: u32, easing: UiEasing) -> UiTransition {
    UiTransition {
        delay_ms: 0,
        duration_ms,
        easing,
        from: UiTransitionState::default(),
        motion_key: None,
    }
}

/// 动画状态
struct AnimState {
    panel_a_expanded: bool,
    panel_b_visible: bool,
    panel_c_state: u8, // 0=normal, 1=warning, 2=error
    panel_d_right: bool,
    panel_e_expanded: bool,
    panel_f_state: u8, // 0=small, 1=medium, 2=large
}

impl AnimState {
    fn new() -> Self {
        Self {
            panel_a_expanded: false,
            panel_b_visible: true,
            panel_c_state: 0,
            panel_d_right: false,
            panel_e_expanded: false,
            panel_f_state: 0,
        }
    }

    fn handle_action(&mut self, action: &str) {
        match action {
            "anim.a.toggle" => self.panel_a_expanded = !self.panel_a_expanded,
            "anim.a.collapse" => self.panel_a_expanded = false,
            "anim.b.toggle" => self.panel_b_visible = !self.panel_b_visible,
            "anim.b.show" => self.panel_b_visible = true,
            "anim.c.normal" => self.panel_c_state = 0,
            "anim.c.warn" => self.panel_c_state = 1,
            "anim.c.error" => self.panel_c_state = 2,
            "anim.c.next" => self.panel_c_state = (self.panel_c_state + 1) % 3,
            "anim.d.toggle" => self.panel_d_right = !self.panel_d_right,
            "anim.d.left" => self.panel_d_right = false,
            "anim.e.toggle" => self.panel_e_expanded = !self.panel_e_expanded,
            "anim.e.collapse" => self.panel_e_expanded = false,
            "anim.f.small" => self.panel_f_state = 0,
            "anim.f.medium" => self.panel_f_state = 1,
            "anim.f.large" => self.panel_f_state = 2,
            "anim.f.next" => self.panel_f_state = (self.panel_f_state + 1) % 3,
            _ => {}
        }
    }

    /// 应用状态到节点树
    fn apply(&self, node: &mut UiNode) {
        match node.node_id.0.as_str() {
            "anim-panel-a" => {
                if self.panel_a_expanded {
                    node.bounds.width = 400.0;
                    node.bounds.height = 200.0;
                } else {
                    node.bounds.width = 200.0;
                    node.bounds.height = 120.0;
                }
            }
            "anim-panel-b" => {
                node.style.opacity = if self.panel_b_visible { 1.0 } else { 0.0 };
            }
            "anim-panel-c" => {
                node.style.background_color = match self.panel_c_state {
                    0 => [0.176, 0.416, 0.310, 1.0],  // #2D6A4F
                    1 => [0.906, 0.435, 0.318, 1.0],  // #E76F51
                    _ => [0.839, 0.157, 0.157, 1.0],  // #D62828
                };
            }
            "anim-panel-d" => {
                if self.panel_d_right {
                    node.bounds.x = 700.0;
                } else {
                    node.bounds.x = 420.0;
                }
            }
            "anim-panel-e" => {
                if self.panel_e_expanded {
                    node.bounds.width = 380.0;
                    node.bounds.height = 200.0;
                } else {
                    node.bounds.width = 200.0;
                    node.bounds.height = 80.0;
                }
            }
            "child-a" => {
                if self.panel_e_expanded {
                    node.bounds.width = 160.0;
                    node.bounds.height = 40.0;
                } else {
                    node.bounds.width = 80.0;
                    node.bounds.height = 30.0;
                }
            }
            "child-b" => {
                if self.panel_e_expanded {
                    node.bounds.x = 600.0;
                    node.bounds.width = 160.0;
                    node.bounds.height = 40.0;
                } else {
                    node.bounds.x = 520.0;
                    node.bounds.width = 80.0;
                    node.bounds.height = 30.0;
                }
            }
            "child-c" => {
                if self.panel_e_expanded {
                    node.bounds.y = 750.0;
                    node.bounds.width = 340.0;
                    node.bounds.height = 80.0;
                    node.style.opacity = 1.0;
                } else {
                    node.bounds.y = 730.0;
                    node.bounds.width = 170.0;
                    node.bounds.height = 20.0;
                    node.style.opacity = 0.0;
                }
            }
            "anim-panel-f" => {
                let (w, h, color): (f32, f32, [f32; 4]) = match self.panel_f_state {
                    0 => (120.0, 120.0, [0.114, 0.208, 0.341, 1.0]),   // #1D3557
                    1 => (200.0, 200.0, [0.271, 0.482, 0.616, 1.0]),   // #457B9D
                    _ => (300.0, 300.0, [0.659, 0.855, 0.863, 1.0]),   // #A8DADC
                };
                node.bounds.width = w;
                node.bounds.height = h;
                node.style.background_color = color;
            }
            _ => {}
        }
        for child in &mut node.children {
            self.apply(child);
        }
    }
}

/// 给动画节点设置 enter_transition
fn apply_transitions(node: &mut UiNode) {
    match node.node_id.0.as_str() {
        "anim-panel-a" => {
            node.enter_transition = Some(make_transition(300, UiEasing::EaseOut));
        }
        "anim-panel-b" => {
            node.enter_transition = Some(make_transition(400, UiEasing::EaseOut));
        }
        "anim-panel-c" => {
            node.enter_transition = Some(make_transition(300, UiEasing::EaseInOut));
        }
        "anim-panel-d" => {
            node.enter_transition = Some(make_transition(350, UiEasing::EaseInOut));
        }
        "anim-panel-e" | "child-a" | "child-b" | "child-c" => {
            node.enter_transition = Some(make_transition(300, UiEasing::EaseOut));
        }
        "anim-panel-f" => {
            node.enter_transition = Some(make_transition(300, UiEasing::EaseOut));
        }
        _ => {}
    }
    for child in &mut node.children {
        apply_transitions(child);
    }
}

fn load_nui_fragment() -> Result<(UiNode, Vec<UiEffect>), String> {
    let source = std::fs::read_to_string(NUI_PATH).map_err(|e| e.to_string())?;
    let document = parse_nui_flow(&source).map_err(|e| format!("NUI parse failed: {e:?}"))?;
    let ir = lower_nui_flow(&document);
    let mut root = ir.root;
    apply_transitions(&mut root);
    let effects = lower_nui_flow_effects(&document);
    Ok((root, effects))
}

fn start_ui_host_server(click_queue: Arc<Mutex<Vec<String>>>) {
    let endpoint: SocketAddr = UI_ENDPOINT.parse().unwrap();
    let server = match RpcServer::bind(endpoint) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("[ui-host] bind failed: {error}");
            return;
        }
    };
    println!("[ui-host] listening on {UI_ENDPOINT}");
    thread::spawn(move || {
        let _ = server.serve_until(move |req: RpcRequest| {
            if req.method == "service.shutdown" {
                return (
                    RpcResponse {
                        request_id: req.request_id.clone(),
                        status: RpcStatus::Accepted,
                        revision: None,
                        result: Some(json!({"state":"accepted"})),
                        error: None,
                        snapshot: None,
                    },
                    false,
                );
            }
            if req.method == "ui.host.inbound" {
                println!("[ui-host] inbound raw: {}", serde_json::to_string(&req.params).unwrap_or_default());
                match serde_json::from_value::<UiSemanticEvent>(req.params.clone()) {
                    Ok(event) => {
                        println!("[ui-host] parsed event, intent={:?}", event.intent);
                        if let neon_ui_schema::UiIntent::Invoke { action, .. } = event.intent {
                            println!("[ui-host] click: {action}");
                            if let Ok(mut q) = click_queue.lock() {
                                q.push(action);
                            }
                        }
                    }
                    Err(e) => println!("[ui-host] parse failed: {e}"),
                }
            }
            (
                RpcResponse {
                    request_id: req.request_id.clone(),
                    status: RpcStatus::Accepted,
                    revision: None,
                    result: Some(json!({"state":"accepted"})),
                    error: None,
                    snapshot: None,
                },
                true,
            )
        });
    });
}

fn main() -> Result<(), String> {
    println!("=== Animation Showcase Probe ===");
    let (root, base_effects) = load_nui_fragment()?;
    println!("Parsed: root={}, effects={}", root.node_id.0, base_effects.len());

    let click_queue = Arc::new(Mutex::new(Vec::<String>::new()));
    start_ui_host_server(click_queue.clone());

    let mut child = launch().map_err(|e| format!("launch failed: {e}"))?;
    thread::sleep(Duration::from_secs(3));
    let endpoint: SocketAddr = ENDPOINT.parse().unwrap();

    let mut root = root;
    root.surface = None;

    let mut state = AnimState::new();
    let mut revision = 1u64;

    // Initial submit
    let mut display_root = root.clone();
    state.apply(&mut display_root);
    let fragment = UiFragment {
        fragment_id: UiFragmentId("anim-showcase".into()),
        revision: Revision(revision),
        root: display_root,
        effects: base_effects.clone(),
    };
    call(
        endpoint,
        "wgpu.ui.submit_fragment",
        revision,
        serde_json::to_value(&UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment),
        })
        .unwrap(),
    )?;
    println!("Initial fragment submitted. Click buttons to trigger animations!");

    loop {
        thread::sleep(Duration::from_millis(50));
        if child.try_wait().ok().flatten().is_some() {
            println!("Runtime exited");
            break;
        }

        let clicks: Vec<String> = {
            let mut q = click_queue.lock().unwrap();
            std::mem::take(&mut *q)
        };
        if clicks.is_empty() {
            continue;
        }

        for action in &clicks {
            state.handle_action(action);
        }

        revision += 1;
        let mut display_root = root.clone();
        state.apply(&mut display_root);
        let fragment = UiFragment {
            fragment_id: UiFragmentId("anim-showcase".into()),
            revision: Revision(revision),
            root: display_root,
            effects: base_effects.clone(),
        };
        if let Err(e) = call(
            endpoint,
            "wgpu.ui.submit_fragment",
            revision,
            serde_json::to_value(&UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment),
            })
            .unwrap(),
        ) {
            eprintln!("submit failed: {e}");
        }
        println!("Submitted revision {revision}");
    }

    let _ = child.kill();
    Ok(())
}
