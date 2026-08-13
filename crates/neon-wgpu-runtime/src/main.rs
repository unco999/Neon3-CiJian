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
        let request_count: usize = args
            .get(3)
            .expect("headless server request count is required")
            .parse()
            .expect("headless server request count must be a number");
        let server =
            neon_ipc::RpcServer::bind(endpoint).expect("headless server must bind loopback");
        let mut runtime = neon_wgpu_runtime::WgpuRuntime::headless(1);
        for _ in 0..request_count {
            server
                .serve_one(|request| runtime.handle(request))
                .expect("headless server request must complete");
        }
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
        let request_count: usize = args
            .get(3)
            .expect("window server request count is required")
            .parse()
            .expect("window server request count must be a number");
        if let Err(error) =
            neon_wgpu_runtime::WindowedRuntime::run_server(1, endpoint, request_count)
        {
            eprintln!("neon-wgpu-runtime failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!(
        "usage: neon-wgpu-runtime --window | --window-server <loopback-endpoint> <request-count> | --headless-server <loopback-endpoint> <request-count>"
    );
    std::process::exit(2);
}
