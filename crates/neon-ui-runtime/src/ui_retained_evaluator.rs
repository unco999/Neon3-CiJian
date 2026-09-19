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
    UiCpuViewport, UiInputValue, UiInteractionKind, UiInvalidationDomain, UiProgram,
    UiProgramRevision, UiResolvedInputs, UiTextHandle, sort_dedup_domains,
};

use crate::{UiLocalPresentationState, ui_input_impact::preview_domains_for};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiChangeCause {
    InputPublication,
    LocalInteractionPreview,
    LocalInteractionCommit,
    ProgramActivation,
}

/// Stable error codes for the incremental path. A rejected delta never leaves
/// the retained frame partially updated, except where the contract says a stale
/// preview must be dropped so it can never be drawn again.
pub mod error_codes {
    pub const STALE_FRAME: &str = "ui_incremental_stale_frame";
    pub const UNSUPPORTED_CAUSE: &str = "ui_incremental_unsupported_cause";
    pub const UNKNOWN_BINDING: &str = "ui_incremental_unknown_binding";
    pub const UNKNOWN_BRANCH: &str = "ui_incremental_unknown_branch";
    pub const DIAGNOSTIC_ESCAPE: &str = "ui_incremental_diagnostic_escape";
    pub const UNKNOWN_INTERACTION_NODE: &str = "ui_incremental_unknown_interaction_node";
    pub const UNDECLARED_INTERACTION_KIND: &str = "ui_incremental_undeclared_interaction_kind";
    pub const STALE_PREVIEW: &str = "ui_incremental_stale_preview";
    pub const PREVIEW_EPOCH_MISMATCH: &str = "ui_incremental_preview_epoch_mismatch";
    pub const NO_ACTIVE_PREVIEW: &str = "ui_incremental_no_active_preview";
    pub const CONFIRM_WITHOUT_AUTHORITY: &str = "ui_incremental_confirm_without_authority";
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
    /// Set by the local-interaction builder only. A preview names exactly one
    /// interaction kind, and it belongs to the renderer epoch that sampled it.
    pub interaction_kind: Option<UiInteractionKind>,
    pub renderer_epoch: Option<u64>,
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
            interaction_kind: None,
            renderer_epoch: None,
        }
    }

    /// Builds the preview scope of one renderer-local interaction from the
    /// compiled interaction impact graph. It never carries binding ids, branch
    /// keys or input keys: a preview is presentation only and must not be able
    /// to reach authoritative state through this set.
    pub fn from_local_interaction(
        program: &UiProgram,
        kind: UiInteractionKind,
        node_key: &str,
        renderer_epoch: u64,
        semantic_sequence: Option<u64>,
        input_revision: Revision,
        fragment_revision: Revision,
    ) -> Result<Self, UiIncrementalError> {
        let Some(impact) = program.dependency_index.interaction_impacts.get(node_key) else {
            return Err(UiIncrementalError {
                code: error_codes::UNKNOWN_INTERACTION_NODE,
            });
        };
        if !impact.interaction_kinds.contains(&kind) {
            return Err(UiIncrementalError {
                code: error_codes::UNDECLARED_INTERACTION_KIND,
            });
        }
        Ok(Self {
            cause: UiChangeCause::LocalInteractionPreview,
            input_keys: Vec::new(),
            interaction_nodes: vec![node_key.to_owned()],
            binding_ids: Vec::new(),
            node_keys: vec![node_key.to_owned()],
            domains: preview_domains_for(std::slice::from_ref(&kind))
                .into_iter()
                .collect(),
            branch_keys: Vec::new(),
            semantic_sequence,
            input_revision,
            fragment_revision,
            interaction_kind: Some(kind),
            renderer_epoch: Some(renderer_epoch),
        })
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
    /// Populated by `LocalInteractionPreview` deltas only. A preview never
    /// changes `changed_states`, `input_revision` or the primitive assembly; it
    /// only names the node and domains whose presentation ranges must update.
    pub preview_kind: Option<UiInteractionKind>,
    pub preview_node_key: Option<String>,
    /// The node that previously held this preview kind, if any. The renderer
    /// must restore it in the same frame.
    pub displaced_preview_node_key: Option<String>,
    pub preview_revision: Revision,
    /// Previews dropped because an authoritative publication reached the node
    /// they were predicting. Their nodes return to authoritative appearance.
    pub superseded_preview_kinds: Vec<UiInteractionKind>,
}

/// How an active renderer preview ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiPreviewResolution {
    /// The authoritative input arrived and matches what the preview showed.
    Confirmed,
    /// The domain rejected or reverted the interaction.
    RolledBack,
    /// Focus loss, capture cancel, or an abandoned interaction.
    Cancelled,
}

/// Bookkeeping record for one ended preview. The renderer uses `domains` to
/// restore exactly the ranges the preview had touched.
#[derive(Clone, Debug, PartialEq)]
pub struct UiPreviewResolutionRecord {
    pub resolution: UiPreviewResolution,
    pub kind: UiInteractionKind,
    pub node_key: String,
    pub domains: Vec<UiInvalidationDomain>,
    pub preview_revision: Revision,
    pub input_revision: Revision,
}

#[derive(Clone, Debug, PartialEq)]
struct ActivePreview {
    node_key: String,
    /// Authoritative input revision the preview was drawn against. Confirming
    /// requires that the authoritative frame has moved past it.
    input_revision: Revision,
    semantic_sequence: Option<u64>,
}

/// Structured record of one incremental update, per the design's diagnostics
/// contract. The CPU side fills everything it owns and leaves
/// `gpu_ranges_written` empty until the renderer reports its local ranges, so
/// neither layer can claim the other's work happened.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiIncrementalUpdateRecord {
    pub event: &'static str,
    pub cause: &'static str,
    pub input_revision: Revision,
    pub fragment_revision: Revision,
    pub input_keys: Vec<String>,
    pub binding_ids: Vec<u32>,
    pub node_keys: Vec<String>,
    pub domains: Vec<UiInvalidationDomain>,
    pub layout_rebuilt: bool,
    pub text_remeasured: bool,
    pub primitives_rebuilt: bool,
    pub preview_kind: Option<UiInteractionKind>,
    pub preview_revision: Revision,
    pub superseded_preview_kinds: Vec<UiInteractionKind>,
    pub gpu_ranges_written: Option<u64>,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
}

impl UiIncrementalUpdateRecord {
    pub fn cause_name(cause: UiChangeCause) -> &'static str {
        match cause {
            UiChangeCause::InputPublication => "input_publication",
            UiChangeCause::LocalInteractionPreview => "local_interaction_preview",
            UiChangeCause::LocalInteractionCommit => "local_interaction_commit",
            UiChangeCause::ProgramActivation => "program_activation",
        }
    }

    pub fn applied(impact: &UiImpactSet, delta: &UiFrameDelta) -> Self {
        let mut node_keys: Vec<String> = delta
            .changed_states
            .iter()
            .map(|state| state.node_key.clone())
            .collect();
        if let Some(node_key) = delta.preview_node_key.as_ref() {
            node_keys.push(node_key.clone());
        }
        node_keys.sort();
        node_keys.dedup();
        Self {
            event: "ui.incremental_update.applied",
            cause: Self::cause_name(impact.cause),
            input_revision: delta.input_revision,
            fragment_revision: impact.fragment_revision,
            input_keys: impact.input_keys.clone(),
            binding_ids: delta.executed_binding_ids.clone(),
            node_keys,
            domains: delta.domains_executed.clone(),
            layout_rebuilt: !delta.layout_unchanged,
            text_remeasured: delta
                .domains_executed
                .contains(&UiInvalidationDomain::TextLayout),
            primitives_rebuilt: delta.render_primitives_rebuilt,
            preview_kind: delta.preview_kind,
            preview_revision: delta.preview_revision,
            superseded_preview_kinds: delta.superseded_preview_kinds.clone(),
            gpu_ranges_written: None,
            status: "applied",
            code: None,
        }
    }

    pub fn rejected(impact: &UiImpactSet, code: &'static str) -> Self {
        Self {
            event: "ui.incremental_update.rejected",
            cause: Self::cause_name(impact.cause),
            input_revision: impact.input_revision,
            fragment_revision: impact.fragment_revision,
            input_keys: impact.input_keys.clone(),
            binding_ids: Vec::new(),
            node_keys: Vec::new(),
            domains: Vec::new(),
            layout_rebuilt: false,
            text_remeasured: false,
            primitives_rebuilt: false,
            preview_kind: impact.interaction_kind,
            preview_revision: Revision(0),
            superseded_preview_kinds: Vec::new(),
            gpu_ranges_written: None,
            status: "rejected",
            code: Some(code),
        }
    }
}

#[derive(Clone, Debug)]
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
    /// One active preview per interaction kind: a single pointer and focus
    /// owner can only predict one node per kind at a time.
    previews: BTreeMap<UiInteractionKind, ActivePreview>,
    preview_revision: Revision,
    renderer_epoch: u64,
}

impl UiRetainedFrame {
    /// Full materialization assembled from retained parts; equals the golden
    /// evaluator output for the same inputs. Previews are deliberately absent:
    /// they are renderer-local prediction, never authoritative CPU node state.
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

    /// Retained authoritative state per compiled node. The production refresh
    /// path reads the impacted entries from here instead of materializing a
    /// whole `UiCpuFrameOutput`.
    pub fn states(&self) -> &BTreeMap<String, UiCpuNodeState> {
        &self.states
    }

    pub fn previews(&self) -> impl Iterator<Item = (UiInteractionKind, &str)> {
        self.previews
            .iter()
            .map(|(kind, active)| (*kind, active.node_key.as_str()))
    }

    pub fn preview_revision(&self) -> Revision {
        self.preview_revision
    }

    pub fn renderer_epoch(&self) -> u64 {
        self.renderer_epoch
    }
}

#[derive(Clone, Debug)]
struct NodeSeed {
    visible: bool,
    enabled: bool,
    opacity: f32,
    scroll_offset: [f32; 2],
    literal_text: Option<UiTextHandle>,
}

pub fn evaluate_ui_program_initial(
    program: &UiProgram,
    inputs: &UiResolvedInputs,
    viewport: UiCpuViewport,
    local: &UiLocalPresentationState,
) -> UiRetainedFrame {
    let golden = crate::evaluate_ui_program(program, inputs, viewport, local);
    let literals = program
        .literal_texts
        .iter()
        .map(|entry| (entry.node_key.as_str(), entry.handle))
        .collect::<BTreeMap<_, _>>();
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
                    literal_text: literals.get(template.node_id.0.as_str()).copied(),
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
        previews: BTreeMap::new(),
        preview_revision: Revision(0),
        renderer_epoch: 0,
    }
}

pub fn apply_ui_impact_set(
    program: &UiProgram,
    retained: &mut UiRetainedFrame,
    inputs: &UiResolvedInputs,
    local: &UiLocalPresentationState,
    impact: &UiImpactSet,
) -> Result<UiFrameDelta, UiIncrementalError> {
    match impact.cause {
        UiChangeCause::InputPublication => {}
        // A preview is renderer prediction. It is applied through the same
        // impact-set entry point but never reads or writes authoritative
        // inputs, so it is handled in its own narrow path.
        UiChangeCause::LocalInteractionPreview => {
            return apply_local_interaction_preview(program, retained, impact);
        }
        // A commit is not a CPU-side consequence: the authoritative path is
        // semantic event -> domain -> input publication, which arrives as an
        // InputPublication set. Commit bookkeeping lives in the preview
        // resolution API.
        UiChangeCause::LocalInteractionCommit | UiChangeCause::ProgramActivation => {
            return Err(UiIncrementalError {
                code: error_codes::UNSUPPORTED_CAUSE,
            });
        }
    }
    if !impact.interaction_nodes.is_empty() {
        return Err(UiIncrementalError {
            code: error_codes::UNSUPPORTED_CAUSE,
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
            code: error_codes::STALE_FRAME,
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
                code: error_codes::UNKNOWN_BINDING,
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
                code: error_codes::UNKNOWN_BRANCH,
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
    // Authoritative state now describes these nodes, so any preview predicting
    // them must stop being drawn. The renderer restores them through the same
    // domains a resolution record would report.
    let mut superseded_preview_kinds: Vec<UiInteractionKind> = retained
        .previews
        .iter()
        .filter(|(_, active)| {
            changed_states
                .iter()
                .any(|state| state.node_key == active.node_key)
        })
        .map(|(kind, _)| *kind)
        .collect();
    if !superseded_preview_kinds.is_empty() {
        for kind in &superseded_preview_kinds {
            retained.previews.remove(kind);
        }
        retained.preview_revision = Revision(retained.preview_revision.0 + 1);
    }
    superseded_preview_kinds.sort();
    superseded_preview_kinds.dedup();
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
        preview_kind: None,
        preview_node_key: None,
        displaced_preview_node_key: None,
        preview_revision: retained.preview_revision,
        superseded_preview_kinds,
    })
}

fn apply_local_interaction_preview(
    program: &UiProgram,
    retained: &mut UiRetainedFrame,
    impact: &UiImpactSet,
) -> Result<UiFrameDelta, UiIncrementalError> {
    let Some(kind) = impact.interaction_kind else {
        return Err(UiIncrementalError {
            code: error_codes::UNSUPPORTED_CAUSE,
        });
    };
    let [node_key] = impact.interaction_nodes.as_slice() else {
        return Err(UiIncrementalError {
            code: error_codes::UNSUPPORTED_CAUSE,
        });
    };
    if retained.degraded || retained.program_revision != program.revision {
        return Err(UiIncrementalError {
            code: error_codes::STALE_FRAME,
        });
    }
    let epoch = impact.renderer_epoch.unwrap_or(retained.renderer_epoch);
    if retained.renderer_epoch != 0 && epoch != retained.renderer_epoch {
        // Previews belong to the renderer session that sampled them. Never map
        // an old epoch's prediction onto the new surface; the caller must
        // clear previews explicitly for the new epoch.
        return Err(UiIncrementalError {
            code: error_codes::PREVIEW_EPOCH_MISMATCH,
        });
    }
    if impact.input_revision != retained.input_revision {
        // The displayed authoritative frame has moved past this interaction, so
        // the prediction can never be confirmed. Drop it as part of rejecting.
        retained.previews.remove(&kind);
        return Err(UiIncrementalError {
            code: error_codes::STALE_PREVIEW,
        });
    }
    let Some(compiled) = program.dependency_index.interaction_impacts.get(node_key) else {
        return Err(UiIncrementalError {
            code: error_codes::UNKNOWN_INTERACTION_NODE,
        });
    };
    if !compiled.interaction_kinds.contains(&kind) {
        return Err(UiIncrementalError {
            code: error_codes::UNDECLARED_INTERACTION_KIND,
        });
    }
    let displaced = retained
        .previews
        .insert(
            kind,
            ActivePreview {
                node_key: node_key.clone(),
                input_revision: impact.input_revision,
                semantic_sequence: impact.semantic_sequence,
            },
        )
        .filter(|active| &active.node_key != node_key)
        .map(|active| active.node_key);
    retained.preview_revision = Revision(retained.preview_revision.0 + 1);
    if retained.renderer_epoch == 0 {
        retained.renderer_epoch = epoch;
    }
    Ok(UiFrameDelta {
        cause: impact.cause,
        // A preview never advances the authoritative revision.
        input_revision: retained.input_revision,
        executed_binding_ids: Vec::new(),
        evaluated_branch_keys: Vec::new(),
        domains_executed: preview_domains_for(std::slice::from_ref(&kind)),
        changed_states: Vec::new(),
        changed_semantic_targets: Vec::new(),
        render_primitives_rebuilt: false,
        layout_unchanged: true,
        preview_kind: Some(kind),
        preview_node_key: Some(node_key.clone()),
        displaced_preview_node_key: displaced,
        preview_revision: retained.preview_revision,
        superseded_preview_kinds: Vec::new(),
    })
}

/// Ends one active preview. `Confirmed` requires that the authoritative frame
/// has actually moved past the revision the preview predicted, which is what
/// keeps a local prediction from being mistaken for domain authority.
pub fn resolve_ui_interaction_preview(
    retained: &mut UiRetainedFrame,
    kind: UiInteractionKind,
    resolution: UiPreviewResolution,
) -> Result<UiPreviewResolutionRecord, UiIncrementalError> {
    let Some(active) = retained.previews.get(&kind).cloned() else {
        return Err(UiIncrementalError {
            code: error_codes::NO_ACTIVE_PREVIEW,
        });
    };
    if resolution == UiPreviewResolution::Confirmed
        && retained.input_revision.0 <= active.input_revision.0
    {
        return Err(UiIncrementalError {
            code: error_codes::CONFIRM_WITHOUT_AUTHORITY,
        });
    }
    retained.previews.remove(&kind);
    retained.preview_revision = Revision(retained.preview_revision.0 + 1);
    Ok(UiPreviewResolutionRecord {
        resolution,
        kind,
        node_key: active.node_key,
        domains: preview_domains_for(std::slice::from_ref(&kind)),
        preview_revision: retained.preview_revision,
        input_revision: active.input_revision,
    })
}

/// Clears every preview because the renderer session changed (epoch reset,
/// focus loss, device loss, or an abandoned capture). Returns one restore
/// record per dropped preview so the renderer can put every touched range back.
pub fn reset_ui_interaction_previews(
    retained: &mut UiRetainedFrame,
    renderer_epoch: u64,
) -> Vec<UiPreviewResolutionRecord> {
    if retained.previews.is_empty() {
        retained.renderer_epoch = renderer_epoch;
        return Vec::new();
    }
    retained.preview_revision = Revision(retained.preview_revision.0 + 1);
    retained.renderer_epoch = renderer_epoch;
    let records = retained
        .previews
        .iter()
        .map(|(kind, active)| UiPreviewResolutionRecord {
            resolution: UiPreviewResolution::Cancelled,
            kind: *kind,
            node_key: active.node_key.clone(),
            domains: preview_domains_for(std::slice::from_ref(kind)),
            preview_revision: retained.preview_revision,
            input_revision: active.input_revision,
        })
        .collect();
    retained.previews.clear();
    records
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

    // --- Phase C: renderer-local interaction previews --------------------

    const INTERACTION_FLOW: &str = "version 1
surface surface.interact revision 1
budget nodes=16 bindings=16 instances=16 text=8 glyphs=64 events=8 clips=8
input flag bool default false
input level_value i32 default 5
surface root row w 400 h 300
  switch tog checked $flag w 40 h 20
  slider level numeric $level_value w 100 h 20
  button act event app.act w 60 h 24
  panel plain w 40 h 40
";

    fn interaction_revision() -> UiProgramRevision {
        UiProgramRevision {
            program_id: "surface.interact".into(),
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

    fn interaction_harness() -> Harness {
        let document = parse_nui_flow(INTERACTION_FLOW).expect("interaction fixture must parse");
        let program_revision = interaction_revision();
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
        fn preview(
            &mut self,
            kind: UiInteractionKind,
            node_key: &str,
        ) -> Result<UiFrameDelta, UiIncrementalError> {
            self.preview_at(kind, node_key, 7)
        }

        fn preview_at(
            &mut self,
            kind: UiInteractionKind,
            node_key: &str,
            renderer_epoch: u64,
        ) -> Result<UiFrameDelta, UiIncrementalError> {
            let impact = UiImpactSet::from_local_interaction(
                &self.program,
                kind,
                node_key,
                renderer_epoch,
                Some(1),
                self.store.snapshot().input_revision,
                Revision(1),
            )?;
            apply_ui_impact_set(
                &self.program,
                &mut self.retained,
                &self.store.snapshot(),
                &UiLocalPresentationState::default(),
                &impact,
            )
        }
    }

    #[test]
    fn interaction_preview_leaves_the_authoritative_frame_untouched() {
        let mut harness = interaction_harness();
        let before = harness.retained.frame();
        let delta = harness
            .preview(UiInteractionKind::Hover, "act")
            .expect("button declares hover");
        assert_eq!(delta.cause, UiChangeCause::LocalInteractionPreview);
        assert!(delta.executed_binding_ids.is_empty());
        assert!(delta.evaluated_branch_keys.is_empty());
        assert!(delta.changed_states.is_empty());
        assert!(!delta.render_primitives_rebuilt);
        assert_eq!(
            delta.domains_executed,
            vec![
                UiInvalidationDomain::ColorInstances,
                UiInvalidationDomain::InteractionPresentation,
            ]
        );
        assert_eq!(
            delta.preview_kind,
            Some(UiInteractionKind::Hover),
            "the delta must name the preview it applied"
        );
        assert_eq!(delta.preview_node_key.as_deref(), Some("act"));
        assert_eq!(delta.input_revision, Revision(0));
        assert_eq!(harness.store.snapshot().input_revision, Revision(0));
        assert_eq!(harness.retained.frame(), before);
        assert_eq!(harness.retained.frame(), harness.golden());
    }

    #[test]
    fn one_preview_per_kind_and_the_displaced_node_is_reported() {
        let mut harness = interaction_harness();
        harness
            .preview(UiInteractionKind::Hover, "level")
            .expect("slider declares hover");
        let delta = harness
            .preview(UiInteractionKind::Hover, "act")
            .expect("button declares hover");
        assert_eq!(delta.displaced_preview_node_key.as_deref(), Some("level"));
        harness
            .preview(UiInteractionKind::Pressed, "act")
            .expect("button declares pressed");
        let previews: Vec<(UiInteractionKind, &str)> = harness.retained.previews().collect();
        assert_eq!(
            previews,
            vec![
                (UiInteractionKind::Hover, "act"),
                (UiInteractionKind::Pressed, "act"),
            ]
        );
    }

    #[test]
    fn undeclared_kinds_and_non_interactive_nodes_are_rejected() {
        let mut harness = interaction_harness();
        assert_eq!(
            harness
                .preview(UiInteractionKind::TogglePreview, "act")
                .unwrap_err()
                .code,
            error_codes::UNDECLARED_INTERACTION_KIND
        );
        assert_eq!(
            harness
                .preview(UiInteractionKind::Hover, "plain")
                .unwrap_err()
                .code,
            error_codes::UNKNOWN_INTERACTION_NODE
        );
        assert_eq!(harness.retained.previews().count(), 0);
        assert_eq!(harness.retained.preview_revision(), Revision(0));
    }

    #[test]
    fn confirm_requires_authority_while_rollback_and_cancel_always_clear() {
        let mut harness = interaction_harness();
        harness
            .preview(UiInteractionKind::TogglePreview, "tog")
            .expect("switch declares toggle preview");
        assert_eq!(
            resolve_ui_interaction_preview(
                &mut harness.retained,
                UiInteractionKind::TogglePreview,
                UiPreviewResolution::Confirmed,
            )
            .unwrap_err()
            .code,
            error_codes::CONFIRM_WITHOUT_AUTHORITY,
            "a prediction may never confirm itself"
        );
        assert_eq!(harness.retained.previews().count(), 1);
        let record = resolve_ui_interaction_preview(
            &mut harness.retained,
            UiInteractionKind::TogglePreview,
            UiPreviewResolution::RolledBack,
        )
        .expect("rollback must always be allowed");
        assert_eq!(record.node_key.as_str(), "tog");
        assert_eq!(
            record.domains,
            vec![
                UiInvalidationDomain::ColorInstances,
                UiInvalidationDomain::InteractionPresentation,
            ]
        );
        assert_eq!(harness.retained.previews().count(), 0);
        assert_eq!(
            resolve_ui_interaction_preview(
                &mut harness.retained,
                UiInteractionKind::TogglePreview,
                UiPreviewResolution::Cancelled,
            )
            .unwrap_err()
            .code,
            error_codes::NO_ACTIVE_PREVIEW
        );
    }

    #[test]
    fn authoritative_publication_supersedes_the_preview_it_predicted() {
        let mut harness = interaction_harness();
        harness
            .preview(UiInteractionKind::TogglePreview, "tog")
            .expect("toggle preview on a switch");
        harness
            .preview(UiInteractionKind::Pressed, "act")
            .expect("pressed on the button");
        let impact = harness.publish("flag", bool_input(true));
        let delta = harness.apply(&impact);
        assert_eq!(
            delta.superseded_preview_kinds,
            vec![UiInteractionKind::TogglePreview],
            "only the predicted node's preview is dropped"
        );
        assert!(
            delta
                .changed_states
                .iter()
                .any(|state| state.node_key == "tog")
        );
        // The button was untouched by authority, so its preview survives and is
        // now confirmable because the authoritative frame moved forward.
        let confirmed = resolve_ui_interaction_preview(
            &mut harness.retained,
            UiInteractionKind::Pressed,
            UiPreviewResolution::Confirmed,
        )
        .expect("authority advanced past the preview");
        assert_eq!(confirmed.node_key.as_str(), "act");
        assert_eq!(harness.retained.previews().count(), 0);
        assert_eq!(harness.retained.frame(), harness.golden());
    }

    #[test]
    fn stale_preview_is_rejected_and_its_overlay_is_dropped() {
        let mut harness = interaction_harness();
        harness
            .preview(UiInteractionKind::Hover, "act")
            .expect("button declares hover");
        let impact = harness.publish("flag", bool_input(true));
        harness.apply(&impact);
        let stale = UiImpactSet::from_local_interaction(
            &harness.program,
            UiInteractionKind::Hover,
            "act",
            7,
            Some(1),
            Revision(0),
            Revision(1),
        )
        .expect("declared kind");
        let error = apply_ui_impact_set(
            &harness.program,
            &mut harness.retained,
            &harness.store.snapshot(),
            &UiLocalPresentationState::default(),
            &stale,
        )
        .unwrap_err();
        assert_eq!(error.code, error_codes::STALE_PREVIEW);
        assert_eq!(
            harness.retained.previews().count(),
            0,
            "a preview that can never be confirmed must not stay drawn"
        );
        let record = UiIncrementalUpdateRecord::rejected(&stale, error.code);
        assert_eq!(record.status, "rejected");
        assert_eq!(record.code, Some(error_codes::STALE_PREVIEW));
    }

    #[test]
    fn epoch_reset_clears_every_preview_before_new_predictions() {
        let mut harness = interaction_harness();
        harness
            .preview(UiInteractionKind::Hover, "act")
            .expect("hover");
        harness
            .preview(UiInteractionKind::NumericPreview, "level")
            .expect("numeric preview");
        let records = reset_ui_interaction_previews(&mut harness.retained, 8);
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|record| record.resolution == UiPreviewResolution::Cancelled)
        );
        assert_eq!(harness.retained.renderer_epoch(), 8);
        assert_eq!(harness.retained.previews().count(), 0);
        assert_eq!(
            harness
                .preview(UiInteractionKind::Hover, "act")
                .unwrap_err()
                .code,
            error_codes::PREVIEW_EPOCH_MISMATCH,
            "old-epoch samples may not map onto the new renderer session"
        );
        harness
            .preview_at(UiInteractionKind::Hover, "act", 8)
            .expect("new epoch previews apply");
        assert_eq!(harness.retained.previews().count(), 1);
    }

    #[test]
    fn repeated_slider_preview_costs_no_revision_and_one_commit_publishes_once() {
        let mut harness = interaction_harness();
        for step in 1..=5 {
            let delta = harness
                .preview(UiInteractionKind::NumericPreview, "level")
                .expect("slider declares numeric preview");
            assert_eq!(delta.input_revision, Revision(0));
            assert_eq!(delta.preview_revision, Revision(step));
            assert_eq!(delta.displaced_preview_node_key, None);
        }
        assert_eq!(harness.store.snapshot().input_revision, Revision(0));
        assert_eq!(harness.retained.frame(), harness.golden());
        let impact = harness.publish("level_value", UiInputValue::I32 { value: 9 });
        let delta = harness.apply(&impact);
        assert_eq!(delta.input_revision, Revision(1));
        assert_eq!(
            delta
                .changed_states
                .iter()
                .map(|state| state.node_key.as_str())
                .collect::<Vec<_>>(),
            vec!["level"]
        );
        assert_eq!(harness.retained.frame(), harness.golden());
    }

    #[test]
    fn interaction_record_links_the_node_to_its_authoritative_input() {
        let mut harness = interaction_harness();
        let toggle = &harness.program.dependency_index.interaction_impacts["tog"];
        assert_eq!(toggle.controlled_input_keys, vec!["flag".to_owned()]);
        assert_eq!(
            harness.program.dependency_index.interaction_impacts["act"].semantic_intents,
            vec!["app.act".to_owned()]
        );
        let expected: Vec<u32> = harness
            .program
            .binding_records
            .iter()
            .filter(|binding| binding.node_key == "tog")
            .map(|binding| binding.binding_id)
            .collect();
        let impact = harness.publish("flag", bool_input(true));
        assert_eq!(impact.binding_ids, expected);
    }

    #[test]
    fn commit_and_activation_causes_never_reach_the_cpu_delta_path() {
        let mut harness = interaction_harness();
        for cause in [
            UiChangeCause::LocalInteractionCommit,
            UiChangeCause::ProgramActivation,
        ] {
            let mut impact = UiImpactSet::from_input_publication(
                &harness.program,
                &[],
                Revision(1),
                Revision(1),
            );
            impact.cause = cause;
            assert_eq!(
                apply_ui_impact_set(
                    &harness.program,
                    &mut harness.retained,
                    &harness.store.snapshot(),
                    &UiLocalPresentationState::default(),
                    &impact,
                )
                .unwrap_err()
                .code,
                error_codes::UNSUPPORTED_CAUSE
            );
        }
    }

    #[test]
    fn incremental_update_record_reports_the_applied_scope() {
        let mut harness = interaction_harness();
        let impact = harness.publish("flag", bool_input(true));
        let delta = harness.apply(&impact);
        let value =
            serde_json::to_value(UiIncrementalUpdateRecord::applied(&impact, &delta)).unwrap();
        assert_eq!(value["event"], "ui.incremental_update.applied");
        assert_eq!(value["cause"], "input_publication");
        assert_eq!(value["status"], "applied");
        assert_eq!(value["input_revision"], 1);
        assert_eq!(value["input_keys"][0], "flag");
        assert_eq!(value["gpu_ranges_written"], serde_json::Value::Null);
        assert_eq!(value["text_remeasured"], false);
        let domains: Vec<&str> = value["domains"]
            .as_array()
            .unwrap()
            .iter()
            .map(|domain| domain.as_str().unwrap())
            .collect();
        assert!(
            domains.contains(&"node_state") && domains.contains(&"color_instances"),
            "the active binding's domains must be reported: {domains:?}"
        );
        assert!(
            !domains.contains(&"text_layout") && !domains.contains(&"hit_target"),
            "an Active binding must not claim text or hit regeneration: {domains:?}"
        );
        assert_eq!(
            value["node_keys"],
            serde_json::json!(["tog"]),
            "only the node whose authoritative state actually moved is reported"
        );
    }
}
