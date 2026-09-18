//! Single-binary Neon3 runtime host.
//!
//! Collapses the split runtime executables (`neon-eventd`, `neon-ui-runtime`,
//! `neon-wgpu-runtime`, `neon-editor-runtime`) into one process. Each service
//! keeps the endpoint it had as a standalone binary (eventd 39101, ui 39102,
//! wgpu 39103, editor 39104 by default), so existing SDKs, `cli.py` and tests
//! keep working unchanged.
//!
//! Usage:
//! ```text
//! neon3-runtime serve [--headless | --window] \
//!   [--eventd 127.0.0.1:39101] [--ui 127.0.0.1:39102] \
//!   [--wgpu 127.0.0.1:39103] [--editor 127.0.0.1:39104]
//! ```
//!
//! `--headless` (default) runs the headless WGPU server; `--window` opens the
//! windowed runtime (window server manages the UI endpoint itself).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use neon_editor::ChangeSet;
use neon_ui_runtime::editor_component::{
    EditorDocumentFrame, EditorDocumentProvider,
};
use neon_ui_schema::UiEditorDocumentBinding;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, RpcRequest, RpcStatus,
    ServiceName,
};

enum EditorDocumentRequest {
    Snapshot {
        binding: UiEditorDocumentBinding,
        initial_source: String,
    },
    Change {
        binding: UiEditorDocumentBinding,
        change_set: ChangeSet,
    },
}

struct EditorRuntimeDocumentProvider {
    requests: mpsc::Sender<EditorDocumentRequest>,
    snapshots: Arc<Mutex<HashMap<String, EditorDocumentFrame>>>,
    pending: Arc<Mutex<HashSet<String>>>,
}

impl EditorRuntimeDocumentProvider {
    fn start(endpoint: SocketAddr) -> Arc<Self> {
        let (requests, receiver) = mpsc::channel();
        let snapshots = Arc::new(Mutex::new(HashMap::new()));
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let cache = snapshots.clone();
        let pending_worker = pending.clone();
        std::thread::spawn(move || {
            let mut sequence = 0_u64;
            while let Ok(request) = receiver.recv() {
                sequence = sequence.saturating_add(1);
                let (method, params, document_id, fallback_source) = match request {
                    EditorDocumentRequest::Snapshot {
                        binding,
                        initial_source,
                    } => {
                        let mut binding = binding;
                        if binding.epoch == 0 {
                            binding.epoch = 1;
                        }
                        let document_id = binding.document_id.clone();
                        (
                            "editor.document.snapshot.get",
                            serde_json::json!({
                                "document_id": binding.document_id,
                                "session_id": binding.session_id,
                                "epoch": binding.epoch,
                            }),
                            document_id,
                            Some(initial_source),
                        )
                    }
                    EditorDocumentRequest::Change { binding, change_set } => {
                        let mut binding = binding;
                        if binding.epoch == 0 {
                            binding.epoch = 1;
                        }
                        let document_id = binding.document_id.clone();
                        (
                            "editor.document.change.apply",
                            serde_json::json!({
                                "document_id": binding.document_id,
                                "session_id": binding.session_id,
                                "epoch": binding.epoch,
                                "change_set": change_set,
                                "kind": "commit",
                            }),
                            document_id,
                            None,
                        )
                    }
                };
                let request = RpcRequest {
                    protocol: "neon3.rpc".into(),
                    version: ProtocolVersion { major: 1, minor: 0 },
                    request_id: RequestId(format!("editor-bridge-{sequence}")),
                    client: ClientIdentity {
                        kind: ClientKind::UiRuntime,
                        instance_id: "neon3-runtime-editor-bridge".into(),
                        pid: std::process::id(),
                        origin: "neon3-runtime".into(),
                    },
                    target: ServiceName("editor-runtime".into()),
                    method: method.into(),
                    params,
                    expected_revision: None,
                    idempotency_key: Some(format!("editor-bridge-{sequence}")),
                };
                let mut response = neon_ipc::RpcClient::connect(endpoint)
                    .and_then(|client| client.with_timeout(Duration::from_secs(3)))
                    .and_then(|mut client| client.call(&request));
                if response
                    .as_ref()
                    .is_ok_and(|response| response.status != RpcStatus::Accepted)
                    && method == "editor.document.snapshot.get"
                {
                    if let Some(initial_source) = fallback_source {
                        let open_request = RpcRequest {
                            request_id: RequestId(format!("editor-bridge-open-{sequence}")),
                            method: "editor.document.open".into(),
                            params: serde_json::json!({
                                "document_id": request.params.get("document_id"),
                                "session_id": request.params.get("session_id"),
                                "language": "nui_flow",
                                "source": initial_source,
                            }),
                            idempotency_key: Some(format!("editor-bridge-open-{sequence}")),
                            ..request.clone()
                        };
                        response = neon_ipc::RpcClient::connect(endpoint)
                            .and_then(|client| client.with_timeout(Duration::from_secs(3)))
                            .and_then(|mut client| client.call(&open_request));
                    }
                }
                if let Ok(response) = response
                    && response.status == RpcStatus::Accepted
                {
                    let snapshot = response
                        .result
                        .as_ref()
                        .and_then(|result| result.get("snapshot"))
                        .and_then(|snapshot| {
                            serde_json::from_value::<neon_editor_runtime::EditorDocumentSnapshot>(
                                snapshot.clone(),
                            )
                            .ok()
                        });
                    if let Some(snapshot) = snapshot {
                        if let Ok(mut cache) = cache.lock() {
                            cache.insert(
                                document_id.clone(),
                                EditorDocumentFrame {
                                    binding: UiEditorDocumentBinding {
                                        document_id: snapshot.document_id.clone(),
                                        session_id: snapshot.session_id.clone(),
                                        epoch: snapshot.epoch,
                                        revision: snapshot.revision.0,
                                        committed_revision: snapshot.committed_revision.0,
                                        source_hash: snapshot.source_hash.clone(),
                                        dirty: snapshot.dirty,
                                    },
                                    source: snapshot.source,
                                },
                            );
                        }
                    }
                }
                if let Ok(mut pending) = pending_worker.lock() {
                    pending.remove(&document_id);
                }
            }
        });
        Arc::new(Self {
            requests,
            snapshots,
            pending,
        })
    }
}

impl EditorDocumentProvider for EditorRuntimeDocumentProvider {
    fn request_snapshot(&self, binding: &UiEditorDocumentBinding, initial_source: &str) {
        let Ok(mut pending) = self.pending.lock() else { return };
        if !pending.insert(binding.document_id.clone()) {
            return;
        }
        if self
            .requests
            .send(EditorDocumentRequest::Snapshot {
                binding: binding.clone(),
                initial_source: initial_source.to_owned(),
            })
            .is_err()
        {
            pending.remove(&binding.document_id);
        }
    }

    fn take_snapshot(&self, document_id: &str) -> Option<EditorDocumentFrame> {
        self.snapshots.lock().ok()?.get(document_id).cloned()
    }

    fn submit_change(&self, binding: &UiEditorDocumentBinding, change_set: ChangeSet) {
        let _ = self.requests.send(EditorDocumentRequest::Change {
            binding: binding.clone(),
            change_set,
        });
    }
}

fn parse_addr(args: &[String], flag: &str, default: &str) -> SocketAddr {
    args.iter()
        .position(|argument| argument == flag)
        .and_then(|index| args.get(index + 1))
        .map(|endpoint| {
            endpoint
                .parse()
                .expect("endpoint must be a socket address")
        })
        .unwrap_or_else(|| default.parse().expect("default endpoint is valid"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|argument| argument == "serve") {
        eprintln!(
            "usage: neon3-runtime serve [--headless | --window] \
             [--eventd <addr>] [--ui <addr>] [--wgpu <addr>] [--editor <addr>]"
        );
        std::process::exit(2);
    }

    let windowed = args.iter().any(|argument| argument == "--window");
    if windowed {
        // Windowed case shells are full-viewport transparent compositions;
        // acrylic is the native backdrop and Flow supplies only transparent
        // content layers above it. Explicit environment values still win.
        if std::env::var_os("NEON_WINDOW_BACKDROP").is_none() {
            unsafe { std::env::set_var("NEON_WINDOW_BACKDROP", "acrylic") };
        }
        if std::env::var_os("NEON_WINDOW_MAXIMIZED").is_none() {
            unsafe { std::env::set_var("NEON_WINDOW_MAXIMIZED", "1") };
        }
        if std::env::var_os("NEON_WINDOW_CHROME").is_none() {
            unsafe { std::env::set_var("NEON_WINDOW_CHROME", "borderless") };
        }
    }
    let eventd_endpoint = parse_addr(&args, "--eventd", "127.0.0.1:39101");
    let ui_endpoint = parse_addr(&args, "--ui", "127.0.0.1:39102");
    let wgpu_endpoint = parse_addr(&args, "--wgpu", "127.0.0.1:39103");
    let editor_endpoint = parse_addr(&args, "--editor", "127.0.0.1:39104");

    eprintln!(
        "[neon3-runtime] serve windowed={windowed} eventd={eventd_endpoint} ui={ui_endpoint} wgpu={wgpu_endpoint} editor={editor_endpoint}"
    );

    // Register the built-in tree-sitter syntax providers and default LSP
    // launch configs into the process-wide language registry. The editor
    // kernel, the editor-runtime service and the NUI code_editor bridge all
    // resolve language capabilities through this registry; nothing
    // language-specific is compiled into the kernel itself.
    {
        let mut registry = neon_editor::default_registry();
        neon_languages::register_builtin_languages(&mut registry);
    }

    let eventd_task = {
        let endpoint = eventd_endpoint;
        std::thread::spawn(move || {
            if let Err(error) = neon_eventd::serve(endpoint, 1) {
                eprintln!("[neon3-runtime] eventd failed: {error}");
                std::process::exit(1);
            }
        })
    };

    let editor_task = {
        let endpoint = editor_endpoint;
        std::thread::spawn(move || {
            if let Err(error) = neon_editor_runtime::serve(endpoint, 1) {
                eprintln!("[neon3-runtime] editor-runtime failed: {error}");
                std::process::exit(1);
            }
        })
    };

    // Shared editor bridge: the ui-runtime component registry + presentations
    // slot. Injected into the renderer (input sink + external presentations)
    // and into the fragment path (observer), so the editor core stays fully
    // outside the wgpu renderer while every feature keeps working.
    let document_provider = EditorRuntimeDocumentProvider::start(editor_endpoint);
    let editor_bridge = std::sync::Arc::new(
        neon_ui_runtime::editor_component::EditorBridge::new()
            .with_document_provider(document_provider),
    );

    if windowed {
        // winit 0.30 requires the event loop on the main thread; the windowed
        // WGPU runtime therefore runs here in `main` instead of a spawned
        // thread (eventd/editor keep running on their own threads).
        let wgpu = wgpu_endpoint;
        let ui = ui_endpoint;
        let editor = editor_endpoint;
        let eventd = eventd_endpoint;

        // Start the UI runtime forwarder on the UI endpoint.  It compiles
        // FLOW source, activates the host adapter, and runs the FLOW state
        // machine locally.
        let _ui_task = {
            let ui = ui;
            let wgpu = wgpu;
            // Dead domain endpoint: the forwarder falls back to empty publications
            // when host RPC fails, which is correct for self-contained FLOW apps
            // whose state machines run locally.  Pointing at editor-runtime would
            // make it reject `ui.host.inbound` instead of failing the connect.
            let dead_domain: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
            let eventd = eventd;
            std::thread::spawn(move || {
                if let Err(error) = neon_ui_runtime::UiRuntime::serve_forwarder(
                    ui,
                    wgpu,
                    dead_domain,
                    Some(eventd),
                    1,
                ) {
                    eprintln!("[neon3-runtime] ui-runtime failed: {error}");
                    std::process::exit(1);
                }
            })
        };

        {
            let input_sink: Box<
                dyn FnMut(
                        neon_ui_schema::UiEditorInputEvent,
                        f32,
                    ) -> Vec<neon_wgpu_runtime::EditorCommit>
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
                dyn FnMut(&std::collections::HashMap<
                    neon_ui_schema::UiFragmentId,
                    neon_ui_schema::UiFragment,
                >) + Send,
            > = {
                let bridge = editor_bridge.clone();
                Box::new(move |fragments| bridge.sync_fragments(fragments))
            };
            let reveal_sink: Box<dyn FnMut(serde_json::Value, f32) -> Option<neon_ui_schema::UiCodeEditorPresentation> + Send> = {
                let bridge = editor_bridge.clone();
                Box::new(move |params, now| {
                    let path = params.get("path")?.as_str()?.to_string();
                    bridge.reveal(
                        &path,
                        params.get("line")?.as_u64()? as u32,
                        params.get("column")?.as_u64()? as u32,
                        params.get("end_line").and_then(|v| v.as_u64()).map(|v| v as u32),
                        params.get("end_column").and_then(|v| v.as_u64()).map(|v| v as u32),
                        params.get("viewport_height").and_then(|v| v.as_f64()).unwrap_or(720.0) as f32,
                        params.get("viewport_width").and_then(|v| v.as_f64()).unwrap_or(1200.0) as f32,
                        params.get("row_height").and_then(|v| v.as_f64()).unwrap_or(20.0) as f32,
                        params.get("gutter_width").and_then(|v| v.as_f64()).unwrap_or(56.0) as f32,
                        now,
                    )
                })
            };
            let handle = neon_wgpu_runtime::EditorBridgeHandle {
                input_sink: Some(input_sink),
                reveal_sink: Some(reveal_sink),
                external_presentations: Some(editor_bridge.presentations.clone()),
                fragment_observer: Some(fragment_observer),
                presentation_refresh: Some(Box::new({
                    let bridge = editor_bridge.clone();
                    move || bridge.refresh_provider_snapshots()
                })),
            };
            if let Err(error) = neon_wgpu_runtime::WindowedRuntime::run_server_with_eventd_bridged(
                1,
                wgpu,
                Some(ui),
                None,
                Some(eventd),
                false,
                Some(handle),
            ) {
                eprintln!("[neon3-runtime] windowed wgpu failed: {error}");
                std::process::exit(1);
            }
        }
        return;
    } else {
        // Headless WGPU server on its own endpoint (mirrors the standalone
        // `--headless-server` mode). The editor fragment observer keeps the
        // component registry in sync even without a window.
        let wgpu = wgpu_endpoint;
        let editor_bridge = editor_bridge.clone();
        let _ = std::thread::spawn(move || {
            let server = neon_ipc::BlockingRpcServer::bind(wgpu)
                .expect("headless server must bind loopback");
            let mut runtime = neon_wgpu_runtime::WgpuRuntime::headless(1);
            let bridge = editor_bridge.clone();
            runtime.set_editor_fragment_observer(Some(Box::new(move |fragments| {
                bridge.sync_fragments(fragments)
            })));
            runtime.set_editor_external_presentations(editor_bridge.presentations.clone());
            let bridge = editor_bridge.clone();
            runtime.set_editor_presentation_refresh(Some(Box::new(move || {
                bridge.refresh_provider_snapshots()
            })));
            let runtime = std::sync::Arc::new(std::sync::Mutex::new(runtime));
            let handler = move |request| {
                let mut guard = runtime.lock().expect("runtime lock");
                guard.handle(request)
            };
            server
                .serve_until(handler, |request| request.method == "service.shutdown")
                .expect("headless server request must complete");
        });

        // UI forwarder: accepts UI declarations on its own endpoint and
        // forwards to the wgpu server, with the editor endpoint as the
        // domain service (mirrors the standalone `--forward-server` mode).
        let ui = ui_endpoint;
        let wgpu = wgpu_endpoint;
        let editor = editor_endpoint;
        let eventd = eventd_endpoint;
        let _ = std::thread::spawn(move || {
            if let Err(error) = neon_ui_runtime::UiRuntime::serve_forwarder(
                ui,
                wgpu,
                editor,
                Some(eventd),
                1,
            ) {
                eprintln!("[neon3-runtime] ui-runtime failed: {error}");
                std::process::exit(1);
            }
        });
    }

    // Keep the host alive until the UI forwarder (which owns the longest
    // lifecycle) exits.
    let _ = eventd_task.join();
    let _ = editor_task.join();
}
