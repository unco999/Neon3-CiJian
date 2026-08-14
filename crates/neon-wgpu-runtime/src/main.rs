//! The sole Neon3 process permitted to own windows and GPU objects.

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args
        .get(1)
        .is_some_and(|argument| argument == "--headless-server")
    {
        let endpoint = args
            .get(2)
            .expect("headless server endpoint is required")
            .parse()
            .expect("headless server endpoint must be a socket address");
        let server =
            neon_ipc::RpcServer::bind(endpoint).expect("headless server must bind loopback");
        let mut runtime = neon_wgpu_runtime::WgpuRuntime::headless(1);
        server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            (runtime.handle(request), !shutdown)
        }).expect("headless server request must complete");
        return;
    }
    if args.get(1).is_some_and(|argument| argument == "--window") {
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
        if let Err(error) =
            neon_wgpu_runtime::WindowedRuntime::run_server(1, endpoint)
        {
            eprintln!("neon-wgpu-runtime failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!(
        "usage: neon-wgpu-runtime --window | --window-server <loopback-endpoint> | --headless-server <loopback-endpoint>"
    );
    std::process::exit(2);
}
