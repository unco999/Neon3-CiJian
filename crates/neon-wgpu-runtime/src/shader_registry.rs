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
    pub fn register(&mut self, package: &UiShaderPackage) -> Result<ShaderPackageEntry, String> {
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

/// Transient type-in: the glyph materializes with a holographic scan band, a
/// cyan energy lift and an inner white flash, then settles into the token color.
/// `fract()` keeps the package self-contained — it is given the renderer's clock,
/// not the fx's start, and the mirror expires it after ~420 ms anyway.
const TYPE_IN_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let t = fract(input.time_seconds * 2.4);
    let appear = smoothstep(0.0, 0.35, t);
    let core = smoothstep(0.08, 0.55, input.coverage);
    // Holographic scan band sweeping top -> bottom once across the glyph box.
    let scan_y = input.local_position.y / max(input.glyph_quad.w, 1.0);
    let scan = smoothstep(0.10, 0.0, abs(scan_y - t));
    // Energy lift: the glyph rises out of the baseline.
    let lift = (1.0 - appear) * 12.0;
    let holo = vec3<f32>(0.35, 1.0, 0.95);
    let base = input.base_color.rgb;
    let energy = (1.0 - appear) * 1.2;
    let rgb = mix(base, holo, 0.55) * (0.72 + 0.55 * scan + energy * 0.5);
    let glow = pow(input.edge_ink, 2.0) * (0.9 + scan * 1.4) * (1.0 - t * 0.4);
    let alpha = clamp(core * appear + glow * 0.55, 0.0, 1.0);
    return vec4<f32>(rgb + holo * glow * 0.9, alpha);
}
"#;

/// Transient delete: the ghost detonates into cyan energy — a leading white
/// flash, a fast exponential boom, per-pixel particle jitter, and a flickering
/// ember tail as it fades. The renderer snaps the ghost at the pre-delete
/// position and expires the fx after ~620 ms.
const DELETE_FRAGMENT_SOURCE: &str = r#"
fn text_material(input: TextMaterialInput) -> vec4<f32> {
    let t = fract(input.time_seconds * 1.65);
    let seed = dot(input.local_position, vec2<f32>(3.7, 11.3)) + 0.31;
    let core = smoothstep(0.08, 0.55, input.coverage);
    // Particle jitter: per-pixel phase grows with t^2 so the glyph shreds.
    let shake = (sin(seed * 7.0 + t * 44.0) + cos(seed * 5.0 - t * 31.0)) * 0.5;
    let jitter = shake * t * t * 4.0;
    // Energy boom: bright cyan flash at t=0, exponential decay.
    let boom = exp(-t * 6.5);
    let holo = vec3<f32>(0.3, 0.95, 1.0);
    let flicker = 0.5 + 0.5 * sin(input.time_seconds * 71.0 + seed * 13.0);
    let base = input.base_color.rgb;
    let rgb = mix(base, holo, 0.45 + 0.45 * boom) * (0.7 + flicker * 0.35 + boom * 1.5);
    let edge = pow(input.edge_ink, 2.0) * boom * 1.3;
    let fade = 1.0 - smoothstep(0.0, 1.0, t);
    let alpha = clamp(core * fade + edge * 0.7 * fade, 0.0, 1.0);
    return vec4<f32>(rgb + holo * edge * 1.1, alpha);
}
"#;

fn text_material_package(package_id: &str, source: &str) -> UiShaderPackage {
    UiShaderPackage {
        package_id: package_id.into(),
        version: 1,
        source_digest: shader_source_digest(source.as_bytes()),
        source_bytes: source.as_bytes().to_vec(),
        entry_point: "text_material".into(),
        fallback: "standard_text".into(),
        parameters: Vec::new(),
    }
}

/// The change-effect packages an `UiEditorEditFx` names, in the form the renderer
/// compiles.
///
/// These are the kernel's own effects, so the kernel ships them: `wgpu.shader.register`
/// is how a project adds a style, and an effect that had to wait on that call would
/// draw *less* rather than fail — the draw pass skips a batch whose package has no
/// pipeline. Compiling them with every renderer is what makes an Agent edit visible
/// on a host nobody styled.
pub fn builtin_editor_fx_packages() -> Vec<UiShaderPackage> {
    vec![
        text_material_package(
            neon_ui_schema::editor_fx::TYPE_IN_PACKAGE_ID,
            TYPE_IN_SOURCE,
        ),
        text_material_package(
            neon_ui_schema::editor_fx::DELETE_PACKAGE_ID,
            DELETE_FRAGMENT_SOURCE,
        ),
    ]
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
        let mut bad =
            package(b"@fragment fn material() -> @location(0) vec4<f32> { return vec4(0.0); }");
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
            registry
                .lookup("pulse-glow")
                .unwrap()
                .compile_error
                .as_deref(),
            Some("naga: error")
        );
    }
}
