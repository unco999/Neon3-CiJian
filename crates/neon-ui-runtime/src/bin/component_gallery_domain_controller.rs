//! Runs the controlled component-gallery demo domain endpoint.

fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .expect("domain endpoint is required")
        .parse()
        .expect("domain endpoint must be a socket address");
    let asset = std::env::args()
        .nth(2)
        .expect("gallery image AssetRef JSON is required");
    let asset =
        serde_json::from_str(&asset).expect("gallery image must be a stable AssetRef JSON value");
    if let Err(error) =
        neon_ui_runtime::demo_domain::DemoInputDomain::serve_component_gallery(endpoint, asset)
    {
        eprintln!("component gallery domain controller failed: {error}");
        std::process::exit(1);
    }
}
