//! Bounded custom-shader package registry for windowed/headless GPU hosts.
//!
//! `wgpu.shader.register` is the only public mutation. The registry stores the
//! validated, immutable `UiShaderPackage` (WGSL source + digest + bounded
//! parameters). Structural validation and the digest check run here so a
//! corrupt or oversized package fails at registration; the real WGSL
//! compile+bind happens later in the renderer with the live device (wgpu
//! parses WGSL eagerly at `create_shader_module`), matching wgpu's lazy
//! pipeline model.
//!
//! The registry never owns a pipeline or a GPU object: pipeline creation and
//! material binding remain with the renderer once a package is adopted. It is
//! deliberately replaceable on device-lost by re-registering the same package.

use neon_ui_schema::UiShaderPackage;
use serde_json::json;

#[derive(Clone, Debug)]
pub struct ShaderPackageEntry {
    pub package: neon_ui_schema::UiShaderPackage,
    pub validated: bool,
    pub compile_error: Option<String>,
}

/// Control-plane registry. `packages` maps package id (stable key) to the last
/// accepted registration. Version replacements are allowed; the calling host is
/// responsible for re-submitting any material-bearing Flow after a bump.
#[derive(Default)]
pub struct ShaderRegistry {
    packages: std::collections::BTreeMap<String, ShaderPackageEntry>,
}

impl ShaderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of registered packages (across all versions).
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Register a package after full structural validation. Re-computes the
    /// source digest and compares it with the request digest so a corrupt
    /// transfer is rejected before the package is cached.
    pub fn register(
        &mut self,
        package: &UiShaderPackage,
    ) -> Result<ShaderPackageEntry, String> {
        if !package.validate().is_ok() {
            return Err("shader package failed budget or parameter validation".into());
        }
        let digest = shader_source_digest(&package.source_bytes);
        if package.source_digest != digest {
            return Err(format!(
                "shader source digest mismatch: expected {}, computed {}",
                package.source_digest, digest
            ));
        }
        if package.entry_point.trim().is_empty() {
            return Err("shader entry point is required".into());
        }
        let entry = ShaderPackageEntry {
            package: package.clone(),
            validated: true,
            compile_error: None,
        };
        self.packages
            .insert(package.package_id.clone(), entry.clone());
        Ok(entry)
    }

    /// Mark a package as compile-failed (or verified) after a renderer-side
    /// WGSL compile attempt with the live device. The entry stays registered
    /// so diagnostics can report the exact failure and the fallback used.
    pub fn record_compile_result(
        &mut self,
        package_id: &str,
        error: Option<String>,
    ) -> Option<&ShaderPackageEntry> {
        if let Some(entry) = self.packages.get_mut(package_id) {
            entry.compile_error = error;
            return Some(entry);
        }
        None
    }

    /// Stable lookup used by diagnostics and material binding.
    pub fn lookup(&self, package_id: &str) -> Option<&ShaderPackageEntry> {
        self.packages.get(package_id)
    }

    /// Immutable packages suitable for transfer to the renderer-owned device
    /// thread. The caller receives source bytes, never a GPU handle.
    pub fn packages(&self) -> Vec<UiShaderPackage> {
        self.packages
            .values()
            .map(|entry| entry.package.clone())
            .collect()
    }

    /// Structured snapshot for `wgpu.shader.state` and `debug.snapshot.get`.
    pub fn snapshot(&self) -> serde_json::Value {
        json!({
            "registry": "ui.shader.package.v1",
            "count": self.packages.len(),
            "packages": self
                .packages
                .values()
                .map(|entry| json!({
                    "package_id": entry.package.package_id,
                    "version": entry.package.version,
                    "entry_point": entry.package.entry_point,
                    "fallback": entry.package.fallback,
                    "source_digest": entry.package.source_digest,
                    "source_bytes": entry.package.source_bytes.len(),
                    "parameters": entry.package.parameters.iter().map(|p| format!("{}:{}", p.key, format_parameter_kind(p.kind))).collect::<Vec<_>>(),
                    "validated": entry.validated,
                    "compile_error": entry.compile_error,
                }))
                .collect::<Vec<_>>(),
        })
    }
}

fn format_parameter_kind(kind: neon_ui_schema::UiShaderParameterKind) -> &'static str {
    match kind {
        neon_ui_schema::UiShaderParameterKind::F32 => "f32",
        neon_ui_schema::UiShaderParameterKind::Vec2 => "vec2",
        neon_ui_schema::UiShaderParameterKind::Vec4 => "vec4",
        neon_ui_schema::UiShaderParameterKind::Color => "color",
    }
}

/// Deterministic FNV-1a 64-bit fingerprint (hex) of the WGSL source. The same
/// helper is used by the frame capture pipeline; shaders are small so collision
/// risk is negligible for a control-plane integrity check.
pub fn shader_source_digest(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(source: &[u8]) -> UiShaderPackage {
        UiShaderPackage {
            package_id: "pulse-glow".into(),
            version: 1,
            source_digest: shader_source_digest(source),
            source_bytes: source.to_vec(),
            entry_point: "material".into(),
            fallback: "standard_ui".into(),
            parameters: Vec::new(),
        }
    }

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        let a = shader_source_digest(b"@fragment fn material() {}");
        let b = shader_source_digest(b"@fragment fn material() {}");
        let c = shader_source_digest(b"@fragment fn material() { return; }");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn register_rejects_digest_mismatch_and_oversize() {
        let mut registry = ShaderRegistry::new();
        let mut bad = package(b"@fragment fn material() -> @location(0) vec4<f32> { return vec4(0.0); }");
        bad.source_digest = "deadbeef".into();
        assert!(registry.register(&bad).is_err());
        let huge = package(&vec![0u8; UiShaderPackage::MAX_SOURCE_BYTES + 1]);
        assert!(registry.register(&huge).is_err());
    }

    #[test]
    fn register_valid_package_and_snapshot() {
        let mut registry = ShaderRegistry::new();
        let source = b"@fragment fn material() -> @location(0) vec4<f32> { return vec4(0.0, 0.0, 0.0, 1.0); }";
        let entry = registry.register(&package(source)).unwrap();
        assert!(entry.validated);
        assert_eq!(registry.len(), 1);
        assert!(registry.lookup("pulse-glow").is_some());
        assert_eq!(registry.snapshot()["count"], 1);
        registry.record_compile_result("pulse-glow", Some("naga: error".into()));
        assert_eq!(
            registry.lookup("pulse-glow").unwrap().compile_error.as_deref(),
            Some("naga: error")
        );
    }
}
