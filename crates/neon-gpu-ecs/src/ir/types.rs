//! Component value types shared by components and resources.

use serde::{Deserialize, Serialize};

/// The concrete GPU-side type of a component or resource value.
///
/// The design notes stored only `size`/`alignment`, but TAC→WGSL generation
/// needs the exact type (`vec3f + vec3f` and `f32 + f32` emit different
/// WGSL). The byte size and alignment are derived from this enum instead of
/// being free-form fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ComponentType {
    F32,
    Vec2F,
    Vec3F,
    Vec4F,
    Mat4F,
    U32,
    I32,
    /// Stored as `u32` (0/1) on the GPU; WGSL storage buffers cannot hold `bool`.
    Bool,
}

impl ComponentType {
    /// Logical byte size of one value.
    pub fn byte_size(self) -> usize {
        match self {
            ComponentType::F32 => 4,
            ComponentType::Vec2F => 8,
            ComponentType::Vec3F => 12,
            ComponentType::Vec4F => 16,
            ComponentType::Mat4F => 64,
            ComponentType::U32 | ComponentType::I32 | ComponentType::Bool => 4,
        }
    }

    /// WGSL uniform/storage alignment.
    pub fn alignment(self) -> usize {
        match self {
            ComponentType::F32 | ComponentType::U32 | ComponentType::I32 | ComponentType::Bool => 4,
            ComponentType::Vec2F => 8,
            ComponentType::Vec3F | ComponentType::Vec4F | ComponentType::Mat4F => 16,
        }
    }

    /// Element stride inside a WGSL `array<T>` (vec3 pads to 16).
    pub fn wgsl_array_stride(self) -> usize {
        match self {
            ComponentType::Vec3F => 16,
            other => other.byte_size().max(other.alignment()),
        }
    }

    /// True for float scalars and float vectors/matrices.
    pub fn is_float(self) -> bool {
        matches!(
            self,
            ComponentType::F32
                | ComponentType::Vec2F
                | ComponentType::Vec3F
                | ComponentType::Vec4F
                | ComponentType::Mat4F
        )
    }

    /// True for float vectors (not scalars, not matrices).
    pub fn is_float_vector(self) -> bool {
        matches!(
            self,
            ComponentType::Vec2F | ComponentType::Vec3F | ComponentType::Vec4F
        )
    }

    /// True for `U32`/`I32`/`Bool` (integer-class values; atomics require non-`Bool`).
    pub fn is_integer(self) -> bool {
        matches!(self, ComponentType::U32 | ComponentType::I32 | ComponentType::Bool)
    }
}
