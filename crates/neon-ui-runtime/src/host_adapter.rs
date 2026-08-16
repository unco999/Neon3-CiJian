//! Generic host boundary for one active compiled UI program.
//!
//! This module validates UI contracts only. It neither identifies nor invokes
//! a domain owner for semantic intents.

use std::collections::{HashMap, HashSet};

use neon_ui_schema::{
    UiDataGridInputFrame, UiHostInbound, UiHostPublication, UiInputSchema, UiProgram,
    UiProgramInputSnapshot, UiProgramSemanticEvent, UiWindowRequest,
};
use serde::{Deserialize, Serialize};

use crate::{UiDataGridStore, UiInputStore, UiInputStoreError, UiInputWriter};

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

#[derive(Clone, Debug, PartialEq)]
pub struct UiHostPublicationResult {
    pub snapshot: UiProgramInputSnapshot,
    pub changed_slots: Vec<String>,
    pub accepted_grid_sources: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiHostInboundResult {
    WindowRequest(UiWindowRequest),
    SemanticIntent(UiProgramSemanticEvent),
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
        })
    }

    pub fn program(&self) -> &UiProgram {
        &self.program
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
        let result = UiHostPublicationResult {
            snapshot: self.snapshot(),
            changed_slots: scalar.changed_slots,
            accepted_grid_sources,
        };
        self.publication_results.insert(key, result.clone());
        Ok(result)
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
            UiHostInbound::DataGridCell { event } => {
                let Some(target) = event.data_grid_cell.as_ref() else {
                    return Err(UiHostAdapterError { code: "ui_host_invalid_data_grid_cell", message: "DataGrid cell event has no cell target" });
                };
                let Some(frame) = self.grids.frame(&target.source_key) else {
                    return Err(UiHostAdapterError { code: "ui_host_grid_unavailable", message: "DataGrid cell event has no active grid frame" });
                };
                if event.renderer_epoch != self.renderer_epoch
                    || !frame.window_rows.iter().any(|row| row.stable_row_key == target.stable_row_key)
                    || !self.program.data_grid_records.iter().any(|grid| {
                        grid.source_key == target.source_key && grid.columns.iter().any(|column| column.key == target.column_key)
                    }) {
                    return Err(UiHostAdapterError { code: "ui_host_invalid_data_grid_cell", message: "DataGrid cell event is not active in the declared grid" });
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use neon_protocol::Revision;
    use neon_ui_schema::{
        UiDataGridCell, UiDataGridColumn, UiDataGridFrame, UiDataGridPresentation,
        UiDataGridRecord, UiDataGridWindowRequest, UiDataGridWindowRow, UiGridInputSlot,
        UiInputChange, UiInputKind, UiInputPacking, UiInputSlot, UiInputUpdateClass, UiInputValue,
        UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
        UiProgramEventDeclaration, UiProgramNode, UiProgramRevision, UiProgramSemanticEventKind,
        UiResourceBudget, UiSemanticInteractionMetadata,
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
        assert_eq!(adapter.snapshot().grid_inputs[0].frame.list_revision, Revision(3));
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
}
