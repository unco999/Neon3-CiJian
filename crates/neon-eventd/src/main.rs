//! Loopback entry point for the sole event hub.

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|argument| argument == "--server") {
        let endpoint = args
            .get(2)
            .expect("eventd server endpoint is required")
            .parse()
            .expect("eventd server endpoint must be a socket address");
        let epoch = args
            .get(3)
            .map(|value| value.parse().expect("epoch must be an integer"))
            .unwrap_or(1);
        if let Err(error) = neon_eventd::serve(endpoint, epoch) {
            eprintln!("neon-eventd failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!("usage: neon-eventd --server <loopback-endpoint> [<epoch>]");
    std::process::exit(2);
}
