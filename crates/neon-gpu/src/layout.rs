//! GPU struct layout.
//!
//! This module is the single source of truth for how a `struct` is laid out
//! inside a `neon-gpu` data pool. It computes the same offsets, sizes and
//! strides that a WGSL storage-buffer struct would use, and can emit the
//! matching WGSL type declarations so the CPU side and the shader side can
//! never drift apart.
//!
//! Layout rules (matching WGSL `storage` address space):
//!
//! | type    | size | align |
//! |---------|------|-------|
//! | f32/i32/u32 | 4  | 4   |
//! | vec2         | 8  | 8   |
//! | vec3         | 12 | 16  |
//! | vec4         | 16 | 16  |
//!
//! - A struct member is placed at `align_up(cursor, member_align)`.
//! - A struct's size is `align_up(cursor, struct_align)` where
//!   `struct_align` is the max member alignment.
//! - An array's element stride is `align_up(elem_size, max(elem_align, 16))`,
//!   required for storage-buffer arrays.
//! - A struct used as a pool element is indexed as `array<T, N>`; each slot
//!   therefore occupies `align_up(struct_size, 16)` bytes.
//!
//! No variable-length data is supported by design: everything is fixed size
//! so the pool can be a plain slab.

use std::collections::HashSet;
use std::fmt::Write as _;

/// A scalar component type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    F32,
    I32,
    U32,
}

impl Scalar {
    pub fn size(self) -> u32 {
        4
    }

    pub fn wgsl(self) -> &'static str {
        match self {
            Scalar::F32 => "f32",
            Scalar::I32 => "i32",
            Scalar::U32 => "u32",
        }
    }
}

/// A field type inside a struct.
#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    F32,
    I32,
    U32,
    Vec2(Scalar),
    Vec3(Scalar),
    Vec4(Scalar),
    /// Fixed-length array. Stride follows the storage-buffer 16-byte rule.
    Array {
        count: u32,
        element: Box<Type>,
    },
    /// A nested struct.
    Struct(StructLayout),
}

impl Type {
    pub fn size(&self) -> u32 {
        match self {
            Type::F32 | Type::I32 | Type::U32 => 4,
            Type::Vec2(_) => 8,
            Type::Vec3(_) => 12,
            Type::Vec4(_) => 16,
            Type::Array { count, element } => element.array_stride() * count,
            Type::Struct(s) => s.size,
        }
    }

    pub fn align(&self) -> u32 {
        match self {
            Type::F32 | Type::I32 | Type::U32 => 4,
            Type::Vec2(_) => 8,
            Type::Vec3(_) | Type::Vec4(_) => 16,
            Type::Array { element, .. } => element.align(),
            Type::Struct(s) => s.align,
        }
    }

    /// Byte stride between array elements of this type (storage rule).
    pub fn array_stride(&self) -> u32 {
        align_up(self.size(), self.align().max(16))
    }

    /// WGSL type name. Registers nested structs into `seen` for emission.
    pub fn wgsl_name(
        &self,
        seen: &mut HashSet<String>,
        out: &mut String,
    ) -> Result<(), LayoutError> {
        match self {
            Type::F32 => out.push_str("f32"),
            Type::I32 => out.push_str("i32"),
            Type::U32 => out.push_str("u32"),
            Type::Vec2(s) => {
                let _ = write!(out, "vec2<{}>", s.wgsl());
            }
            Type::Vec3(s) => {
                let _ = write!(out, "vec3<{}>", s.wgsl());
            }
            Type::Vec4(s) => {
                let _ = write!(out, "vec4<{}>", s.wgsl());
            }
            Type::Array { count, element } => {
                let _ = write!(out, "array<");
                element.wgsl_name(seen, out)?;
                let _ = write!(out, ", {count}>");
            }
            Type::Struct(s) => {
                s.register(seen, out)?;
                out.push_str(&s.name);
            }
        }
        Ok(())
    }
}

/// A single named struct field.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    /// Byte offset of this field within the struct.
    pub offset: u32,
}

/// A finished struct layout: offsets, size, alignment and array stride.
#[derive(Clone, Debug, PartialEq)]
pub struct StructLayout {
    pub name: String,
    pub fields: Vec<Field>,
    pub size: u32,
    pub align: u32,
    /// `align_up(size, 16)`: bytes each element occupies when stored in a pool.
    pub array_stride: u32,
}

impl StructLayout {
    /// Resolve a field path to its byte offset, e.g. `["inner", "a"]`.
    ///
    /// Nested structs are walked by name; arrays are not indexed (the caller
    /// scales by `array_stride` for the base, then adds `index * stride`).
    pub fn offset_of(&self, path: &[&str]) -> Result<u32, LayoutError> {
        let Some((first, rest)) = path.split_first() else {
            return Err(LayoutError::EmptyPath);
        };
        let field = self
            .fields
            .iter()
            .find(|f| f.name == *first)
            .ok_or_else(|| LayoutError::UnknownField(self.name.clone(), (*first).to_string()))?;
        let base = field.offset;
        match &field.ty {
            Type::Struct(inner) if !rest.is_empty() => Ok(base + inner.offset_of(rest)?),
            Type::Struct(_) if rest.is_empty() => Ok(base),
            _ if rest.is_empty() => Ok(base),
            _ => Err(LayoutError::PathIntoNonStruct {
                struct_name: self.name.clone(),
                field: (*first).to_string(),
            }),
        }
    }

    /// Emit WGSL declarations for this struct and every nested struct it
    /// references, once each.
    pub fn wgsl_source(&self) -> Result<String, LayoutError> {
        let mut seen = HashSet::new();
        let mut out = String::new();
        self.register(&mut seen, &mut out)?;
        Ok(out)
    }

    fn register(&self, seen: &mut HashSet<String>, out: &mut String) -> Result<(), LayoutError> {
        if !seen.insert(self.name.clone()) {
            return Ok(());
        }
        // Emit nested structs first so every reference is defined.
        for field in &self.fields {
            if let Type::Struct(inner) = &field.ty {
                inner.register(seen, out)?;
            }
        }
        let _ = writeln!(out, "struct {} {{", self.name);
        for field in &self.fields {
            let align = field.ty.align();
            let size = field.ty.size();
            let _ = write!(out, "    @align({align}) @size({size}) {}: ", field.name);
            field.ty.wgsl_name(seen, out)?;
            out.push_str(",\n");
        }
        let _ = writeln!(out, "}};");
        Ok(())
    }
}

/// Builder for a [`StructLayout`].
#[derive(Debug)]
pub struct LayoutBuilder {
    name: String,
    fields: Vec<(String, Type)>,
}

impl LayoutBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: Vec::new(),
        }
    }

    pub fn field(mut self, name: impl Into<String>, ty: Type) -> Self {
        self.fields.push((name.into(), ty));
        self
    }

    pub fn build(self) -> Result<StructLayout, LayoutError> {
        if self.name.is_empty() {
            return Err(LayoutError::EmptyName);
        }
        let mut cursor = 0u32;
        let mut align = 1u32;
        let mut fields = Vec::with_capacity(self.fields.len());
        for (name, ty) in self.fields {
            if name.is_empty() {
                return Err(LayoutError::EmptyName);
            }
            if fields.iter().any(|f: &Field| f.name == name) {
                return Err(LayoutError::DuplicateField {
                    struct_name: self.name.clone(),
                    field: name,
                });
            }
            let field_align = ty.align();
            align = align.max(field_align);
            cursor = align_up(cursor, field_align);
            fields.push(Field {
                name,
                offset: cursor,
                ty,
            });
            cursor += fields.last().expect("just pushed").ty.size();
        }
        let size = align_up(cursor, align);
        Ok(StructLayout {
            name: self.name,
            fields,
            size,
            align,
            array_stride: align_up(size, 16),
        })
    }
}

/// Convenience field types.
pub mod ty {
    use super::{Scalar, Type};

    pub const F32: Type = Type::F32;
    pub const I32: Type = Type::I32;
    pub const U32: Type = Type::U32;
    pub const fn vec2f() -> Type {
        Type::Vec2(Scalar::F32)
    }
    pub const fn vec3f() -> Type {
        Type::Vec3(Scalar::F32)
    }
    pub const fn vec4f() -> Type {
        Type::Vec4(Scalar::F32)
    }
    pub const fn vec2i() -> Type {
        Type::Vec2(Scalar::I32)
    }
    pub const fn vec3i() -> Type {
        Type::Vec3(Scalar::I32)
    }
    pub const fn vec2u() -> Type {
        Type::Vec2(Scalar::U32)
    }
    pub fn array(count: u32, element: Type) -> Type {
        Type::Array {
            count,
            element: Box::new(element),
        }
    }
    pub const fn strukt(layout: crate::layout::StructLayout) -> Type {
        Type::Struct(layout)
    }
}

/// Round `n` up to a multiple of `alignment` (power of two).
pub(crate) const fn align_up(n: u32, alignment: u32) -> u32 {
    debug_assert!(alignment.is_power_of_two());
    (n + alignment - 1) & !(alignment - 1)
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("struct name must not be empty")]
    EmptyName,
    #[error("field path must not be empty")]
    EmptyPath,
    #[error("duplicate field `{field}` in struct `{struct_name}`")]
    DuplicateField { struct_name: String, field: String },
    #[error("struct `{0}` has no field `{1}`")]
    UnknownField(String, String),
    #[error("field `{field}` of struct `{struct_name}` is not a struct; cannot descend")]
    PathIntoNonStruct { struct_name: String, field: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ty::*;

    #[test]
    fn scalar_offsets() {
        let layout = LayoutBuilder::new("Scalars")
            .field("a", F32)
            .field("b", U32)
            .field("c", I32)
            .build()
            .unwrap();
        assert_eq!(layout.size, 12);
        assert_eq!(layout.align, 4);
        assert_eq!(layout.array_stride, 16);
        assert_eq!(layout.offset_of(&["a"]).unwrap(), 0);
        assert_eq!(layout.offset_of(&["b"]).unwrap(), 4);
        assert_eq!(layout.offset_of(&["c"]).unwrap(), 8);
    }

    #[test]
    fn vec3_aligns_to_16() {
        // The next vec3 member must start at a 16-byte boundary.
        let layout = LayoutBuilder::new("V")
            .field("a", vec3f())
            .field("b", vec3f())
            .build()
            .unwrap();
        assert_eq!(layout.offset_of(&["a"]).unwrap(), 0);
        assert_eq!(layout.offset_of(&["b"]).unwrap(), 16);
        assert_eq!(layout.size, 32);
        assert_eq!(layout.align, 16);
        assert_eq!(layout.array_stride, 32);
    }

    #[test]
    fn scalar_after_vec3_uses_its_own_alignment() {
        // WGSL storage rule: each member aligns to ITS OWN alignment, so `w`
        // (align 4) lands right after `pos` at 12; the struct size still
        // rounds up to the struct alignment of 16.
        let layout = LayoutBuilder::new("V")
            .field("pos", vec3f())
            .field("w", F32)
            .build()
            .unwrap();
        assert_eq!(layout.offset_of(&["pos"]).unwrap(), 0);
        assert_eq!(layout.offset_of(&["w"]).unwrap(), 12);
        assert_eq!(layout.size, 16);
        assert_eq!(layout.align, 16);
        assert_eq!(layout.array_stride, 16);
    }

    #[test]
    fn nested_struct_offsets() {
        let inner = LayoutBuilder::new("Inner")
            .field("a", F32)
            .field("b", F32)
            .build()
            .unwrap();
        assert_eq!(inner.size, 8);
        assert_eq!(inner.align, 4);

        let outer = LayoutBuilder::new("Outer")
            .field("tag", U32)
            .field("inner", ty::strukt(inner))
            .field("weight", F32)
            .build()
            .unwrap();
        assert_eq!(outer.offset_of(&["tag"]).unwrap(), 0);
        assert_eq!(outer.offset_of(&["inner"]).unwrap(), 4);
        assert_eq!(outer.offset_of(&["inner", "a"]).unwrap(), 4);
        assert_eq!(outer.offset_of(&["inner", "b"]).unwrap(), 8);
        assert_eq!(outer.offset_of(&["weight"]).unwrap(), 12);
        assert_eq!(outer.size, 16);
        assert_eq!(outer.array_stride, 16);
    }

    #[test]
    fn array_stride_follows_storage_rule() {
        // array<f32, 2> in storage: stride 16 (16-byte storage rule), size 32.
        let arr = ty::array(2, F32);
        assert_eq!(arr.size(), 32);
        assert_eq!(arr.align(), 4);

        let layout = LayoutBuilder::new("WithArray")
            .field("head", F32)
            .field("samples", ty::array(2, F32))
            .build()
            .unwrap();
        // Member offset uses the member's own alignment (element align = 4).
        assert_eq!(layout.offset_of(&["samples"]).unwrap(), 4);
        assert_eq!(layout.size, 36);
        assert_eq!(layout.array_stride, 48);
    }

    #[test]
    fn duplicate_field_rejected() {
        let err = LayoutBuilder::new("Dup")
            .field("a", F32)
            .field("a", U32)
            .build()
            .unwrap_err();
        assert!(matches!(err, LayoutError::DuplicateField { .. }));
    }

    #[test]
    fn wgsl_source_emits_nested_defs_once() {
        let inner = LayoutBuilder::new("Inner").field("a", F32).build().unwrap();
        let outer = LayoutBuilder::new("Outer")
            .field("tag", U32)
            .field("inner", ty::strukt(inner.clone()))
            .field("inner_again", ty::strukt(inner))
            .build()
            .unwrap();
        let src = outer.wgsl_source().unwrap();
        assert!(src.contains("struct Inner {"));
        assert!(src.contains("struct Outer {"));
        // Referenced twice, emitted once.
        assert_eq!(src.matches("struct Inner {").count(), 1);
        assert_eq!(src.matches("struct Outer {").count(), 1);
        // Member layout is pinned explicitly.
        assert!(src.contains("@align(4) @size(4) a: f32"));
    }
}
