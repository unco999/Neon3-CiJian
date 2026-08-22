//! The sole Neon3 process permitted to own windows and GPU objects.

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args
        .get(1)
        .is_some_and(|argument| argument == "--headless-server")
    {
        if args
            .iter()
            .any(|argument| argument == "--enable-world-ui-lab-camera")
        {
            eprintln!("--enable-world-ui-lab-camera is accepted only with --window-server");
            std::process::exit(2);
        }
        let endpoint = args
            .get(2)
            .expect("headless server endpoint is required")
            .parse()
            .expect("headless server endpoint must be a socket address");
        let server = neon_ipc::BlockingRpcServer::bind(endpoint)
            .expect("headless server must bind loopback");
        let runtime = std::sync::Arc::new(std::sync::Mutex::new(
            neon_wgpu_runtime::WgpuRuntime::headless(1),
        ));
        let handler = move |request| {
            let mut guard = runtime.lock().expect("runtime lock");
            guard.handle(request)
        };
        server
            .serve_until(handler, |request| request.method == "service.shutdown")
            .expect("headless server request must complete");
        return;
    }
    if args.get(1).is_some_and(|argument| argument == "--window") {
        if args
            .iter()
            .any(|argument| argument == "--enable-world-ui-lab-camera")
        {
            eprintln!("--enable-world-ui-lab-camera is accepted only with --window-server");
            std::process::exit(2);
        }
        if let Err(error) = neon_wgpu_runtime::WindowedRuntime::run(1) {
            eprintln!("neon-wgpu-runtime failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args
        .get(1)
        .is_some_and(|argument| argument == "--window-server")
    {
        let endpoint = args
            .get(2)
            .expect("window server endpoint is required")
            .parse()
            .expect("window server endpoint must be a socket address");
        let ui_endpoint = args.get(3).map(|endpoint| {
            endpoint
                .parse()
                .expect("UI runtime endpoint must be a socket address")
        });
        let projectd_endpoint = args
            .get(4)
            .filter(|argument| !argument.starts_with("--"))
            .map(|endpoint| {
                endpoint
                    .parse()
                    .expect("projectd endpoint must be a socket address")
            });
        let enable_world_ui_lab_camera = args
            .iter()
            .any(|argument| argument == "--enable-world-ui-lab-camera");
        if let Err(error) = neon_wgpu_runtime::WindowedRuntime::run_server(
            1,
            endpoint,
            ui_endpoint,
            projectd_endpoint,
            enable_world_ui_lab_camera,
        ) {
            eprintln!("neon-wgpu-runtime failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!(
        "usage: neon-wgpu-runtime --window | --window-server <loopback-endpoint> [ui-runtime-endpoint] [projectd-endpoint] [--enable-world-ui-lab-camera] | --headless-server <loopback-endpoint>"
    );
    std::process::exit(2);
}
