//! CPU-side world initialisation and reference computations.
//!
//! Everything here is pure (no wgpu), unit testable, and reused both by the
//! runtime (to seed buffers) and by integration tests (as the reference the
//! GPU sorting results are compared against).

use crate::ir::{EcsIr, QueryFilter};

/// Entity id ranges materialised by each prototype, in declaration order.
pub fn prototype_entity_ranges(ir: &EcsIr) -> Vec<std::ops::Range<u32>> {
    let mut ranges = Vec::new();
    let mut start = 0u32;
    for proto in &ir.initial_entities {
        ranges.push(start..start + proto.count);
        start += proto.count;
    }
    ranges
}

/// Total number of entities created from prototypes.
pub fn prototype_entity_total(ir: &EcsIr) -> u32 {
    ir.initial_entities.iter().map(|p| p.count).sum()
}

/// True when `entity` (under the initial prototype population) carries
/// `component_id`.
pub fn initial_has_component(ir: &EcsIr, entity: u32, component_id: u32) -> bool {
    for (proto, range) in ir.initial_entities.iter().zip(prototype_entity_ranges(ir)) {
        if range.contains(&entity) {
            return proto.component_ids.contains(&component_id);
        }
    }
    false
}

/// CPU reference: entities matching `query_index` under the initial
/// population, where baseline versions equal current versions, so
/// `Changed`/`Added` filters never pass. `RenderData` matches everything.
pub fn initial_query_match(ir: &EcsIr, query_index: usize) -> Vec<u32> {
    let Some(query) = ir.queries.get(query_index) else {
        return Vec::new();
    };
    let total = prototype_entity_total(ir);
    (0..total)
        .filter(|e| {
            let has = |cid: u32| initial_has_component(ir, *e, cid);
            query.with.iter().all(|a| has(a.component_id))
                && query.without.iter().all(|c| !has(*c))
                && query.filters.iter().all(|f| match f {
                    QueryFilter::RenderData => true,
                    QueryFilter::Changed(_) | QueryFilter::Added(_) => false,
                })
        })
        .collect()
}

/// `entityActive` buffer content: u32 per entity, 1 for every prototype
/// entity, zero padded to `max_entities`.
pub fn initial_entity_active(ir: &EcsIr, max_entities: u32) -> Vec<u8> {
    let total = prototype_entity_total(ir);
    assert!(total <= max_entities, "max_entities too small for prototypes");
    let mut bytes = vec![0u8; max_entities as usize * 4];
    for e in 0..total {
        bytes[e as usize * 4..e as usize * 4 + 4].copy_from_slice(&1u32.to_le_bytes());
    }
    bytes
}

/// Component SoA buffer content with WGSL array stride padding (vec3f
/// occupies 16 bytes per element). Entities without the component get zero
/// bytes.
pub fn initial_component_bytes(ir: &EcsIr, component_id: u32, max_entities: u32) -> Vec<u8> {
    let comp = &ir.components[component_id as usize];
    let stride = comp.ty.wgsl_array_stride();
    let mut bytes = vec![0u8; max_entities as usize * stride];
    for (proto, range) in ir.initial_entities.iter().zip(prototype_entity_ranges(ir)) {
        let pos = proto
            .component_ids
            .iter()
            .position(|c| *c == component_id);
        let Some(pos) = pos else { continue };
        for entity in range.clone() {
            let value = match &proto.initial_values {
                Some(values) => values[pos].as_slice(),
                None => comp.default_value.as_slice(),
            };
            assert_eq!(
                value.len(),
                comp.ty.byte_size(),
                "prototype value size mismatch for component {component_id}"
            );
            let offset = entity as usize * stride;
            bytes[offset..offset + value.len()].copy_from_slice(value);
        }
    }
    bytes
}

/// Version buffer content (current or baseline): u32 per entity, 1 when the
/// entity carries the component, 0 otherwise. Baseline starts equal to
/// current so `Changed`/`Added` filters miss on the first frame.
pub fn initial_version_bytes(ir: &EcsIr, component_id: u32, max_entities: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; max_entities as usize * 4];
    for e in 0..max_entities {
        if initial_has_component(ir, e, component_id) {
            bytes[e as usize * 4..e as usize * 4 + 4].copy_from_slice(&1u32.to_le_bytes());
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::*;
    use crate::tests_support::physics_world;

    fn two_pop_world() -> EcsIr {
        let mut ir = physics_world();
        // Prototype A: 4 entities with Transform+Velocity.
        ir.initial_entities = vec![
            EntityPrototype { component_ids: vec![0, 1], count: 4, initial_values: None },
            EntityPrototype { component_ids: vec![0, 1, 2], count: 4, initial_values: None },
        ];
        // Query 1: Transform but WITHOUT Health -> only population A.
        ir.queries.push(QueryDef {
            id: 1,
            with: vec![ComponentAccess { component_id: 0, access_type: AccessType::Read }],
            without: vec![2],
            filters: vec![],
        });
        ir
    }

    #[test]
    fn ranges_and_totals_cover_prototypes() {
        let ir = two_pop_world();
        assert_eq!(prototype_entity_ranges(&ir), vec![0..4, 4..8]);
        assert_eq!(prototype_entity_total(&ir), 8);
    }

    #[test]
    fn query_match_respects_without_clause() {
        let ir = two_pop_world();
        assert_eq!(initial_query_match(&ir, 0), (0..8).collect::<Vec<_>>());
        assert_eq!(initial_query_match(&ir, 1), vec![0, 1, 2, 3]);
    }

    #[test]
    fn entity_active_marks_prototype_entities() {
        let ir = two_pop_world();
        let bytes = initial_entity_active(&ir, 10);
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(words[..8], [1; 8]);
        assert_eq!(words[8..], [0, 0]);
    }

    #[test]
    fn component_bytes_pad_vec3_to_16_stride() {
        let mut ir = two_pop_world();
        // Give population A a known Transform initial value. Initial values
        // apply to every entity of the population (per EntityPrototype
        // semantics), so entities 0..4 all carry it.
        ir.initial_entities[0].initial_values = Some(vec![
            {
                let mut v = Vec::new();
                v.extend_from_slice(&1.0f32.to_le_bytes());
                v.extend_from_slice(&2.0f32.to_le_bytes());
                v.extend_from_slice(&3.0f32.to_le_bytes());
                v
            },
            vec![0; 12],
        ]);
        let bytes = initial_component_bytes(&ir, 0, 8);
        // vec3f stride is 16: 8 elements -> 128 bytes.
        assert_eq!(bytes.len(), 128);
        let x = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let y = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let z = f32::from_le_bytes(bytes[8..12].try_into().unwrap());
        assert_eq!((x, y, z), (1.0, 2.0, 3.0));
        // Padding word untouched.
        assert_eq!(&bytes[12..16], &[0u8; 4]);
        // Entity 3 (same population) carries the same initial value.
        let x3 = f32::from_le_bytes(bytes[48..52].try_into().unwrap());
        assert_eq!(x3, 1.0);
        // Entity 4 (population B, no initial_values) got the default zeros.
        assert_eq!(&bytes[64..76], &[0u8; 12]);
    }

    #[test]
    fn version_bytes_track_component_presence() {
        let ir = two_pop_world();
        let bytes = initial_version_bytes(&ir, 2, 8);
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        // Only population B (entities 4..8) carries Health.
        assert_eq!(words, vec![0, 0, 0, 0, 1, 1, 1, 1]);
    }
}
