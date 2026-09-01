//! Fixed global resources (singletons such as `DeltaTime` or the camera matrix).

use super::types::ComponentType;
use serde::{Deserialize, Serialize};

/// Bind group 1 binding reserved for the RenderData instance output buffer.
/// Resource `binding_slot` values must stay below this.
pub const RESERVED_RENDER_BINDING: u32 = 30;

/// A fixed global resource, uploaded by the CPU every frame and bound as a
/// `var<uniform>` in bind group 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceDef {
    /// Must equal the resource's index inside `EcsIr::resources`.
    pub id: u32,
    pub name: String,
    pub ty: ComponentType,
    /// Binding slot inside bind group 1; must be unique and below
    /// [`RESERVED_RENDER_BINDING`].
    pub binding_slot: u32,
    /// Initial bytes, exactly `ty.byte_size()`; uploaded before the first frame.
    pub default_value: Vec<u8>,
}

/// How a system references a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRef {
    pub resource_id: u32,
    pub access_type: super::query::AccessType,
}
