//! Runs the generic local drag/drop demo domain endpoint.

fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .expect("domain endpoint is required")
        .parse()
        .expect("domain endpoint must be a socket address");
    let component_gallery = std::env::args().any(|argument| argument == "--component-gallery");
    let result = if component_gallery {
        let asset = std::env::args()
            .skip_while(|argument| argument != "--gallery-image")
            .nth(1)
            .expect("component gallery requires --gallery-image <AssetRef>");
        let asset = serde_json::from_str(&asset)
            .expect("gallery image must be a stable AssetRef JSON value");
        neon_ui_runtime::demo_domain::DemoInputDomain::serve_component_gallery(endpoint, asset)
    } else {
        neon_ui_runtime::demo_domain::DemoDragDropDomain::serve(endpoint)
    };
    if let Err(error) = result {
        eprintln!("demo domain controller failed: {error}");
        std::process::exit(1);
    }
}
