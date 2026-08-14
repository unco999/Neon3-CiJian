//! Loopback entry point for the sole project and asset authority.

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|argument| argument == "--server") {
        let endpoint = args
            .get(2)
            .expect("projectd server endpoint is required")
            .parse()
            .expect("projectd server endpoint must be a socket address");
        if let Err(error) = neon_projectd::serve(endpoint, 1) {
            eprintln!("neon-projectd failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!("usage: neon-projectd --server <loopback-endpoint>");
    std::process::exit(2);
}
