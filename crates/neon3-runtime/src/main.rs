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

    if windowed {
        let wgpu = wgpu_endpoint;
        let ui = ui_endpoint;
        let eventd = eventd_endpoint;
        let _ = std::thread::spawn(move || {
            if let Err(error) = neon_wgpu_runtime::WindowedRuntime::run_server_with_eventd(
                1,
                wgpu,
                Some(ui),
                None,
                Some(eventd),
                false,
            ) {
                eprintln!("[neon3-runtime] windowed wgpu failed: {error}");
                std::process::exit(1);
            }
        });
    } else {
        // Headless WGPU server on its own endpoint (mirrors the standalone
        // `--headless-server` mode).
        let wgpu = wgpu_endpoint;
        let _ = std::thread::spawn(move || {
            let server = neon_ipc::BlockingRpcServer::bind(wgpu)
                .expect("headless server must bind loopback");
            let runtime = Arc::new(Mutex::new(
                neon_wgpu_runtime::WgpuRuntime::headless(1),
            ));
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
