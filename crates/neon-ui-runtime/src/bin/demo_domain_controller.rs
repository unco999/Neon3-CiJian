//! Runs the generic local drag/drop demo domain endpoint.

fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .expect("domain endpoint is required")
        .parse()
        .expect("domain endpoint must be a socket address");
    if let Err(error) = neon_ui_runtime::demo_domain::DemoDragDropDomain::serve(endpoint) {
        eprintln!("demo domain controller failed: {error}");
        std::process::exit(1);
    }
}
