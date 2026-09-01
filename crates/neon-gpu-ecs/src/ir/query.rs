//! System queries: which entities a system iterates over.

use serde::{Deserialize, Serialize};

/// How a system accesses a component or resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessType {
    Read,
    Write,
    ReadWrite,
}

impl AccessType {
    /// True when the access may modify data.
    pub fn is_write(self) -> bool {
        matches!(self, AccessType::Write | AccessType::ReadWrite)
    }
}

/// A query selects entities by required components, excluded components and
/// change filters. The sorting kernel compacts matching entity IDs per
/// query so systems run with `dispatch_workgroups_indirect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryDef {
    /// Must equal the query's index inside `EcsIr::queries`.
    pub id: u32,
    /// Components that must be present, with their access modes.
    pub with: Vec<ComponentAccess>,
    /// Components that must NOT be present.
    pub without: Vec<u32>,
    /// Change filters applied on top of presence checks.
    pub filters: Vec<QueryFilter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentAccess {
    pub component_id: u32,
    pub access_type: AccessType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryFilter {
    /// Entity passes only if it had the component at the previous frame's
    /// sorting point (baseline version != 0) and the component was written
    /// since (current version != baseline version).
    Changed(u32),
    /// Entity passes only if the component's baseline version is 0 and its
    /// current version is != 0 (first write / add since the previous
    /// frame's sorting point).
    Added(u32),
    /// Marks the query as the RenderData population. Every active entity
    /// passes (no condition) and gets a contiguous instance-buffer slot;
    /// only systems on this query may use `Instr::StoreRender`.
    RenderData,
}
