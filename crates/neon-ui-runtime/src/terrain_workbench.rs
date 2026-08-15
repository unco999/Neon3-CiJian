//! Canonical NUI Flow terrain-workbench fixture and headless scenario helpers.
//! Business values are supplied only through the typed input/repeat boundaries.

use std::collections::BTreeMap;

use neon_protocol::Revision;
use neon_ui_schema::{
    UiCpuViewport, UiInputChange, UiInputFrame, UiInputValue, UiProgramCapability,
    UiProgramCapabilityOwner, UiProgramCapabilityStatus, UiProgramRevision, UiRepeatFrame,
    UiRepeatRow, UiProgramResource, UiProgramResourceKind,
    UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_CAPABILITY_NAME,
    UI_PROGRAM_SCHEMA_VERSION, UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
    UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
};

use crate::{compile_nui_flow_program, parse_nui_flow};

pub const TERRAIN_WORKBENCH_FLOW: &str = include_str!("../../../tests/fixtures/ui/terrain-workbench.nui");

pub fn terrain_workbench_program_revision() -> UiProgramRevision {
    UiProgramRevision {
        program_id: "surface.editor.terrain-workbench".into(), revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: [
            UI_PROGRAM_CAPABILITY_NAME,
            UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
            UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
            UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
        ].into_iter().map(|name| UiProgramCapability {
            name: name.into(), version: 1, owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        }).collect(),
    }
}

pub fn terrain_workbench_document() -> neon_ui_schema::NuiFlowDocument {
    let mut document = parse_nui_flow(TERRAIN_WORKBENCH_FLOW)
        .expect("checked-in terrain workbench Flow must validate");
    document.ir.resources.push(UiProgramResource {
        key: "terrain-render-surface".into(),
        kind: UiProgramResourceKind::RenderSurface,
        has_fallback: true,
    });
    document
}

pub fn terrain_workbench_program() -> neon_ui_schema::UiProgram {
    compile_nui_flow_program(&terrain_workbench_document(), terrain_workbench_program_revision())
        .expect("checked-in terrain workbench Flow must compile")
}

pub fn terrain_workbench_viewport(width: f32, height: f32, revision: u64) -> UiCpuViewport {
    UiCpuViewport {
        logical_bounds: neon_ui_schema::UiBounds { x: 0.0, y: 0.0, width, height },
        revision: Revision(revision),
    }
}

pub fn terrain_workbench_input_frame(expected_input_revision: Revision, request_id: &str, changes: Vec<UiInputChange>) -> UiInputFrame {
    UiInputFrame {
        program_revision: terrain_workbench_program_revision(), expected_input_revision,
        request_id: request_id.into(), idempotency_key: format!("workbench:{}", request_id), changes,
    }
}

pub fn terrain_workbench_material_rows(revision: u64, count: u32) -> UiRepeatFrame {
    UiRepeatFrame {
        template_key: "material-rows".into(), list_revision: Revision(revision),
        expected_program_revision: terrain_workbench_program_revision(),
        rows: (0..count).map(|index| UiRepeatRow {
            stable_row_key: format!("material-{}", index + 1),
            values: BTreeMap::from([("row_key".into(), UiInputValue::U32 { value: index + 1 })]),
            semantic_payload: BTreeMap::from([("material_ref".into(), format!("material:{}", index + 1))]),
        }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apply_nui_ir_patch, evaluate_ui_program, parse_nui_flow_patch, UiInputStore, UiInputWriter, UiLocalPresentationState, UiRepeatStore, UiTextRegistry};
    use neon_ui_schema::{UiProgramSemanticEvent, UiProgramSemanticEventKind, UiProgramSemanticEventStatus, UiSemanticInteractionMetadata, UiTextSourceCategory};

    #[test]
    fn terrain_workbench_default_update_branch_repeat_and_patch_scenarios_are_headless() {
        let document = terrain_workbench_document();
        let program = terrain_workbench_program();
        let mut store = UiInputStore::activate(terrain_workbench_program_revision(), document.input_schema.clone()).unwrap();
        let defaults = evaluate_ui_program(&program, &store.snapshot(), terrain_workbench_viewport(1440.0, 900.0, 1), &UiLocalPresentationState { revision: Revision(0) });
        assert!(defaults.nodes.iter().any(|node| node.node_key == "terrain-render-surface"));
        assert!(defaults.nodes.iter().any(|node| node.node_key == "loading-state" && node.visible));

        let update = terrain_workbench_input_frame(Revision(0), "domain-ready", vec![
            UiInputChange { key: "workbench_state".into(), value: UiInputValue::Enum { value: "ready".into() } },
            UiInputChange { key: "can_commit".into(), value: UiInputValue::Bool { value: true } },
        ]);
        store.apply(UiInputWriter::External, update).unwrap();
        let ready = evaluate_ui_program(&program, &store.snapshot(), terrain_workbench_viewport(720.0, 900.0, 2), &UiLocalPresentationState { revision: Revision(0) });
        assert!(ready.nodes.iter().any(|node| node.node_key == "ready-state" && node.visible));
        assert!(ready.nodes.iter().any(|node| node.node_key == "loading-state" && !node.visible));

        let mut events = crate::UiProgramSemanticEventRouter::new(program.clone(), store.snapshot(), 7);
        let activate = UiProgramSemanticEvent {
            event_id: "tool-select".into(), kind: UiProgramSemanticEventKind::Activate,
            intent: "terrain.tool.select".into(), source_node_key: "tool-select".into(),
            payload: BTreeMap::new(), program_revision: terrain_workbench_program_revision(),
            input_revision: Revision(1), request_id: "tool-request".into(), idempotency_key: "tool-select-1".into(),
            interaction: UiSemanticInteractionMetadata { interaction_id: "pointer-capture-1".into(), sequence: 1, renderer_epoch: 7 },
        };
        assert_eq!(events.validate(&activate).status, UiProgramSemanticEventStatus::Accepted);
        let mut stale_event = activate;
        stale_event.event_id = "tool-select-stale".into();
        stale_event.idempotency_key = "tool-select-stale".into();
        stale_event.input_revision = Revision(0);
        assert_eq!(events.validate(&stale_event).status, UiProgramSemanticEventStatus::Rejected);

        let mut repeats = UiRepeatStore::default();
        let rows = repeats.apply(&program, terrain_workbench_material_rows(1, 9)).unwrap();
        assert_eq!((rows.accepted_rows, rows.overflow_rows), (8, 1));
        assert!(!rows.diagnostics.is_empty());

        let stale = terrain_workbench_input_frame(Revision(0), "stale", Vec::new());
        assert!(store.apply(UiInputWriter::External, stale).is_err());
        let patch = parse_nui_flow_patch("@ revision 1\n~ inspector visible false\n").unwrap();
        assert_eq!(apply_nui_ir_patch(&document.ir, &patch).unwrap().revision, Revision(2));
    }

    #[test]
    fn terrain_workbench_dynamic_text_uses_replacement_handles_not_frame_strings() {
        let document = terrain_workbench_document();
        let mut registry = UiTextRegistry::new("terrain-workbench", 8, 256).unwrap();
        let title = registry.insert_dynamic(Revision(0), "Terrain Alpha".into()).unwrap();
        let mut store = UiInputStore::activate(
            terrain_workbench_program_revision(), document.input_schema,
        ).unwrap();
        let update = terrain_workbench_input_frame(Revision(0), "title", vec![
            UiInputChange { key: "project_title".into(), value: UiInputValue::TextHandle { value: title } },
        ]);
        store.apply_with_text_registry(UiInputWriter::External, update, &registry).unwrap();
        registry.replace_dynamic(Revision(1), title, "Terrain Beta".into()).unwrap();
        assert_eq!(registry.handle_diagnostic(title).status, neon_ui_schema::UiTextHandleStatus::Ready);
        assert_eq!(registry.snapshot(true).records[0].category, UiTextSourceCategory::Dynamic);
    }
}
