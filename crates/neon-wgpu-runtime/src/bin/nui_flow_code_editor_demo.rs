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
        submit_code_editor_fragment(wgpu_endpoint, fragment);
    });

    // 4. Window server: the only window + GPU owner, driven by the public
    //    neon3.rpc protocol. Fragments arrive over `wgpu.ui.submit_fragment`.
    let window_result = neon_wgpu_runtime::WindowedRuntime::run_server(
        1,
        wgpu_endpoint,
        Some(host_endpoint),
        None,
        false,
    );

    let _ = submitter.join();
    let _ = host_thread.join();
    if let Err(error) = window_result {
        eprintln!("window runtime exited with error: {error}");
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
                        _ => "",
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
