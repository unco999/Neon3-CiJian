//! Transport-independent public protocol types belong here.
//! This crate must not create GPU or window objects.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    #[test]
    fn only_the_wgpu_runtime_may_declare_gpu_dependencies() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifests = [
            "neon-protocol",
            "neon-ipc",
            "neon-observability",
            "neon-ui-schema",
            "neon-ui-runtime",
            "neon-cli",
        ];

        for crate_name in manifests {
            let manifest = workspace.join("crates").join(crate_name).join("Cargo.toml");
            let content = fs::read_to_string(&manifest).expect("workspace manifest must exist");
            assert!(
                !content.contains("wgpu") && !content.contains("winit"),
                "{crate_name} must not declare wgpu or winit"
            );
        }
    }
}
