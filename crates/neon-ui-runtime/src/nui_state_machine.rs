//! Deterministic executor for NUI Flow's finite presentation statecharts.

use std::collections::BTreeMap;

use neon_protocol::Revision;
use neon_ui_schema::{
    NuiFlowDocument, NuiFlowDragAxis, NuiFlowStateTrigger, UiBranchPredicate, UiDropPlacement,
    UiInputValue, UiProgramSemanticEvent, UiResolvedInputs,
};

use crate::UiLocalPresentationState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NuiFlowStateMachineRuntime {
    states: BTreeMap<String, String>,
    revision: Revision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NuiFlowStateTransitionResult {
    pub machine_key: String,
    pub previous_state: String,
    pub state: String,
    pub emitted_intent: Option<String>,
    pub revision: Revision,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NuiFlowDragUpdate {
    pub drag_key: String,
    pub source_node_key: String,
    pub offset: [f32; 2],
    pub intent: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NuiFlowDropResult {
    pub drop_key: String,
    pub source_node_key: String,
    pub target_node_key: String,
    pub placement: UiDropPlacement,
    pub presentation_template_key: Option<String>,
    pub offset: [f32; 2],
    pub intent: String,
}

#[derive(Clone, Debug)]
struct ActiveDrag {
    key: String,
    source_node_key: String,
    axis: NuiFlowDragAxis,
    snap: f32,
    threshold: f32,
    start: [f32; 2],
    origin: [f32; 2],
}

#[derive(Clone, Debug, Default)]
pub struct NuiFlowDragController {
    offsets: BTreeMap<String, [f32; 2]>,
    active: Option<ActiveDrag>,
}

impl NuiFlowDragController {
    pub fn offset(&self, source_node_key: &str) -> [f32; 2] {
        self.offsets
            .get(source_node_key)
            .copied()
            .unwrap_or([0.0; 2])
    }
    pub fn apply_to_presentation_state(&self, state: &mut UiLocalPresentationState) {
        state.drag_offsets = self.offsets.clone();
    }

    pub fn begin(
        &mut self,
        document: &NuiFlowDocument,
        drag_key: &str,
        pointer: [f32; 2],
    ) -> Result<NuiFlowDragUpdate, &'static str> {
        let declaration = document
            .drags
            .iter()
            .find(|drag| drag.key == drag_key)
            .ok_or("unknown_drag")?;
        let origin = self.offset(&declaration.source_node_key);
        self.active = Some(ActiveDrag {
            key: declaration.key.clone(),
            source_node_key: declaration.source_node_key.clone(),
            axis: declaration.axis,
            snap: declaration.snap,
            threshold: declaration.threshold,
            start: pointer,
            origin,
        });
        Ok(NuiFlowDragUpdate {
            drag_key: declaration.key.clone(),
            source_node_key: declaration.source_node_key.clone(),
            offset: origin,
            intent: "ui.drag.begin".into(),
        })
    }

    pub fn update(
        &mut self,
        pointer: [f32; 2],
        parent_size: [f32; 2],
    ) -> Option<NuiFlowDragUpdate> {
        let active = self.active.as_ref()?.clone();
        let mut delta = [pointer[0] - active.start[0], pointer[1] - active.start[1]];
        if delta[0].hypot(delta[1]) < active.threshold {
            return None;
        }
        match active.axis {
            NuiFlowDragAxis::Horizontal => delta[1] = 0.0,
            NuiFlowDragAxis::Vertical => delta[0] = 0.0,
            NuiFlowDragAxis::Both => {}
        }
        let mut offset = [active.origin[0] + delta[0], active.origin[1] + delta[1]];
        if active.snap > 0.0 {
            offset = [
                (offset[0] / active.snap).round() * active.snap,
                (offset[1] / active.snap).round() * active.snap,
            ];
        }
        offset[0] = offset[0].clamp(-active.origin[0], parent_size[0] - active.origin[0]);
        offset[1] = offset[1].clamp(-active.origin[1], parent_size[1] - active.origin[1]);
        self.offsets.insert(active.source_node_key.clone(), offset);
        Some(NuiFlowDragUpdate {
            drag_key: active.key.clone(),
            source_node_key: active.source_node_key.clone(),
            offset,
            intent: "ui.drag.update".into(),
        })
    }

    pub fn end(&mut self) -> Option<NuiFlowDragUpdate> {
        let active = self.active.take()?;
        Some(NuiFlowDragUpdate {
            drag_key: active.key,
            source_node_key: active.source_node_key.clone(),
            offset: self.offset(&active.source_node_key),
            intent: "ui.drag.end".into(),
        })
    }

    pub fn drop_on(
        &self,
        document: &NuiFlowDocument,
        drop_key: &str,
    ) -> Result<NuiFlowDropResult, &'static str> {
        let active = self.active.as_ref().ok_or("drag_not_active")?;
        let drop = document
            .drops
            .iter()
            .find(|drop| drop.key == drop_key)
            .ok_or("unknown_drop")?;
        if drop.accepts_drag_key != active.key {
            return Err("drop_rejects_drag");
        }
        Ok(NuiFlowDropResult {
            drop_key: drop.key.clone(),
            source_node_key: active.source_node_key.clone(),
            target_node_key: drop.target_node_key.clone(),
            placement: drop.placement,
            presentation_template_key: drop.presentation_template_key.clone(),
            offset: self.offset(&active.source_node_key),
            intent: drop.emit_intent.clone(),
        })
    }
}

impl NuiFlowStateMachineRuntime {
    pub fn new(document: &NuiFlowDocument) -> Self {
        Self {
            states: document
                .state_machines
                .iter()
                .map(|machine| (machine.key.clone(), machine.initial_state.clone()))
                .collect(),
            revision: Revision(0),
        }
    }

    pub fn state(&self, machine_key: &str) -> Option<&str> {
        self.states.get(machine_key).map(String::as_str)
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn presentation_state(&self) -> UiLocalPresentationState {
        UiLocalPresentationState {
            revision: self.revision,
            machine_states: self.states.clone(),
            drag_offsets: BTreeMap::new(),
        }
    }

    pub fn synchronize(
        &mut self,
        document: &NuiFlowDocument,
        inputs: &UiResolvedInputs,
    ) -> Vec<NuiFlowStateTransitionResult> {
        document
            .state_machines
            .iter()
            .filter_map(|machine| {
                let transition = machine.transitions.iter().find(|transition| {
                    matches!(transition.trigger, NuiFlowStateTrigger::Sync)
                        && transition
                            .predicate
                            .as_ref()
                            .is_some_and(|predicate| predicate_matches(predicate, inputs))
                })?;
                self.transition(
                    machine.key.as_str(),
                    &transition.target_state,
                    transition.emit_intent.clone(),
                )
            })
            .collect()
    }

    pub fn dispatch(
        &mut self,
        document: &NuiFlowDocument,
        inputs: &UiResolvedInputs,
        intent: &str,
    ) -> Vec<NuiFlowStateTransitionResult> {
        document.state_machines.iter().filter_map(|machine| {
            let transition = machine.transitions.iter().find(|transition| matches!(&transition.trigger, NuiFlowStateTrigger::Intent { name } if name == intent) && transition.predicate.as_ref().is_none_or(|predicate| predicate_matches(predicate, inputs)))?;
            self.transition(machine.key.as_str(), &transition.target_state, transition.emit_intent.clone())
        }).collect()
    }

    pub fn dispatch_semantic_event(
        &mut self,
        document: &NuiFlowDocument,
        inputs: &UiResolvedInputs,
        event: &UiProgramSemanticEvent,
    ) -> Vec<NuiFlowStateTransitionResult> {
        self.dispatch(document, inputs, &event.intent)
    }

    fn transition(
        &mut self,
        machine_key: &str,
        target_state: &str,
        emitted_intent: Option<String>,
    ) -> Option<NuiFlowStateTransitionResult> {
        let previous_state = self.states.get(machine_key)?.clone();
        if previous_state == target_state {
            return None;
        }
        self.revision = Revision(self.revision.0 + 1);
        self.states.insert(machine_key.into(), target_state.into());
        Some(NuiFlowStateTransitionResult {
            machine_key: machine_key.into(),
            previous_state,
            state: target_state.into(),
            emitted_intent,
            revision: self.revision,
        })
    }
}

fn predicate_matches(predicate: &UiBranchPredicate, inputs: &UiResolvedInputs) -> bool {
    match predicate {
        UiBranchPredicate::Bool {
            input_key,
            expected,
        } => {
            matches!(inputs.values.get(input_key).map(|value| &value.value), Some(UiInputValue::Bool { value }) if value == expected)
        }
        UiBranchPredicate::EnumEquals { input_key, variant } => {
            matches!(inputs.values.get(input_key).map(|value| &value.value), Some(UiInputValue::Enum { value }) if value == variant)
        }
        UiBranchPredicate::MachineState { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_ui_schema::{
        UiInputValueSource, UiProgramCapability, UiProgramCapabilityOwner,
        UiProgramCapabilityStatus, UiProgramRevision, UiResolvedInputValue,
    };

    fn inputs(can_publish: bool, workspace_state: &str) -> UiResolvedInputs {
        UiResolvedInputs {
            program_revision: UiProgramRevision {
                program_id: "test".into(),
                revision: Revision(1),
                schema_version: 1,
                capabilities: vec![UiProgramCapability {
                    name: "ui.program.v1".into(),
                    version: 1,
                    owner: UiProgramCapabilityOwner::SharedContract,
                    status: UiProgramCapabilityStatus::Supported,
                }],
            },
            input_revision: Revision(0),
            values: BTreeMap::from([
                (
                    "can_publish".into(),
                    UiResolvedInputValue {
                        value: UiInputValue::Bool { value: can_publish },
                        source: UiInputValueSource::ReliableExternal,
                        last_update_revision: Revision(0),
                    },
                ),
                (
                    "workspace_state".into(),
                    UiResolvedInputValue {
                        value: UiInputValue::Enum {
                            value: workspace_state.into(),
                        },
                        source: UiInputValueSource::ReliableExternal,
                        last_update_revision: Revision(0),
                    },
                ),
            ]),
            changed_slots: Vec::new(),
        }
    }

    #[test]
    fn state_machine_synchronizes_and_emits_only_declared_intents() {
        let document = crate::parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/asset-review-workbench.nui"
        ))
        .unwrap();
        let mut runtime = NuiFlowStateMachineRuntime::new(&document);
        assert_eq!(
            runtime.synchronize(&document, &inputs(false, "ready"))[0].state,
            "ready"
        );
        let transition =
            runtime.dispatch(&document, &inputs(true, "ready"), "asset.review.publish");
        assert_eq!(transition[0].state, "publishing");
        assert_eq!(
            transition[0].emitted_intent.as_deref(),
            Some("asset.review.publish")
        );
    }

    #[test]
    fn machine_state_branch_changes_the_evaluated_view_after_an_intent() {
        let source = "version 1\nsurface surface.demo revision 1\ninput enabled bool default true\nmachine demo initial idle\nstate demo active\non demo demo.activate -> active emit demo.activate\ndrag demo-drag source idle-panel axis both snap 8 threshold 3 within parent\nsurface demo-root column w 320 h 180\n  branch idle-panel in demo.idle\n    text idle-label value \"Idle\"\n  branch active-panel in demo.active\n    text active-label value \"Active\"\n";
        let document = crate::parse_nui_flow(source).unwrap();
        let revision = UiProgramRevision {
            program_id: "surface.demo".into(),
            revision: Revision(1),
            schema_version: 1,
            capabilities: vec![UiProgramCapability {
                name: "ui.program.v1".into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Supported,
            }],
        };
        let program = crate::compile_nui_flow_program(&document, revision.clone()).unwrap();
        let inputs = crate::UiInputStore::activate(revision, document.input_schema.clone())
            .unwrap()
            .snapshot();
        let mut machine = NuiFlowStateMachineRuntime::new(&document);
        let viewport = neon_ui_schema::UiCpuViewport {
            logical_bounds: neon_ui_schema::UiBounds {
                x: 0.0,
                y: 0.0,
                width: 320.0,
                height: 180.0,
            },
            revision: Revision(0),
        };
        let idle =
            crate::evaluate_ui_program(&program, &inputs, viewport, &machine.presentation_state());
        assert!(idle
            .nodes
            .iter()
            .any(|node| node.node_key == "idle-panel" && node.visible));
        assert!(idle
            .nodes
            .iter()
            .any(|node| node.node_key == "active-panel" && !node.visible));
        let mut drag = NuiFlowDragController::default();
        drag.begin(&document, "demo-drag", [4.0, 4.0]).unwrap();
        drag.update([25.0, 19.0], [320.0, 180.0]).unwrap();
        let mut dragged_presentation = machine.presentation_state();
        drag.apply_to_presentation_state(&mut dragged_presentation);
        let dragged =
            crate::evaluate_ui_program(&program, &inputs, viewport, &dragged_presentation);
        let idle_layout = dragged
            .logical_layout
            .iter()
            .find(|record| record.node_key == "idle-panel")
            .unwrap();
        assert_eq!([idle_layout.bounds.x, idle_layout.bounds.y], [24.0, 16.0]);
        machine.dispatch(&document, &inputs, "demo.activate");
        let active =
            crate::evaluate_ui_program(&program, &inputs, viewport, &machine.presentation_state());
        assert!(active
            .nodes
            .iter()
            .any(|node| node.node_key == "idle-panel" && !node.visible));
        assert!(active
            .nodes
            .iter()
            .any(|node| node.node_key == "active-panel" && node.visible));
    }

    #[test]
    fn drag_controller_snaps_motion_and_drives_accepted_state() {
        let document = crate::parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/asset-review-workbench.nui"
        ))
        .unwrap();
        let mut drag = NuiFlowDragController::default();
        let mut machine = NuiFlowStateMachineRuntime::new(&document);
        let input = inputs(true, "ready");
        let begin = drag
            .begin(&document, "inspector-drag", [10.0, 10.0])
            .unwrap();
        let begin_transition = machine.dispatch(&document, &input, &begin.intent);
        assert_eq!(begin_transition[0].state, "dragging");
        let update = drag.update([31.0, 25.0], [800.0, 600.0]).unwrap();
        assert_eq!(update.offset, [24.0, 16.0]);
        let end = drag.end().unwrap();
        let end_transition = machine.dispatch(&document, &input, &end.intent);
        assert_eq!(end_transition[0].state, "accepted");
        assert_eq!(
            end_transition[0].emitted_intent.as_deref(),
            Some("asset.review.inspector.position.accept")
        );
    }

    #[test]
    fn drop_target_proposes_reparent_without_mutating_the_flow_tree() {
        let document = crate::parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/kanban-reparent-workbench.nui"
        ))
        .unwrap();
        let original_parent = document.ir.root.children[1].children[0].children[1]
            .node_id
            .0
            .clone();
        let mut drag = NuiFlowDragController::default();
        drag.begin(&document, "backlog-card-drag", [16.0, 16.0])
            .unwrap();
        drag.update([49.0, 31.0], [1200.0, 700.0]).unwrap();
        let drop = drag.drop_on(&document, "progress-drop").unwrap();
        assert_eq!(drop.source_node_key, "backlog-card-01");
        assert_eq!(drop.target_node_key, "in-progress-panel");
        assert_eq!(drop.intent, "workspace.card.reparent");
        assert_eq!(drop.placement, UiDropPlacement::Into);
        assert_eq!(
            drop.presentation_template_key.as_deref(),
            Some("progress-template")
        );
        assert_eq!(
            document.ir.root.children[1].children[0].children[1]
                .node_id
                .0,
            original_parent
        );
    }

    #[test]
    fn complex_kanban_declares_multiple_drag_paths_and_no_blocked_drop_path() {
        let document = crate::parse_nui_flow(include_str!(
            "../../../tests/fixtures/ui/kanban-reparent-workbench.nui"
        ))
        .unwrap();
        let mut drag = NuiFlowDragController::default();
        for (drag_key, drop_key, source, target) in [
            (
                "backlog-card-drag",
                "progress-drop",
                "backlog-card-01",
                "in-progress-panel",
            ),
            (
                "audit-card-drag",
                "progress-audit-drop",
                "backlog-card-02",
                "in-progress-panel",
            ),
            (
                "bindings-card-drag",
                "done-bindings-drop",
                "backlog-card-03",
                "done-panel",
            ),
        ] {
            drag.begin(&document, drag_key, [16.0, 16.0]).unwrap();
            drag.update([48.0, 32.0], [1440.0, 900.0]).unwrap();
            let proposal = drag.drop_on(&document, drop_key).unwrap();
            assert_eq!(proposal.source_node_key, source);
            assert_eq!(proposal.target_node_key, target);
            assert_eq!(proposal.intent, "workspace.card.reparent");
            if drop_key == "progress-drop" || drop_key == "progress-audit-drop" {
                assert_eq!(
                    proposal.presentation_template_key.as_deref(),
                    Some("progress-template")
                );
            }
            if drop_key == "done-bindings-drop" {
                assert_eq!(
                    proposal.presentation_template_key.as_deref(),
                    Some("accepted-template")
                );
            }
            drag.end();
        }
        drag.begin(&document, "backlog-card-drag", [16.0, 16.0])
            .unwrap();
        assert_eq!(drag.drop_on(&document, "blocked-drop"), Err("unknown_drop"));
    }
}
