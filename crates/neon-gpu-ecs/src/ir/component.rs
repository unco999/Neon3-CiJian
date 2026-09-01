//! Component registration metadata.

use super::types::ComponentType;
use serde::{Deserialize, Serialize};

/// A registered component type. One SoA storage buffer is allocated per
/// component by the runtime.
///
/// `id` must equal the component's position inside `EcsIr::components`; this
/// keeps slot assignment deterministic for binding generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentDef {
    pub id: u32,
    /// Human-readable name, used in generated WGSL identifiers.
    pub name: String,
    pub ty: ComponentType,
    /// Initial bytes for new entities; must be exactly `ty.byte_size()` bytes.
    pub default_value: Vec<u8>,
}

impl ComponentDef {
    /// Byte size of one instance, derived from `ty`.
    pub fn size(&self) -> u32 {
        self.ty.byte_size() as u32
    }

    /// Alignment of one instance, derived from `ty`.
    pub fn alignment(&self) -> u32 {
        self.ty.alignment() as u32
    }
}
