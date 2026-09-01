//! Bind group layout shared by the generator and the runtime.
//!
//! Group 0 holds every storage buffer, all with `read_write` access so the
//! sorting entry points and every system entry point can share one identical
//! `BindGroupLayout` (multi-entry-point design). Group 1 holds the uniform
//! frame resources and the RenderData instance output.
//!
//! Total group 0 bindings: `8 + 3 * n_components` (data / current version /
//! baseline version per component). The host device must provide at least
//! that many storage buffers per shader stage.

/// Fixed group 0 slots.
pub const ENTITY_ACTIVE_BINDING: u32 = 0;
pub const QUERY_COUNTS_BINDING: u32 = 1;
pub const QUERY_CURSORS_BINDING: u32 = 2;
pub const FRAME_PREP_BINDING: u32 = 3;
pub const COMPACTED_IDS_BINDING: u32 = 4;
pub const INDIRECT_ARGS_BINDING: u32 = 5;
pub const COMMAND_BUFFER_BINDING: u32 = 6;
pub const COMMAND_COUNT_BINDING: u32 = 7;
/// Component buffers start here; each component occupies 3 consecutive slots.
pub const COMPONENTS_BASE_BINDING: u32 = 8;

/// Group 1 slot reserved for the RenderData instance buffer.
pub const RENDER_INSTANCES_BINDING: u32 = 30;

/// Storage binding holding the component's SoA values.
pub const fn component_data_binding(component_id: u32) -> u32 {
    COMPONENTS_BASE_BINDING + component_id * 3
}

/// Storage binding holding the component's current version numbers.
/// Version 0 means the entity does not have the component; the first write
/// (spawn / AddComponent / Store) moves it to >= 1 and every Store increments.
pub const fn component_version_binding(component_id: u32) -> u32 {
    COMPONENTS_BASE_BINDING + component_id * 3 + 1
}

/// Storage binding holding the component's baseline version numbers, copied
/// from the current versions after each frame's sorting pass.
pub const fn component_baseline_binding(component_id: u32) -> u32 {
    COMPONENTS_BASE_BINDING + component_id * 3 + 2
}

/// Number of storage buffers bound in group 0 for a world with
/// `component_count` components.
pub const fn group0_storage_bindings(component_count: u32) -> u32 {
    COMPONENTS_BASE_BINDING + component_count * 3
}

/// Fixed-size per-query sorting result: `{ start, count }`, 8 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct QueryRange {
    pub start: u32,
    pub count: u32,
}

/// Fixed-size structural-change command record, 16 bytes. Mirrors the WGSL
/// `StructuralCommand` struct. The CPU readback replays these commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct StructuralCommand {
    /// One of [`COMMAND_KIND_SPAWN`] etc.
    pub kind: u32,
    /// Spawn: prototype index; Delete/Add/Remove: entity id.
    pub a: u32,
    /// Add/Remove: component id; otherwise 0.
    pub b: u32,
    /// Reserved, must be 0.
    pub c: u32,
}

pub const COMMAND_KIND_SPAWN: u32 = 0;
pub const COMMAND_KIND_DELETE: u32 = 1;
pub const COMMAND_KIND_ADD_COMPONENT: u32 = 2;
pub const COMMAND_KIND_REMOVE_COMPONENT: u32 = 3;

/// RenderData instance layout: `vec4f` transform + `vec4f` color, 32 bytes.
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct RenderInstance {
    /// World position in xyz; w is free (scale, health, ...).
    pub transform: [f32; 4],
    /// RGBA color.
    pub color: [f32; 4],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_sizes_match_wgsl_layouts() {
        assert_eq!(std::mem::size_of::<QueryRange>(), 8);
        assert_eq!(std::mem::align_of::<QueryRange>(), 4);
        assert_eq!(std::mem::size_of::<StructuralCommand>(), 16);
        assert_eq!(std::mem::align_of::<StructuralCommand>(), 4);
        assert_eq!(std::mem::size_of::<RenderInstance>(), 32);
        assert_eq!(std::mem::align_of::<RenderInstance>(), 4);
    }

    #[test]
    fn component_binding_slots_are_disjoint() {
        assert_eq!(component_data_binding(0), 8);
        assert_eq!(component_version_binding(0), 9);
        assert_eq!(component_baseline_binding(0), 10);
        assert_eq!(component_data_binding(1), 11);
        assert_eq!(group0_storage_bindings(1), 11);
        assert_eq!(group0_storage_bindings(4), 20);
    }
}
