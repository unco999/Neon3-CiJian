//! Sorting kernels: count → scan → fill.
//!
//! Three entry points inside the shared module, all dispatched by the CPU
//! with `ceil(max_entities / 64)` workgroups (scan with one):
//!
//! - `system_prep_count`: one thread per entity; every matching query bumps
//!   its atomic counter in `queryCounts`.
//! - `system_prep_scan`: single invocation; exclusive prefix sum into
//!   `queryCursors`, publishes `{start, count}` into `framePrepBuffer`,
//!   derives `indirectArgs` and resets `queryCounts` for the next frame.
//! - `system_prep_fill`: one thread per entity; re-evaluates the same
//!   conditions and scatters entity ids into `compactedEntityIds` at
//!   atomically reserved slots.
//!
//! Presence semantics: a component version of 0 means the entity does not
//! have the component; any write moves it to >= 1 and increments it. This
//! replaces the 32-component bitmask proposal and supports unbounded
//! component counts.

use crate::ir::{EcsIr, QueryFilter};

/// Conditions a query imposes, emitted as one WGSL predicate function.
pub(super) fn emit_query_predicate(ir: &EcsIr, query_index: usize) -> String {
    let query = &ir.queries[query_index];
    let mut out = String::new();
    out.push_str(&format!(
        "fn ecs_pass_{query_index}(ecs_e : u32) -> bool {{\n"
    ));

    // with: current version != 0.
    for access in &query.with {
        out.push_str(&format!(
            "    if (atomicLoad(&ecs_cv{}[ecs_e]) == 0u) {{ return false; }}\n",
            access.component_id
        ));
    }
    // without: current version == 0.
    for cid in &query.without {
        out.push_str(&format!(
            "    if (atomicLoad(&ecs_cv{cid}[ecs_e]) != 0u) {{ return false; }}\n"
        ));
    }
    // Filters. RenderData adds no condition.
    for filter in &query.filters {
        match filter {
            QueryFilter::Changed(cid) => {
                out.push_str(&format!(
                    "    if (!(atomicLoad(&ecs_cb{cid}[ecs_e]) != 0u && atomicLoad(&ecs_cv{cid}[ecs_e]) != atomicLoad(&ecs_cb{cid}[ecs_e]))) {{ return false; }}\n"
                ));
            }
            QueryFilter::Added(cid) => {
                out.push_str(&format!(
                    "    if (!(atomicLoad(&ecs_cb{cid}[ecs_e]) == 0u && atomicLoad(&ecs_cv{cid}[ecs_e]) != 0u)) {{ return false; }}\n"
                ));
            }
            QueryFilter::RenderData => {}
        }
    }

    out.push_str("    return true;\n}\n");
    out
}

/// The three sorting entry points. `n_queries` sizes the scan loops.
pub(super) fn emit_sorting(ir: &EcsIr) -> String {
    let n = ir.queries.len();
    let mut out = String::new();

    // --- count ---
    out.push_str(
        "@compute @workgroup_size(64)\nfn system_prep_count(@builtin(global_invocation_id) ecs_gid : vec3u) {\n    let ecs_e = ecs_gid.x;\n    if (ecs_e >= arrayLength(&entityActive)) { return; }\n    if (entityActive[ecs_e] == 0u) { return; }\n",
    );
    for q in 0..n {
        out.push_str(&format!(
            "    if (ecs_pass_{q}(ecs_e)) {{ atomicAdd(&queryCounts[{q}u], 1u); }}\n"
        ));
    }
    out.push_str("}\n");

    // --- scan (single invocation) ---
    // Counts are kept in a local array so the indirect-args loop never
    // re-reads the storage buffer this same invocation just wrote (write-then
    // read of storage within one invocation is unreliable on some drivers).
    out.push_str(&format!(
        "@compute @workgroup_size(1)\nfn system_prep_scan() {{\n    var ecs_counts : array<u32, {n}u>;\n    var ecs_total : u32 = 0u;\n    for (var ecs_q : u32 = 0u; ecs_q < {n}u; ecs_q += 1u) {{\n        let ecs_c = atomicLoad(&queryCounts[ecs_q]);\n        ecs_counts[ecs_q] = ecs_c;\n        framePrepBuffer[ecs_q] = QueryRange(ecs_total, ecs_c);\n        atomicStore(&queryCursors[ecs_q], ecs_total);\n        atomicStore(&queryCounts[ecs_q], 0u);\n        ecs_total += ecs_c;\n    }}\n    for (var ecs_q : u32 = 0u; ecs_q < {n}u; ecs_q += 1u) {{\n        let ecs_c = ecs_counts[ecs_q];\n        indirectArgs[ecs_q] = vec3u((ecs_c + 63u) / 64u, 1u, 1u);\n    }}\n}}\n"
    ));

    // --- fill ---
    out.push_str(
        "@compute @workgroup_size(64)\nfn system_prep_fill(@builtin(global_invocation_id) ecs_gid : vec3u) {\n    let ecs_e = ecs_gid.x;\n    if (ecs_e >= arrayLength(&entityActive)) { return; }\n    if (entityActive[ecs_e] == 0u) { return; }\n",
    );
    for q in 0..n {
        out.push_str(&format!(
            "    if (ecs_pass_{q}(ecs_e)) {{\n        let ecs_slot = atomicAdd(&queryCursors[{q}u], 1u);\n        compactedEntityIds[ecs_slot] = ecs_e;\n    }}\n"
        ));
    }
    out.push_str("}\n");

    out
}
