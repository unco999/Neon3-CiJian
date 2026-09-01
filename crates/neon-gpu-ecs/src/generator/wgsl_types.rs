//! Component/resource type → WGSL mapping and byte/stride helpers.

use crate::ir::ComponentType;

impl ComponentType {
    /// WGSL scalar/vector/matrix name used in generated storage bindings.
    /// `Bool` is stored as `u32` because storage buffers cannot hold `bool`.
    pub fn wgsl_storage_type(self) -> &'static str {
        match self {
            ComponentType::F32 => "f32",
            ComponentType::Vec2F => "vec2f",
            ComponentType::Vec3F => "vec3f",
            ComponentType::Vec4F => "vec4f",
            ComponentType::Mat4F => "mat4x4f",
            ComponentType::U32 => "u32",
            ComponentType::I32 => "i32",
            ComponentType::Bool => "u32",
        }
    }

    /// WGSL type of a local variable holding this value. `Bool` locals are
    /// real WGSL `bool`; they convert to/from `u32` at the storage boundary.
    pub fn wgsl_local_type(self) -> &'static str {
        match self {
            ComponentType::Bool => "bool",
            other => other.wgsl_storage_type(),
        }
    }

    /// True when this type may appear in a `var<uniform>` resource binding.
    /// `Bool` is not a host-shareable uniform type and `Vec3F` has a 16-byte
    /// uniform footprint while `byte_size()` reports 12, which would
    /// desynchronise CPU uploads.
    pub fn is_uniform_safe(self) -> bool {
        matches!(
            self,
            ComponentType::F32
                | ComponentType::Vec2F
                | ComponentType::Vec4F
                | ComponentType::Mat4F
                | ComponentType::U32
                | ComponentType::I32
        )
    }
}
