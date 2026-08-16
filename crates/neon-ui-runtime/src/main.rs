//! Headless UI declaration runtime. It must not create windows or GPU objects.

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|argument| argument == "--forward-server") {
        let endpoint = args.get(2).expect("UI runtime endpoint is required").parse().expect("UI runtime endpoint must be a socket address");
        let wgpu_endpoint = args.get(3).expect("WGPU endpoint is required").parse().expect("WGPU endpoint must be a socket address");
        let domain_endpoint = args.get(4).expect("domain endpoint is required").parse().expect("domain endpoint must be a socket address");
        if let Err(error) = neon_ui_runtime::UiRuntime::serve_forwarder(endpoint, wgpu_endpoint, domain_endpoint, 1) {
            eprintln!("neon-ui-runtime failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!("usage: neon-ui-runtime --forward-server <ui-loopback-endpoint> <wgpu-loopback-endpoint> <ui-host-loopback-endpoint>");
    std::process::exit(2);
}
