//! Runs the generic local drag/drop demo domain endpoint.

fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .expect("domain endpoint is required")
        .parse()
        .expect("domain endpoint must be a socket address");
    let component_gallery = std::env::args().any(|argument| argument == "--component-gallery");
    let result = if component_gallery {
        neon_ui_runtime::demo_domain::DemoInputDomain::serve_component_gallery(endpoint)
    } else {
        neon_ui_runtime::demo_domain::DemoDragDropDomain::serve(endpoint)
    };
    if let Err(error) = result {
        eprintln!("demo domain controller failed: {error}");
        std::process::exit(1);
    }
}
