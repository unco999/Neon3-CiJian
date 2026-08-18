//! Generic host boundary for one active compiled UI program.
//!
//! This module validates UI contracts only. It neither identifies nor invokes
//! a domain owner for semantic intents.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use neon_protocol::ClientIdentity;
use neon_ui_schema::{
    UiDataGridInputFrame, UiFragment, UiHostInbound, UiHostPresentationUpdate, UiHostPublication,
    UiInputSchema, UiProgram, UiProgramDragDropEvent, UiProgramInputSnapshot,
    UiProgramSemanticEvent, UiWindowRequest,
};
use serde::{Deserialize, Serialize};

use crate::{
    UiDataGridStore, UiInputStore, UiInputStoreError, UiInputWriter, UiVariableEventPublisher,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiHostAdapterError {
    pub code: &'static str,
    pub message: &'static str,
}

/// Host-owned declaration metadata needed to validate inbound renderer data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiHostAdapterConfig {
    pub program: UiProgram,
    pub input_schema: UiInputSchema,
}

impl From<UiInputStoreError> for UiHostAdapterError {
    fn from(error: UiInputStoreError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UiHostPublicationResult {
    pub snapshot: UiProgramInputSnapshot,
    pub changed_slots: Vec<String>,
    pub accepted_grid_sources: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiHostInboundResult {
    WindowRequest(UiWindowRequest),
    SemanticIntent(UiProgramSemanticEvent),
    DragDrop(UiProgramDragDropEvent),
    DataGridCell(neon_ui_schema::UiSemanticEvent),
}

/// Owns the active program's resolved inputs and bounded grid windows.
#[derive(Clone, Debug)]
pub struct UiHostAdapter {
    program: UiProgram,
    inputs: UiInputStore,
    grids: UiDataGridStore,
    renderer_epoch: u64,
    publication_results: HashMap<String, UiHostPublicationResult>,
    /// Optional directed-event forwarder. When present and the active Flow
    /// declared `emitevent` variables, `apply_publication` publishes
    /// `flow.<flow_name>.<variable_key>` observations to `neon-eventd`.
    publisher: Option<UiVariableEventPublisher>,
}

impl UiHostAdapter {
    pub fn activate(
        program: UiProgram,
        schema: UiInputSchema,
        renderer_epoch: u64,
    ) -> Result<Self, UiHostAdapterError> {
        program
            .revision
            .validate_baseline()
            .map_err(|_| UiHostAdapterError {
                code: "ui_host_invalid_program",
                message: "active program revision is invalid",
            })?;
        if !program.data_grid_records.iter().all(|grid| {
            !grid.node_key.trim().is_empty()
                && !grid.source_key.trim().is_empty()
                && grid.max_window_rows > 0
                && grid.row_height > 0
                && grid.overscan <= grid.max_window_rows
                && !grid.columns.is_empty()
                && grid.columns.iter().all(|column| column.validate())
                && program.nodes.iter().any(|node| {
                    node.key == grid.node_key && node.kind == neon_ui_schema::UiNodeKind::DataGrid
                })
                && schema
                    .grid_slots
                    .iter()
                    .any(|slot| slot.key == grid.source_key)
        }) {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_program",
                message: "program grid declaration is invalid or absent from the input schema",
            });
        }
        let mut sources = HashSet::new();
        if program
            .data_grid_records
            .iter()
            .any(|grid| !sources.insert(grid.source_key.as_str()))
        {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_program",
                message: "program declares a grid source more than once",
            });
        }
        let inputs = UiInputStore::activate(program.revision.clone(), schema)?;
        Ok(Self {
            program,
            inputs,
            grids: UiDataGridStore::default(),
            renderer_epoch,
            publication_results: HashMap::new(),
            publisher: None,
        })
    }

    pub fn program(&self) -> &UiProgram {
        &self.program
    }

    /// Attaches the UI Runtime's directed-event publisher. The event name is
    /// derived from the active Flow's `flow <name>` declaration and each input
    /// slot's `emitevent` attribute: only declared variables emit
    /// `flow.<flow_name>.<variable_key>`, and only when an `neon-eventd`
    /// endpoint is provided.
    pub fn with_event_publisher(
        mut self,
        eventd_endpoint: Option<SocketAddr>,
        client: ClientIdentity,
    ) -> Self {
        let schema = self.inputs.schema();
        let flow_name = schema.flow_name.clone();
        let publisher = UiVariableEventPublisher::new(
            eventd_endpoint,
            client,
            if flow_name.is_empty() {
                "ui_runtime".to_owned()
            } else {
                flow_name.clone()
            },
            schema.schema_id.clone(),
            flow_name,
            schema.emit_event_keys.clone(),
        );
        self.publisher = Some(publisher);
        self
    }

    pub fn input_schema(&self) -> &UiInputSchema {
        self.inputs.schema()
    }

    pub fn snapshot(&self) -> UiProgramInputSnapshot {
        UiProgramInputSnapshot {
            scalar_inputs: self.inputs.snapshot(),
            grid_inputs: self
                .program
                .data_grid_records
                .iter()
                .filter_map(|grid| {
                    self.grids
                        .frame(&grid.source_key)
                        .cloned()
                        .map(|frame| UiDataGridInputFrame {
                            source_key: grid.source_key.clone(),
                            frame,
                        })
                })
                .collect(),
        }
    }

    pub fn activate_from_snapshot(
        program: UiProgram,
        schema: UiInputSchema,
        snapshot: UiProgramInputSnapshot,
        renderer_epoch: u64,
    ) -> Result<Self, UiHostAdapterError> {
        let mut adapter = Self::activate(program, schema, renderer_epoch)?;
        adapter.inputs.restore_snapshot(snapshot.scalar_inputs)?;
        adapter.seed_grid_inputs(snapshot.grid_inputs)?;
        Ok(adapter)
    }

    /// Hydrates renderer-visible grid windows submitted before this adapter was
    /// activated. It is declaration state, not a host mutation.
    pub fn seed_grid_inputs(
        &mut self,
        grid_inputs: Vec<UiDataGridInputFrame>,
    ) -> Result<(), UiHostAdapterError> {
        let mut seen = HashSet::new();
        if grid_inputs
            .iter()
            .any(|input| !seen.insert(input.source_key.as_str()))
        {
            return Err(UiHostAdapterError {
                code: "ui_host_duplicate_grid_input",
                message: "initial fragment updates a grid source more than once",
            });
        }
        let mut grids = self.grids.clone();
        for input in grid_inputs {
            grids
                .apply(&self.program, input)
                .map_err(|error| UiHostAdapterError {
                    code: error.code,
                    message: "initial grid input is invalid for the active program",
                })?;
        }
        self.grids = grids;
        Ok(())
    }

    pub fn apply_publication(
        &mut self,
        publication: UiHostPublication,
    ) -> Result<UiHostPublicationResult, UiHostAdapterError> {
        let key = publication.scalar_frame.idempotency_key.clone();
        if let Some(result) = self.publication_results.get(&key) {
            return Ok(result.clone());
        }
        if key.trim().is_empty() {
            return Err(UiHostAdapterError {
                code: "invalid_request",
                message: "publication idempotency key is required",
            });
        }
        let mut seen = HashSet::new();
        if publication
            .grid_inputs
            .iter()
            .any(|input| !seen.insert(input.source_key.as_str()))
        {
            return Err(UiHostAdapterError {
                code: "ui_host_duplicate_grid_input",
                message: "publication updates a grid source more than once",
            });
        }

        let mut candidate_inputs = self.inputs.clone();
        let mut candidate_grids = self.grids.clone();
        let scalar = candidate_inputs.apply(UiInputWriter::External, publication.scalar_frame)?;
        let mut accepted_grid_sources = Vec::with_capacity(publication.grid_inputs.len());
        for grid_input in publication.grid_inputs {
            candidate_grids
                .apply(&self.program, grid_input.clone())
                .map_err(|error| UiHostAdapterError {
                    code: error.code,
                    message: "grid input publication was rejected",
                })?;
            accepted_grid_sources.push(grid_input.source_key);
        }

        self.inputs = candidate_inputs;
        self.grids = candidate_grids;
        if let Some(publisher) = &self.publisher {
            // Best-effort directed observation; never rolls back the applied frame.
            let _ = publisher.publish_variable_changes(&scalar.variable_changes);
        }
        let result = UiHostPublicationResult {
            snapshot: self.snapshot(),
            changed_slots: scalar.changed_slots,
            accepted_grid_sources,
        };
        self.publication_results.insert(key, result.clone());
        Ok(result)
    }

    /// Applies a host-owned sparse input frame without pretending it was a UI
    /// interaction. Semantic clicks use `ui.host.inbound`; ECS state updates use
    /// this separate reliable input path.
    pub fn apply_external_input(
        &mut self,
        frame: neon_ui_schema::UiInputFrame,
    ) -> Result<UiHostPublicationResult, UiHostAdapterError> {
        self.apply_publication(UiHostPublication {
            scalar_frame: frame,
            grid_inputs: Vec::new(),
            presentation_update: None,
        })
    }

    pub fn apply_presentation_update(
        &self,
        update: UiHostPresentationUpdate,
        active_fragment: &UiFragment,
    ) -> Result<(Self, UiFragment), UiHostAdapterError> {
        let Some(next_revision) = active_fragment.revision.0.checked_add(1) else {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_fragment_revision",
                message: "active fragment revision cannot be advanced",
            });
        };
        if update.expected_fragment_revision != active_fragment.revision {
            return Err(UiHostAdapterError {
                code: "ui_host_stale_fragment_revision",
                message: "presentation update fragment revision is stale",
            });
        }
        if update.replacement_fragment.fragment_id != active_fragment.fragment_id {
            return Err(UiHostAdapterError {
                code: "ui_host_fragment_identity_change",
                message: "presentation update cannot change the active fragment identity",
            });
        }
        if update.replacement_fragment.revision != neon_protocol::Revision(next_revision) {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_fragment_revision",
                message: "presentation replacement must use the next fragment revision",
            });
        }
        if update.replacement_program != self.program
            || update.replacement_input_schema != *self.input_schema()
        {
            return Err(UiHostAdapterError {
                code: "ui_host_presentation_state_change",
                message: "presentation update must preserve the active program and input schema",
            });
        }
        if update.replacement_fragment.validate().is_err() {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_fragment",
                message: "host presentation replacement is not a valid fragment",
            });
        }
        Ok((self.clone(), update.replacement_fragment))
    }

    pub fn validate_inbound(
        &self,
        inbound: UiHostInbound,
    ) -> Result<UiHostInboundResult, UiHostAdapterError> {
        match inbound {
            UiHostInbound::WindowRequest { request } => {
                self.validate_window_request(&request)?;
                Ok(UiHostInboundResult::WindowRequest(request))
            }
            UiHostInbound::SemanticIntent { event } => {
                self.validate_semantic_intent(&event)?;
                Ok(UiHostInboundResult::SemanticIntent(event))
            }
            UiHostInbound::DragDrop {
                event,
                active_fragment,
            } => {
                let fragment = active_fragment.into_fragment();
                if fragment.validate().is_err() {
                    return Err(UiHostAdapterError {
                        code: "ui_host_invalid_active_fragment",
                        message: "drag/drop active fragment context is invalid",
                    });
                }
                self.validate_drag_drop(&event)?;
                Ok(UiHostInboundResult::DragDrop(event))
            }
            UiHostInbound::DataGridCell { event } => {
                let Some(target) = event.data_grid_cell.as_ref() else {
                    return Err(UiHostAdapterError {
                        code: "ui_host_invalid_data_grid_cell",
                        message: "DataGrid cell event has no cell target",
                    });
                };
                let Some(frame) = self.grids.frame(&target.source_key) else {
                    return Err(UiHostAdapterError {
                        code: "ui_host_grid_unavailable",
                        message: "DataGrid cell event has no active grid frame",
                    });
                };
                if event.renderer_epoch != self.renderer_epoch
                    || !frame
                        .window_rows
                        .iter()
                        .any(|row| row.stable_row_key == target.stable_row_key)
                    || !self.program.data_grid_records.iter().any(|grid| {
                        grid.source_key == target.source_key
                            && grid
                                .columns
                                .iter()
                                .any(|column| column.key == target.column_key)
                    })
                {
                    return Err(UiHostAdapterError {
                        code: "ui_host_invalid_data_grid_cell",
                        message: "DataGrid cell event is not active in the declared grid",
                    });
                }
                Ok(UiHostInboundResult::DataGridCell(event))
            }
        }
    }

    fn validate_window_request(&self, request: &UiWindowRequest) -> Result<(), UiHostAdapterError> {
        let UiWindowRequest::DataGrid { request } = request;
        let grid = self
            .program
            .data_grid_records
            .iter()
            .find(|grid| grid.source_key == request.source_key)
            .ok_or(UiHostAdapterError {
                code: "ui_host_unknown_grid_source",
                message: "window request references an undeclared grid source",
            })?;
        let active = self
            .grids
            .frame(&request.source_key)
            .ok_or(UiHostAdapterError {
                code: "ui_host_grid_unavailable",
                message: "window request has no active grid frame",
            })?;
        if request.renderer_epoch != self.renderer_epoch
            || request.fragment.id.0.trim().is_empty()
            || request.source_key.trim().is_empty()
            || request.sequence == 0
            || request.max_window_rows == 0
            || request.max_window_rows > grid.max_window_rows
        {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_window_request",
                message: "window request is malformed or exceeds grid capacity",
            });
        }
        if request.expected_list_revision != active.list_revision {
            return Err(UiHostAdapterError {
                code: "ui_host_stale_grid_revision",
                message: "window request list revision is stale",
            });
        }
        Ok(())
    }

    fn validate_semantic_intent(
        &self,
        event: &UiProgramSemanticEvent,
    ) -> Result<(), UiHostAdapterError> {
        if event.event_id.trim().is_empty()
            || event.request_id.trim().is_empty()
            || event.idempotency_key.trim().is_empty()
            || event.intent.trim().is_empty()
            || event.source_node_key.trim().is_empty()
            || event.interaction.interaction_id.trim().is_empty()
        {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_semantic_intent",
                message: "semantic intent identity is incomplete",
            });
        }
        if event.program_revision != self.program.revision
            || event.input_revision != self.inputs.snapshot().input_revision
        {
            return Err(UiHostAdapterError {
                code: "ui_host_stale_semantic_intent",
                message: "semantic intent program or input revision is stale",
            });
        }
        if event.interaction.renderer_epoch != self.renderer_epoch {
            return Err(UiHostAdapterError {
                code: "ui_host_renderer_epoch_mismatch",
                message: "semantic intent renderer epoch is not active",
            });
        }
        if !self
            .program
            .nodes
            .iter()
            .any(|node| node.key == event.source_node_key)
            || !self.program.event_records.iter().any(|declaration| {
                declaration.node_key == event.source_node_key && declaration.intent == event.intent
            })
        {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_semantic_intent",
                message: "semantic intent is not declared by the active program",
            });
        }
        Ok(())
    }

    fn validate_drag_drop(&self, event: &UiProgramDragDropEvent) -> Result<(), UiHostAdapterError> {
        if event.event_id.trim().is_empty()
            || event.request_id.trim().is_empty()
            || event.idempotency_key.trim().is_empty()
            || event.drag_key.trim().is_empty()
            || event.drop_key.trim().is_empty()
            || event.intent.trim().is_empty()
            || event.interaction.interaction_id.trim().is_empty()
        {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_drag_drop",
                message: "drag/drop identity is incomplete",
            });
        }
        if event.program_revision != self.program.revision
            || event.input_revision != self.inputs.snapshot().input_revision
        {
            return Err(UiHostAdapterError {
                code: "ui_host_stale_drag_drop",
                message: "drag/drop program or input revision is stale",
            });
        }
        if event.interaction.renderer_epoch != self.renderer_epoch {
            return Err(UiHostAdapterError {
                code: "ui_host_renderer_epoch_mismatch",
                message: "drag/drop renderer epoch is not active",
            });
        }
        let drag = self
            .program
            .drag_records
            .iter()
            .find(|record| record.key == event.drag_key)
            .ok_or(UiHostAdapterError {
                code: "ui_host_invalid_drag_drop",
                message: "drag key is not declared by the active program",
            })?;
        let drop = self
            .program
            .drop_records
            .iter()
            .find(|record| record.key == event.drop_key)
            .ok_or(UiHostAdapterError {
                code: "ui_host_invalid_drag_drop",
                message: "drop key is not declared by the active program",
            })?;
        if drag.source_node_key != event.payload.source_key
            || drop.target_node_key != event.payload.target_key
            || drop.accepts_drag_key != drag.key
            || drop.intent != event.intent
            || drop.placement != event.payload.placement
            || drop.presentation_template_key != event.payload.presentation_template_key
        {
            return Err(UiHostAdapterError {
                code: "ui_host_invalid_drag_drop",
                message: "drag/drop payload does not match the active program declaration",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use neon_protocol::Revision;
    use neon_ui_schema::{
        UiBounds, UiDataGridCell, UiDataGridColumn, UiDataGridFrame, UiDataGridPresentation,
        UiDataGridRecord, UiDataGridWindowRequest, UiDataGridWindowRow, UiFragmentId,
        UiGridInputSlot, UiHostFragmentContext, UiInputChange, UiInputKind, UiInputPacking,
        UiInputSlot, UiInputUpdateClass, UiInputValue, UiNode, UiNodeId, UiNodeKind,
        UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
        UiProgramDragRecord, UiProgramDropRecord, UiProgramEventDeclaration, UiProgramNode,
        UiProgramRevision, UiProgramSemanticEventKind, UiResourceBudget,
        UiSemanticInteractionMetadata, UiStyle,
    };

    use super::*;

    fn revision() -> UiProgramRevision {
        UiProgramRevision {
            program_id: "program".into(),
            revision: Revision(1),
            schema_version: 1,
            capabilities: vec![UiProgramCapability {
                name: "ui.program.v1".into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Supported,
            }],
        }
    }

    fn adapter() -> UiHostAdapter {
        let revision = revision();
        let schema = UiInputSchema {
            schema_id: "inputs".into(),
            version: 1,
            layout_hash: "layout".into(),
            slots: vec![UiInputSlot {
                key: "enabled".into(),
                kind: UiInputKind::Bool,
                default_value: UiInputValue::Bool { value: false },
                update_class: UiInputUpdateClass::ReliableExternal,
                semantic_label: "enabled".into(),
                packing: UiInputPacking {
                    alignment: 4,
                    lanes: 1,
                    offset: 0,
                    representation: neon_ui_schema::UiGpuScalarRepresentation::Bool32,
                },
            }],
            grid_slots: vec![UiGridInputSlot { key: "grid".into() }],
            flow_name: String::new(),
            emit_event_keys: Vec::new(),
        };
        let program = UiProgram {
            revision: revision.clone(),
            nodes: vec![
                UiProgramNode {
                    key: "control".into(),
                    parent_key: None,
                    kind: neon_ui_schema::UiNodeKind::Button,
                    source_span: None,
                },
                UiProgramNode {
                    key: "grid_node".into(),
                    parent_key: None,
                    kind: neon_ui_schema::UiNodeKind::DataGrid,
                    source_span: None,
                },
            ],
            node_templates: Vec::new(),
            literal_texts: Vec::new(),
            layout_records: Vec::new(),
            binding_records: Vec::new(),
            branch_records: Vec::new(),
            template_records: Vec::new(),
            data_grid_records: vec![UiDataGridRecord {
                node_key: "grid_node".into(),
                source_key: "grid".into(),
                max_window_rows: 2,
                row_height: 1,
                overscan: 0,
                columns: vec![UiDataGridColumn {
                    key: "value".into(),
                    label: "Value".into(),
                    width: 1,
                    presentation: UiDataGridPresentation::Text,
                }],
            }],
            drag_records: Vec::new(),
            drop_records: Vec::new(),
            event_records: vec![UiProgramEventDeclaration {
                node_key: "control".into(),
                intent: "invoke".into(),
                allowed_payload_keys: Vec::new(),
                literal_payload: BTreeMap::new(),
                bound_input_keys: Vec::new(),
            }],
            resource_budget: UiResourceBudget {
                max_nodes: 2,
                max_bindings: 0,
                max_instances: 1,
                max_text_records: 0,
                max_glyph_instances: 0,
                max_events: 1,
                max_clips: 0,
            },
            dependency_index: neon_ui_schema::UiDependencyIndex {
                input_to_bindings: BTreeMap::new(),
                node_to_source_span: BTreeMap::new(),
                node_to_dependents: BTreeMap::new(),
            },
            layout_hash: "layout".into(),
        };
        UiHostAdapter::activate(program, schema, 7).unwrap()
    }

    fn grid_input(list_revision: u64) -> UiDataGridInputFrame {
        UiDataGridInputFrame {
            source_key: "grid".into(),
            frame: UiDataGridFrame {
                list_revision: Revision(list_revision),
                total_rows: 1,
                first_row: 0,
                window_rows: vec![UiDataGridWindowRow {
                    stable_row_key: "row".into(),
                    cells: BTreeMap::from([(
                        "value".into(),
                        UiDataGridCell {
                            value: UiInputValue::Bool { value: true },
                            display: neon_ui_schema::UiTextHandle {
                                id: 1,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    )]),
                }],
                expected_program_revision: revision(),
            },
        }
    }

    fn active_fragment() -> UiHostFragmentContext {
        UiHostFragmentContext {
            fragment: neon_ui_schema::UiFragmentRevision {
                id: UiFragmentId("arbitrary-fragment".into()),
                revision: Revision(1),
            },
            root: UiNode {
                node_id: UiNodeId("root".into()),
                kind: UiNodeKind::Panel,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle::default(),
                enter_transition: None,
                children: Vec::new(),
            },
            effects: Vec::new(),
        }
    }

    fn publication(
        expected_input_revision: u64,
        grid_inputs: Vec<UiDataGridInputFrame>,
    ) -> UiHostPublication {
        UiHostPublication {
            scalar_frame: neon_ui_schema::UiInputFrame {
                program_revision: revision(),
                expected_input_revision: Revision(expected_input_revision),
                request_id: "request".into(),
                idempotency_key: format!("key-{expected_input_revision}"),
                changes: vec![UiInputChange {
                    key: "enabled".into(),
                    value: UiInputValue::Bool { value: true },
                }],
            },
            grid_inputs,
            presentation_update: None,
        }
    }

    #[test]
    fn publication_updates_scalar_and_grid_state_together() {
        let mut adapter = adapter();
        let result = adapter
            .apply_publication(publication(0, vec![grid_input(1)]))
            .unwrap();
        assert_eq!(result.snapshot.scalar_inputs.input_revision, Revision(1));
        assert_eq!(result.snapshot.grid_inputs.len(), 1);
        assert_eq!(result.accepted_grid_sources, vec!["grid"]);
    }

    #[test]
    fn presentation_update_preserves_active_identity_program_and_inputs() {
        let adapter = adapter();
        let active = active_fragment().into_fragment();
        let original_program = adapter.program().clone();
        let original_snapshot = adapter.snapshot();
        let mut replacement = active.clone();
        replacement.revision = Revision(2);
        replacement.root.enabled = false;
        let update = UiHostPresentationUpdate {
            expected_fragment_revision: Revision(1),
            replacement_fragment: replacement.clone(),
            replacement_program: original_program.clone(),
            replacement_input_schema: adapter.input_schema().clone(),
        };

        let (next, accepted) = adapter
            .apply_presentation_update(update.clone(), &active)
            .unwrap();
        assert_eq!(accepted.fragment_id, active.fragment_id);
        assert_eq!(accepted.revision, Revision(2));
        assert_eq!(next.program(), &original_program);
        assert_eq!(next.snapshot(), original_snapshot);

        let mut changed_program = update.clone();
        changed_program.replacement_program.layout_hash = "changed".into();
        assert_eq!(
            adapter
                .apply_presentation_update(changed_program, &active)
                .unwrap_err()
                .code,
            "ui_host_presentation_state_change"
        );

        let mut changed_schema = update.clone();
        changed_schema.replacement_input_schema.layout_hash = "changed".into();
        assert_eq!(
            adapter
                .apply_presentation_update(changed_schema, &active)
                .unwrap_err()
                .code,
            "ui_host_presentation_state_change"
        );

        let mut hardcoded = update;
        hardcoded.replacement_fragment.fragment_id = UiFragmentId("component-gallery-host".into());
        assert_eq!(
            adapter
                .apply_presentation_update(hardcoded, &active)
                .unwrap_err()
                .code,
            "ui_host_fragment_identity_change"
        );
        assert_eq!(adapter.program(), &original_program);
        assert_eq!(adapter.snapshot(), original_snapshot);
    }

    #[test]
    fn drag_presentation_carries_forward_seeded_grid_and_scalar_snapshot() {
        fn node(key: &str, kind: UiNodeKind) -> UiNode {
            UiNode {
                node_id: UiNodeId(key.into()),
                kind,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle::default(),
                enter_transition: None,
                children: Vec::new(),
            }
        }

        let mut adapter = adapter();
        adapter.seed_grid_inputs(vec![grid_input(3)]).unwrap();
        adapter
            .apply_publication(publication(0, Vec::new()))
            .unwrap();
        let original_snapshot = adapter.snapshot();
        let original_bytes = serde_json::to_vec(&original_snapshot).unwrap();
        assert_eq!(original_snapshot.scalar_inputs.input_revision, Revision(1));
        assert_eq!(original_snapshot.grid_inputs, vec![grid_input(3)]);

        let mut active = active_fragment().into_fragment();
        active.root.children = vec![
            node("drag-source", UiNodeKind::Button),
            node("drop-target", UiNodeKind::Panel),
        ];
        let mut replacement = active.clone();
        replacement.revision = Revision(2);
        replacement
            .root
            .children
            .retain(|child| child.node_id.0 != "drag-source");
        let representation = node("target-representation", UiNodeKind::Button);
        replacement
            .root
            .children
            .iter_mut()
            .find(|child| child.node_id.0 == "drop-target")
            .unwrap()
            .children
            .push(representation.clone());

        let update = UiHostPresentationUpdate {
            expected_fragment_revision: Revision(1),
            replacement_fragment: replacement,
            replacement_program: adapter.program().clone(),
            replacement_input_schema: adapter.input_schema().clone(),
        };
        let mut encoded_update = serde_json::to_value(&update).unwrap();
        assert!(encoded_update.get("replacement_input_snapshot").is_none());
        encoded_update.as_object_mut().unwrap().insert(
            "replacement_input_snapshot".into(),
            serde_json::to_value(&original_snapshot).unwrap(),
        );
        assert!(serde_json::from_value::<UiHostPresentationUpdate>(encoded_update).is_err());

        let (next, accepted) = adapter.apply_presentation_update(update, &active).unwrap();
        let carried_snapshot = next.snapshot();
        assert_eq!(carried_snapshot, original_snapshot);
        assert_eq!(
            serde_json::to_vec(&carried_snapshot).unwrap(),
            original_bytes
        );
        assert!(
            accepted
                .root
                .children
                .iter()
                .all(|child| child.node_id.0 != "drag-source")
        );
        let inserted = accepted
            .root
            .children
            .iter()
            .find(|child| child.node_id.0 == "drop-target")
            .and_then(|target| target.children.first())
            .unwrap();
        assert_eq!(inserted.node_id, representation.node_id);
        assert!(inserted.visible);
    }

    #[test]
    fn rejected_grid_leaves_scalar_state_unchanged() {
        let mut adapter = adapter();
        let invalid = UiDataGridInputFrame {
            source_key: "missing".into(),
            frame: grid_input(1).frame,
        };
        assert_eq!(
            adapter
                .apply_publication(publication(0, vec![invalid]))
                .unwrap_err()
                .code,
            "ui_program_invalid_branch_template"
        );
        assert_eq!(adapter.snapshot().scalar_inputs.input_revision, Revision(0));
        assert!(adapter.snapshot().grid_inputs.is_empty());
    }

    #[test]
    fn inbound_window_request_requires_current_grid_revision() {
        let mut adapter = adapter();
        adapter
            .apply_publication(publication(0, vec![grid_input(3)]))
            .unwrap();
        let request = UiWindowRequest::DataGrid {
            request: UiDataGridWindowRequest {
                renderer_epoch: 7,
                composition_revision: Revision(1),
                fragment: neon_ui_schema::UiFragmentRevision {
                    id: neon_ui_schema::UiFragmentId("fragment".into()),
                    revision: Revision(1),
                },
                source_key: "grid".into(),
                expected_list_revision: Revision(3),
                requested_first_row: 0,
                max_window_rows: 2,
                sequence: 1,
            },
        };
        assert!(matches!(
            adapter.validate_inbound(UiHostInbound::WindowRequest { request }),
            Ok(UiHostInboundResult::WindowRequest(_))
        ));
    }

    #[test]
    fn seeded_grid_is_available_without_advancing_scalar_inputs() {
        let mut adapter = adapter();
        adapter.seed_grid_inputs(vec![grid_input(3)]).unwrap();
        assert_eq!(adapter.snapshot().scalar_inputs.input_revision, Revision(0));
        assert_eq!(
            adapter.snapshot().grid_inputs[0].frame.list_revision,
            Revision(3)
        );
    }

    #[test]
    fn inbound_semantic_intent_is_validated_without_domain_dispatch() {
        let adapter = adapter();
        let event = UiProgramSemanticEvent {
            event_id: "event".into(),
            kind: UiProgramSemanticEventKind::Activate,
            intent: "invoke".into(),
            source_node_key: "control".into(),
            payload: BTreeMap::new(),
            program_revision: revision(),
            input_revision: Revision(0),
            request_id: "request".into(),
            idempotency_key: "event-key".into(),
            requested_value: None,
            interaction: UiSemanticInteractionMetadata {
                interaction_id: "interaction".into(),
                sequence: 1,
                renderer_epoch: 7,
            },
        };
        assert!(matches!(
            adapter.validate_inbound(UiHostInbound::SemanticIntent { event }),
            Ok(UiHostInboundResult::SemanticIntent(_))
        ));
    }

    #[test]
    fn inbound_drag_drop_requires_the_active_declared_contract() {
        let mut adapter = adapter();
        adapter.program.drag_records.push(UiProgramDragRecord {
            key: "item-drag".into(),
            source_node_key: "control".into(),
            axis: neon_ui_schema::UiDragAxis::Both,
            snap: 0.0,
            threshold: 0.0,
            boundary: neon_ui_schema::UiDragBoundary::Free,
        });
        adapter.program.drop_records.push(UiProgramDropRecord {
            key: "target-drop".into(),
            target_node_key: "grid_node".into(),
            accepts_drag_key: "item-drag".into(),
            placement: neon_ui_schema::UiDropPlacement::Into,
            presentation_template_key: Some("target-template".into()),
            intent: "workspace.item.move".into(),
        });
        let event = UiProgramDragDropEvent {
            event_id: "drop-event".into(),
            drag_key: "item-drag".into(),
            drop_key: "target-drop".into(),
            intent: "workspace.item.move".into(),
            payload: neon_ui_schema::UiDragDropPayload {
                source_key: "control".into(),
                target_key: "grid_node".into(),
                placement: neon_ui_schema::UiDropPlacement::Into,
                presentation_template_key: Some("target-template".into()),
            },
            program_revision: revision(),
            input_revision: Revision(0),
            request_id: "drop-request".into(),
            idempotency_key: "drop-key".into(),
            interaction: UiSemanticInteractionMetadata {
                interaction_id: "drop-interaction".into(),
                sequence: 1,
                renderer_epoch: 7,
            },
        };
        assert!(matches!(
            adapter.validate_inbound(UiHostInbound::DragDrop {
                event: event.clone(),
                active_fragment: active_fragment(),
            }),
            Ok(UiHostInboundResult::DragDrop(_))
        ));
        let mut invalid = event;
        invalid.payload.presentation_template_key = None;
        assert_eq!(
            adapter
                .validate_inbound(UiHostInbound::DragDrop {
                    event: invalid,
                    active_fragment: active_fragment(),
                })
                .unwrap_err()
                .code,
            "ui_host_invalid_drag_drop"
        );
    }

    #[test]
    fn apply_publication_publishes_emitevent_to_eventd() {
        use neon_ipc::{read_json_frame, write_json_frame, DEFAULT_MAX_FRAME_SIZE};
        use neon_protocol::{ClientKind, EventAck, EventAckStatus, EventFrame, EventId, EventResponse};

        // Stand in for neon-eventd: accept one publish frame and assert it is the
        // directed `flow.<flow_name>.<variable>` observation.
        let server = neon_ipc::RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut stream = server.accept().unwrap();
            let frame: EventFrame =
                read_json_frame(&mut stream, DEFAULT_MAX_FRAME_SIZE).unwrap();
            let EventFrame::Publish(publish) = frame else {
                panic!("expected a publish frame");
            };
            assert_eq!(publish.name, "flow.test-flow.enabled");
            assert_eq!(publish.payload["variable_key"], "enabled");
            assert_eq!(publish.payload["kind"], "bool");
            assert_eq!(publish.payload["module"], "test-flow");
            assert_eq!(publish.payload["surface"], "surface.test-flow.inputs");
            write_json_frame(
                &mut stream,
                &EventResponse::Ack(EventAck {
                    protocol: publish.protocol,
                    version: publish.version,
                    request_id: publish.request_id,
                    status: EventAckStatus::Accepted,
                    event_id: Some(EventId("evt-1-1".into())),
                    epoch: Some(1),
                    sequence: Some(1),
                    current_sequence: Some(1),
                    error: None,
                }),
                DEFAULT_MAX_FRAME_SIZE,
            )
            .unwrap();
        });

        let revision = revision();
        let schema = UiInputSchema {
            schema_id: "surface.test-flow.inputs".into(),
            version: 1,
            layout_hash: "layout".into(),
            slots: vec![UiInputSlot {
                key: "enabled".into(),
                kind: UiInputKind::Bool,
                default_value: UiInputValue::Bool { value: false },
                update_class: UiInputUpdateClass::ReliableExternal,
                semantic_label: "enabled".into(),
                packing: UiInputPacking {
                    alignment: 4,
                    lanes: 1,
                    offset: 0,
                    representation: neon_ui_schema::UiGpuScalarRepresentation::Bool32,
                },
            }],
            grid_slots: Vec::new(),
            flow_name: "test-flow".into(),
            emit_event_keys: vec!["enabled".into()],
        };
        let program = UiProgram {
            revision: revision.clone(),
            nodes: Vec::new(),
            node_templates: Vec::new(),
            literal_texts: Vec::new(),
            layout_records: Vec::new(),
            binding_records: Vec::new(),
            branch_records: Vec::new(),
            template_records: Vec::new(),
            data_grid_records: Vec::new(),
            drag_records: Vec::new(),
            drop_records: Vec::new(),
            event_records: Vec::new(),
            resource_budget: UiResourceBudget {
                max_nodes: 0,
                max_bindings: 0,
                max_instances: 0,
                max_text_records: 0,
                max_glyph_instances: 0,
                max_events: 0,
                max_clips: 0,
            },
            dependency_index: neon_ui_schema::UiDependencyIndex {
                input_to_bindings: BTreeMap::new(),
                node_to_source_span: BTreeMap::new(),
                node_to_dependents: BTreeMap::new(),
            },
            layout_hash: "layout".into(),
        };
        let mut adapter = UiHostAdapter::activate(program, schema, 7)
            .unwrap()
            .with_event_publisher(
                Some(endpoint),
                ClientIdentity {
                    kind: ClientKind::UiRuntime,
                    instance_id: "host-test".into(),
                    pid: 1,
                    origin: "test".into(),
                },
            );
        let publication = UiHostPublication {
            scalar_frame: neon_ui_schema::UiInputFrame {
                program_revision: revision,
                expected_input_revision: Revision(0),
                request_id: "pub-request".into(),
                idempotency_key: "pub-key".into(),
                changes: vec![UiInputChange {
                    key: "enabled".into(),
                    value: UiInputValue::Bool { value: true },
                }],
            },
            grid_inputs: Vec::new(),
            presentation_update: None,
        };
        let result = adapter.apply_publication(publication).unwrap();
        assert_eq!(result.changed_slots, vec!["enabled".to_owned()]);
        server_thread.join().unwrap();
    }
}
