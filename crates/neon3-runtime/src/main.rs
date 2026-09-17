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

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

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
    let editor_bridge =
        std::sync::Arc::new(neon_ui_runtime::editor_component::EditorBridge::new());

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
            let editor = editor;
            let eventd = eventd;
            std::thread::spawn(move || {
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
            let handle = neon_wgpu_runtime::EditorBridgeHandle {
                input_sink: Some(input_sink),
                external_presentations: Some(editor_bridge.presentations.clone()),
                fragment_observer: Some(fragment_observer),
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
