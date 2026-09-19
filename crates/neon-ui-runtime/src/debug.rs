//! Headless authoring inspection, dry-run, and deterministic replay support.
//! It is intentionally built on the CPU program backend and never serializes
//! renderer handles, physical coordinates, or unapproved text contents.

use std::collections::BTreeMap;

use neon_protocol::Revision;
use neon_ui_schema::{
    UiCpuFrameOutput, UiCpuViewport, UiDebugBundle, UiDiagnostic, UiDiagnosticSeverity,
    UiInputFrame, UiInputSchema, UiIrDocument, UiIrOutlineEntry, UiIrOutlinePage, UiIrPatch,
    UiLayoutDiagnosticSnapshot, UiNodeInspection, UiPatchDryRun, UiProgram, UiProgramDescription,
    UiProgramRevision, UiProgramSemanticEvent, UiResolvedInputs, UiTextHandle,
    UiTextHandleDiagnostic,
};

use crate::{
    UiInputStore, UiInputWriter, UiLocalPresentationState, UiProgramSemanticEventRouter,
    UiRepeatStore, UiTextRegistry, apply_nui_ir_patch, compile_ui_program, evaluate_ui_program,
};

#[derive(Clone, Debug, PartialEq)]
pub struct UiReplayResult {
    pub frames: Vec<UiCpuFrameOutput>,
    pub diagnostics: Vec<UiDiagnostic>,
    pub matched_expected_frames: bool,
}

/// Machine-readable answer to "what invalidates this node". It reports only
/// stable node keys, binding ids, branch keys and invalidation domains.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct UiNodeImpactSummary {
    pub node_key: String,
    pub binding_ids: Vec<u32>,
    pub bound_properties: Vec<String>,
    pub branch_keys: Vec<String>,
    pub input_keys: Vec<String>,
    pub domains: Vec<neon_ui_schema::UiInvalidationDomain>,
}

/// Runtime-owned state for the public debug methods. The domain is still
/// responsible for supplying frames; this facade only validates and explains.
pub struct UiDebugSession {
    document: UiIrDocument,
    program: UiProgram,
    inputs: UiInputStore,
    initial_inputs: UiResolvedInputs,
    text: UiTextRegistry,
    repeats: UiRepeatStore,
    events: UiProgramSemanticEventRouter,
    input_timeline: Vec<UiInputFrame>,
    repeat_timeline: Vec<neon_ui_schema::UiRepeatFrame>,
    event_timeline: Vec<UiProgramSemanticEvent>,
    viewport: UiCpuViewport,
    source_hash: String,
}

impl UiDebugSession {
    pub fn activate(
        document: UiIrDocument,
        revision: UiProgramRevision,
        schema: UiInputSchema,
        text: UiTextRegistry,
        viewport: UiCpuViewport,
        flow_source: &str,
        renderer_epoch: u64,
    ) -> Result<Self, String> {
        let program = compile_ui_program(&document, revision.clone(), &schema)
            .map_err(|error| error.message)?;
        let inputs =
            UiInputStore::activate(revision, schema).map_err(|error| error.message.to_owned())?;
        let events =
            UiProgramSemanticEventRouter::new(program.clone(), inputs.snapshot(), renderer_epoch);
        let initial_inputs = inputs.snapshot();
        Ok(Self {
            document,
            program,
            inputs,
            initial_inputs,
            text,
            repeats: UiRepeatStore::default(),
            events,
            input_timeline: Vec::new(),
            repeat_timeline: Vec::new(),
            event_timeline: Vec::new(),
            viewport,
            source_hash: hash(flow_source),
        })
    }

    pub fn outline(&self, offset: u32, limit: u32) -> UiIrOutlinePage {
        let limit = limit.clamp(1, 256);
        let frame = self.frame();
        let entries = self
            .program
            .nodes
            .iter()
            .skip(offset as usize)
            .take(limit as usize)
            .map(|node| {
                let template = self
                    .program
                    .node_templates
                    .iter()
                    .find(|value| value.node_id.0 == node.key)
                    .expect("compiled template");
                let bindings = self
                    .program
                    .binding_records
                    .iter()
                    .filter(|binding| binding.node_key == node.key)
                    .map(|binding| format!("{}:{:?}", binding.input_key, binding.property))
                    .collect();
                let branch_key = self
                    .program
                    .branch_records
                    .iter()
                    .find(|branch| branch.node_range.contains(&node.key))
                    .map(|branch| branch.branch_key.clone());
                let template_key = self
                    .program
                    .template_records
                    .iter()
                    .find(|record| record.node_range.contains(&node.key))
                    .map(|record| record.template_key.clone());
                UiIrOutlineEntry {
                    node_key: node.key.clone(),
                    parent_key: node.parent_key.clone(),
                    kind: node.kind.clone(),
                    static_properties: BTreeMap::from([
                        ("visible".into(), serde_json::json!(template.visible)),
                        ("enabled".into(), serde_json::json!(template.enabled)),
                    ]),
                    binding_summary: bindings,
                    branch_key,
                    template_key,
                    source_span: node.source_span.clone(),
                    diagnostic_count: frame
                        .diagnostics
                        .iter()
                        .filter(|diagnostic| diagnostic.node_key.as_deref() == Some(&node.key))
                        .count() as u32,
                }
            })
            .collect();
        let total = self.program.nodes.len() as u32;
        let next = offset.saturating_add(limit);
        UiIrOutlinePage {
            entries,
            offset,
            limit,
            total,
            next_offset: (next < total).then_some(next),
        }
    }

    pub fn node(&self, key: &str) -> Option<UiNodeInspection> {
        let node = self
            .program
            .nodes
            .iter()
            .find(|node| node.key == key)?
            .clone();
        let frame = self.frame();
        let state = frame.nodes.iter().find(|state| state.node_key == key)?;
        let template = self
            .program
            .node_templates
            .iter()
            .find(|node| node.node_id.0 == key)?;
        let mut provenance = BTreeMap::new();
        for binding in self
            .program
            .binding_records
            .iter()
            .filter(|binding| binding.node_key == key)
        {
            provenance.insert(
                format!("{:?}", binding.property),
                format!("input:{}", binding.input_key),
            );
        }
        let hidden = !state.visible;
        Some(UiNodeInspection {
            node: node.clone(),
            declared_properties: serde_json::json!(template),
            effective_properties: serde_json::json!(state),
            provenance,
            layout: frame
                .logical_layout
                .iter()
                .find(|layout| layout.node_key == key)
                .cloned(),
            clip: frame.clips.get(key).copied(),
            visibility_reason: if hidden {
                "branch predicate or visible binding resolved false".into()
            } else {
                "visible".into()
            },
            resources: self.program_resource_refs(key),
            events: self
                .program
                .event_records
                .iter()
                .filter(|event| event.node_key == key)
                .cloned()
                .collect(),
            source_span: node.source_span,
            diagnostics: frame
                .diagnostics
                .into_iter()
                .filter(|diagnostic| diagnostic.node_key.as_deref() == Some(key))
                .collect(),
        })
    }

    pub fn validate_flow(&self, flow: &str) -> Vec<UiDiagnostic> {
        match crate::parse_nui_flow(flow) {
            Ok(_) => Vec::new(),
            Err(error) => error
                .diagnostics
                .into_iter()
                .map(|item| {
                    diagnostic(
                        &item.code,
                        &item.message,
                        None,
                        self.program.revision.revision,
                    )
                })
                .collect(),
        }
    }
    pub fn dry_run_patch(&self, patch: &UiIrPatch) -> UiPatchDryRun {
        match apply_nui_ir_patch(&self.document, patch) {
            Ok(document) => match compile_ui_program(
                &document,
                next_revision(&self.program.revision),
                self.inputs.schema(),
            ) {
                Ok(program) => UiPatchDryRun {
                    accepted: true,
                    base_revision: self.document.revision,
                    resulting_revision: document.revision,
                    diff: serde_json::json!({"node_count": {"before": self.program.nodes.len(), "after": program.nodes.len()}}),
                    impacted_nodes: program.nodes.iter().map(|node| node.key.clone()).collect(),
                    required_input_schema_changes: Vec::new(),
                    budget: program.resource_budget,
                    diagnostics: Vec::new(),
                },
                Err(error) => rejected_patch(
                    self.document.revision,
                    self.program.resource_budget.clone(),
                    error.code,
                    error.message,
                ),
            },
            Err(error) => rejected_patch(
                self.document.revision,
                self.program.resource_budget.clone(),
                "ui_ir_patch_rejected",
                error
                    .diagnostics
                    .into_iter()
                    .map(|item| item.message)
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        }
    }
    pub fn layout_diagnostics(&self) -> UiLayoutDiagnosticSnapshot {
        let frame = self.frame();
        let visibility_reasons = frame
            .nodes
            .iter()
            .map(|node| {
                (
                    node.node_key.clone(),
                    if node.visible {
                        "visible".into()
                    } else {
                        "branch predicate or visible binding resolved false".into()
                    },
                )
            })
            .collect();
        UiLayoutDiagnosticSnapshot {
            program_revision: self.program.revision.clone(),
            input_revision: frame.input_revision,
            logical_layout: frame.logical_layout,
            clips: frame.clips,
            visibility_reasons,
            diagnostics: frame.diagnostics,
            gpu_differential_mismatches: Vec::new(),
        }
    }
    pub fn input_schema(&self) -> &UiInputSchema {
        self.inputs.schema()
    }
    pub fn input_snapshot(&self) -> UiResolvedInputs {
        self.inputs.snapshot()
    }
    /// Compiled invalidation scope of one input slot. An input with no bindings
    /// still returns a record with empty impacts so "no effect" is provable
    /// rather than inferred from a missing entry.
    pub fn input_impact(&self, key: &str) -> Option<&neon_ui_schema::UiInputImpact> {
        self.program.dependency_index.input_impacts.get(key)
    }
    /// Compiled renderer preview scope of one node: which interaction kinds it
    /// declares, which domains a preview touches, and which intents and inputs
    /// its authoritative events carry.
    pub fn interaction_impact(&self, key: &str) -> Option<&neon_ui_schema::UiInteractionImpact> {
        self.program.dependency_index.interaction_impacts.get(key)
    }
    /// Reverse lookup: everything that can invalidate one node.
    pub fn node_impact(&self, key: &str) -> Option<UiNodeImpactSummary> {
        if !self.program.nodes.iter().any(|node| node.key == key) {
            return None;
        }
        let mut binding_ids = Vec::new();
        let mut bound_properties = Vec::new();
        for binding in self
            .program
            .binding_records
            .iter()
            .filter(|binding| binding.node_key == key)
        {
            binding_ids.push(binding.binding_id);
            bound_properties.push(format!("{:?}", binding.property));
        }
        let branch_keys: Vec<String> = self
            .program
            .branch_records
            .iter()
            .filter(|branch| branch.node_range.iter().any(|node| node == key))
            .map(|branch| branch.branch_key.clone())
            .collect();
        let mut input_keys = Vec::new();
        let mut domains: Vec<neon_ui_schema::UiInvalidationDomain> = Vec::new();
        for (input_key, impact) in &self.program.dependency_index.input_impacts {
            let reaches = impact.affected_node_keys.iter().any(|node| node == key)
                || impact
                    .binding_impacts
                    .iter()
                    .any(|binding| binding.node_key == key)
                || impact
                    .branch_keys
                    .iter()
                    .any(|branch| branch_keys.contains(branch));
            if reaches {
                input_keys.push(input_key.clone());
                domains.extend(impact.domains.iter().copied());
            }
        }
        input_keys.sort();
        neon_ui_schema::sort_dedup_domains(&mut domains);
        Some(UiNodeImpactSummary {
            node_key: key.to_owned(),
            binding_ids,
            bound_properties,
            branch_keys,
            input_keys,
            domains,
        })
    }
    pub fn text_handle(
        &self,
        handle: UiTextHandle,
        include_text: bool,
    ) -> (UiTextHandleDiagnostic, Option<String>) {
        let status = self.text.handle_diagnostic(handle);
        let content = if include_text {
            self.text
                .snapshot(true)
                .records
                .into_iter()
                .find(|record| record.handle == handle)
                .map(|record| record.text)
        } else {
            None
        };
        (status, content)
    }
    pub fn event_trace(&self) -> &[neon_ui_schema::UiEventTraceRecord] {
        self.events.trace()
    }
    pub fn description(&self) -> UiProgramDescription {
        UiProgramDescription {
            revision: self.program.revision.clone(),
            layout_hash: self.program.layout_hash.clone(),
            active_capabilities: self.program.revision.capabilities.clone(),
            resource_budget: self.program.resource_budget.clone(),
            runtime_high_water_marks: BTreeMap::from([
                ("nodes".into(), self.program.nodes.len() as u32),
                (
                    "input_slots".into(),
                    self.inputs.schema().slots.len() as u32,
                ),
            ]),
            overflow_counters: BTreeMap::new(),
        }
    }
    pub fn bundle(&self) -> UiDebugBundle {
        UiDebugBundle {
            version: 1,
            flow_source_hash: self.source_hash.clone(),
            ir_hash: hash(&serde_json::to_string(&self.document).expect("IR serializes")),
            program: self.program.clone(),
            schema: self.inputs.schema().clone(),
            initial_inputs: self.initial_inputs.clone(),
            input_timeline: self.input_timeline.clone(),
            repeat_timeline: self.repeat_timeline.clone(),
            text_registry: self.text.debug_snapshot(),
            event_timeline: self.event_timeline.clone(),
            viewport: self.viewport,
            expected_frames: vec![self.frame()],
            diagnostics: Vec::new(),
            gpu_readbacks: None,
        }
    }
    pub fn apply_input(&mut self, frame: UiInputFrame) -> Result<(), String> {
        self.inputs
            .apply_with_text_registry(UiInputWriter::External, frame.clone(), &self.text)
            .map_err(|error| error.message.to_owned())?;
        self.input_timeline.push(frame);
        self.events.replace_resolved_inputs(self.inputs.snapshot());
        Ok(())
    }
    pub fn apply_repeat(&mut self, frame: neon_ui_schema::UiRepeatFrame) -> Result<(), String> {
        self.repeats
            .apply(&self.program, frame.clone())
            .map_err(|error| error.message)?;
        self.repeat_timeline.push(frame);
        Ok(())
    }
    pub fn validate_event(&mut self, event: UiProgramSemanticEvent) {
        self.events.validate(&event);
        self.event_timeline.push(event);
    }
    fn frame(&self) -> UiCpuFrameOutput {
        evaluate_ui_program(
            &self.program,
            &self.inputs.snapshot(),
            self.viewport,
            &UiLocalPresentationState::default(),
        )
    }
    fn program_resource_refs(&self, key: &str) -> Vec<neon_ui_schema::UiProgramResource> {
        let Some(node) = self
            .program
            .node_templates
            .iter()
            .find(|node| node.node_id.0 == key)
        else {
            return Vec::new();
        };
        let mut resources = Vec::new();
        if let Some(image) = &node.image {
            resources.push(neon_ui_schema::UiProgramResource {
                key: format!("{}:{}", image.project_id, image.asset_id),
                kind: neon_ui_schema::UiProgramResourceKind::Image,
                has_fallback: false,
                asset_ref: Some(image.clone()),
            });
        }
        if let Some(surface) = &node.surface {
            resources.push(neon_ui_schema::UiProgramResource {
                key: surface.target_id.clone(),
                kind: neon_ui_schema::UiProgramResourceKind::RenderSurface,
                has_fallback: false,
                asset_ref: None,
            });
        }
        resources
    }
}

pub fn replay_debug_bundle(bundle: &UiDebugBundle) -> Result<UiReplayResult, String> {
    let mut store = UiInputStore::activate(bundle.program.revision.clone(), bundle.schema.clone())
        .map_err(|error| error.message.to_owned())?;
    let mut frames = vec![evaluate_ui_program(
        &bundle.program,
        &store.snapshot(),
        bundle.viewport,
        &UiLocalPresentationState::default(),
    )];
    for input in &bundle.input_timeline {
        store
            .apply(UiInputWriter::External, input.clone())
            .map_err(|error| error.message.to_owned())?;
        frames.push(evaluate_ui_program(
            &bundle.program,
            &store.snapshot(),
            bundle.viewport,
            &UiLocalPresentationState::default(),
        ));
    }
    let matched_expected_frames = bundle
        .expected_frames
        .last()
        .is_none_or(|expected| frames.last() == Some(expected));
    let diagnostics = if matched_expected_frames {
        Vec::new()
    } else {
        vec![diagnostic(
            "ui_debug_replay_mismatch",
            "replayed logical frame differs from the captured expected frame",
            None,
            bundle.program.revision.revision,
        )]
    };
    Ok(UiReplayResult {
        frames,
        diagnostics,
        matched_expected_frames,
    })
}

fn next_revision(revision: &UiProgramRevision) -> UiProgramRevision {
    let mut next = revision.clone();
    next.revision = Revision(next.revision.0 + 1);
    next
}
fn hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64-{hash:016x}")
}
fn diagnostic(
    code: &str,
    message: &str,
    node_key: Option<String>,
    revision: Revision,
) -> UiDiagnostic {
    UiDiagnostic {
        code: code.into(),
        severity: UiDiagnosticSeverity::Error,
        message: message.into(),
        node_key,
        input_key: None,
        source_span: None,
        revision,
    }
}
fn rejected_patch(
    base: Revision,
    budget: neon_ui_schema::UiResourceBudget,
    code: &str,
    message: String,
) -> UiPatchDryRun {
    UiPatchDryRun {
        accepted: false,
        base_revision: base,
        resulting_revision: base,
        diff: serde_json::json!({}),
        impacted_nodes: Vec::new(),
        required_input_schema_changes: Vec::new(),
        budget,
        diagnostics: vec![diagnostic(code, &message, None, base)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_nui_flow;
    use neon_ui_schema::{
        UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiBounds, UiCpuViewport,
        UiInputSchema, UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
        UiProgramRevision,
    };

    const FLOW: &str = "version 1
surface surface.debug revision 1
budget nodes=8 bindings=8 instances=8 text=4 glyphs=32 events=4 clips=4
input show bool default false
input amount i32 default 3
input unused bool default false
surface root row w 200 h 200
  panel hidden-panel visible $show w 40 h 40
  slider gauge numeric $amount w 60 h 20
  button act event app.act w 40 h 20
";

    fn session() -> UiDebugSession {
        let document = parse_nui_flow(FLOW).expect("debug fixture must parse");
        let schema: UiInputSchema = document.input_schema.clone();
        let revision = UiProgramRevision {
            program_id: "surface.debug".into(),
            revision: Revision(1),
            schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: vec![UiProgramCapability {
                name: UI_PROGRAM_CAPABILITY_NAME.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Supported,
            }],
        };
        let viewport = UiCpuViewport {
            logical_bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 200.0,
            },
            revision: Revision(1),
        };
        UiDebugSession::activate(
            document.ir,
            revision,
            schema,
            UiTextRegistry::new("debug", 8, 64).unwrap(),
            viewport,
            FLOW,
            1,
        )
        .expect("debug session must activate")
    }

    #[test]
    fn impact_queries_answer_from_the_compiled_graph() {
        let session = session();
        let show = session.input_impact("show").expect("declared input slot");
        assert!(
            show.affected_node_keys.contains(&"hidden-panel".to_owned()),
            "the bound node must be listed: {:?}",
            show.affected_node_keys
        );
        assert!(
            session
                .input_impact("unused")
                .is_some_and(|impact| impact.binding_ids.is_empty()),
            "an unbound input must still answer with an empty, diagnosable impact"
        );
        let gauge = session.node_impact("gauge").expect("compiled node");
        assert_eq!(gauge.input_keys, vec!["amount".to_owned()]);
        assert_eq!(gauge.binding_ids.len(), 1);
        assert!(
            gauge
                .domains
                .contains(&neon_ui_schema::UiInvalidationDomain::NodeState),
            "{:?}",
            gauge.domains
        );
        let act = session.interaction_impact("act").expect("interactive node");
        assert_eq!(act.semantic_intents, vec!["app.act".to_owned()]);
        assert!(session.interaction_impact("hidden-panel").is_none());
        assert!(session.node_impact("missing-node").is_none());
    }
}
