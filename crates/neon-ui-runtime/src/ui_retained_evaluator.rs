//! Retained CPU evaluation driven by the compile-time impact graph.
//!
//! `evaluate_ui_program` stays the golden reference: it reseeds every node and
//! traverses every binding. This module keeps a retained frame so an input
//! publication only replays the bindings and branch predicates that the
//! serialized impact graph names. The final assembly lists (render
//! primitives, semantic targets) are re-linked when visibility flips, but no
//! dependency discovery ever re-scans binding records.

use std::collections::{BTreeMap, BTreeSet};

use neon_protocol::Revision;
use neon_ui_schema::{
    UiBounds, UiCpuFrameOutput, UiCpuNodeState, UiCpuRenderPrimitive, UiCpuSemanticTarget,
    UiCpuViewport, UiInputValue, UiInvalidationDomain, UiProgram, UiProgramRevision,
    UiResolvedInputs, UiTextHandle, sort_dedup_domains,
};

use crate::UiLocalPresentationState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiChangeCause {
    InputPublication,
    LocalInteractionPreview,
    LocalInteractionCommit,
    ProgramActivation,
}

/// The proven and consequence set of one change, normalized before it reaches
/// the evaluator or renderer. Builders deduplicate and sort so two causes that
/// overlap produce one identical set.
#[derive(Clone, Debug, PartialEq)]
pub struct UiImpactSet {
    pub cause: UiChangeCause,
    pub input_keys: Vec<String>,
    pub interaction_nodes: Vec<String>,
    pub binding_ids: Vec<u32>,
    pub node_keys: Vec<String>,
    pub domains: BTreeSet<UiInvalidationDomain>,
    pub branch_keys: Vec<String>,
    pub semantic_sequence: Option<u64>,
    pub input_revision: Revision,
    pub fragment_revision: Revision,
}

impl UiImpactSet {
    pub fn from_input_publication(
        program: &UiProgram,
        changed_slots: &[String],
        input_revision: Revision,
        fragment_revision: Revision,
    ) -> Self {
        let mut binding_ids = BTreeSet::new();
        let mut node_keys = BTreeSet::new();
        let mut branch_keys = BTreeSet::new();
        let mut domains = BTreeSet::new();
        for key in changed_slots {
            let Some(impact) = program.dependency_index.input_impacts.get(key) else {
                continue;
            };
            binding_ids.extend(impact.binding_ids.iter().copied());
            node_keys.extend(impact.affected_node_keys.iter().cloned());
            branch_keys.extend(impact.branch_keys.iter().cloned());
            domains.extend(impact.domains.iter().copied());
        }
        Self {
            cause: UiChangeCause::InputPublication,
            input_keys: changed_slots.to_vec(),
            interaction_nodes: Vec::new(),
            binding_ids: binding_ids.into_iter().collect(),
            node_keys: node_keys.into_iter().collect(),
            domains,
            branch_keys: branch_keys.into_iter().collect(),
            semantic_sequence: None,
            input_revision,
            fragment_revision,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiIncrementalError {
    pub code: &'static str,
}

/// What one impact-set application actually redid. This is the record the
/// probe asserts against and the payload the renderer narrows into
/// process-local buffer ranges.
#[derive(Clone, Debug, PartialEq)]
pub struct UiFrameDelta {
    pub cause: UiChangeCause,
    pub input_revision: Revision,
    pub executed_binding_ids: Vec<u32>,
    pub evaluated_branch_keys: Vec<String>,
    pub domains_executed: Vec<UiInvalidationDomain>,
    pub changed_states: Vec<UiCpuNodeState>,
    pub changed_semantic_targets: Vec<UiCpuSemanticTarget>,
    /// True when a visibility or bounds change forced the primitive assembly
    /// list to be re-linked (no dependency work happens in that pass).
    pub render_primitives_rebuilt: bool,
    pub layout_unchanged: bool,
}

struct NodeSeed {
    visible: bool,
    enabled: bool,
    opacity: f32,
    scroll_offset: [f32; 2],
    literal_text: Option<UiTextHandle>,
}

pub struct UiRetainedFrame {
    program_revision: UiProgramRevision,
    input_revision: Revision,
    viewport: UiCpuViewport,
    presentation_revision: Revision,
    states: BTreeMap<String, UiCpuNodeState>,
    logical_layout: Vec<neon_ui_schema::UiProgramLayoutRecord>,
    clips: BTreeMap<String, UiBounds>,
    primitives: Vec<UiCpuRenderPrimitive>,
    semantic_targets: Vec<UiCpuSemanticTarget>,
    diagnostics: Vec<neon_ui_schema::UiDiagnostic>,
    seeds: BTreeMap<String, NodeSeed>,
    bindings_by_node: BTreeMap<String, Vec<u32>>,
    branches_covering_node: BTreeMap<String, Vec<usize>>,
    degraded: bool,
}

impl UiRetainedFrame {
    /// Full materialization assembled from retained parts; equals the golden
    /// evaluator output for the same inputs.
    pub fn frame(&self) -> UiCpuFrameOutput {
        UiCpuFrameOutput {
            program_revision: self.program_revision.clone(),
            input_revision: self.input_revision,
            nodes: self.states.values().cloned().collect(),
            logical_layout: self.logical_layout.clone(),
            clips: self.clips.clone(),
            render_primitives: self.primitives.clone(),
            semantic_targets: self.semantic_targets.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }

    pub fn degraded(&self) -> bool {
        self.degraded
    }
}

pub fn evaluate_ui_program_initial(
    program: &UiProgram,
    inputs: &UiResolvedInputs,
    viewport: UiCpuViewport,
    local: &UiLocalPresentationState,
) -> UiRetainedFrame {
    let golden = crate::evaluate_ui_program(program, inputs, viewport, local);
    let seeds = program
        .node_templates
        .iter()
        .map(|template| {
            (
                template.node_id.0.clone(),
                NodeSeed {
                    visible: template.visible,
                    enabled: template.enabled,
                    opacity: template.style.opacity,
                    scroll_offset: template
                        .layout
                        .map_or([0.0; 2], |layout| layout.scroll_offset),
                    literal_text: program
                        .literal_texts
                        .iter()
                        .find(|entry| entry.node_key == template.node_id.0)
                        .map(|entry| entry.handle),
                },
            )
        })
        .collect();
    let mut bindings_by_node: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for binding in &program.binding_records {
        bindings_by_node
            .entry(binding.node_key.clone())
            .or_default()
            .push(binding.binding_id);
    }
    let mut branches_covering_node: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, branch) in program.branch_records.iter().enumerate() {
        for node_key in &branch.node_range {
            branches_covering_node
                .entry(node_key.clone())
                .or_default()
                .push(index);
        }
    }
    UiRetainedFrame {
        program_revision: program.revision.clone(),
        input_revision: inputs.input_revision,
        viewport,
        presentation_revision: local.revision,
        states: golden
            .nodes
            .iter()
            .map(|state| (state.node_key.clone(), state.clone()))
            .collect(),
        logical_layout: golden.logical_layout.clone(),
        clips: golden.clips.clone(),
        primitives: golden.render_primitives.clone(),
        semantic_targets: golden.semantic_targets.clone(),
        diagnostics: golden.diagnostics.clone(),
        seeds,
        bindings_by_node,
        branches_covering_node,
        // Any activation-time diagnostic means the program is outside the
        // healthy contract; incremental updates must then fall back to the
        // golden evaluator instead of partially reproducing error ordering.
        degraded: !golden.diagnostics.is_empty(),
    }
}

pub fn apply_ui_impact_set(
    program: &UiProgram,
    retained: &mut UiRetainedFrame,
    inputs: &UiResolvedInputs,
    local: &UiLocalPresentationState,
    impact: &UiImpactSet,
) -> Result<UiFrameDelta, UiIncrementalError> {
    if impact.cause != UiChangeCause::InputPublication || !impact.interaction_nodes.is_empty() {
        return Err(UiIncrementalError {
            code: "ui_incremental_unsupported_cause",
        });
    }
    if retained.degraded
        || retained.program_revision != program.revision
        || inputs.program_revision != program.revision
        || impact.input_revision != inputs.input_revision
        || retained.input_revision.0 >= inputs.input_revision.0
        || retained.presentation_revision != local.revision
    {
        return Err(UiIncrementalError {
            code: "ui_incremental_stale_frame",
        });
    }
    let mut replay_nodes: BTreeSet<&str> = BTreeSet::new();
    for binding_id in &impact.binding_ids {
        let Some(binding) = program
            .binding_records
            .iter()
            .find(|binding| binding.binding_id == *binding_id)
        else {
            return Err(UiIncrementalError {
                code: "ui_incremental_unknown_binding",
            });
        };
        replay_nodes.insert(binding.node_key.as_str());
    }
    let mut evaluated_branch_keys = Vec::new();
    for branch_key in &impact.branch_keys {
        let Some(branch) = program
            .branch_records
            .iter()
            .find(|branch| &branch.branch_key == branch_key)
        else {
            return Err(UiIncrementalError {
                code: "ui_incremental_unknown_branch",
            });
        };
        evaluated_branch_keys.push(branch.branch_key.clone());
        replay_nodes.extend(branch.node_range.iter().map(String::as_str));
    }

    let mut executed_binding_ids = Vec::new();
    let mut changed_states = Vec::new();
    let mut visibility_flipped = false;
    let mut diagnostics: Vec<neon_ui_schema::UiDiagnostic> = Vec::new();
    for node_key in replay_nodes {
        let Some(seed) = retained.seeds.get(node_key) else {
            continue;
        };
        let mut state = UiCpuNodeState {
            node_key: node_key.to_owned(),
            visible: seed.visible,
            enabled: seed.enabled,
            selected: false,
            active: false,
            numeric_value: None,
            state_token: None,
            text: seed.literal_text,
            image: None,
            opacity: seed.opacity,
            scroll_offset: seed.scroll_offset,
        };
        for binding_id in retained
            .bindings_by_node
            .get(node_key)
            .cloned()
            .unwrap_or_default()
        {
            let binding = program
                .binding_records
                .iter()
                .find(|binding| binding.binding_id == binding_id)
                .expect("bindings_by_node only holds compiled binding ids");
            executed_binding_ids.push(binding_id);
            if let Some(value) = inputs
                .values
                .get(&binding.input_key)
                .map(|resolved| &resolved.value)
            {
                apply_one_binding(program, &mut state, binding, value, &mut diagnostics);
            } else {
                diagnostics.push(crate::cpu_diagnostic(
                    "ui_program_unknown_input_key",
                    "resolved input is absent",
                    Some(&binding.node_key),
                    Some(&binding.input_key),
                    program.revision.revision,
                ));
            }
        }
        for branch_index in retained
            .branches_covering_node
            .get(node_key)
            .cloned()
            .unwrap_or_default()
        {
            let branch = &program.branch_records[branch_index];
            if !evaluated_branch_keys.contains(&branch.branch_key) {
                evaluated_branch_keys.push(branch.branch_key.clone());
            }
            if !crate::branch_predicate_matches(&branch.predicate, inputs, local) {
                state.visible = false;
            }
        }
        let previous = retained
            .states
            .get(node_key)
            .expect("retained frame covers every compiled node");
        if previous != &state {
            visibility_flipped |= previous.visible != state.visible;
            changed_states.push(state.clone());
            retained.states.insert(state.node_key.clone(), state);
        }
    }
    executed_binding_ids.sort_unstable();
    executed_binding_ids.dedup();
    evaluated_branch_keys.sort();
    evaluated_branch_keys.dedup();
    if !diagnostics.is_empty() {
        retained.degraded = true;
        return Err(UiIncrementalError {
            code: "ui_incremental_diagnostic_escape",
        });
    }

    let mut changed_semantic_targets = Vec::new();
    for target in &mut retained.semantic_targets {
        let Some(state) = retained.states.get(&target.node_key) else {
            continue;
        };
        if target.enabled != state.enabled || target.visible != state.visible {
            target.enabled = state.enabled;
            target.visible = state.visible;
            changed_semantic_targets.push(target.clone());
        }
    }
    let render_primitives_rebuilt = if visibility_flipped {
        rebuild_primitive_assembly(program, retained);
        true
    } else {
        false
    };
    retained.input_revision = impact.input_revision;
    let mut domains_executed: Vec<UiInvalidationDomain> = impact.domains.iter().copied().collect();
    sort_dedup_domains(&mut domains_executed);
    Ok(UiFrameDelta {
        cause: impact.cause,
        input_revision: impact.input_revision,
        executed_binding_ids,
        evaluated_branch_keys,
        domains_executed,
        changed_states,
        changed_semantic_targets,
        render_primitives_rebuilt,
        layout_unchanged: true,
    })
}

fn apply_one_binding(
    program: &UiProgram,
    state: &mut UiCpuNodeState,
    binding: &neon_ui_schema::UiBinding,
    value: &UiInputValue,
    diagnostics: &mut Vec<neon_ui_schema::UiDiagnostic>,
) {
    crate::apply_binding(
        state,
        &binding.property,
        value,
        diagnostics,
        program.revision.revision,
        &binding.node_key,
        &binding.input_key,
    );
}

/// Re-link the visible-primitive list after a visibility flip. This walks the
/// compiled layout order and the retained states only; it never resolves
/// dependencies.
fn rebuild_primitive_assembly(program: &UiProgram, retained: &mut UiRetainedFrame) {
    let root_key = program
        .nodes
        .first()
        .map(|node| node.key.as_str())
        .unwrap_or("");
    retained.primitives = retained
        .logical_layout
        .iter()
        .filter(|record| {
            retained
                .states
                .get(&record.node_key)
                .is_some_and(|state| state.visible)
        })
        .map(|record| {
            let mut bounds = record.bounds;
            if record.node_key == root_key {
                bounds.width = bounds.width.min(retained.viewport.logical_bounds.width);
                bounds.height = bounds.height.min(retained.viewport.logical_bounds.height);
            }
            UiCpuRenderPrimitive {
                node_key: record.node_key.clone(),
                kind: program
                    .nodes
                    .iter()
                    .find(|node| node.key == record.node_key)
                    .expect("compiled node exists")
                    .kind
                    .clone(),
                bounds,
                clip: retained.clips.get(&record.node_key).copied(),
            }
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        UiInputStore, UiInputWriter, compile_nui_flow_program, evaluate_ui_program, parse_nui_flow,
    };
    use neon_ui_schema::{
        UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiBounds, UiInputChange,
        UiInputFrame, UiInputValue, UiProgramCapability, UiProgramCapabilityOwner,
        UiProgramCapabilityStatus, UiProgramRevision,
    };

    const FLOW: &str = "version 1
surface surface.retained revision 1
budget nodes=16 bindings=16 instances=16 text=8 glyphs=64 events=8 clips=8
input left bool default false
input right bool default false
input amount i32 default 10
input show bool default false
surface root row w 400 h 300
  panel left-panel visible $left w 80 h 40
  panel right-panel visible $right w 80 h 40
  slider gauge numeric $amount w 100 h 20
  branch details h 120 when $show
    text note value \"note\" w 100 h 20
";

    fn revision() -> UiProgramRevision {
        UiProgramRevision {
            program_id: "surface.retained".into(),
            revision: Revision(1),
            schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: vec![UiProgramCapability {
                name: UI_PROGRAM_CAPABILITY_NAME.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Supported,
            }],
        }
    }

    fn viewport() -> UiCpuViewport {
        UiCpuViewport {
            logical_bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 300.0,
            },
            revision: Revision(1),
        }
    }

    struct Harness {
        program: UiProgram,
        store: UiInputStore,
        retained: UiRetainedFrame,
    }

    fn harness() -> Harness {
        let document = parse_nui_flow(FLOW).expect("retained fixture must parse");
        let program_revision = revision();
        let program = compile_nui_flow_program(&document, program_revision.clone()).unwrap();
        let store =
            UiInputStore::activate(program_revision, document.input_schema.clone()).unwrap();
        let retained = evaluate_ui_program_initial(
            &program,
            &store.snapshot(),
            viewport(),
            &UiLocalPresentationState::default(),
        );
        assert!(!retained.degraded());
        Harness {
            program,
            store,
            retained,
        }
    }

    impl Harness {
        fn publish(&mut self, key: &str, value: UiInputValue) -> UiImpactSet {
            let expected = self.store.snapshot().input_revision;
            let applied = self
                .store
                .apply(
                    UiInputWriter::External,
                    UiInputFrame {
                        program_revision: self.program.revision.clone(),
                        expected_input_revision: expected,
                        request_id: format!("request-{key}-{}", expected.0 + 1),
                        idempotency_key: format!("idem-{key}-{}", expected.0 + 1),
                        changes: vec![UiInputChange {
                            key: key.into(),
                            value,
                        }],
                    },
                )
                .expect("publication must apply");
            UiImpactSet::from_input_publication(
                &self.program,
                &applied.changed_slots,
                applied.input_revision,
                Revision(1),
            )
        }

        fn golden(&self) -> UiCpuFrameOutput {
            evaluate_ui_program(
                &self.program,
                &self.store.snapshot(),
                viewport(),
                &UiLocalPresentationState::default(),
            )
        }

        fn apply(&mut self, impact: &UiImpactSet) -> UiFrameDelta {
            apply_ui_impact_set(
                &self.program,
                &mut self.retained,
                &self.store.snapshot(),
                &UiLocalPresentationState::default(),
                impact,
            )
            .expect("incremental apply must succeed for healthy programs")
        }
    }

    fn bool_input(value: bool) -> UiInputValue {
        UiInputValue::Bool { value }
    }
    #[test]
    fn incremental_frame_equals_the_golden_evaluator_after_each_change() {
        let mut harness = harness();
        let mut sequence = 0_u64;
        for (key, value) in [
            ("left", bool_input(true)),
            ("amount", UiInputValue::I32 { value: 42 }),
            ("show", bool_input(true)),
            ("right", bool_input(true)),
            ("left", bool_input(false)),
            ("amount", UiInputValue::I32 { value: 7 }),
            ("show", bool_input(false)),
        ] {
            sequence += 1;
            let impact = harness.publish(key, value);
            let delta = harness.apply(&impact);
            assert_eq!(
                delta.input_revision.0, sequence,
                "each effective change must advance the revision once"
            );
            assert_eq!(
                harness.retained.frame(),
                harness.golden(),
                "incremental frame must equal the golden frame after change {sequence} ({key})"
            );
        }
    }

    #[test]
    fn replay_executes_only_the_impacted_binding_ids() {
        let mut harness = harness();
        let impact = harness.publish("left", bool_input(true));
        let delta = harness.apply(&impact);
        let left_id = harness
            .program
            .binding_records
            .iter()
            .find(|binding| binding.input_key == "left")
            .unwrap()
            .binding_id;
        let unrelated: Vec<u32> = harness
            .program
            .binding_records
            .iter()
            .map(|binding| binding.binding_id)
            .filter(|id| *id != left_id)
            .collect();
        assert_eq!(delta.executed_binding_ids, vec![left_id]);
        assert!(
            !delta
                .executed_binding_ids
                .iter()
                .any(|id| unrelated.contains(id)),
            "unrelated bindings must not be traversed"
        );
    }

    #[test]
    fn unrelated_node_states_are_byte_identical_across_a_sparse_change() {
        let mut harness = harness();
        let before = harness.retained.frame();
        let impact = harness.publish("amount", UiInputValue::I32 { value: 99 });
        let delta = harness.apply(&impact);
        let after = harness.retained.frame();
        let encode = |state: &UiCpuNodeState| serde_json::to_vec(state).unwrap();
        let changed: BTreeSet<&str> = delta
            .changed_states
            .iter()
            .map(|state| state.node_key.as_str())
            .collect();
        assert_eq!(changed, BTreeSet::from(["gauge"]));
        for (old, new) in before.nodes.iter().zip(after.nodes.iter()) {
            if !changed.contains(&old.node_key.as_str()) {
                assert_eq!(encode(old), encode(new), "unchanged node {}", old.node_key);
            }
        }
        assert!(!delta.render_primitives_rebuilt);
        assert_eq!(harness.retained.frame(), harness.golden());
    }

    #[test]
    fn branch_toggle_updates_only_its_subtree_and_relinks_primitives() {
        let mut harness = harness();
        let impact = harness.publish("show", bool_input(true));
        let delta = harness.apply(&impact);
        let changed: BTreeSet<&str> = delta
            .changed_states
            .iter()
            .map(|state| state.node_key.as_str())
            .collect();
        assert_eq!(changed, BTreeSet::from(["details", "note"]));
        assert!(delta.render_primitives_rebuilt);
        assert_eq!(delta.evaluated_branch_keys, vec!["details".to_owned()]);
        assert_eq!(harness.retained.frame(), harness.golden());
    }

    #[test]
    fn same_canonical_value_produces_no_dirty_slot_and_no_revision_advance() {
        let mut harness = harness();
        let before = harness.store.snapshot().input_revision;
        let impact = harness.publish("left", bool_input(false));
        assert!(impact.input_keys.is_empty(), "no slot actually changed");
        assert_eq!(impact.input_revision, before);
        assert!(harness.store.dirty_slots().is_empty());
        assert_eq!(harness.retained.frame(), harness.golden());
    }

    #[test]
    fn stale_impact_revision_is_rejected_without_mutating_the_frame() {
        let mut harness = harness();
        let mut impact = harness.publish("left", bool_input(true));
        impact.input_revision = Revision(99);
        let error = apply_ui_impact_set(
            &harness.program,
            &mut harness.retained,
            &harness.store.snapshot(),
            &UiLocalPresentationState::default(),
            &impact,
        )
        .expect_err("revision mismatch must be rejected");
        assert_eq!(error.code, "ui_incremental_stale_frame");
    }

    #[test]
    fn multi_slot_publication_replays_exactly_the_union_of_impacts() {
        let expected = Revision(0);
        let mut harness = harness();
        let applied = harness
            .store
            .apply(
                UiInputWriter::External,
                UiInputFrame {
                    program_revision: harness.program.revision.clone(),
                    expected_input_revision: expected,
                    request_id: "request-multi".into(),
                    idempotency_key: "idem-multi".into(),
                    changes: vec![
                        UiInputChange {
                            key: "left".into(),
                            value: bool_input(true),
                        },
                        UiInputChange {
                            key: "amount".into(),
                            value: UiInputValue::I32 { value: 21 },
                        },
                    ],
                },
            )
            .unwrap();
        assert_eq!(
            applied.changed_slots,
            vec!["left".to_owned(), "amount".to_owned()]
        );
        let impact = UiImpactSet::from_input_publication(
            &harness.program,
            &applied.changed_slots,
            applied.input_revision,
            Revision(1),
        );
        let delta = harness.apply(&impact);
        let mut expected_ids: Vec<u32> = impact.binding_ids.clone();
        expected_ids.sort_unstable();
        assert_eq!(delta.executed_binding_ids, expected_ids);
        assert_eq!(harness.retained.frame(), harness.golden());
    }
}
