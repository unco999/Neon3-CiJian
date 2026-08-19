//! Headless UI declaration runtime. It must not create windows or GPU objects.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    net::SocketAddr,
};

use neon_ipc::{RpcClient, RpcServer, TransportError};
use neon_observability::{
    CommandJournal, CommandReceipt, CommandState, DebugSnapshot, EVENT_COMMAND_ACCEPTED,
    EVENT_COMMAND_RECEIVED, EVENT_COMMAND_REJECTED, JournalFilter, TraceLevel, TraceRecord,
};
use neon_protocol::{
    AiTerrainCondition, ClientIdentity, ClientKind, HealthStatus, InteractionId,
    InteractionSemanticTarget, InteractionTraceError, InteractionTraceOutcome,
    InteractionTraceQuery, InteractionTraceRecord, InteractionTraceStage, PROTOCOL_VERSION,
    ProtocolVersion, RequestId, Revision, RpcError, RpcRequest, RpcResponse, RpcStatus,
    ServiceDescription, ServiceHealth, ServiceName,
};
use neon_ui_schema::{
    ERROR_DATA_GRID_CELL_INVALID, ERROR_FRAGMENT_REVISION_STALE, ERROR_INPUT_SEQUENCE_STALE,
    ERROR_INTENT_NOT_BOUND, ERROR_RENDERER_EPOCH_MISMATCH, ERROR_UI_PROGRAM_CAPACITY_OVERFLOW,
    ERROR_UI_PROGRAM_DUPLICATE_INPUT_CHANGE, ERROR_UI_PROGRAM_EVENT_CONTROL_UNAVAILABLE,
    ERROR_UI_PROGRAM_EVENT_DUPLICATE_IDEMPOTENCY_KEY,
    ERROR_UI_PROGRAM_EVENT_INTERACTION_EPOCH_MISMATCH, ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE,
    ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, ERROR_UI_PROGRAM_EVENT_STALE_REVISION,
    ERROR_UI_PROGRAM_INPUT_TYPE_MISMATCH, ERROR_UI_PROGRAM_INPUT_UPDATE_FORBIDDEN,
    ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, ERROR_UI_PROGRAM_STALE_INPUT_REVISION,
    ERROR_UI_PROGRAM_TEXT_REGISTRY_CAPACITY_OVERFLOW,
    ERROR_UI_PROGRAM_TEXT_REGISTRY_GENERATION_MISMATCH,
    ERROR_UI_PROGRAM_TEXT_REGISTRY_STALE_REVISION, ERROR_UI_PROGRAM_TEXT_TOO_LONG,
    ERROR_UI_PROGRAM_UNKNOWN_INPUT_KEY, ERROR_UI_PROGRAM_UNKNOWN_TEXT_HANDLE, TextRef,
    UI_SURFACE_SCHEMA_VERSION, UiBinding, UiBoundProperty, UiBounds, UiBranchPredicate,
    UiBranchRecord, UiCommand, UiCpuFrameOutput, UiCpuNodeState, UiCpuRenderPrimitive,
    UiCpuSemanticTarget, UiCpuViewport, UiDataGridCellTarget, UiDataGridDeclaration,
    UiDataGridFrame, UiDataGridInputFrame, UiDataGridRecord, UiDependencyIndex, UiDiagnostic,
    UiDiagnosticSeverity, UiDiagnosticsState, UiEffect, UiEventTraceRecord, UiFragment,
    UiFragmentId, UiFragmentSubmission, UiHostFragmentContext, UiHostInbound, UiHostPublication,
    UiInputChange, UiInputFrame, UiInputSchema, UiInputUpdateClass, UiInputValue,
    UiInputValueSource, UiInspectorState, UiInspectorTab, UiIntent, UiIrDocument, UiNode, UiNodeId,
    UiNodeKind, UiProgram, UiProgramDragDropEvent, UiProgramLayoutRecord, UiProgramLiteralText,
    UiProgramNode, UiProgramResourceKind, UiProgramRevision, UiProgramSemanticEvent,
    UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
    UiProgramSemanticEventKind, UiProgramSemanticEventResult, UiProgramSemanticEventStatus,
    UiRepeatFrame, UiResolvedInputValue, UiResolvedInputs, UiSchemaError, UiSemanticEvent,
    UiSemanticInteractionMetadata, UiSemanticPayloadValue, UiStyle, UiSurfaceEvent,
    UiSurfaceEventKind, UiSurfaceEventRequest, UiSurfaceId, UiSurfaceSnapshot, UiSurfaceState,
    UiTemplateRecord, UiTextHandle, UiTextHandleDiagnostic, UiTextHandleStatus, UiTextRecord,
    UiTextRegistryDebugSnapshot, UiTextRegistryEntryMetadata, UiTextRegistrySnapshot,
    UiTextSourceCategory, UiTransition, UiTransitionState,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(test)]
use neon_ui_schema::UiRepeatRow;

pub mod debug;
pub mod demo_domain;
pub mod event_publisher;
pub mod host_adapter;
pub mod nui_flow;
pub mod nui_state_machine;
pub mod terrain_workbench;
use host_adapter::{UiHostAdapter, UiHostAdapterConfig};
pub use event_publisher::{EVENT_VARIABLE_CHANGED, FLOW_EVENT_PREFIX, UiVariableEventPublisher};
pub use nui_flow::{
    NuiFlowError, apply_nui_ir_patch, bind_nui_flow_resources, compile_nui_flow_program,
    format_nui_flow, lower_nui_flow, lower_nui_flow_effects, parse_nui_flow, parse_nui_flow_patch,
};
pub use nui_state_machine::{
    NuiFlowDragController, NuiFlowDragUpdate, NuiFlowDropResult, NuiFlowStateMachineRuntime,
    NuiFlowStateTransitionResult,
};

pub const SERVICE_NAME: &str = "ui-runtime";
pub const WORKBENCH_SURFACE_ID: &str = "surface.ui-workbench";
pub const AI_TERRAIN_SURFACE_ID: &str = "surface.ai.terrain-generator";
pub const CAPABILITY_DEBUG_INTERACTION: &str = "debug.interaction.v1";
const INTERACTION_TRACE_CAPACITY: usize = 256;

/// Materializes one visible instance from a hidden declarative template prototype.
/// The prototype remains unchanged; instance IDs are scoped by the caller's stable key.
pub fn instantiate_ui_template(
    prototype: &UiNode,
    into_parent: Option<&UiNode>,
    instance_namespace: &str,
    content: Option<TextRef>,
) -> UiNode {
    fn namespace(node: &mut UiNode, prefix: &str) {
        node.node_id = UiNodeId(format!("{prefix}-{}", node.node_id.0));
        for child in &mut node.children {
            namespace(child, prefix);
        }
    }

    fn set_first_text(node: &mut UiNode, content: &TextRef) -> bool {
        if node.text.is_some() {
            node.text = Some(content.clone());
            return true;
        }
        node.children
            .iter_mut()
            .any(|child| set_first_text(child, content))
    }

    let mut instance = prototype.clone();
    namespace(&mut instance, instance_namespace);
    if let Some(content) = content.as_ref() {
        set_first_text(&mut instance, content);
    }
    if let Some(parent) = into_parent {
        let parent_layout = parent.layout.unwrap_or_default();
        let instance_layout = instance.layout.get_or_insert_default();
        match parent_layout.mode {
            neon_ui_schema::UiLayoutMode::Column if instance.bounds.width == 0.0 => {
                instance_layout.align_self = Some(neon_ui_schema::UiAlignItems::Stretch);
            }
            neon_ui_schema::UiLayoutMode::Row if instance.bounds.height == 0.0 => {
                instance_layout.align_self = Some(neon_ui_schema::UiAlignItems::Stretch);
            }
            _ => {}
        }
    }
    instance.visible = true;
    instance.enabled = true;
    instance
}

#[derive(Default)]
struct InteractionTraceStore {
    next_sequence: u64,
    records: VecDeque<InteractionTraceRecord>,
}

#[derive(Clone)]
struct InteractionTraceContext {
    interaction_id: InteractionId,
    semantic_target: Option<InteractionSemanticTarget>,
    fragment_revision: Option<Revision>,
    composition_revision: Revision,
}

fn inbound_interaction_context(
    inbound: &UiHostInbound,
    fallback_composition_revision: Revision,
) -> Option<InteractionTraceContext> {
    match inbound {
        UiHostInbound::SemanticIntent { event } => Some(InteractionTraceContext {
            interaction_id: InteractionId(event.interaction.interaction_id.clone()),
            semantic_target: Some(InteractionSemanticTarget {
                node_path: event.source_node_key.clone(),
            }),
            fragment_revision: Some(event.program_revision.revision),
            composition_revision: fallback_composition_revision,
        }),
        UiHostInbound::DragDrop {
            event,
            active_fragment,
        } => Some(InteractionTraceContext {
            interaction_id: InteractionId(event.interaction.interaction_id.clone()),
            semantic_target: Some(InteractionSemanticTarget {
                node_path: event.payload.target_key.clone(),
            }),
            fragment_revision: Some(active_fragment.fragment.revision),
            composition_revision: active_fragment.fragment.revision,
        }),
        UiHostInbound::DataGridCell { event } => Some(InteractionTraceContext {
            interaction_id: InteractionId(event.event_id.clone()),
            semantic_target: event.data_grid_cell.as_ref().map(|target| {
                InteractionSemanticTarget {
                    node_path: target.source_key.clone(),
                }
            }),
            fragment_revision: Some(event.fragment.revision),
            composition_revision: event.composition_revision,
        }),
        UiHostInbound::WindowRequest { .. } => None,
    }
}

impl InteractionTraceStore {
    fn append(
        &mut self,
        interaction_id: InteractionId,
        stage: InteractionTraceStage,
        outcome: InteractionTraceOutcome,
        error: Option<InteractionTraceError>,
        semantic_target: Option<InteractionSemanticTarget>,
        fragment_revision: Option<Revision>,
        composition_revision: Revision,
        downstream_request_id: Option<RequestId>,
    ) {
        if self.next_sequence == 0 {
            self.next_sequence = 1;
        }
        if self.records.len() == INTERACTION_TRACE_CAPACITY {
            self.records.pop_front();
        }
        self.records.push_back(InteractionTraceRecord {
            sequence: self.next_sequence,
            interaction_id,
            stage,
            outcome,
            error,
            semantic_source_key: None,
            semantic_target,
            semantic_intent: None,
            fragment_revision,
            composition_revision,
            downstream_request_id,
        });
        self.next_sequence += 1;
    }

    fn get(&self, interaction_id: &InteractionId) -> Vec<InteractionTraceRecord> {
        self.records
            .iter()
            .filter(|record| &record.interaction_id == interaction_id)
            .cloned()
            .collect()
    }

    fn query(&self, query: &InteractionTraceQuery) -> Vec<InteractionTraceRecord> {
        let limit = query.limit.unwrap_or(100).min(INTERACTION_TRACE_CAPACITY);
        self.records
            .iter()
            .filter(|record| query.after.is_none_or(|after| record.sequence > after))
            .filter(|record| {
                query.filters.as_ref().is_none_or(|filters| {
                    filters
                        .interaction_id
                        .as_ref()
                        .is_none_or(|id| id == &record.interaction_id)
                        && filters.stage.is_none_or(|stage| stage == record.stage)
                        && filters
                            .outcome
                            .is_none_or(|outcome| outcome == record.outcome)
                        && filters
                            .semantic_source_key
                            .as_ref()
                            .is_none_or(|key| record.semantic_source_key.as_ref() == Some(key))
                        && filters.semantic_node_path.as_ref().is_none_or(|path| {
                            record
                                .semantic_target
                                .as_ref()
                                .is_some_and(|target| &target.node_path == path)
                        })
                        && filters
                            .downstream_request_id
                            .as_ref()
                            .is_none_or(|id| record.downstream_request_id.as_ref() == Some(id))
                })
            })
            .take(limit)
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AiTerrainPanelState {
    revision: Revision,
    condition: AiTerrainCondition,
    guidance: f32,
    steps: u32,
    seed: u64,
    last_seed: Option<u64>,
    size: u32,
    target_id: String,
    state: String,
    job_id: Option<String>,
    elapsed_ms: Option<f64>,
    error_code: Option<String>,
}

impl Default for AiTerrainPanelState {
    fn default() -> Self {
        Self {
            revision: Revision(1),
            condition: AiTerrainCondition {
                sub: Some(6),
                parent: Some(1),
                relief: Some(3),
                texture: Some(2),
                water: Some(2),
            },
            guidance: 0.0,
            steps: 4,
            seed: 42,
            last_seed: None,
            size: 256,
            target_id: "ai.terrain.preview".into(),
            state: "idle".into(),
            job_id: None,
            elapsed_ms: None,
            error_code: None,
        }
    }
}

impl AiTerrainPanelState {
    fn snapshot(&self) -> Value {
        json!({
            "schema_version": 1,
            "surface_id": AI_TERRAIN_SURFACE_ID,
            "revision": self.revision,
            "condition": self.condition,
            "guidance": self.guidance,
            "steps": self.steps,
            "seed": self.seed,
            "last_seed": self.last_seed,
            "size": self.size,
            "target_id": self.target_id,
            "state": self.state,
            "job_id": self.job_id,
            "elapsed_ms": self.elapsed_ms,
            "error_code": self.error_code,
        })
    }

    fn advance(&mut self) {
        self.revision = Revision(self.revision.0 + 1);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiSurfaceTransition {
    pub snapshot: UiSurfaceSnapshot,
    pub effects: Vec<UiEffect>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiSurfaceMachine {
    surface_id: UiSurfaceId,
    revision: Revision,
    state: UiSurfaceState,
}

impl UiSurfaceMachine {
    fn new(surface_id: UiSurfaceId) -> Self {
        Self {
            surface_id,
            revision: Revision(0),
            state: UiSurfaceState {
                diagnostics: UiDiagnosticsState::Collapsed,
                inspector: UiInspectorState {
                    tab: UiInspectorTab::Overview,
                },
            },
        }
    }

    fn snapshot(&self) -> UiSurfaceSnapshot {
        UiSurfaceSnapshot {
            schema_version: UI_SURFACE_SCHEMA_VERSION,
            surface_id: self.surface_id.clone(),
            revision: self.revision,
            value: self.state.clone(),
            available_events: vec![
                UiSurfaceEventKind::DiagnosticsToggle,
                UiSurfaceEventKind::InspectorTabSelect,
            ],
        }
    }

    fn transition(
        &mut self,
        event: UiSurfaceEvent,
    ) -> Result<UiSurfaceTransition, (&'static str, &'static str)> {
        match event {
            UiSurfaceEvent::DiagnosticsToggle => {
                self.state.diagnostics = match self.state.diagnostics {
                    UiDiagnosticsState::Collapsed => UiDiagnosticsState::Expanded,
                    UiDiagnosticsState::Expanded => UiDiagnosticsState::Collapsed,
                };
            }
            UiSurfaceEvent::InspectorTabSelect { tab } => {
                if self.state.inspector.tab == tab {
                    return Err(("ui_guard_rejected", "inspector tab is already selected"));
                }
                self.state.inspector.tab = tab;
            }
        }
        self.revision = Revision(self.revision.0 + 1);
        Ok(UiSurfaceTransition {
            snapshot: self.snapshot(),
            effects: Vec::new(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiInputWriter {
    External,
    LocalPresentation,
}
#[derive(Clone, Debug, PartialEq)]
pub struct UiInputApplyResult {
    pub input_revision: Revision,
    pub changed_slots: Vec<String>,
    pub snapshot: UiResolvedInputs,
    /// Detailed variable changes for event forwarding. Each entry carries the
    /// stable variable key, its input kind, and old/new values.
    pub variable_changes: Vec<UiVariableChange>,
}

/// One UI input variable that changed during an input frame application.
/// This is an observation record for the event protocol; it is not a domain
/// command and does not authorize any receiver to mutate authoritative state.
#[derive(Clone, Debug, PartialEq)]
pub struct UiVariableChange {
    pub key: String,
    pub kind: String,
    pub old_value: Option<serde_json::Value>,
    pub new_value: serde_json::Value,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiInputStoreError {
    pub code: &'static str,
    pub message: &'static str,
}
impl UiInputStoreError {
    fn schema(error: UiSchemaError) -> Self {
        match error {
            UiSchemaError::DuplicateInputKey => Self {
                code: "ui_program_duplicate_input_key",
                message: "input schema contains a duplicate slot key",
            },
            UiSchemaError::InvalidInputDefault => Self {
                code: "ui_program_invalid_default",
                message: "input slot default does not match its declared kind",
            },
            _ => Self {
                code: "ui_program_invalid_input_schema",
                message: "input schema is invalid",
            },
        }
    }
}
/// CPU-only transactional input state. Defaults are installed at activation;
/// WGPU is deliberately absent from this supported headless API.
#[derive(Clone, Debug)]
pub struct UiInputStore {
    schema: UiInputSchema,
    resolved_inputs: UiResolvedInputs,
    dirty_slots: HashSet<String>,
    idempotent_results: HashMap<String, UiInputApplyResult>,
}
impl UiInputStore {
    pub fn activate(
        program_revision: UiProgramRevision,
        schema: UiInputSchema,
    ) -> Result<Self, UiInputStoreError> {
        program_revision
            .validate_baseline()
            .map_err(UiInputStoreError::schema)?;
        schema.validate().map_err(UiInputStoreError::schema)?;
        let values = schema
            .slots
            .iter()
            .map(|slot| {
                (
                    slot.key.clone(),
                    UiResolvedInputValue {
                        value: slot.default_value.clone(),
                        source: UiInputValueSource::Default,
                        last_update_revision: Revision(0),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        Ok(Self {
            schema,
            resolved_inputs: UiResolvedInputs {
                program_revision,
                input_revision: Revision(0),
                values,
                changed_slots: Vec::new(),
            },
            dirty_slots: HashSet::new(),
            idempotent_results: HashMap::new(),
        })
    }
    /// Activates a text-registry-enabled program only when every default text
    /// handle resolves. This guarantees default-first rendering cannot select
    /// stale or unrelated text.
    pub fn activate_with_text_registry(
        program_revision: UiProgramRevision,
        schema: UiInputSchema,
        registry: &UiTextRegistry,
    ) -> Result<Self, UiInputStoreError> {
        for slot in &schema.slots {
            if let UiInputValue::TextHandle { value } = &slot.default_value {
                registry
                    .validate_handle(*value)
                    .map_err(UiInputStoreError::text)?;
            }
        }
        Self::activate(program_revision, schema)
    }
    pub fn schema(&self) -> &UiInputSchema {
        &self.schema
    }
    pub fn snapshot(&self) -> UiResolvedInputs {
        self.resolved_inputs.clone()
    }
    /// Installs a host-provided snapshot for an atomically replaced program.
    /// The snapshot must be complete and schema-compatible; it is never merged.
    pub fn restore_snapshot(
        &mut self,
        snapshot: UiResolvedInputs,
    ) -> Result<(), UiInputStoreError> {
        if snapshot.program_revision != self.resolved_inputs.program_revision
            || snapshot.values.len() != self.schema.slots.len()
            || self.schema.slots.iter().any(|slot| {
                !snapshot
                    .values
                    .get(&slot.key)
                    .is_some_and(|value| slot.kind.accepts(&value.value))
            })
        {
            return Err(UiInputStoreError {
                code: "ui_program_invalid_input_snapshot",
                message: "replacement input snapshot does not match the active program schema",
            });
        }
        self.resolved_inputs = snapshot;
        self.dirty_slots.clear();
        self.idempotent_results.clear();
        Ok(())
    }
    pub fn dirty_slots(&self) -> Vec<String> {
        let mut slots = self.dirty_slots.iter().cloned().collect::<Vec<_>>();
        slots.sort();
        slots
    }
    pub fn take_dirty_slots(&mut self) -> Vec<String> {
        let slots = self.dirty_slots();
        self.dirty_slots.clear();
        slots
    }
    pub fn apply(
        &mut self,
        writer: UiInputWriter,
        frame: UiInputFrame,
    ) -> Result<UiInputApplyResult, UiInputStoreError> {
        if let Some(result) = self.idempotent_results.get(&frame.idempotency_key) {
            return Ok(result.clone());
        }
        if frame.idempotency_key.trim().is_empty() || frame.request_id.trim().is_empty() {
            return Err(UiInputStoreError {
                code: "invalid_request",
                message: "request_id and idempotency_key are required",
            });
        }
        if frame.program_revision != self.resolved_inputs.program_revision
            || frame.expected_input_revision != self.resolved_inputs.input_revision
        {
            return Err(UiInputStoreError {
                code: ERROR_UI_PROGRAM_STALE_INPUT_REVISION,
                message: "program or input revision is stale",
            });
        }
        let mut seen = HashSet::new();
        for UiInputChange { key, value } in &frame.changes {
            if !seen.insert(key.as_str()) {
                return Err(UiInputStoreError {
                    code: ERROR_UI_PROGRAM_DUPLICATE_INPUT_CHANGE,
                    message: "input frame changes a slot more than once",
                });
            }
            let Some(slot) = self.schema.slots.iter().find(|slot| slot.key == *key) else {
                return Err(UiInputStoreError {
                    code: ERROR_UI_PROGRAM_UNKNOWN_INPUT_KEY,
                    message: "input frame references an unknown slot",
                });
            };
            if !slot.kind.accepts(value) {
                return Err(UiInputStoreError {
                    code: ERROR_UI_PROGRAM_INPUT_TYPE_MISMATCH,
                    message: "input frame value does not match the declared slot kind",
                });
            }
            if !Self::writer_is_allowed(writer, slot.update_class) {
                return Err(UiInputStoreError {
                    code: ERROR_UI_PROGRAM_INPUT_UPDATE_FORBIDDEN,
                    message: "writer is not authorized for this input slot",
                });
            }
        }
        let next_revision = Revision(self.resolved_inputs.input_revision.0 + 1);
        let mut changed_slots = Vec::new();
        let mut variable_changes = Vec::new();
        let mut values = self.resolved_inputs.values.clone();
        for UiInputChange { key, value } in frame.changes {
            let slot = self
                .schema
                .slots
                .iter()
                .find(|slot| slot.key == key)
                .expect("validated slot exists");
            let source = match slot.update_class {
                UiInputUpdateClass::ReliableExternal => UiInputValueSource::ReliableExternal,
                UiInputUpdateClass::LocalPresentation => UiInputValueSource::LocalPresentation,
                UiInputUpdateClass::TextRegistryReference => {
                    UiInputValueSource::TextRegistryReference
                }
                UiInputUpdateClass::StaticAtProgramActivation => {
                    unreachable!("static slots are not writable")
                }
            };
            let old_value = values.get(&key).map(|current| current.value.clone());
            let changed = old_value.as_ref().is_none_or(|old| *old != value);
            if changed {
                changed_slots.push(key.clone());
                self.dirty_slots.insert(key.clone());
                variable_changes.push(UiVariableChange {
                    key: key.clone(),
                    kind: ui_input_kind_name(&slot.kind),
                    old_value: old_value.map(|value| input_value_to_json(&value)),
                    new_value: input_value_to_json(&value),
                });
            }
            values.insert(
                key,
                UiResolvedInputValue {
                    value,
                    source,
                    last_update_revision: next_revision,
                },
            );
        }
        let snapshot = UiResolvedInputs {
            program_revision: self.resolved_inputs.program_revision.clone(),
            input_revision: next_revision,
            values,
            changed_slots: changed_slots.clone(),
        };
        self.resolved_inputs = snapshot.clone();
        let result = UiInputApplyResult {
            input_revision: next_revision,
            changed_slots,
            variable_changes,
            snapshot,
        };
        self.idempotent_results
            .insert(frame.idempotency_key, result.clone());
        Ok(result)
    }
    /// Applies a frame only after every text-handle value resolves in the supplied
    /// registry. This is the capability-enabled path; raw text never enters frames.
    pub fn apply_with_text_registry(
        &mut self,
        writer: UiInputWriter,
        frame: UiInputFrame,
        registry: &UiTextRegistry,
    ) -> Result<UiInputApplyResult, UiInputStoreError> {
        for change in &frame.changes {
            if let UiInputValue::TextHandle { value } = &change.value {
                registry
                    .validate_handle(*value)
                    .map_err(UiInputStoreError::text)?;
            }
        }
        self.apply(writer, frame)
    }
    fn writer_is_allowed(writer: UiInputWriter, update_class: UiInputUpdateClass) -> bool {
        matches!(
            (writer, update_class),
            (
                UiInputWriter::External,
                UiInputUpdateClass::ReliableExternal | UiInputUpdateClass::TextRegistryReference
            ) | (
                UiInputWriter::LocalPresentation,
                UiInputUpdateClass::LocalPresentation
            )
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiTextRegistryError {
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Debug)]
struct UiTextRegistryEntry {
    record: UiTextRecord,
    reference_count: u32,
}

/// CPU-owned bounded text registry. WGPU may mirror resident text, but only the
/// renderer creates glyph resources; this type holds no renderer state.
#[derive(Clone, Debug)]
pub struct UiTextRegistry {
    registry_id: String,
    revision: Revision,
    capacity: u32,
    max_bytes_per_record: u32,
    next_id: u64,
    entries: BTreeMap<u64, UiTextRegistryEntry>,
    released_generations: HashMap<u64, u32>,
    free_ids: Vec<u64>,
}

impl UiTextRegistry {
    pub fn new(
        registry_id: impl Into<String>,
        capacity: u32,
        max_bytes_per_record: u32,
    ) -> Result<Self, UiTextRegistryError> {
        let registry_id = registry_id.into();
        if registry_id.trim().is_empty() || capacity == 0 || max_bytes_per_record == 0 {
            return Err(UiTextRegistryError {
                code: "ui_program_invalid_text_registry",
                message: "registry id, capacity, and record byte limit must be nonzero",
            });
        }
        Ok(Self {
            registry_id,
            revision: Revision(0),
            capacity,
            max_bytes_per_record,
            next_id: 1,
            entries: BTreeMap::new(),
            released_generations: HashMap::new(),
            free_ids: Vec::new(),
        })
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn snapshot(&self, include_text: bool) -> UiTextRegistrySnapshot {
        let records = self
            .entries
            .values()
            .map(|entry| {
                let mut record = entry.record.clone();
                if !include_text {
                    record.text.clear();
                }
                record
            })
            .collect();
        UiTextRegistrySnapshot {
            registry_id: self.registry_id.clone(),
            revision: self.revision,
            capacity: self.capacity,
            used: self.entries.len() as u32,
            records,
        }
    }
    /// Returns handle metadata without textual content. WGPU residency remains
    /// false here because this CPU registry never owns glyph resources.
    pub fn debug_snapshot(&self) -> UiTextRegistryDebugSnapshot {
        UiTextRegistryDebugSnapshot {
            registry_id: self.registry_id.clone(),
            revision: self.revision,
            capacity: self.capacity,
            used: self.entries.len() as u32,
            records: self
                .entries
                .values()
                .map(|entry| UiTextRegistryEntryMetadata {
                    handle: entry.record.handle,
                    category: entry.record.category,
                    revision: entry.record.revision,
                    byte_length: entry.record.byte_length,
                    reference_count: entry.reference_count,
                    resident: false,
                })
                .collect(),
        }
    }
    pub fn handle_diagnostic(&self, handle: UiTextHandle) -> UiTextHandleDiagnostic {
        match self.entries.get(&handle.id) {
            Some(entry) if entry.record.handle.generation == handle.generation => {
                UiTextHandleDiagnostic {
                    handle,
                    status: UiTextHandleStatus::Ready,
                    reference_count: entry.reference_count,
                    resident: false,
                    message: "text handle is registered".into(),
                }
            }
            Some(_) => UiTextHandleDiagnostic {
                handle,
                status: UiTextHandleStatus::GenerationMismatch,
                reference_count: 0,
                resident: false,
                message: "text handle generation is stale".into(),
            },
            None if self.released_generations.contains_key(&handle.id) => UiTextHandleDiagnostic {
                handle,
                status: UiTextHandleStatus::Released,
                reference_count: 0,
                resident: false,
                message: "text handle has been released".into(),
            },
            None => UiTextHandleDiagnostic {
                handle,
                status: UiTextHandleStatus::Missing,
                reference_count: 0,
                resident: false,
                message: "text handle is not registered".into(),
            },
        }
    }
    pub fn insert_dynamic(
        &mut self,
        expected_revision: Revision,
        text: String,
    ) -> Result<UiTextHandle, UiTextRegistryError> {
        self.insert(expected_revision, text, UiTextSourceCategory::Dynamic)
    }
    pub fn register_literal(
        &mut self,
        expected_revision: Revision,
        text: String,
    ) -> Result<UiTextHandle, UiTextRegistryError> {
        self.require_revision(expected_revision)?;
        self.validate_text(&text)?;
        if let Some(entry) = self.entries.values().find(|entry| {
            entry.record.category == UiTextSourceCategory::Literal && entry.record.text == text
        }) {
            return Ok(entry.record.handle);
        }
        self.insert(expected_revision, text, UiTextSourceCategory::Literal)
    }
    pub fn replace_dynamic(
        &mut self,
        expected_revision: Revision,
        handle: UiTextHandle,
        text: String,
    ) -> Result<(), UiTextRegistryError> {
        self.require_revision(expected_revision)?;
        self.validate_text(&text)?;
        if self.resolve(handle)?.category != UiTextSourceCategory::Dynamic {
            return Err(UiTextRegistryError {
                code: "ui_program_text_literal_immutable",
                message: "literal program text cannot be replaced",
            });
        }
        self.revision = Revision(self.revision.0 + 1);
        let revision = self.revision;
        let entry = self
            .entries
            .get_mut(&handle.id)
            .expect("validated text entry exists");
        entry.record.text = text;
        entry.record.byte_length = entry.record.text.len() as u32;
        entry.record.revision = revision;
        Ok(())
    }
    pub fn retain(
        &mut self,
        expected_revision: Revision,
        handle: UiTextHandle,
    ) -> Result<(), UiTextRegistryError> {
        self.require_revision(expected_revision)?;
        self.validate_handle(handle)?;
        let entry = self
            .entries
            .get_mut(&handle.id)
            .expect("validated text entry exists");
        entry.reference_count = entry.reference_count.saturating_add(1);
        self.revision = Revision(self.revision.0 + 1);
        Ok(())
    }
    pub fn release(
        &mut self,
        expected_revision: Revision,
        handle: UiTextHandle,
    ) -> Result<(), UiTextRegistryError> {
        self.require_revision(expected_revision)?;
        self.validate_handle(handle)?;
        let remove = {
            let entry = self
                .entries
                .get_mut(&handle.id)
                .expect("validated text entry exists");
            if entry.record.category == UiTextSourceCategory::Literal {
                return Err(UiTextRegistryError {
                    code: "ui_program_text_literal_immutable",
                    message: "literal program text is released with its program, not individually",
                });
            }
            entry.reference_count = entry.reference_count.saturating_sub(1);
            entry.reference_count == 0
        };
        if remove {
            self.entries.remove(&handle.id);
            self.released_generations
                .insert(handle.id, handle.generation);
            self.free_ids.push(handle.id);
        }
        self.revision = Revision(self.revision.0 + 1);
        Ok(())
    }
    pub fn resolve(&self, handle: UiTextHandle) -> Result<&UiTextRecord, UiTextRegistryError> {
        self.validate_handle(handle)?;
        Ok(&self.entries[&handle.id].record)
    }
    pub fn validate_handle(&self, handle: UiTextHandle) -> Result<(), UiTextRegistryError> {
        match self.entries.get(&handle.id) {
            Some(entry) if entry.record.handle.generation == handle.generation => Ok(()),
            Some(_) => Err(UiTextRegistryError {
                code: ERROR_UI_PROGRAM_TEXT_REGISTRY_GENERATION_MISMATCH,
                message: "text handle generation does not match the active record",
            }),
            None if self.released_generations.contains_key(&handle.id) => {
                Err(UiTextRegistryError {
                    code: ERROR_UI_PROGRAM_TEXT_REGISTRY_GENERATION_MISMATCH,
                    message: "text handle refers to a released or reused record",
                })
            }
            None => Err(UiTextRegistryError {
                code: ERROR_UI_PROGRAM_UNKNOWN_TEXT_HANDLE,
                message: "text handle is not present in this registry",
            }),
        }
    }
    pub fn diagnostic(&self, handle: UiTextHandle) -> UiTextHandleDiagnostic {
        match self.entries.get(&handle.id) {
            Some(entry) if entry.record.handle.generation == handle.generation => {
                UiTextHandleDiagnostic {
                    handle,
                    status: UiTextHandleStatus::Ready,
                    reference_count: entry.reference_count,
                    resident: false,
                    message: "text record is available; renderer residency is reported by WGPU"
                        .into(),
                }
            }
            Some(_) => UiTextHandleDiagnostic {
                handle,
                status: UiTextHandleStatus::GenerationMismatch,
                reference_count: 0,
                resident: false,
                message: "text handle generation does not match the active record".into(),
            },
            None if self.released_generations.contains_key(&handle.id) => UiTextHandleDiagnostic {
                handle,
                status: UiTextHandleStatus::Released,
                reference_count: 0,
                resident: false,
                message: "text record was released".into(),
            },
            None => UiTextHandleDiagnostic {
                handle,
                status: UiTextHandleStatus::Missing,
                reference_count: 0,
                resident: false,
                message: "text record is absent".into(),
            },
        }
    }
    fn insert(
        &mut self,
        expected_revision: Revision,
        text: String,
        category: UiTextSourceCategory,
    ) -> Result<UiTextHandle, UiTextRegistryError> {
        self.require_revision(expected_revision)?;
        self.validate_text(&text)?;
        if self.entries.len() >= self.capacity as usize {
            return Err(UiTextRegistryError {
                code: ERROR_UI_PROGRAM_TEXT_REGISTRY_CAPACITY_OVERFLOW,
                message: "text registry capacity is exhausted",
            });
        }
        let id = self.free_ids.pop().unwrap_or_else(|| {
            let id = self.next_id;
            self.next_id += 1;
            id
        });
        let generation = self
            .released_generations
            .get(&id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let handle = UiTextHandle { id, generation };
        self.revision = Revision(self.revision.0 + 1);
        self.entries.insert(
            id,
            UiTextRegistryEntry {
                record: UiTextRecord {
                    handle,
                    byte_length: text.len() as u32,
                    text,
                    category,
                    revision: self.revision,
                },
                reference_count: 1,
            },
        );
        Ok(handle)
    }
    fn require_revision(&self, expected: Revision) -> Result<(), UiTextRegistryError> {
        if expected == self.revision {
            Ok(())
        } else {
            Err(UiTextRegistryError {
                code: ERROR_UI_PROGRAM_TEXT_REGISTRY_STALE_REVISION,
                message: "text registry revision is stale",
            })
        }
    }
    fn validate_text(&self, text: &str) -> Result<(), UiTextRegistryError> {
        if text.len() > self.max_bytes_per_record as usize {
            Err(UiTextRegistryError {
                code: ERROR_UI_PROGRAM_TEXT_TOO_LONG,
                message: "text exceeds the registry record byte limit",
            })
        } else {
            Ok(())
        }
    }
}

impl UiInputStoreError {
    fn text(error: UiTextRegistryError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
}

/// A renderer-free compiler error. The code is stable for RPC/debug adapters;
/// the message provides author-facing context without exposing renderer state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiProgramCompileError {
    pub code: &'static str,
    pub message: String,
}

/// Renderer-local presentation values are deliberately distinct from resolved
/// domain inputs. This first CPU backend has no local overrides yet, but the
/// type makes the ownership boundary explicit for future WGPU parity work.
#[derive(Clone, Debug, PartialEq)]
pub struct UiLocalPresentationState {
    pub revision: Revision,
    pub machine_states: BTreeMap<String, String>,
    pub drag_offsets: BTreeMap<String, [f32; 2]>,
}

impl Default for UiLocalPresentationState {
    fn default() -> Self {
        Self {
            revision: Revision(0),
            machine_states: BTreeMap::new(),
            drag_offsets: BTreeMap::new(),
        }
    }
}

/// UI-program semantic event gate. It validates only declaration and resolved
/// UI state; routing an accepted intent to its domain owner remains the caller's
/// responsibility. This keeps the CPU backend useful without a WGPU dependency.
#[derive(Clone, Debug)]
pub struct UiProgramSemanticEventRouter {
    program: UiProgram,
    inputs: UiResolvedInputs,
    renderer_epoch: u64,
    next_trace_sequence: u64,
    idempotent_results: HashMap<String, UiProgramSemanticEventResult>,
    trace: Vec<UiEventTraceRecord>,
}

impl UiProgramSemanticEventRouter {
    pub fn new(program: UiProgram, inputs: UiResolvedInputs, renderer_epoch: u64) -> Self {
        Self {
            program,
            inputs,
            renderer_epoch,
            next_trace_sequence: 0,
            idempotent_results: HashMap::new(),
            trace: Vec::new(),
        }
    }

    pub fn replace_resolved_inputs(&mut self, inputs: UiResolvedInputs) {
        self.inputs = inputs;
    }
    pub fn set_renderer_epoch(&mut self, renderer_epoch: u64) {
        self.renderer_epoch = renderer_epoch;
    }
    pub fn trace(&self) -> &[UiEventTraceRecord] {
        &self.trace
    }

    pub fn validate(&mut self, event: &UiProgramSemanticEvent) -> UiProgramSemanticEventResult {
        if let Some(result) = self.idempotent_results.get(&event.idempotency_key) {
            let mut replay = result.clone();
            replay.status = UiProgramSemanticEventStatus::Duplicate;
            replay.code = Some(ERROR_UI_PROGRAM_EVENT_DUPLICATE_IDEMPOTENCY_KEY.into());
            self.record(event, &replay);
            return replay;
        }
        let failure = self.validate_fresh(event).err();
        let result = match failure {
            Some((code, message)) => UiProgramSemanticEventResult { event_id: event.event_id.clone(), status: UiProgramSemanticEventStatus::Rejected, code: Some(code.into()), accepted_input_revision: None, message: message.into() },
            None => UiProgramSemanticEventResult { event_id: event.event_id.clone(), status: UiProgramSemanticEventStatus::Accepted, code: None, accepted_input_revision: Some(self.inputs.input_revision), message: "semantic event accepted; controlled values remain pending until an external input frame arrives".into() },
        };
        self.idempotent_results
            .insert(event.idempotency_key.clone(), result.clone());
        self.record(event, &result);
        result
    }

    fn validate_fresh(
        &self,
        event: &UiProgramSemanticEvent,
    ) -> Result<(), (&'static str, &'static str)> {
        if event.event_id.trim().is_empty()
            || event.request_id.trim().is_empty()
            || event.idempotency_key.trim().is_empty()
            || event.interaction.interaction_id.trim().is_empty()
        {
            return Err((
                ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE,
                "event identity and interaction metadata are required",
            ));
        }
        if event.program_revision != self.program.revision
            || event.input_revision != self.inputs.input_revision
        {
            return Err((
                ERROR_UI_PROGRAM_EVENT_STALE_REVISION,
                "program or input revision is stale",
            ));
        }
        if event.interaction.renderer_epoch != self.renderer_epoch {
            return Err((
                ERROR_UI_PROGRAM_EVENT_INTERACTION_EPOCH_MISMATCH,
                "renderer epoch does not match the active event gate",
            ));
        }
        let node = self
            .program
            .nodes
            .iter()
            .find(|node| node.key == event.source_node_key)
            .ok_or((
                ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE,
                "event source node is not declared by this program",
            ))?;
        let declaration = self
            .program
            .event_records
            .iter()
            .find(|declaration| {
                declaration.node_key == node.key && declaration.intent == event.intent
            })
            .ok_or((
                ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE,
                "event intent is not declared by the source node",
            ))?;
        let state = evaluate_ui_program(
            &self.program,
            &self.inputs,
            UiCpuViewport {
                logical_bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: f32::MAX,
                    height: f32::MAX,
                },
                revision: Revision(0),
            },
            &UiLocalPresentationState::default(),
        )
        .nodes
        .into_iter()
        .find(|state| state.node_key == event.source_node_key)
        .ok_or((
            ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE,
            "event source has no evaluated state",
        ))?;
        if !state.visible || !state.enabled {
            return Err((
                ERROR_UI_PROGRAM_EVENT_CONTROL_UNAVAILABLE,
                "event source is hidden or disabled",
            ));
        }
        if matches!(event.kind, UiProgramSemanticEventKind::TextEditCommit)
            && !event
                .payload
                .values()
                .any(|value| matches!(value, UiSemanticPayloadValue::TextHandle { .. }))
        {
            return Err((
                ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED,
                "text edit commits require a bounded text handle payload",
            ));
        }
        let mut expected = declaration.literal_payload.clone();
        for key in &declaration.bound_input_keys {
            let value = self.inputs.values.get(key).ok_or((
                ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED,
                "event references an absent bound input",
            ))?;
            expected.insert(
                key.clone(),
                input_value_as_event_payload(&value.value).ok_or((
                    ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED,
                    "bound input kind cannot cross the semantic event boundary",
                ))?,
            );
        }
        if event.payload != expected {
            return Err((
                ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED,
                "event payload differs from declared literals or resolved bound inputs",
            ));
        }
        if let Some(requested) = &event.requested_value {
            let Some(key) = declaration.bound_input_keys.first() else {
                return Err((
                    ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED,
                    "requested values require a bound input",
                ));
            };
            let Some(current) = expected.get(key) else {
                return Err((
                    ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED,
                    "requested value input is absent",
                ));
            };
            if !same_payload_kind(current, requested) {
                return Err((
                    ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED,
                    "requested value does not match the bound input kind",
                ));
            }
        }
        Ok(())
    }

    fn record(&mut self, event: &UiProgramSemanticEvent, result: &UiProgramSemanticEventResult) {
        self.next_trace_sequence += 1;
        self.trace.push(UiEventTraceRecord {
            sequence: self.next_trace_sequence,
            event_id: event.event_id.clone(),
            intent: event.intent.clone(),
            source_node_key: event.source_node_key.clone(),
            program_revision: event.program_revision.revision,
            input_revision: event.input_revision,
            renderer_epoch: event.interaction.renderer_epoch,
            result: result.status,
            code: result.code.clone(),
            timestamp_unix_ms: 0,
        });
    }
}

fn input_value_as_event_payload(value: &UiInputValue) -> Option<UiSemanticPayloadValue> {
    Some(match value {
        UiInputValue::Bool { value } => UiSemanticPayloadValue::Bool { value: *value },
        UiInputValue::I32 { value } => UiSemanticPayloadValue::I32 { value: *value },
        UiInputValue::U32 { value } => UiSemanticPayloadValue::U32 { value: *value },
        UiInputValue::F32 { value } => UiSemanticPayloadValue::F32 { value: *value },
        UiInputValue::Enum { value } => UiSemanticPayloadValue::Enum {
            value: value.clone(),
        },
        UiInputValue::TextHandle { value } => UiSemanticPayloadValue::TextHandle { value: *value },
        UiInputValue::AssetHandle { id, generation } => UiSemanticPayloadValue::AssetHandle {
            id: *id,
            generation: *generation,
        },
        UiInputValue::Vec2 { .. } | UiInputValue::Vec4 { .. } | UiInputValue::Color { .. } => {
            return None;
        }
    })
}

/// Stable string name of an input kind, used in `nui.variable.changed` payloads.
fn ui_input_kind_name(kind: &neon_ui_schema::UiInputKind) -> String {
    use neon_ui_schema::UiInputKind;
    match kind {
        UiInputKind::Bool => "bool".into(),
        UiInputKind::I32 | UiInputKind::I32Range { .. } => "i32".into(),
        UiInputKind::U32 | UiInputKind::U32Range { .. } => "u32".into(),
        UiInputKind::F32 | UiInputKind::F32Range { .. } => "f32".into(),
        UiInputKind::Vec2 => "vec2".into(),
        UiInputKind::Vec4 => "vec4".into(),
        UiInputKind::Color => "color".into(),
        UiInputKind::Enum { .. } => "enum".into(),
        UiInputKind::TextHandle => "text".into(),
        UiInputKind::AssetHandle => "asset".into(),
    }
}

/// Converts a typed input value into a JSON observation for event payloads.
fn input_value_to_json(value: &UiInputValue) -> serde_json::Value {
    use neon_ui_schema::UiInputValue;
    match value {
        UiInputValue::Bool { value } => json!({"value": value}),
        UiInputValue::I32 { value } => json!({"value": value}),
        UiInputValue::U32 { value } => json!({"value": value}),
        UiInputValue::F32 { value } => json!({"value": value}),
        UiInputValue::Enum { value } => json!({"value": value}),
        UiInputValue::TextHandle { value } => json!({"text_handle": {"id": value.id, "generation": value.generation}}),
        UiInputValue::AssetHandle { id, generation } => {
            json!({"asset_id": id, "generation": generation})
        }
        UiInputValue::Vec2 { value } => json!({"value": value}),
        UiInputValue::Vec4 { value } | UiInputValue::Color { value } => json!({"value": value}),
    }
}

/// Converts legacy renderer transport events at the compiled-program boundary.
/// Pointer clicks are transport details; the active control determines intent.
fn program_semantic_event_kind(
    node_kind: &UiNodeKind,
    event_kind: &neon_ui_schema::UiSemanticEventType,
) -> UiProgramSemanticEventKind {
    use neon_ui_schema::UiSemanticEventType;
    match event_kind {
        UiSemanticEventType::ValuePreview => UiProgramSemanticEventKind::ValueTentative,
        UiSemanticEventType::FocusChanged | UiSemanticEventType::InteractionCancelled => {
            UiProgramSemanticEventKind::InteractionCancel
        }
        UiSemanticEventType::DragDrop => UiProgramSemanticEventKind::ValueCommit,
        UiSemanticEventType::PointerClick
        | UiSemanticEventType::ValueCommit
        | UiSemanticEventType::SelectionChanged
        | UiSemanticEventType::TextInputCommit => match node_kind {
            UiNodeKind::Checkbox
            | UiNodeKind::RadioButton
            | UiNodeKind::Selectable
            | UiNodeKind::Combo
            | UiNodeKind::Dropdown
            | UiNodeKind::Tabs
            | UiNodeKind::ListBox => UiProgramSemanticEventKind::SelectionChanged,
            UiNodeKind::Slider
            | UiNodeKind::DragValue
            | UiNodeKind::Scrollbar
            | UiNodeKind::ProgressBar => UiProgramSemanticEventKind::ValueCommit,
            UiNodeKind::TextInput => UiProgramSemanticEventKind::TextEditCommit,
            UiNodeKind::Button => UiProgramSemanticEventKind::Activate,
            _ => UiProgramSemanticEventKind::Activate,
        },
    }
}

/// Re-applies the compiled program to a submitted fragment after an external
/// scalar publication. Grid effects are intentionally maintained separately.
fn refresh_fragment_from_program(
    fragment: &mut UiFragment,
    program: &UiProgram,
    inputs: &UiResolvedInputs,
    schema: &UiInputSchema,
) {
    let evaluated = evaluate_ui_program(
        program,
        inputs,
        UiCpuViewport {
            logical_bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: f32::MAX,
                height: f32::MAX,
            },
            revision: Revision(0),
        },
        &UiLocalPresentationState::default(),
    );
    let states = evaluated
        .nodes
        .into_iter()
        .map(|state| (state.node_key.clone(), state))
        .collect::<BTreeMap<_, _>>();
    fn apply(node: &mut UiNode, states: &BTreeMap<String, UiCpuNodeState>) {
        if let Some(state) = states.get(&node.node_id.0) {
            node.visible = state.visible;
            node.enabled = state.enabled;
            node.style.opacity = state.opacity;
        }
        for child in &mut node.children {
            apply(child, states);
        }
    }
    apply(&mut fragment.root, &states);
    fragment
        .effects
        .retain(|effect| !matches!(effect, UiEffect::ControlPresentation { .. }));
    for node in &program.nodes {
        let Some(state) = states.get(&node.key) else {
            continue;
        };
        let presentation = match node.kind {
            UiNodeKind::Checkbox | UiNodeKind::RadioButton | UiNodeKind::Selectable => {
                Some(neon_ui_schema::UiControlPresentation::Toggle {
                    selected: state.active || state.selected,
                })
            }
            UiNodeKind::Slider | UiNodeKind::DragValue => state.numeric_value.and_then(|value| {
                program
                    .binding_records
                    .iter()
                    .find(|binding| {
                        binding.node_key == node.key
                            && binding.property == UiBoundProperty::NumericValue
                    })
                    .and_then(|binding| {
                        schema
                            .slots
                            .iter()
                            .find(|slot| slot.key == binding.input_key)
                    })
                    .and_then(|slot| match slot.kind {
                        neon_ui_schema::UiInputKind::I32Range { minimum, maximum } => {
                            Some((minimum as f32, maximum as f32))
                        }
                        neon_ui_schema::UiInputKind::U32Range { minimum, maximum } => {
                            Some((minimum as f32, maximum as f32))
                        }
                        neon_ui_schema::UiInputKind::F32Range { minimum, maximum } => {
                            Some((minimum, maximum))
                        }
                        _ => None,
                    })
                    .map(|bounds| neon_ui_schema::UiControlPresentation::Numeric {
                        value,
                        min: bounds.0,
                        max: bounds.1,
                    })
            }),
            UiNodeKind::Scrollbar => state.numeric_value.and_then(|value| {
                program
                    .binding_records
                    .iter()
                    .find(|binding| {
                        binding.node_key == node.key
                            && binding.property == UiBoundProperty::NumericValue
                    })
                    .and_then(|binding| {
                        schema
                            .slots
                            .iter()
                            .find(|slot| slot.key == binding.input_key)
                    })
                    .and_then(|slot| match slot.kind {
                        neon_ui_schema::UiInputKind::I32Range { minimum, maximum } => {
                            Some((minimum as f32, maximum as f32))
                        }
                        neon_ui_schema::UiInputKind::U32Range { minimum, maximum } => {
                            Some((minimum as f32, maximum as f32))
                        }
                        neon_ui_schema::UiInputKind::F32Range { minimum, maximum } => {
                            Some((minimum, maximum))
                        }
                        _ => None,
                    })
                    .map(
                        |(minimum, maximum)| neon_ui_schema::UiControlPresentation::Scroll {
                            position: if maximum > minimum {
                                ((value - minimum) / (maximum - minimum)).clamp(0.0, 1.0)
                            } else {
                                0.0
                            },
                        },
                    )
            }),
            UiNodeKind::Combo | UiNodeKind::Dropdown | UiNodeKind::Tabs | UiNodeKind::ListBox => {
                state.state_token.as_ref().map(|token| {
                    let options = program
                        .binding_records
                        .iter()
                        .find(|binding| {
                            binding.node_key == node.key
                                && binding.property == UiBoundProperty::StateToken
                        })
                        .and_then(|binding| {
                            schema
                                .slots
                                .iter()
                                .find(|slot| slot.key == binding.input_key)
                        })
                        .and_then(|slot| match &slot.kind {
                            neon_ui_schema::UiInputKind::Enum { variants } => {
                                Some(variants.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    neon_ui_schema::UiControlPresentation::Choice {
                        token: token.clone(),
                        options,
                        selected: state.selected || state.active,
                    }
                })
            }
            _ => None,
        };
        if let Some(state) = presentation {
            fragment.effects.push(UiEffect::ControlPresentation {
                node_id: UiNodeId(node.key.clone()),
                state,
            });
        }
    }
}

fn same_payload_kind(left: &UiSemanticPayloadValue, right: &UiSemanticPayloadValue) -> bool {
    matches!(
        (left, right),
        (
            UiSemanticPayloadValue::Bool { .. },
            UiSemanticPayloadValue::Bool { .. }
        ) | (
            UiSemanticPayloadValue::I32 { .. },
            UiSemanticPayloadValue::I32 { .. }
        ) | (
            UiSemanticPayloadValue::U32 { .. },
            UiSemanticPayloadValue::U32 { .. }
        ) | (
            UiSemanticPayloadValue::F32 { .. },
            UiSemanticPayloadValue::F32 { .. }
        ) | (
            UiSemanticPayloadValue::Enum { .. },
            UiSemanticPayloadValue::Enum { .. }
        )
    )
}

/// UI-owned cache of bounded, domain-prepared repeat rows. It preserves row
/// identity by stable key and never sorts, filters, or derives business data.
#[derive(Clone, Debug, Default)]
pub struct UiRepeatStore {
    frames: BTreeMap<String, UiRepeatFrame>,
}

/// UI-owned cache of bounded, domain-prepared virtual DataGrid windows.
/// It retains no off-window rows and never derives sorting or filtering.
#[derive(Clone, Debug, Default)]
pub struct UiDataGridStore {
    frames: BTreeMap<String, UiDataGridFrame>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiDataGridApplyResult {
    pub accepted_rows: u32,
}

impl UiDataGridStore {
    pub fn frame(&self, source_key: &str) -> Option<&UiDataGridFrame> {
        self.frames.get(source_key)
    }

    /// Attaches the current bounded grid windows to a fragment produced from the
    /// same compiled program. Frames are presentation data, not fragment topology.
    pub fn attach_to_fragment(
        &self,
        program: &UiProgram,
        fragment: &mut UiFragment,
    ) -> Result<(), UiProgramCompileError> {
        let mut effects = fragment
            .effects
            .iter()
            .filter(|effect| !matches!(effect, UiEffect::DataGridFrame { .. }))
            .cloned()
            .collect::<Vec<_>>();
        for grid in &program.data_grid_records {
            let Some(frame) = self.frame(&grid.source_key) else {
                continue;
            };
            if frame.expected_program_revision != program.revision {
                return Err(compile_error(
                    ERROR_UI_PROGRAM_STALE_INPUT_REVISION,
                    "DataGrid frame belongs to a different program revision",
                ));
            }
            effects.push(UiEffect::DataGridFrame {
                declaration: UiDataGridDeclaration {
                    node_key: grid.node_key.clone(),
                    source_key: grid.source_key.clone(),
                    max_window_rows: grid.max_window_rows,
                    row_height: grid.row_height,
                    overscan: grid.overscan,
                    columns: grid.columns.clone(),
                },
                frame: frame.clone(),
            });
        }
        let mut candidate = fragment.clone();
        candidate.effects = effects;
        candidate.validate().map_err(schema_compile_error)?;
        fragment.effects = candidate.effects;
        Ok(())
    }

    pub fn apply(
        &mut self,
        program: &UiProgram,
        input: UiDataGridInputFrame,
    ) -> Result<UiDataGridApplyResult, UiProgramCompileError> {
        let UiDataGridInputFrame { source_key, frame } = input;
        if frame.expected_program_revision != program.revision {
            return Err(compile_error(
                ERROR_UI_PROGRAM_STALE_INPUT_REVISION,
                "DataGrid frame belongs to a different program revision",
            ));
        }
        let grid = program
            .data_grid_records
            .iter()
            .find(|record| record.source_key == source_key)
            .ok_or_else(|| {
                compile_error(
                    ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
                    "DataGrid frame references an unknown grid source",
                )
            })?;
        if let Some(previous) = self.frames.get(&source_key)
            && frame.list_revision.0 < previous.list_revision.0
        {
            return Err(compile_error(
                ERROR_UI_PROGRAM_STALE_INPUT_REVISION,
                "DataGrid frame revision is stale",
            ));
        }
        let row_count = u64::try_from(frame.window_rows.len()).map_err(|_| {
            compile_error(
                ERROR_UI_PROGRAM_CAPACITY_OVERFLOW,
                "DataGrid window row count overflows u64",
            )
        })?;
        if row_count > u64::from(grid.max_window_rows)
            || frame.first_row > frame.total_rows
            || row_count > frame.total_rows - frame.first_row
        {
            return Err(compile_error(
                ERROR_UI_PROGRAM_CAPACITY_OVERFLOW,
                "DataGrid window exceeds its declared bounds",
            ));
        }
        let mut keys = HashSet::new();
        if frame
            .window_rows
            .iter()
            .any(|row| row.stable_row_key.trim().is_empty() || !keys.insert(&row.stable_row_key))
        {
            return Err(compile_error(
                ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
                "DataGrid window rows require unique nonempty stable row keys",
            ));
        }
        let column_keys = grid
            .columns
            .iter()
            .map(|column| column.key.as_str())
            .collect::<HashSet<_>>();
        if frame.window_rows.iter().any(|row| {
            row.cells.len() != column_keys.len()
                || row
                    .cells
                    .keys()
                    .any(|key| !column_keys.contains(key.as_str()))
                || row.cells.values().any(|cell| !cell.validate())
        }) {
            return Err(compile_error(
                ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
                "DataGrid rows must contain one valid typed/display cell for every declared column",
            ));
        }
        let accepted_rows = frame.window_rows.len() as u32;
        self.frames.insert(source_key, frame);
        Ok(UiDataGridApplyResult { accepted_rows })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiRepeatApplyResult {
    pub accepted_rows: u32,
    pub overflow_rows: u32,
    pub diagnostics: Vec<UiDiagnostic>,
}

impl UiRepeatStore {
    pub fn frame(&self, template_key: &str) -> Option<&UiRepeatFrame> {
        self.frames.get(template_key)
    }

    pub fn apply(
        &mut self,
        program: &UiProgram,
        frame: UiRepeatFrame,
    ) -> Result<UiRepeatApplyResult, UiProgramCompileError> {
        if frame.expected_program_revision != program.revision {
            return Err(compile_error(
                ERROR_UI_PROGRAM_STALE_INPUT_REVISION,
                "repeat frame belongs to a different program revision",
            ));
        }
        let template = program
            .template_records
            .iter()
            .find(|record| record.template_key == frame.template_key)
            .ok_or_else(|| {
                compile_error(
                    ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
                    "repeat frame references an unknown template",
                )
            })?;
        if let Some(previous) = self.frames.get(&frame.template_key) {
            if frame.list_revision.0 <= previous.list_revision.0 {
                return Err(compile_error(
                    ERROR_UI_PROGRAM_STALE_INPUT_REVISION,
                    "repeat frame revision is stale",
                ));
            }
        }
        let mut keys = HashSet::new();
        for row in &frame.rows {
            if row.stable_row_key.trim().is_empty() || !keys.insert(&row.stable_row_key) {
                return Err(compile_error(
                    ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
                    "repeat rows require unique nonempty stable row keys",
                ));
            }
            if row.values.len() != template.row_schema.len()
                || template.row_schema.iter().any(|(key, kind)| {
                    !row.values.get(key).is_some_and(|value| kind.accepts(value))
                })
            {
                return Err(compile_error(
                    ERROR_UI_PROGRAM_INPUT_TYPE_MISMATCH,
                    "repeat row values do not match the declared template row schema",
                ));
            }
        }
        let overflow_rows = frame
            .rows
            .len()
            .saturating_sub(template.max_instances as usize) as u32;
        if overflow_rows != 0 && !template.overflow_summary {
            return Err(compile_error(
                ERROR_UI_PROGRAM_CAPACITY_OVERFLOW,
                "repeat rows exceed capacity and this template declares no overflow summary",
            ));
        }
        let accepted_rows = frame.rows.len().min(template.max_instances as usize) as u32;
        let mut accepted = frame;
        accepted.rows.truncate(accepted_rows as usize);
        self.frames.insert(accepted.template_key.clone(), accepted);
        let diagnostics = if overflow_rows == 0 {
            Vec::new()
        } else {
            vec![cpu_diagnostic(
                ERROR_UI_PROGRAM_CAPACITY_OVERFLOW,
                "repeat rows exceed capacity; the declared overflow summary must be rendered",
                Some(&template.template_key),
                None,
                program.revision.revision,
            )]
        };
        Ok(UiRepeatApplyResult {
            accepted_rows,
            overflow_rows,
            diagnostics,
        })
    }
}

/// Pure deterministic lowering from canonical IR plus a validated input schema.
/// It performs no I/O, text-registry mutation, GPU allocation, or domain lookup.
pub fn compile_ui_program(
    document: &UiIrDocument,
    revision: UiProgramRevision,
    schema: &UiInputSchema,
) -> Result<UiProgram, UiProgramCompileError> {
    revision.validate_baseline().map_err(schema_compile_error)?;
    schema.validate().map_err(schema_compile_error)?;
    document.validate().map_err(schema_compile_error)?;
    let mut templates = Vec::new();
    let mut nodes = Vec::new();
    let mut layouts = Vec::new();
    let mut node_keys = HashSet::new();
    collect_program_nodes(
        &document.root,
        None,
        &mut templates,
        &mut nodes,
        &mut layouts,
        &mut node_keys,
    )?;
    if nodes.len() as u32 > document.resource_budget.max_nodes
        || nodes.len() as u32 > document.resource_budget.max_instances
    {
        return Err(compile_error(
            "ui_program_capacity_overflow",
            "node count exceeds the declared program budget",
        ));
    }
    if document.bindings.len() as u32 > document.resource_budget.max_bindings
        || document.events.len() as u32 > document.resource_budget.max_events
    {
        return Err(compile_error(
            "ui_program_capacity_overflow",
            "binding or event count exceeds the declared program budget",
        ));
    }
    let clip_count = templates
        .iter()
        .filter(|node| {
            node.layout
                .as_ref()
                .is_some_and(|layout| layout.clip != neon_ui_schema::UiClipPolicy::None)
        })
        .count() as u32;
    if clip_count > document.resource_budget.max_clips {
        return Err(compile_error(
            "ui_program_capacity_overflow",
            "clip count exceeds the declared program budget",
        ));
    }
    let literal_texts = compile_literal_texts(&templates)?;
    let glyph_count = literal_texts
        .iter()
        .map(|entry| entry.text.chars().count() as u32)
        .sum::<u32>();
    if literal_texts.len() as u32 > document.resource_budget.max_text_records
        || glyph_count > document.resource_budget.max_glyph_instances
    {
        return Err(compile_error(
            "ui_program_capacity_overflow",
            "literal text or glyph count exceeds the declared program budget",
        ));
    }
    let branch_records = compile_branch_records(document, schema, &nodes)?;
    let template_records = compile_template_records(document, &nodes)?;
    let data_grid_records = compile_data_grid_records(document, schema, &nodes)?;
    let template_instances = template_records
        .iter()
        .try_fold(0u32, |total, template| {
            let count = template.node_range.len() as u32;
            total
                .checked_add(count.saturating_mul(template.max_instances))
                .ok_or(())
        })
        .map_err(|_| {
            compile_error(
                ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
                "template instance count overflows the declared resource budget",
            )
        })?;
    if template_instances > document.resource_budget.max_instances {
        return Err(compile_error(
            ERROR_UI_PROGRAM_CAPACITY_OVERFLOW,
            "preallocated template instances exceed the declared program budget",
        ));
    }
    for node in &templates {
        if matches!(node.kind, UiNodeKind::Image | UiNodeKind::RenderSurface) {
            let kind = if node.kind == UiNodeKind::Image {
                UiProgramResourceKind::Image
            } else {
                UiProgramResourceKind::RenderSurface
            };
            let resource_key = if node.kind == UiNodeKind::Image {
                document
                    .image_resources
                    .get(&node.node_id.0)
                    .map(String::as_str)
                    .unwrap_or(&node.node_id.0)
            } else {
                &node.node_id.0
            };
            if !document.resources.iter().any(|resource| {
                resource.key == resource_key && (resource.kind == kind || resource.has_fallback)
            }) {
                return Err(compile_error(
                    "ui_program_missing_resource",
                    "node resource is not declared and has no fallback",
                ));
            }
        }
    }
    let mut bindings = Vec::new();
    let mut input_to_bindings = BTreeMap::new();
    let mut node_to_dependents = BTreeMap::new();
    for (index, declaration) in document.bindings.iter().enumerate() {
        if declaration.node_key.trim().is_empty()
            || declaration.input_key.trim().is_empty()
            || !node_keys.contains(&declaration.node_key)
        {
            return Err(compile_error(
                "ui_program_unknown_binding_target",
                "binding references an unknown semantic node key",
            ));
        }
        let Some(slot) = schema
            .slots
            .iter()
            .find(|slot| slot.key == declaration.input_key)
        else {
            return Err(compile_error(
                "ui_program_unknown_input_key",
                "binding references an undeclared input slot",
            ));
        };
        if !binding_accepts(&declaration.property, &slot.kind) {
            return Err(compile_error(
                "ui_program_input_type_mismatch",
                "binding property is incompatible with the declared input kind",
            ));
        }
        let id = index as u32;
        bindings.push(UiBinding {
            binding_id: id,
            input_key: declaration.input_key.clone(),
            node_key: declaration.node_key.clone(),
            property: declaration.property.clone(),
            expected_kind: slot.kind.clone(),
            default_resolved_value: slot.default_value.clone(),
        });
        input_to_bindings
            .entry(declaration.input_key.clone())
            .or_insert_with(Vec::new)
            .push(id);
        node_to_dependents
            .entry(declaration.node_key.clone())
            .or_insert_with(Vec::new)
            .push(id);
    }
    for event in &document.events {
        if event.node_key.trim().is_empty()
            || event.intent.trim().is_empty()
            || !node_keys.contains(&event.node_key)
            || event
                .allowed_payload_keys
                .iter()
                .any(|key| key.trim().is_empty())
        {
            return Err(compile_error(
                "ui_program_invalid_event",
                "event declaration must target a node and have a nonempty typed intent",
            ));
        }
        let mut payload_keys = HashSet::new();
        for key in event
            .literal_payload
            .keys()
            .chain(event.bound_input_keys.iter())
        {
            if key.trim().is_empty() || !payload_keys.insert(key) {
                return Err(compile_error(
                    "ui_program_invalid_event",
                    "event payload keys must be nonempty and unique",
                ));
            }
        }
        for input_key in &event.bound_input_keys {
            let Some(slot) = schema.slots.iter().find(|slot| &slot.key == input_key) else {
                return Err(compile_error(
                    ERROR_UI_PROGRAM_UNKNOWN_INPUT_KEY,
                    "event binding references an unknown input slot",
                ));
            };
            if input_value_as_event_payload(&slot.default_value).is_none() {
                return Err(compile_error(
                    ERROR_UI_PROGRAM_INPUT_TYPE_MISMATCH,
                    "event bindings support only scalar, enum, handle, and text-handle inputs",
                ));
            }
        }
    }
    let node_to_source_span = nodes
        .iter()
        .map(|node| (node.key.clone(), node.source_span.clone()))
        .collect();
    let dependencies = UiDependencyIndex {
        input_to_bindings,
        node_to_source_span,
        node_to_dependents,
    };
    let layout_hash = stable_program_hash(document, schema, &nodes, &bindings);
    Ok(UiProgram {
        revision,
        nodes,
        node_templates: templates,
        literal_texts,
        layout_records: layouts,
        binding_records: bindings,
        branch_records,
        template_records,
        data_grid_records,
        drag_records: Vec::new(),
        drop_records: Vec::new(),
        event_records: document.events.clone(),
        resource_budget: document.resource_budget.clone(),
        dependency_index: dependencies,
        layout_hash,
    })
}

fn compile_data_grid_records(
    document: &UiIrDocument,
    schema: &UiInputSchema,
    nodes: &[neon_ui_schema::UiProgramNode],
) -> Result<Vec<UiDataGridRecord>, UiProgramCompileError> {
    document.data_grids.iter().map(|grid| {
        if grid.max_window_rows == 0 || grid.row_height == 0 || grid.overscan > grid.max_window_rows || grid.columns.is_empty()
            || grid.columns.iter().any(|column| !column.validate())
            || grid.columns.iter().map(|column| &column.key).collect::<HashSet<_>>().len() != grid.columns.len()
            || !nodes.iter().any(|node| node.key == grid.node_key && node.kind == UiNodeKind::DataGrid)
            || !schema.grid_slots.iter().any(|slot| slot.key == grid.source_key) {
            Err(compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "DataGrid declaration must target a DataGrid node with positive bounded metrics and columns"))
        } else {
            Ok(UiDataGridRecord {
                node_key: grid.node_key.clone(), source_key: grid.source_key.clone(), max_window_rows: grid.max_window_rows,
                row_height: grid.row_height, overscan: grid.overscan, columns: grid.columns.clone(),
            })
        }
    }).collect()
}

/// Supported CPU execution backend. It consumes the same program and resolved
/// input snapshot planned for the renderer backend and only emits logical data.
pub fn evaluate_ui_program(
    program: &UiProgram,
    inputs: &UiResolvedInputs,
    viewport: UiCpuViewport,
    local: &UiLocalPresentationState,
) -> UiCpuFrameOutput {
    let mut diagnostics = Vec::new();
    if program.revision != inputs.program_revision {
        diagnostics.push(cpu_diagnostic(
            "ui_program_stale_input_revision",
            "resolved inputs belong to another program revision",
            None,
            None,
            program.revision.revision,
        ));
    }
    let mut states = BTreeMap::new();
    for template in &program.node_templates {
        let literal = program
            .literal_texts
            .iter()
            .find(|entry| entry.node_key == template.node_id.0)
            .map(|entry| entry.handle);
        states.insert(
            template.node_id.0.clone(),
            UiCpuNodeState {
                node_key: template.node_id.0.clone(),
                visible: template.visible,
                enabled: template.enabled,
                selected: false,
                active: false,
                numeric_value: None,
                state_token: None,
                text: literal,
                image: None,
                opacity: template.style.opacity,
                scroll_offset: template
                    .layout
                    .map_or([0.0; 2], |layout| layout.scroll_offset),
            },
        );
    }
    for binding in &program.binding_records {
        let Some(value) = inputs
            .values
            .get(&binding.input_key)
            .map(|resolved| &resolved.value)
        else {
            diagnostics.push(cpu_diagnostic(
                "ui_program_unknown_input_key",
                "resolved input is absent",
                Some(&binding.node_key),
                Some(&binding.input_key),
                program.revision.revision,
            ));
            continue;
        };
        let state = states
            .get_mut(&binding.node_key)
            .expect("compiled binding target exists");
        apply_binding(
            state,
            &binding.property,
            value,
            &mut diagnostics,
            program.revision.revision,
            &binding.node_key,
            &binding.input_key,
        );
    }
    // Branch topology is compiled once. Evaluation only gates its precompiled
    // nodes, so a domain input cannot allocate or alter component kinds.
    for branch in &program.branch_records {
        let active = branch_predicate_matches(&branch.predicate, inputs, local);
        if !active {
            for node_key in &branch.node_range {
                if let Some(state) = states.get_mut(node_key) {
                    state.visible = false;
                }
            }
        }
    }
    let mut logical_layout = program.layout_records.clone();
    for record in &mut logical_layout {
        let offset = cumulative_drag_offset(&record.node_key, &program.nodes, &local.drag_offsets);
        record.bounds.x += offset[0];
        record.bounds.y += offset[1];
    }
    let mut clips = BTreeMap::new();
    let mut primitives = Vec::new();
    let mut semantic_targets = Vec::new();
    for record in &logical_layout {
        let state = states.get(&record.node_key).expect("compiled state exists");
        let mut bounds = record.bounds;
        if record.node_key
            == program
                .nodes
                .first()
                .map(|node| node.key.as_str())
                .unwrap_or("")
        {
            bounds.width = bounds.width.min(viewport.logical_bounds.width);
            bounds.height = bounds.height.min(viewport.logical_bounds.height);
        }
        if program
            .node_templates
            .iter()
            .find(|node| node.node_id.0 == record.node_key)
            .and_then(|node| node.layout)
            .is_some_and(|layout| layout.clip != neon_ui_schema::UiClipPolicy::None)
        {
            clips.insert(record.node_key.clone(), bounds);
        }
        if state.visible {
            primitives.push(UiCpuRenderPrimitive {
                node_key: record.node_key.clone(),
                kind: program
                    .nodes
                    .iter()
                    .find(|node| node.key == record.node_key)
                    .expect("compiled node exists")
                    .kind
                    .clone(),
                bounds,
                clip: clips.get(&record.node_key).copied(),
            });
        }
        let intents = program
            .event_records
            .iter()
            .filter(|event| event.node_key == record.node_key)
            .map(|event| event.intent.clone())
            .collect::<Vec<_>>();
        if !intents.is_empty() {
            semantic_targets.push(UiCpuSemanticTarget {
                node_key: record.node_key.clone(),
                intents,
                enabled: state.enabled,
                visible: state.visible,
            });
        }
    }
    UiCpuFrameOutput {
        program_revision: program.revision.clone(),
        input_revision: inputs.input_revision,
        nodes: states.into_values().collect(),
        logical_layout,
        clips,
        render_primitives: primitives,
        semantic_targets,
        diagnostics,
    }
}

fn cumulative_drag_offset(
    node_key: &str,
    nodes: &[UiProgramNode],
    offsets: &BTreeMap<String, [f32; 2]>,
) -> [f32; 2] {
    let mut key = Some(node_key);
    let mut total = [0.0; 2];
    while let Some(current) = key {
        if let Some(offset) = offsets.get(current) {
            total[0] += offset[0];
            total[1] += offset[1];
        }
        key = nodes
            .iter()
            .find(|node| node.key == current)
            .and_then(|node| node.parent_key.as_deref());
    }
    total
}

fn branch_predicate_matches(
    predicate: &UiBranchPredicate,
    inputs: &UiResolvedInputs,
    local: &UiLocalPresentationState,
) -> bool {
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
        UiBranchPredicate::MachineState { machine_key, state } => local
            .machine_states
            .get(machine_key)
            .is_some_and(|active| active == state),
    }
}

fn collect_program_nodes(
    node: &UiNode,
    parent: Option<String>,
    templates: &mut Vec<UiNode>,
    nodes: &mut Vec<UiProgramNode>,
    layouts: &mut Vec<UiProgramLayoutRecord>,
    keys: &mut HashSet<String>,
) -> Result<(), UiProgramCompileError> {
    if node.node_id.0.trim().is_empty() || !keys.insert(node.node_id.0.clone()) {
        return Err(compile_error(
            "ui_program_duplicate_node_key",
            "IR node keys must be unique across the document",
        ));
    }
    nodes.push(UiProgramNode {
        key: node.node_id.0.clone(),
        parent_key: parent.clone(),
        kind: node.kind.clone(),
        source_span: None,
    });
    templates.push(node.clone());
    layouts.push(UiProgramLayoutRecord {
        node_key: node.node_id.0.clone(),
        bounds: node.bounds,
        layout: node.layout,
    });
    for child in &node.children {
        collect_program_nodes(
            child,
            Some(node.node_id.0.clone()),
            templates,
            nodes,
            layouts,
            keys,
        )?;
    }
    Ok(())
}

fn compile_branch_records(
    document: &UiIrDocument,
    schema: &UiInputSchema,
    nodes: &[UiProgramNode],
) -> Result<Vec<UiBranchRecord>, UiProgramCompileError> {
    document.branches.iter().map(|branch| {
        let valid = match &branch.predicate {
            UiBranchPredicate::MachineState { .. } => true,
            UiBranchPredicate::Bool { input_key, .. } => matches!(schema.slots.iter().find(|slot| slot.key == *input_key).map(|slot| &slot.kind), Some(neon_ui_schema::UiInputKind::Bool)),
            UiBranchPredicate::EnumEquals { input_key, variant } => matches!(schema.slots.iter().find(|slot| slot.key == *input_key).map(|slot| &slot.kind), Some(neon_ui_schema::UiInputKind::Enum { variants }) if variants.iter().any(|value| value == variant)),
        };
        if !valid { return Err(compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "branch predicate must be a direct bool or declared enum equality")); }
        Ok(UiBranchRecord {
            branch_key: branch.branch_key.clone(), predicate: branch.predicate.clone(),
            node_range: subtree_keys(nodes, &branch.root_node_key),
            layout_participation: branch.layout_participation.clone(),
        })
    }).collect()
}

fn compile_template_records(
    document: &UiIrDocument,
    nodes: &[UiProgramNode],
) -> Result<Vec<UiTemplateRecord>, UiProgramCompileError> {
    document
        .templates
        .iter()
        .map(|template| {
            if template
                .row_schema
                .values()
                .any(|kind| matches!(kind, neon_ui_schema::UiInputKind::AssetHandle))
            {
                return Err(compile_error(
                    ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
                    "template row schema cannot contain renderer resource handles",
                ));
            }
            let node_range = subtree_keys(nodes, &template.root_node_key);
            if node_range.is_empty() {
                return Err(compile_error(
                    ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
                    "template root must identify a compiled subtree",
                ));
            }
            Ok(UiTemplateRecord {
                template_key: template.template_key.clone(),
                node_range,
                max_instances: template.max_instances,
                row_schema: template.row_schema.clone(),
                instance_key_field: template.instance_key_field.clone(),
                overflow_summary: template.overflow_summary,
            })
        })
        .collect()
}

fn subtree_keys(nodes: &[UiProgramNode], root: &str) -> Vec<String> {
    let mut result = vec![root.to_owned()];
    let mut index = 0;
    while index < result.len() {
        let parent = result[index].clone();
        result.extend(
            nodes
                .iter()
                .filter(|node| node.parent_key.as_deref() == Some(parent.as_str()))
                .map(|node| node.key.clone()),
        );
        index += 1;
    }
    result
}
fn compile_literal_texts(
    templates: &[UiNode],
) -> Result<Vec<UiProgramLiteralText>, UiProgramCompileError> {
    let mut handles = HashMap::new();
    let mut entries = Vec::new();
    for node in templates {
        if let Some(TextRef::Literal { value }) = &node.text {
            let id = stable_text_id(value);
            if let Some(existing) = handles.insert(id, value) {
                if existing != value {
                    return Err(compile_error(
                        "ui_program_literal_text_collision",
                        "literal text table hash collision requires an explicit registry entry",
                    ));
                }
            }
            entries.push(UiProgramLiteralText {
                node_key: node.node_id.0.clone(),
                handle: UiTextHandle { id, generation: 1 },
                text: value.clone(),
            });
        }
    }
    Ok(entries)
}
fn stable_text_id(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
fn binding_accepts(property: &UiBoundProperty, kind: &neon_ui_schema::UiInputKind) -> bool {
    use neon_ui_schema::UiInputKind::*;
    match property {
        UiBoundProperty::TextValue => matches!(kind, TextHandle),
        UiBoundProperty::Visible
        | UiBoundProperty::Enabled
        | UiBoundProperty::Selected
        | UiBoundProperty::Active => matches!(kind, Bool),
        UiBoundProperty::NumericValue => {
            matches!(
                kind,
                I32 | U32 | F32 | I32Range { .. } | U32Range { .. } | F32Range { .. }
            )
        }
        UiBoundProperty::ImageAsset => matches!(kind, AssetHandle),
        UiBoundProperty::Opacity => matches!(kind, F32),
        UiBoundProperty::StateToken => matches!(kind, Enum { .. }),
        UiBoundProperty::ScrollOffset => matches!(kind, Vec2),
    }
}
fn apply_binding(
    state: &mut UiCpuNodeState,
    property: &UiBoundProperty,
    value: &UiInputValue,
    diagnostics: &mut Vec<UiDiagnostic>,
    revision: Revision,
    node_key: &str,
    input_key: &str,
) {
    match (property, value) {
        (UiBoundProperty::TextValue, UiInputValue::TextHandle { value }) => {
            state.text = Some(*value)
        }
        (UiBoundProperty::Visible, UiInputValue::Bool { value }) => state.visible = *value,
        (UiBoundProperty::Enabled, UiInputValue::Bool { value }) => state.enabled = *value,
        (UiBoundProperty::Selected, UiInputValue::Bool { value }) => state.selected = *value,
        (UiBoundProperty::Active, UiInputValue::Bool { value }) => state.active = *value,
        (UiBoundProperty::ImageAsset, value @ UiInputValue::AssetHandle { .. }) => {
            state.image = Some(value.clone())
        }
        (UiBoundProperty::Opacity, UiInputValue::F32 { value }) => {
            state.opacity = value.clamp(0.0, 1.0)
        }
        (UiBoundProperty::ScrollOffset, UiInputValue::Vec2 { value }) => {
            state.scroll_offset = *value
        }
        (UiBoundProperty::NumericValue, UiInputValue::I32 { value }) => {
            state.numeric_value = Some(*value as f32)
        }
        (UiBoundProperty::NumericValue, UiInputValue::U32 { value }) => {
            state.numeric_value = Some(*value as f32)
        }
        (UiBoundProperty::NumericValue, UiInputValue::F32 { value }) => {
            state.numeric_value = Some(*value)
        }
        (UiBoundProperty::StateToken, UiInputValue::Enum { value }) => {
            state.state_token = Some(value.clone())
        }
        _ => diagnostics.push(cpu_diagnostic(
            "ui_program_input_type_mismatch",
            "resolved value does not match its compiled binding",
            Some(node_key),
            Some(input_key),
            revision,
        )),
    }
}
fn cpu_diagnostic(
    code: &'static str,
    message: &'static str,
    node_key: Option<&str>,
    input_key: Option<&str>,
    revision: Revision,
) -> UiDiagnostic {
    UiDiagnostic {
        code: code.into(),
        severity: UiDiagnosticSeverity::Error,
        message: message.into(),
        node_key: node_key.map(str::to_owned),
        input_key: input_key.map(str::to_owned),
        source_span: None,
        revision,
    }
}
fn compile_error(code: &'static str, message: impl Into<String>) -> UiProgramCompileError {
    UiProgramCompileError {
        code,
        message: message.into(),
    }
}
fn schema_compile_error(error: UiSchemaError) -> UiProgramCompileError {
    compile_error(
        "ui_program_invalid_schema",
        format!("program schema validation failed: {error:?}"),
    )
}
fn stable_program_hash(
    document: &UiIrDocument,
    schema: &UiInputSchema,
    nodes: &[UiProgramNode],
    bindings: &[UiBinding],
) -> String {
    let source = serde_json::to_vec(&(document, schema, nodes, bindings))
        .expect("program schema serializes");
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in source {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64-{hash:016x}")
}

pub struct UiRuntime {
    epoch: u64,
    client: ClientIdentity,
    cached_fragment: Option<UiFragment>,
    journal: CommandJournal,
    receipts: HashMap<RequestId, CommandReceipt>,
    last_input_sequence: HashMap<u64, u64>,
    idempotent_responses: HashMap<String, RpcResponse>,
    surface: UiSurfaceMachine,
    ai_terrain: AiTerrainPanelState,
    showcase_text: String,
    host_adapter: Option<UiHostAdapter>,
    eventd_endpoint: Option<SocketAddr>,
    interaction_traces: InteractionTraceStore,
}

impl UiRuntime {
    pub fn new(epoch: u64, instance_id: impl Into<String>) -> Self {
        Self {
            epoch,
            client: ClientIdentity {
                kind: ClientKind::UiRuntime,
                instance_id: instance_id.into(),
                pid: std::process::id(),
                origin: "neon-ui-runtime".into(),
            },
            cached_fragment: None,
            journal: CommandJournal::new(ServiceName(SERVICE_NAME.into()), epoch, 128),
            receipts: HashMap::new(),
            last_input_sequence: HashMap::new(),
            idempotent_responses: HashMap::new(),
            surface: UiSurfaceMachine::new(UiSurfaceId(WORKBENCH_SURFACE_ID.into())),
            ai_terrain: AiTerrainPanelState::default(),
            showcase_text: String::new(),
            host_adapter: None,
            eventd_endpoint: None,
            interaction_traces: InteractionTraceStore::default(),
        }
    }

    pub fn service_health(&self) -> ServiceHealth {
        ServiceHealth {
            service: ServiceName(SERVICE_NAME.into()),
            status: HealthStatus::Healthy,
            epoch: self.epoch,
        }
    }

    /// Sets the `neon-eventd` endpoint used to publish directed `emitevent`
    /// observations. Pass `None` to run without event forwarding.
    pub fn with_eventd_endpoint(mut self, endpoint: Option<SocketAddr>) -> Self {
        self.eventd_endpoint = endpoint;
        self
    }

    pub fn service_description(&self) -> ServiceDescription {
        ServiceDescription {
            service: ServiceName(SERVICE_NAME.into()),
            protocol_version: PROTOCOL_VERSION,
            endpoint: "headless://ui-runtime".into(),
            epoch: self.epoch,
            capabilities: vec![
                "ui.static_fragment.submit.v1".into(),
                "ui.fragment.submit.v1".into(),
                "ui.semantic_input.v1".into(),
                "ui.intent_dispatch.v1".into(),
                "ui.surface.machine.v1".into(),
                "ui.ai.terrain.panel.v1".into(),
                "ui.text_input.commit.v1".into(),
                "ui.program.input.v1".into(),
                "ui.input.repeat.v1".into(),
                "ui.data_grid.window.v1".into(),
                CAPABILITY_DEBUG_INTERACTION.into(),
            ],
        }
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        DebugSnapshot {
            service: ServiceName(SERVICE_NAME.into()),
            epoch: self.epoch,
            revision: self
                .cached_fragment
                .as_ref()
                .map_or(Revision(0), |fragment| fragment.revision),
            health: HealthStatus::Healthy,
            capabilities: self.service_description().capabilities,
            active_jobs: Vec::new(),
        }
    }

    pub fn handle_service_request(&mut self, request: RpcRequest) -> RpcResponse {
        let result = match request.method.as_str() {
            "service.health" => Some(json!(self.service_health())),
            "service.describe" => Some(json!(self.service_description())),
            "service.shutdown" => Some(json!({"state": "accepted"})),
            "debug.snapshot.get" => Some(json!(self.debug_snapshot())),
            "debug.ui.host.snapshot" => Some(json!(
                self.host_adapter.as_ref().map(UiHostAdapter::snapshot)
            )),
            "debug.command.get" => return self.handle_debug_command(request),
            "debug.trace.query" => return self.handle_debug_trace(request),
            "debug.interaction.get" => return self.handle_interaction_get(request),
            "debug.interaction.query" => return self.handle_interaction_query(request),
            "ui.fragment.submit" => return self.handle_fragment_submit(request),
            "ui.surface.snapshot.get" => Some(self.surface_value()),
            "ui.ai.terrain.snapshot.get" => Some(self.ai_terrain.snapshot()),
            "ui.surface.event" => return self.handle_surface_event(request),
            "ui.input.event" => return self.handle_input_event(request),
            "ui.input.frame" => return self.handle_external_input_frame(request),
            "ui.input.repeat" => return self.handle_repeat_input(request),
            "ui.intent.dispatch" => return self.handle_intent_dispatch(request),
            _ => None,
        };
        match result {
            Some(result) => RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Accepted,
                revision: Some(
                    self.cached_fragment
                        .as_ref()
                        .map_or(Revision(0), |fragment| fragment.revision),
                ),
                result: Some(result),
                snapshot: None,
                error: None,
            },
            None => RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Rejected,
                revision: None,
                result: None,
                snapshot: None,
                error: Some(neon_protocol::RpcError {
                    code: "unsupported_method".into(),
                    message: "method is not supported".into(),
                    current_revision: None,
                    object_id: None,
                }),
            },
        }
    }

    fn handle_surface_event(&mut self, request: RpcRequest) -> RpcResponse {
        let Some(key) = request.idempotency_key.clone() else {
            return self.rejected(
                request.request_id,
                "invalid_request",
                "idempotency_key is required",
            );
        };
        if let Some(response) = self.idempotent_responses.get(&key) {
            let mut response = response.clone();
            response.request_id = request.request_id;
            return response;
        }
        if request.expected_revision != Some(self.surface.revision) {
            return self.rejected(
                request.request_id,
                "revision_conflict",
                "UI surface revision is stale",
            );
        }
        let event = match self.parse_surface_event(&request.params) {
            Ok(event) => event,
            Err((code, message)) => return self.rejected(request.request_id, code, message),
        };
        let transition = match self.surface.transition(event) {
            Ok(transition) => transition,
            Err((code, message)) => return self.rejected(request.request_id, code, message),
        };
        self.record_receipt(&request.request_id, CommandState::Accepted, None);
        let response = RpcResponse {
            request_id: request.request_id,
            status: RpcStatus::Accepted,
            revision: Some(transition.snapshot.revision),
            result: Some(json!(transition.snapshot)),
            snapshot: None,
            error: None,
        };
        self.idempotent_responses.insert(key, response.clone());
        response
    }

    fn parse_surface_event(
        &self,
        params: &Value,
    ) -> Result<UiSurfaceEvent, (&'static str, &'static str)> {
        let request =
            serde_json::from_value::<UiSurfaceEventRequest>(params.clone()).map_err(|_| {
                (
                    "invalid_request",
                    "a supported UI surface event is required",
                )
            })?;
        if request.schema_version != UI_SURFACE_SCHEMA_VERSION {
            return Err((
                "unsupported_ui_surface_schema",
                "UI surface schema version is not supported",
            ));
        }
        if request.surface_id != self.surface.surface_id {
            return Err(("surface_not_found", "UI surface is not available"));
        }
        Ok(request.event)
    }

    fn surface_value(&self) -> Value {
        json!(self.surface.snapshot())
    }

    fn handle_debug_command(&mut self, request: RpcRequest) -> RpcResponse {
        let Some(request_id) = request.params.get("request_id").and_then(Value::as_str) else {
            return self.rejected(
                request.request_id,
                "invalid_request",
                "request_id is required",
            );
        };
        match self.command_receipt(&RequestId(request_id.into())).cloned() {
            Some(receipt) => self.accepted(request.request_id, json!(receipt)),
            None => self.rejected(
                request.request_id,
                "not_found",
                "command receipt was not found",
            ),
        }
    }

    fn handle_debug_trace(&mut self, request: RpcRequest) -> RpcResponse {
        let filter = JournalFilter {
            request_id: request
                .params
                .get("request_id")
                .and_then(Value::as_str)
                .map(|value| RequestId(value.into())),
            event_id: request
                .params
                .get("event_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..JournalFilter::default()
        };
        self.accepted(request.request_id, json!(self.traces(&filter)))
    }

    fn apply_ai_terrain_intent(
        &mut self,
        action: &str,
        params: &Value,
    ) -> Result<Option<Value>, (&'static str, &'static str)> {
        match action {
            "ai.terrain.condition.set" => {
                let dimension = params
                    .get("dimension")
                    .and_then(Value::as_str)
                    .ok_or(("invalid_request", "condition dimension is required"))?;
                let index = params
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(("invalid_request", "condition index is required"))?;
                let slot = match dimension {
                    "sub" if index < 23 => &mut self.ai_terrain.condition.sub,
                    "parent" if index < 8 => &mut self.ai_terrain.condition.parent,
                    "relief" if index < 5 => &mut self.ai_terrain.condition.relief,
                    "texture" if index < 4 => &mut self.ai_terrain.condition.texture,
                    "water" if index < 3 => &mut self.ai_terrain.condition.water,
                    _ => return Err(("invalid_request", "condition index is out of range")),
                };
                *slot = Some(index);
            }
            "ai.terrain.condition.reset" => {
                self.ai_terrain.condition = AiTerrainCondition::default()
            }
            "ai.terrain.settings.set" => {
                if let Some(steps) = params
                    .get("steps")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                {
                    if !(1..=200).contains(&steps) {
                        return Err(("invalid_request", "steps must be in 1..=200"));
                    }
                    self.ai_terrain.steps = steps;
                }
                if let Some(guidance) = params.get("guidance").and_then(Value::as_f64) {
                    if !(0.0..=16.0).contains(&guidance) {
                        return Err(("invalid_request", "guidance must be in 0..=16"));
                    }
                    self.ai_terrain.guidance = guidance as f32;
                }
            }
            "ai.terrain.seed.next" => self.ai_terrain.seed = self.ai_terrain.seed.wrapping_add(1),
            _ => return Ok(None),
        }
        self.ai_terrain.state = "idle".into();
        self.ai_terrain.job_id = None;
        self.ai_terrain.elapsed_ms = None;
        self.ai_terrain.error_code = None;
        self.ai_terrain.advance();
        Ok(Some(self.ai_terrain.snapshot()))
    }

    /// Runs the UI declaration control plane. The configured host endpoint owns
    /// program-specific behavior; this runtime validates only the generic host contract.
    pub fn serve_forwarder(
        endpoint: SocketAddr,
        wgpu_endpoint: SocketAddr,
        domain_endpoint: SocketAddr,
        eventd_endpoint: Option<SocketAddr>,
        epoch: u64,
    ) -> Result<(), TransportError> {
        let server = RpcServer::bind(endpoint)?;
        let mut runtime =
            Self::new(epoch, "ui-runtime-forwarder").with_eventd_endpoint(eventd_endpoint);
        server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            let request_id = request.request_id.clone();
            let response = if request.method == "ui.fragment.submit" {
                eprintln!("[neon-ui-runtime] received ui.fragment.submit request={}", request.request_id.0);
                runtime
                    .forward_fragment(wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else if request.method == "ui.flow.submit" {
                eprintln!("[neon-ui-runtime] received ui.flow.submit request={}", request.request_id.0);
                runtime
                    .forward_flow_source(wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "ui_flow_submit_failed", &error.to_string())
                    })
            } else if request.method == "ui.host.inbound" {
                runtime
                    .forward_host_request(domain_endpoint, wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else if request.method == "ui.input.event" && renderer_event_targets_wgpu(&request) {
                runtime
                    .forward_wgpu_event(wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else if request.method == "ui.input.event" {
                runtime
                    .forward_host_request(domain_endpoint, wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else {
                runtime.handle_service_request(request)
            };
            (response, !shutdown)
        })
    }

    /// Accepts NUI Flow source at runtime. Parsing, lowering, program
    /// activation, and fragment submission remain UI Runtime responsibilities;
    /// external hosts do not need to compile NUI or run a domain controller just
    /// to display a declarative UI.
    pub fn forward_flow_source(
        &mut self,
        wgpu_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let source = request
            .params
            .get("source")
            .and_then(Value::as_str)
            .filter(|source| !source.trim().is_empty())
            .ok_or_else(|| TransportError::Io(std::io::Error::other("NUI source is required")))?;
        let document = parse_nui_flow(source)
            .map_err(|error| TransportError::Io(std::io::Error::other(format!("NUI parse failed: {error:?}"))))?;
        let revision = UiProgramRevision {
            program_id: document.ir.surface_id.0.clone(),
            revision: Revision(1),
            schema_version: neon_ui_schema::UI_PROGRAM_SCHEMA_VERSION,
            capabilities: [
                neon_ui_schema::UI_PROGRAM_CAPABILITY_NAME,
                neon_ui_schema::UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
                neon_ui_schema::UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
                neon_ui_schema::UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
            ]
            .into_iter()
            .map(|name| UiProgramCapability {
                name: name.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Supported,
            })
            .collect(),
        };
        let program = compile_nui_flow_program(&document, revision)
            .map_err(|error| TransportError::Io(std::io::Error::other(format!("NUI compile failed: {error:?}"))))?;
        // Re-submitting the same flow advances the fragment revision so the
        // renderer accepts the replacement instead of treating it as stale.
        let fragment_revision = self
            .cached_fragment
            .as_ref()
            .map_or(Revision(1), |current| Revision(current.revision.0 + 1));
        let fragment = UiFragment {
            fragment_id: UiFragmentId(document.ir.surface_id.0.clone()),
            revision: fragment_revision,
            root: document.ir.root.clone(),
            effects: lower_nui_flow_effects(&document),
        };
        let adapter = UiHostAdapter::activate(
            program.clone(),
            document.input_schema.clone(),
            self.epoch,
        )
        .map_err(|error| TransportError::Io(std::io::Error::other(error.message)))?
        .with_event_publisher(self.eventd_endpoint, self.client.clone());
        self.host_adapter = Some(adapter);
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            client: self.client.clone(),
            target: ServiceName("ui-runtime".into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment)
            }),
            expected_revision: None,
            idempotency_key: request.idempotency_key.clone(),
        };
        let response = self.forward_fragment(wgpu_endpoint, forwarded)?;
        if response.status == RpcStatus::Accepted {
            let mut enriched = response;
            enriched.result = Some(json!({
                "state": "accepted",
                "program_revision": program.revision,
                "input_schema": document.input_schema,
                "surface_id": document.ir.surface_id,
                "renderer": enriched.result,
            }));
            return Ok(enriched);
        }
        Ok(response)
    }

    fn handle_interaction_get(&mut self, request: RpcRequest) -> RpcResponse {
        let Some(interaction_id) = request.params.get("interaction_id").and_then(Value::as_str)
        else {
            return self.rejected(
                request.request_id,
                "invalid_request",
                "interaction_id is required",
            );
        };
        let interaction_id = InteractionId(interaction_id.into());
        let records = self.interaction_traces.get(&interaction_id);
        if records.is_empty() {
            return self.rejected(
                request.request_id,
                "not_found",
                "interaction trace was not found",
            );
        }
        self.accepted(
            request.request_id,
            json!({"interaction_id": interaction_id, "records": records}),
        )
    }

    fn handle_interaction_query(&mut self, request: RpcRequest) -> RpcResponse {
        let query = match serde_json::from_value::<InteractionTraceQuery>(request.params) {
            Ok(query) => query,
            Err(_) => {
                return self.rejected(
                    request.request_id,
                    "invalid_request",
                    "invalid interaction trace query",
                );
            }
        };
        self.accepted(
            request.request_id,
            json!({"records": self.interaction_traces.query(&query)}),
        )
    }

    fn record_interaction(
        &mut self,
        context: &InteractionTraceContext,
        stage: InteractionTraceStage,
        outcome: InteractionTraceOutcome,
        error: Option<InteractionTraceError>,
        downstream_request_id: Option<RequestId>,
    ) {
        self.interaction_traces.append(
            context.interaction_id.clone(),
            stage,
            outcome,
            error,
            context.semantic_target.clone(),
            context.fragment_revision,
            context.composition_revision,
            downstream_request_id,
        );
    }

    fn forward_host_request(
        &mut self,
        host_endpoint: SocketAddr,
        wgpu_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let request_id = request.request_id.clone();
        let Some(idempotency_key) = request.idempotency_key.clone() else {
            return Ok(self.rejected(request_id, "invalid_request", "idempotency_key is required"));
        };
        if let Some(response) = self.idempotent_responses.get(&idempotency_key) {
            let mut response = response.clone();
            response.request_id = request_id;
            return Ok(response);
        }
        self.ensure_host_adapter(host_endpoint)?;
        let inbound = match serde_json::from_value::<UiHostInbound>(request.params.clone()) {
            Ok(inbound) => inbound,
            Err(_) => self.renderer_event_to_host_inbound(request.params.clone())?,
        };
        if let UiHostInbound::DragDrop {
            active_fragment, ..
        } = &inbound
            && self.cached_fragment.as_ref() != Some(&active_fragment.clone().into_fragment())
        {
            return Ok(self.rejected(
                request_id,
                "ui_host_stale_fragment_revision",
                "drag/drop active fragment context is not the cached fragment",
            ));
        }
        let context = inbound_interaction_context(
            &inbound,
            self.cached_fragment
                .as_ref()
                .map_or(Revision(0), |fragment| fragment.revision),
        );
        if let Some(context) = &context {
            self.record_interaction(
                context,
                InteractionTraceStage::InboundReceived,
                InteractionTraceOutcome::Accepted,
                None,
                None,
            );
        }
        let adapter = self
            .host_adapter
            .as_ref()
            .expect("host adapter is activated");
        if let Err(error) = adapter.validate_inbound(inbound.clone()) {
            if let Some(context) = &context {
                self.record_interaction(
                    context,
                    InteractionTraceStage::AdapterValidationRejected,
                    InteractionTraceOutcome::Rejected,
                    Some(InteractionTraceError {
                        code: error.code.into(),
                        message: error.message.into(),
                    }),
                    None,
                );
            }
            return Ok(self.rejected(request_id, error.code, error.message));
        }
        if let Some(context) = &context {
            self.record_interaction(
                context,
                InteractionTraceStage::AdapterValidationAccepted,
                InteractionTraceOutcome::Accepted,
                None,
                None,
            );
        }
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId(format!("{}-host", request_id.0)),
            client: self.client.clone(),
            target: ServiceName("ui-host".into()),
            method: "ui.host.inbound".into(),
            params: json!(inbound),
            expected_revision: None,
            idempotency_key: Some(idempotency_key.clone()),
        };
        if let Some(context) = &context {
            self.record_interaction(
                context,
                InteractionTraceStage::HostForwarded,
                InteractionTraceOutcome::Pending,
                None,
                Some(forwarded.request_id.clone()),
            );
        }
        let host_response = RpcClient::connect(host_endpoint)?.call(&forwarded)?;
        if host_response.status != RpcStatus::Accepted {
            if let Some(context) = &context {
                let error = host_response
                    .error
                    .as_ref()
                    .map(|error| InteractionTraceError {
                        code: error.code.clone(),
                        message: error.message.clone(),
                    })
                    .unwrap_or(InteractionTraceError {
                        code: "ui_host_response_rejected".into(),
                        message: "UI host rejected the inbound interaction".into(),
                    });
                self.record_interaction(
                    context,
                    InteractionTraceStage::HostResponseRejected,
                    InteractionTraceOutcome::Rejected,
                    Some(error),
                    Some(forwarded.request_id),
                );
            }
            let mut response = host_response;
            response.request_id = request_id;
            return Ok(response);
        }
        if let Some(context) = &context {
            self.record_interaction(
                context,
                InteractionTraceStage::HostResponseAccepted,
                InteractionTraceOutcome::Accepted,
                None,
                Some(forwarded.request_id.clone()),
            );
        }
        let publication: UiHostPublication = match host_response
            .result
            .clone()
            .and_then(|value| serde_json::from_value(value).ok())
        {
            Some(publication) => publication,
            None => {
                if let Some(context) = &context {
                    self.record_interaction(
                        context,
                        InteractionTraceStage::PublicationRejected,
                        InteractionTraceOutcome::Rejected,
                        Some(InteractionTraceError {
                            code: "ui_host_invalid_publication".into(),
                            message: "host accepted without a valid UI publication".into(),
                        }),
                        None,
                    );
                }
                return Ok(self.rejected(
                    request_id,
                    "ui_host_invalid_publication",
                    "host accepted without a valid UI publication",
                ));
            }
        };
        let mut candidate = self
            .host_adapter
            .clone()
            .expect("host adapter is activated");
        let replacement = publication.presentation_update.clone();
        let publication_result = if replacement.is_some() {
            None
        } else {
            match candidate.apply_publication(publication) {
                Ok(result) => Some(result),
                Err(error) => {
                    if let Some(context) = &context {
                        self.record_interaction(
                            context,
                            InteractionTraceStage::PublicationRejected,
                            InteractionTraceOutcome::Rejected,
                            Some(InteractionTraceError {
                                code: error.code.into(),
                                message: error.message.into(),
                            }),
                            None,
                        );
                    }
                    return Ok(self.rejected(request_id, error.code, error.message));
                }
            }
        };
        let fragment = match self.cached_fragment.clone() {
            Some(fragment) => fragment,
            None => {
                if let Some(context) = &context {
                    self.record_interaction(
                        context,
                        InteractionTraceStage::PublicationRejected,
                        InteractionTraceOutcome::Rejected,
                        Some(InteractionTraceError {
                            code: "ui_host_fragment_unavailable".into(),
                            message: "no UI fragment has been submitted".into(),
                        }),
                        None,
                    );
                }
                return Ok(self.rejected(
                    request_id,
                    "ui_host_fragment_unavailable",
                    "no UI fragment has been submitted",
                ));
            }
        };
        let mut updated = if let Some(update) = replacement {
            let (replacement_adapter, replacement_fragment) =
                match candidate.apply_presentation_update(update, &fragment) {
                    Ok(replacement) => replacement,
                    Err(error) => {
                        return Ok(self.rejected(request_id, error.code, error.message));
                    }
                };
            candidate = replacement_adapter;
            replacement_fragment
        } else {
            let mut updated = fragment.clone();
            updated.revision = Revision(updated.revision.0 + 1);
            refresh_fragment_from_program(
                &mut updated,
                candidate.program(),
                &publication_result
                    .as_ref()
                    .expect("ordinary publication has a result")
                    .snapshot
                    .scalar_inputs,
                candidate.input_schema(),
            );
            updated
        };
        if let Some(publication_result) = publication_result {
            for effect in &mut updated.effects {
                if let UiEffect::DataGridFrame { declaration, frame } = effect
                    && let Some(input) = publication_result
                        .snapshot
                        .grid_inputs
                        .iter()
                        .find(|input| input.source_key == declaration.source_key)
                {
                    *frame = input.frame.clone();
                }
            }
            for input in &publication_result.snapshot.grid_inputs {
                if updated.effects.iter().any(|effect| matches!(effect, UiEffect::DataGridFrame { declaration, .. } if declaration.source_key == input.source_key)) {
                    continue;
                }
                if let Some(record) = candidate
                    .program()
                    .data_grid_records
                    .iter()
                    .find(|record| record.source_key == input.source_key)
                {
                    updated.effects.push(UiEffect::DataGridFrame {
                        declaration: UiDataGridDeclaration {
                            node_key: record.node_key.clone(),
                            source_key: record.source_key.clone(),
                            max_window_rows: record.max_window_rows,
                            row_height: record.row_height,
                            overscan: record.overscan,
                            columns: record.columns.clone(),
                        },
                        frame: input.frame.clone(),
                    });
                }
            }
        }
        if updated.validate().is_err() {
            if let Some(context) = &context {
                self.record_interaction(
                    context,
                    InteractionTraceStage::PublicationRejected,
                    InteractionTraceOutcome::Rejected,
                    Some(InteractionTraceError {
                        code: "ui_host_invalid_fragment".into(),
                        message: "host publication cannot be applied to the active fragment".into(),
                    }),
                    None,
                );
            }
            return Ok(self.rejected(
                request_id,
                "ui_host_invalid_fragment",
                "host publication cannot be applied to the active fragment",
            ));
        }
        if let Some(context) = &context {
            self.record_interaction(
                context,
                InteractionTraceStage::PublicationApplied,
                InteractionTraceOutcome::Accepted,
                None,
                None,
            );
        }
        let submit = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId(format!("{}-fragment", request_id.0)),
            client: self.client.clone(),
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(updated)
            }),
            expected_revision: Some(fragment.revision),
            idempotency_key: Some(format!("ui-host-fragment:{idempotency_key}")),
        };
        let mut response = self.forward_fragment(wgpu_endpoint, submit)?;
        if let Some(context) = &context {
            let stage = if response.status == RpcStatus::Accepted {
                InteractionTraceStage::WgpuFragmentSubmissionAccepted
            } else {
                InteractionTraceStage::WgpuFragmentSubmissionRejected
            };
            let outcome = if response.status == RpcStatus::Accepted {
                InteractionTraceOutcome::Accepted
            } else {
                InteractionTraceOutcome::Rejected
            };
            let error = (response.status != RpcStatus::Accepted).then(|| {
                response
                    .error
                    .as_ref()
                    .map(|error| InteractionTraceError {
                        code: error.code.clone(),
                        message: error.message.clone(),
                    })
                    .unwrap_or(InteractionTraceError {
                        code: "wgpu_fragment_submission_rejected".into(),
                        message: "WGPU rejected the UI fragment submission".into(),
                    })
            });
            self.record_interaction(
                context,
                stage,
                outcome,
                error,
                Some(RequestId(format!("{}-fragment", request_id.0))),
            );
        }
        if response.status == RpcStatus::Accepted {
            self.host_adapter = Some(candidate);
            self.idempotent_responses
                .insert(idempotency_key, response.clone());
        }
        response.request_id = request_id;
        Ok(response)
    }

    fn ensure_host_adapter(&mut self, host_endpoint: SocketAddr) -> Result<(), TransportError> {
        if self.host_adapter.is_some() {
            return Ok(());
        }
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("ui-host-adapter-config".into()),
            client: self.client.clone(),
            target: ServiceName("ui-host".into()),
            method: "ui.host.adapter.get".into(),
            params: json!({}),
            expected_revision: None,
            idempotency_key: Some("ui-host-adapter-config".into()),
        };
        let response = RpcClient::connect(host_endpoint)?.call(&request)?;
        let config: UiHostAdapterConfig = response
            .result
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| {
                TransportError::Io(std::io::Error::other(
                    "host did not provide adapter configuration",
                ))
            })?;
        let mut adapter = UiHostAdapter::activate(config.program, config.input_schema, self.epoch)
            .map_err(|error| TransportError::Io(std::io::Error::other(error.message)))?
            .with_event_publisher(self.eventd_endpoint, self.client.clone());
        let initial_grids = self
            .cached_fragment
            .as_ref()
            .map(|fragment| {
                fragment
                    .effects
                    .iter()
                    .filter_map(|effect| match effect {
                        UiEffect::DataGridFrame { declaration, frame } => {
                            Some(UiDataGridInputFrame {
                                source_key: declaration.source_key.clone(),
                                frame: frame.clone(),
                            })
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        adapter
            .seed_grid_inputs(initial_grids)
            .map_err(|error| TransportError::Io(std::io::Error::other(error.message)))?;
        self.host_adapter = Some(adapter);
        Ok(())
    }

    fn renderer_event_to_host_inbound(
        &mut self,
        params: Value,
    ) -> Result<UiHostInbound, TransportError> {
        let event: UiSemanticEvent = serde_json::from_value(params)
            .map_err(|_| TransportError::Io(std::io::Error::other("invalid UI host inbound")))?;
        self.validate_semantic_event(&event)
            .map_err(|code| TransportError::Io(std::io::Error::other(code)))?;
        if event.data_grid_cell.is_some() {
            return Ok(UiHostInbound::DataGridCell { event });
        }
        if let Some(payload) = event.drag_drop {
            let adapter = self
                .host_adapter
                .as_ref()
                .expect("host adapter is activated");
            let drag = adapter
                .program()
                .drag_records
                .iter()
                .find(|record| record.source_node_key == payload.source_key)
                .ok_or_else(|| {
                    TransportError::Io(std::io::Error::other(
                        "renderer drag source is not declared by the active host program",
                    ))
                })?;
            let neon_ui_schema::UiIntent::Invoke { action, .. } = &event.intent;
            let drop = adapter
                .program()
                .drop_records
                .iter()
                .find(|record| {
                    record.target_node_key == payload.target_key
                        && record.accepts_drag_key == drag.key
                        && record.placement == payload.placement
                        && record.presentation_template_key == payload.presentation_template_key
                        && record.intent == *action
                })
                .ok_or_else(|| {
                    TransportError::Io(std::io::Error::other(
                        "renderer drop is not declared by the active host program",
                    ))
                })?;
            return Ok(UiHostInbound::DragDrop {
                event: UiProgramDragDropEvent {
                    event_id: event.event_id.clone(),
                    drag_key: drag.key.clone(),
                    drop_key: drop.key.clone(),
                    intent: action.clone(),
                    payload,
                    program_revision: adapter.program().revision.clone(),
                    input_revision: adapter.snapshot().scalar_inputs.input_revision,
                    request_id: event.event_id.clone(),
                    idempotency_key: event.event_id.clone(),
                    interaction: UiSemanticInteractionMetadata {
                        interaction_id: event.event_id,
                        sequence: event.pointer.map_or(1, |pointer| pointer.sequence),
                        renderer_epoch: event.renderer_epoch,
                    },
                },
                active_fragment: UiHostFragmentContext::from_fragment(
                    self.cached_fragment
                        .as_ref()
                        .expect("validated renderer event has an active fragment"),
                ),
            });
        }
        let neon_ui_schema::UiIntent::Invoke { action, .. } = &event.intent;
        let adapter = self
            .host_adapter
            .as_ref()
            .expect("host adapter is activated");
        let declaration = adapter
            .program()
            .event_records
            .iter()
            .find(|declaration| declaration.intent == *action)
            .ok_or_else(|| {
                TransportError::Io(std::io::Error::other(
                    "renderer intent is not declared by the active host program",
                ))
            })?;
        let mut payload = declaration.literal_payload.clone();
        for key in &declaration.bound_input_keys {
            let value = adapter
                .snapshot()
                .scalar_inputs
                .values
                .get(key)
                .and_then(|value| input_value_as_event_payload(&value.value))
                .ok_or_else(|| {
                    TransportError::Io(std::io::Error::other("host bound input is unavailable"))
                })?;
            payload.insert(key.clone(), value);
        }
        let node = adapter
            .program()
            .nodes
            .iter()
            .find(|node| node.key == declaration.node_key)
            .ok_or_else(|| {
                TransportError::Io(std::io::Error::other(
                    "event declaration targets no active program node",
                ))
            })?;
        let kind = program_semantic_event_kind(&node.kind, &event.event);
        Ok(UiHostInbound::SemanticIntent {
            event: UiProgramSemanticEvent {
                event_id: event.event_id.clone(),
                kind,
                intent: action.clone(),
                source_node_key: declaration.node_key.clone(),
                payload,
                program_revision: adapter.program().revision.clone(),
                input_revision: adapter.snapshot().scalar_inputs.input_revision,
                request_id: event.event_id.clone(),
                idempotency_key: event.event_id.clone(),
                requested_value: event.control_value,
                interaction: UiSemanticInteractionMetadata {
                    interaction_id: event.event_id,
                    sequence: event.pointer.map_or(1, |pointer| pointer.sequence),
                    renderer_epoch: event.renderer_epoch,
                },
            },
        })
    }

    /// The cache is published only after the renderer accepted the exact declaration.
    pub fn forward_fragment(
        &mut self,
        wgpu_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let request_id = request.request_id.clone();
        let Some(idempotency_key) = request.idempotency_key.clone() else {
            return Ok(self.rejected(request_id, "invalid_request", "idempotency_key is required"));
        };
        if let Some(cached) = self.idempotent_responses.get(&idempotency_key) {
            let mut response = cached.clone();
            response.request_id = request_id;
            return Ok(response);
        }
        let fragment = match self.validate_fragment_submission(&request) {
            Ok(fragment) => fragment,
            Err((code, message)) => return Ok(self.rejected(request_id, code, message)),
        };
        self.record_receipt(&request.request_id, CommandState::Received, None);
        self.journal.append(TraceLevel::Info, EVENT_COMMAND_RECEIVED, Some(request.request_id.clone()), None, None, None, Some(self.debug_snapshot().revision), None, json!({"method": "ui.fragment.submit", "fragment_id": fragment.fragment_id.0, "fragment_revision": fragment.revision.0}));
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            client: self.client.clone(),
            target: ServiceName("wgpu-runtime".into()),
            method: "wgpu.ui.submit_fragment".into(),
            params: request.params,
            expected_revision: None,
            idempotency_key: Some(idempotency_key.clone()),
        };
        let response = RpcClient::connect(wgpu_endpoint)?.call(&forwarded)?;
        if response.status == RpcStatus::Accepted {
            self.cached_fragment = Some(fragment);
            self.record_receipt(&request.request_id, CommandState::Accepted, None);
            self.journal.append(
                TraceLevel::Info,
                EVENT_COMMAND_ACCEPTED,
                Some(request.request_id.clone()),
                None,
                None,
                None,
                response.revision,
                response.revision,
                json!({"target": "wgpu-runtime", "state": "accepted"}),
            );
            self.idempotent_responses
                .insert(idempotency_key, response.clone());
        } else {
            self.record_receipt(
                &request.request_id,
                CommandState::Rejected,
                response.error.as_ref().map(|error| error.code.clone()),
            );
            self.journal.append(TraceLevel::Warn, EVENT_COMMAND_REJECTED, Some(request.request_id.clone()), None, None, None, Some(self.debug_snapshot().revision), response.revision, json!({"target": "wgpu-runtime", "state": "rejected", "code": response.error.as_ref().map(|error| error.code.clone())}));
        }
        Ok(response)
    }

    fn handle_fragment_submit(&mut self, request: RpcRequest) -> RpcResponse {
        let fragment = match self.validate_fragment_submission(&request) {
            Ok(fragment) => fragment,
            Err((code, message)) => return self.rejected(request.request_id, code, message),
        };
        let revision = fragment.revision;
        self.cached_fragment = Some(fragment);
        self.accepted(
            request.request_id,
            json!({"fragment_revision": revision, "state": "accepted"}),
        )
    }

    fn validate_fragment_submission(
        &self,
        request: &RpcRequest,
    ) -> Result<UiFragment, (&'static str, &'static str)> {
        let command: UiCommand = serde_json::from_value(request.params.clone())
            .map_err(|_| ("invalid_request", "invalid UI command"))?;
        let UiCommand::SubmitFragment { submission } = command else {
            return Err(("invalid_request", "expected submit_fragment command"));
        };
        if submission.validate().is_err() {
            return Err(("invalid_request", "invalid UI fragment submission"));
        }
        if request
            .expected_revision
            .is_some_and(|expected| expected != self.debug_snapshot().revision)
        {
            return Err(("revision_conflict", "UI fragment revision is stale"));
        }
        if self
            .cached_fragment
            .as_ref()
            .is_some_and(|current| submission.fragment.fragment_id != current.fragment_id)
        {
            return Err((
                "fragment_identity_change",
                "active UI fragment identity cannot be changed by submission",
            ));
        }
        if self
            .cached_fragment
            .as_ref()
            .is_some_and(|current| submission.fragment.revision <= current.revision)
        {
            return Err(("revision_conflict", "UI fragment revision is stale"));
        }
        Ok(submission.fragment)
    }

    /// Forwards a renderer-resolved semantic event as a typed domain RPC.
    /// Render IDs and fragment-local node IDs are not accepted or emitted here.
    pub fn dispatch_semantic_event(
        &mut self,
        terrain_endpoint: SocketAddr,
        event: UiSemanticEvent,
        request_id: RequestId,
        idempotency_key: String,
    ) -> Result<RpcResponse, TransportError> {
        self.validate_semantic_event(&event)
            .map_err(|code| TransportError::Io(std::io::Error::other(code)))?;
        let neon_ui_schema::UiIntent::Invoke { action, mut params } = event.intent.clone();
        if let Some(drop) = &event.drag_drop {
            if let Some(object) = params.as_object_mut() {
                object.insert("source_key".into(), json!(drop.source_key));
                object.insert("target_key".into(), json!(drop.target_key));
                object.insert("placement".into(), json!(drop.placement));
            }
        }
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            client: self.client.clone(),
            target: ServiceName("terrain-runtime".into()),
            method: action,
            params,
            expected_revision: Some(event.fragment.revision),
            idempotency_key: Some(idempotency_key),
        };
        self.record_receipt(&request_id, CommandState::Received, None);
        self.journal.append(TraceLevel::Info, EVENT_COMMAND_RECEIVED, Some(request_id.clone()), None, None, None, Some(event.fragment.revision), None, json!({"event_id": event.event_id, "target": "terrain-runtime", "method": request.method}));
        let response = RpcClient::connect(terrain_endpoint)?.call(&request)?;
        let accepted = response.status == RpcStatus::Accepted;
        self.record_receipt(
            &request_id,
            if accepted {
                CommandState::Accepted
            } else {
                CommandState::Rejected
            },
            response.error.as_ref().map(|error| error.code.clone()),
        );
        self.journal.append(if accepted { TraceLevel::Info } else { TraceLevel::Warn }, if accepted { EVENT_COMMAND_ACCEPTED } else { EVENT_COMMAND_REJECTED }, Some(request_id), None, None, None, Some(event.fragment.revision), response.revision, json!({"event_id": event.event_id, "renderer_epoch": event.renderer_epoch, "fragment_id": event.fragment.id.0, "target": "terrain-runtime", "method": request.method}));
        Ok(response)
    }

    pub fn forward_wgpu_event(
        &mut self,
        wgpu_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let event: UiSemanticEvent = serde_json::from_value(request.params.clone())
            .map_err(|_| TransportError::Io(std::io::Error::other("invalid UI semantic event")))?;
        self.validate_semantic_event(&event)
            .map_err(|code| TransportError::Io(std::io::Error::other(code)))?;
        let neon_ui_schema::UiIntent::Invoke { action, params } = event.intent.clone();
        if action != "ai.terrain.generate" {
            return Err(TransportError::Io(std::io::Error::other(
                "intent is not a WGPU AI command",
            )));
        }
        let request_id = request.request_id.clone();
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: request_id.clone(),
            client: self.client.clone(),
            target: ServiceName("wgpu-runtime".into()),
            method: "wgpu.ai.terrain.generate".into(),
            params,
            expected_revision: Some(event.fragment.revision),
            idempotency_key: request.idempotency_key,
        };
        self.record_receipt(&request_id, CommandState::Received, None);
        self.journal.append(
            TraceLevel::Info,
            EVENT_COMMAND_RECEIVED,
            Some(request_id.clone()),
            None,
            None,
            None,
            Some(event.fragment.revision),
            None,
            json!({"event_id": event.event_id, "target": "wgpu-runtime", "method": forwarded.method}),
        );
        self.ai_terrain.state = "generating".into();
        self.ai_terrain.job_id = None;
        self.ai_terrain.elapsed_ms = None;
        self.ai_terrain.error_code = None;
        self.ai_terrain.advance();
        let response = match RpcClient::connect(wgpu_endpoint)
            .and_then(|mut client| client.call(&forwarded))
        {
            Ok(response) => response,
            Err(error) => {
                self.ai_terrain.state = "failed".into();
                self.ai_terrain.error_code = Some("service_unavailable".into());
                self.ai_terrain.advance();
                return Err(error);
            }
        };
        let accepted = response.status == RpcStatus::Accepted;
        if accepted {
            self.ai_terrain.state = "ready".into();
            self.ai_terrain.job_id = response
                .result
                .as_ref()
                .and_then(|result| result.get("job_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let rendered_seed = response
                .result
                .as_ref()
                .and_then(|result| result.get("seed"))
                .and_then(Value::as_u64)
                .unwrap_or(self.ai_terrain.seed);
            self.ai_terrain.last_seed = Some(rendered_seed);
            self.ai_terrain.seed = rendered_seed.wrapping_add(1);
            self.ai_terrain.elapsed_ms = response
                .result
                .as_ref()
                .and_then(|result| result.get("elapsed_ms"))
                .and_then(Value::as_f64);
        } else {
            self.ai_terrain.state = "failed".into();
            self.ai_terrain.error_code = response.error.as_ref().map(|error| error.code.clone());
        }
        self.ai_terrain.advance();
        self.record_receipt(
            &request_id,
            if accepted {
                CommandState::Accepted
            } else {
                CommandState::Rejected
            },
            response.error.as_ref().map(|error| error.code.clone()),
        );
        self.journal.append(
            if accepted { TraceLevel::Info } else { TraceLevel::Warn },
            if accepted { EVENT_COMMAND_ACCEPTED } else { EVENT_COMMAND_REJECTED },
            Some(request_id),
            None,
            None,
            None,
            Some(event.fragment.revision),
            response.revision,
            json!({"event_id": event.event_id, "target": "wgpu-runtime", "method": forwarded.method}),
        );
        Ok(response)
    }

    pub fn command_receipt(&self, request_id: &RequestId) -> Option<&CommandReceipt> {
        self.receipts.get(request_id)
    }

    fn handle_input_event(&mut self, mut request: RpcRequest) -> RpcResponse {
        let event: UiSemanticEvent = match serde_json::from_value(request.params.clone()) {
            Ok(event) => event,
            Err(_) => {
                return self.rejected(
                    request.request_id,
                    "invalid_request",
                    "invalid UI semantic event",
                );
            }
        };
        match self.validate_semantic_event(&event) {
            Ok(()) => {
                let neon_ui_schema::UiIntent::Invoke { action, params } = event.intent.clone();
                if event.event == neon_ui_schema::UiSemanticEventType::TextInputCommit {
                    let Some(text) = event.text else {
                        return self.rejected(
                            request.request_id,
                            "invalid_text_commit",
                            "a committed text value is required",
                        );
                    };
                    if action != "ui.showcase.text.commit" || text.value.chars().count() > 256 {
                        return self.rejected(
                            request.request_id,
                            "invalid_text_commit",
                            "text commit is not accepted by this surface",
                        );
                    }
                    self.showcase_text = text.value;
                    return self.accepted(request.request_id, json!({"surface_id": "surface.ui-platform-showcase", "value": self.showcase_text}));
                }
                if action == "ui.surface.event" {
                    request.method = "ui.surface.event".into();
                    request.params = params;
                    request.expected_revision = Some(event.fragment.revision);
                    return self.handle_surface_event(request);
                }
                match self.apply_ai_terrain_intent(&action, &params) {
                    Ok(Some(snapshot)) => return self.accepted(request.request_id, snapshot),
                    Ok(None) => {}
                    Err((code, message)) => {
                        return self.rejected(request.request_id, code, message);
                    }
                }
                self.accepted(
                    request.request_id,
                    json!({"event_id": event.event_id, "intent": event.intent}),
                )
            }
            Err(code) => self.rejected(request.request_id, code, "UI semantic event was rejected"),
        }
    }

    fn handle_external_input_frame(&mut self, request: RpcRequest) -> RpcResponse {
        let frame = match serde_json::from_value::<UiInputFrame>(request.params) {
            Ok(frame) => frame,
            Err(_) => {
                return self.rejected(
                    request.request_id,
                    "invalid_input_frame",
                    "invalid external UI input frame",
                );
            }
        };
        let Some(adapter) = self.host_adapter.as_mut() else {
            return self.rejected(
                request.request_id,
                "ui_host_adapter_unavailable",
                "no active UI host adapter is configured",
            );
        };
        match adapter.apply_external_input(frame) {
            Ok(result) => self.accepted(request.request_id, json!(result)),
            Err(error) => self.rejected(request.request_id, error.code, error.message),
        }
    }

    fn handle_repeat_input(&mut self, request: RpcRequest) -> RpcResponse {
        let frame = match serde_json::from_value::<UiRepeatFrame>(request.params) {
            Ok(frame) => frame,
            Err(_) => {
                return self.rejected(
                    request.request_id,
                    "invalid_repeat_frame",
                    "invalid external UI repeat frame",
                );
            }
        };
        let Some(adapter) = self.host_adapter.as_mut() else {
            return self.rejected(
                request.request_id,
                "ui_host_adapter_unavailable",
                "no active UI host adapter is configured",
            );
        };
        match adapter.apply_repeat(frame) {
            Ok(result) => self.accepted(request.request_id, json!(result)),
            Err(error) => self.rejected(request.request_id, error.code, error.message),
        }
    }

    fn handle_intent_dispatch(&mut self, mut request: RpcRequest) -> RpcResponse {
        let intent: neon_ui_schema::UiIntent = match serde_json::from_value(request.params.clone())
        {
            Ok(intent) => intent,
            Err(_) => {
                return self.rejected(request.request_id, "invalid_request", "invalid UI intent");
            }
        };
        if intent.validate().is_err() {
            return self.rejected(
                request.request_id,
                ERROR_INTENT_NOT_BOUND,
                "UI intent is not bound",
            );
        }
        let neon_ui_schema::UiIntent::Invoke { action, params } = intent.clone();
        if action == "ui.surface.event" {
            request.method = "ui.surface.event".into();
            request.params = params;
            return self.handle_surface_event(request);
        }
        match self.apply_ai_terrain_intent(&action, &params) {
            Ok(Some(snapshot)) => return self.accepted(request.request_id, snapshot),
            Ok(None) => {}
            Err((code, message)) => return self.rejected(request.request_id, code, message),
        }
        self.accepted(request.request_id, json!({"intent": intent}))
    }

    fn validate_semantic_event(&mut self, event: &UiSemanticEvent) -> Result<(), &'static str> {
        if event.renderer_epoch != self.epoch {
            return Err(ERROR_RENDERER_EPOCH_MISMATCH);
        }
        if event.event == neon_ui_schema::UiSemanticEventType::TextInputCommit
            && event.text.is_none()
        {
            return Err(ERROR_INTENT_NOT_BOUND);
        }
        let Some(fragment) = self.cached_fragment.as_ref() else {
            return Err(ERROR_INTENT_NOT_BOUND);
        };
        if fragment.fragment_id != event.fragment.id || fragment.revision != event.fragment.revision
        {
            return Err(ERROR_FRAGMENT_REVISION_STALE);
        }
        let data_grid_cell = event
            .data_grid_cell
            .as_ref()
            .map(|target| validate_data_grid_cell_event(fragment, event, target))
            .transpose()?;
        let bound = fragment.effects.iter().any(|effect| matches!(effect, UiEffect::SemanticIntent { intent } | UiEffect::BoundSemanticIntent { intent, .. } if intent == &event.intent));
        let declared_drop = event.drag_drop.as_ref().is_some_and(|drop| fragment.effects.iter().any(|effect| {
            let UiEffect::DropBinding { binding } = effect else { return false; };
            binding.intent == event.intent
                && binding.target_node_id.0 == drop.target_key
                && binding.placement == drop.placement
                && fragment.effects.iter().any(|effect| matches!(effect, UiEffect::DragBinding { binding: drag } if drag.key == binding.accepts_drag_key && drag.source_node_id.0 == drop.source_key))
        }));
        if !(bound || declared_drop || data_grid_cell.is_some()) {
            return Err(ERROR_INTENT_NOT_BOUND);
        }
        if event.event == neon_ui_schema::UiSemanticEventType::DragDrop && event.drag_drop.is_none()
        {
            return Err(ERROR_INTENT_NOT_BOUND);
        }
        if let Some(pointer) = &event.pointer {
            if self
                .last_input_sequence
                .get(&pointer.id)
                .is_some_and(|last| pointer.sequence <= *last)
            {
                return Err(ERROR_INPUT_SEQUENCE_STALE);
            }
            self.last_input_sequence
                .insert(pointer.id, pointer.sequence);
        }
        event.intent.validate().map_err(|_| ERROR_INTENT_NOT_BOUND)
    }

    fn accepted(&mut self, request_id: RequestId, result: Value) -> RpcResponse {
        self.record_receipt(&request_id, CommandState::Accepted, None);
        RpcResponse {
            request_id,
            status: RpcStatus::Accepted,
            revision: Some(self.debug_snapshot().revision),
            result: Some(result),
            snapshot: None,
            error: None,
        }
    }

    fn rejected(&mut self, request_id: RequestId, code: &str, message: &str) -> RpcResponse {
        self.record_receipt(&request_id, CommandState::Rejected, Some(code.into()));
        RpcResponse {
            request_id,
            status: RpcStatus::Rejected,
            revision: Some(self.debug_snapshot().revision),
            result: None,
            snapshot: None,
            error: Some(RpcError {
                code: code.into(),
                message: message.into(),
                current_revision: Some(self.debug_snapshot().revision),
                object_id: None,
            }),
        }
    }

    fn record_receipt(
        &mut self,
        request_id: &RequestId,
        state: CommandState,
        error_code: Option<String>,
    ) {
        let revision = self.debug_snapshot().revision;
        self.receipts.insert(
            request_id.clone(),
            CommandReceipt {
                request_id: request_id.clone(),
                state,
                revision_before: Some(revision),
                revision_after: Some(revision),
                error_code,
            },
        );
    }

    pub fn static_fragment(&self, revision: Revision) -> UiFragment {
        UiFragment {
            fragment_id: UiFragmentId("static-editor-shell".into()),
            revision,
            root: UiNode {
                node_id: UiNodeId("root-panel".into()),
                kind: UiNodeKind::Panel,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 320.0,
                    height: 160.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: None,
                image: None,
                surface: None,
                style: UiStyle {
                    background_color: [0.055, 0.07, 0.09, 0.98],
                    border_color: [0.22, 0.76, 0.88, 0.8],
                    border_width: 1.0,
                    corner_radius: 6.0,
                    opacity: 1.0,
                },
                enter_transition: Some(UiTransition {
                    delay_ms: 0,
                    duration_ms: 220,
                    easing: neon_ui_schema::UiEasing::EaseOut,
                    from: UiTransitionState {
                        opacity: Some(0.0),
                        bounds: Some(UiBounds {
                            x: 0.0,
                            y: 12.0,
                            width: 320.0,
                            height: 160.0,
                        }),
                        ..UiTransitionState::default()
                    },
                }),
                world_depth: None,
                children: vec![UiNode {
                    node_id: UiNodeId("title-label".into()),
                    kind: UiNodeKind::Label,
                    bounds: UiBounds {
                        x: 16.0,
                        y: 16.0,
                        width: 160.0,
                        height: 24.0,
                    },
                    layout: None,
                    visible: true,
                    enabled: true,
                    text_key: Some("ui.static.title".into()),
                    text: None,
                    image: None,
                    surface: None,
                    style: UiStyle {
                        background_color: [0.16, 0.23, 0.28, 0.9],
                        corner_radius: 3.0,
                        ..UiStyle::default()
                    },
                    enter_transition: Some(UiTransition {
                        delay_ms: 80,
                        duration_ms: 180,
                        easing: neon_ui_schema::UiEasing::EaseOut,
                        from: UiTransitionState {
                            opacity: Some(0.0),
                            ..UiTransitionState::default()
                        },
                    }),
                    world_depth: None,
                    children: Vec::new(),
                }],
            },
            effects: vec![
                UiEffect::SemanticAction {
                    action: "ui.static.ready".into(),
                },
                UiEffect::SemanticIntent {
                    intent: neon_ui_schema::UiIntent::Invoke {
                        action: "terrain.tool.select".into(),
                        params: json!({"tool": "water_inject"}),
                    },
                },
            ],
        }
    }

    pub fn submit_static_fragment(
        &mut self,
        endpoint: SocketAddr,
        request_id: RequestId,
        revision: Revision,
        idempotency_key: String,
    ) -> Result<RpcResponse, TransportError> {
        let fragment = self.static_fragment(revision);
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: ProtocolVersion { major: 1, minor: 0 },
            request_id: request_id.clone(),
            client: self.client.clone(),
            target: ServiceName("wgpu-runtime".into()),
            method: "wgpu.ui.submit_fragment".into(),
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment.clone())
            }),
            expected_revision: None,
            idempotency_key: Some(idempotency_key),
        };
        let mut client = RpcClient::connect(endpoint)?;
        let response = client.call(&request)?;
        let event = match response.status {
            RpcStatus::Accepted => {
                self.cached_fragment = Some(fragment);
                EVENT_COMMAND_ACCEPTED
            }
            RpcStatus::Rejected | RpcStatus::Failed => EVENT_COMMAND_REJECTED,
        };
        self.journal.append(
            if response.status == RpcStatus::Accepted {
                TraceLevel::Info
            } else {
                TraceLevel::Warn
            },
            event,
            Some(request_id),
            None,
            None,
            None,
            None,
            response.revision,
            json!({"target": "wgpu-runtime", "status": response.status}),
        );
        Ok(response)
    }

    pub fn cached_fragment(&self) -> Option<&UiFragment> {
        self.cached_fragment.as_ref()
    }

    pub fn traces(&self, filter: &JournalFilter) -> Vec<TraceRecord> {
        self.journal.query(filter)
    }
}

fn renderer_event_targets_wgpu(request: &RpcRequest) -> bool {
    serde_json::from_value::<UiSemanticEvent>(request.params.clone())
        .ok()
        .is_some_and(|event| {
            matches!(event.intent, neon_ui_schema::UiIntent::Invoke { ref action, .. } if action == "ai.terrain.generate")
        })
}

fn validate_data_grid_cell_event(
    fragment: &UiFragment,
    event: &UiSemanticEvent,
    target: &UiDataGridCellTarget,
) -> Result<(), &'static str> {
    if target.source_key.trim().is_empty()
        || target.stable_row_key.trim().is_empty()
        || target.column_key.trim().is_empty()
        || event.drag_drop.is_some()
    {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    }
    let Some((declaration, frame)) = fragment.effects.iter().find_map(|effect| match effect {
        UiEffect::DataGridFrame { declaration, frame }
            if declaration.source_key == target.source_key =>
        {
            Some((declaration, frame))
        }
        _ => None,
    }) else {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    };
    let Some(column) = declaration
        .columns
        .iter()
        .find(|column| column.key == target.column_key)
    else {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    };
    let Some(cell) = frame
        .window_rows
        .iter()
        .find(|row| row.stable_row_key == target.stable_row_key)
        .and_then(|row| row.cells.get(&target.column_key))
    else {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    };
    let (intent, effective) = match (&column.presentation, cell.presentation_override.as_ref()) {
        (neon_ui_schema::UiDataGridPresentation::Text, _) => {
            return Err(ERROR_DATA_GRID_CELL_INVALID);
        }
        (neon_ui_schema::UiDataGridPresentation::Select { intent }, None) => (intent, None),
        (neon_ui_schema::UiDataGridPresentation::Dropdown { intent, .. }, None)
        | (neon_ui_schema::UiDataGridPresentation::Edit { intent, .. }, None)
        | (neon_ui_schema::UiDataGridPresentation::Select { intent }, Some(_))
        | (neon_ui_schema::UiDataGridPresentation::Dropdown { intent, .. }, Some(_))
        | (neon_ui_schema::UiDataGridPresentation::Edit { intent, .. }, Some(_)) => {
            (intent, cell.presentation_override.as_ref())
        }
    };
    let UiIntent::Invoke { action, params } = &event.intent;
    if action != intent || params.as_object().is_none_or(|params| !params.is_empty()) {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    }
    match effective {
        None => match &column.presentation {
            neon_ui_schema::UiDataGridPresentation::Select { .. } => {
                match (&cell.value, &event.control_value) {
                    (
                        UiInputValue::Bool { value: current },
                        Some(UiSemanticPayloadValue::Bool { value }),
                    ) if event.event == neon_ui_schema::UiSemanticEventType::SelectionChanged
                        && *value == !*current
                        && event.text.is_none() =>
                    {
                        Ok(())
                    }
                    _ => Err(ERROR_DATA_GRID_CELL_INVALID),
                }
            }
            neon_ui_schema::UiDataGridPresentation::Dropdown { options, .. } => {
                validate_data_grid_dropdown_event(event, options)
            }
            neon_ui_schema::UiDataGridPresentation::Edit { max_chars, .. } => {
                validate_data_grid_edit_event(event, &cell.value, *max_chars)
            }
            neon_ui_schema::UiDataGridPresentation::Text => Err(ERROR_DATA_GRID_CELL_INVALID),
        },
        Some(neon_ui_schema::UiDataGridCellPresentation::Text) => Err(ERROR_DATA_GRID_CELL_INVALID),
        Some(neon_ui_schema::UiDataGridCellPresentation::Dropdown { options }) => {
            validate_data_grid_dropdown_event(event, options)
        }
        Some(neon_ui_schema::UiDataGridCellPresentation::Edit { max_chars }) => {
            validate_data_grid_edit_event(event, &cell.value, *max_chars)
        }
    }
}

fn validate_data_grid_dropdown_event(
    event: &UiSemanticEvent,
    options: &[String],
) -> Result<(), &'static str> {
    match &event.control_value {
        Some(UiSemanticPayloadValue::Enum { value })
            if event.event == neon_ui_schema::UiSemanticEventType::SelectionChanged
                && options.iter().any(|option| option == value)
                && event.text.is_none() =>
        {
            Ok(())
        }
        _ => Err(ERROR_DATA_GRID_CELL_INVALID),
    }
}

fn validate_data_grid_edit_event(
    event: &UiSemanticEvent,
    cell_value: &UiInputValue,
    max_chars: u32,
) -> Result<(), &'static str> {
    match (cell_value, &event.control_value, &event.text) {
        (
            UiInputValue::TextHandle { value: expected },
            Some(UiSemanticPayloadValue::TextHandle { value }),
            Some(text),
        ) if event.event == neon_ui_schema::UiSemanticEventType::TextInputCommit
            && value == expected
            && text.value.chars().count() <= max_chars as usize =>
        {
            Ok(())
        }
        _ => Err(ERROR_DATA_GRID_CELL_INVALID),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gallery_asset() -> neon_protocol::AssetRef {
        neon_protocol::AssetRef {
            project_id: "test-project".into(),
            asset_id: 1,
            revision: Revision(1),
            kind: "image".into(),
        }
    }
    use neon_ipc::RpcServer;
    use neon_protocol::RpcError;
    use neon_ui_schema::{UiProgramEventDeclaration, UiResourceBudget};
    use std::thread;

    fn accepted(request: RpcRequest) -> RpcResponse {
        RpcResponse {
            request_id: request.request_id,
            status: RpcStatus::Accepted,
            revision: Some(Revision(1)),
            result: Some(json!({"fragment_count": 1})),
            snapshot: None,
            error: None,
        }
    }

    #[test]
    fn static_fragment_is_valid_ui_schema() {
        let runtime = UiRuntime::new(1, "ui-test");
        runtime.static_fragment(Revision(1)).validate().unwrap();
    }

    #[test]
    fn headless_service_methods_are_explicit() {
        let mut runtime = UiRuntime::new(1, "ui-test");
        for method in ["service.health", "service.describe", "service.shutdown"] {
            let response = runtime.handle_service_request(RpcRequest {
                protocol: "neon3.rpc".into(),
                version: PROTOCOL_VERSION,
                request_id: RequestId(method.into()),
                client: ClientIdentity {
                    kind: ClientKind::Cli,
                    instance_id: "test".into(),
                    pid: 1,
                    origin: "test".into(),
                },
                target: ServiceName(SERVICE_NAME.into()),
                method: method.into(),
                params: json!({}),
                expected_revision: None,
                idempotency_key: None,
            });
            assert_eq!(response.status, RpcStatus::Accepted);
        }
    }

    #[test]
    fn react_fragment_submission_is_cached_and_revisioned() {
        let mut runtime = UiRuntime::new(1, "ui-react-test");
        let fragment = runtime.static_fragment(Revision(1));
        let response = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("react-fragment-1".into()),
            client: ClientIdentity {
                kind: ClientKind::UiReactClient,
                instance_id: "react-client".into(),
                pid: 1,
                origin: "neon-ui-react-client".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment)
            }),
            expected_revision: None,
            idempotency_key: Some("react-fragment-key-1".into()),
        });
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.cached_fragment().unwrap().revision, Revision(1));
        assert_eq!(
            runtime
                .service_description()
                .capabilities
                .iter()
                .filter(|capability| *capability == "ui.fragment.submit.v1")
                .count(),
            1
        );
    }

    #[test]
    fn active_fragment_submission_rejects_identity_change_without_mutating_cache() {
        let mut runtime = UiRuntime::new(1, "fragment-identity-test");
        let mut active = runtime.static_fragment(Revision(1));
        active.fragment_id = UiFragmentId("nui-flow-case-component-gallery".into());
        runtime.cached_fragment = Some(active.clone());
        let mut hardcoded = active.clone();
        hardcoded.fragment_id = UiFragmentId("component-gallery-host".into());
        hardcoded.revision = Revision(2);

        let response = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("identity-change".into()),
            client: runtime.client.clone(),
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(hardcoded)
            }),
            expected_revision: Some(Revision(1)),
            idempotency_key: Some("identity-change".into()),
        });

        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "fragment_identity_change");
        assert_eq!(runtime.cached_fragment(), Some(&active));
    }

    #[test]
    fn surface_actions_are_revisioned_idempotent_discrete_state() {
        let mut runtime = UiRuntime::new(1, "ui-surface-test");
        let action = |id: &str, expected_revision, key: &str, params| RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId(id.into()),
            client: ClientIdentity {
                kind: ClientKind::UiReactClient,
                instance_id: "react-client".into(),
                pid: 1,
                origin: "neon-ui-react-client".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.surface.event".into(),
            params,
            expected_revision: Some(Revision(expected_revision)),
            idempotency_key: Some(key.into()),
        };
        let event = |event| json!({"schema_version": UI_SURFACE_SCHEMA_VERSION, "surface_id": WORKBENCH_SURFACE_ID, "event": event});
        let first = runtime.handle_service_request(action(
            "toggle-1",
            0,
            "toggle-key",
            event(json!({"type": "DIAGNOSTICS_TOGGLE"})),
        ));
        assert_eq!(first.status, RpcStatus::Accepted);
        assert_eq!(
            first.result.as_ref().unwrap()["value"]["diagnostics"],
            "expanded"
        );
        assert_eq!(first.revision, Some(Revision(1)));
        let retry = runtime.handle_service_request(action(
            "toggle-retry",
            0,
            "toggle-key",
            event(json!({"type": "DIAGNOSTICS_TOGGLE"})),
        ));
        assert_eq!(retry.status, RpcStatus::Accepted);
        assert_eq!(retry.revision, Some(Revision(1)));
        assert_eq!(runtime.surface.revision, Revision(1));
        let stale = runtime.handle_service_request(action(
            "tab-stale",
            0,
            "tab-key",
            event(json!({"type": "INSPECTOR_TAB_SELECT", "tab": "materials"})),
        ));
        assert_eq!(stale.error.unwrap().code, "revision_conflict");
        let tab = runtime.handle_service_request(action(
            "tab-1",
            1,
            "tab-key",
            event(json!({"type": "INSPECTOR_TAB_SELECT", "tab": "materials"})),
        ));
        assert_eq!(tab.status, RpcStatus::Accepted);
        assert_eq!(
            runtime.surface.state.inspector.tab,
            UiInspectorTab::Materials
        );
        assert_eq!(runtime.surface.revision, Revision(2));
        let duplicate = runtime.handle_service_request(action(
            "tab-duplicate",
            2,
            "tab-duplicate-key",
            event(json!({"type": "INSPECTOR_TAB_SELECT", "tab": "materials"})),
        ));
        assert_eq!(duplicate.status, RpcStatus::Rejected);
        assert_eq!(duplicate.error.unwrap().code, "ui_guard_rejected");
    }

    #[test]
    fn local_surface_intent_dispatches_through_the_typed_machine() {
        let mut runtime = UiRuntime::new(1, "ui-surface-intent-test");
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("surface-intent-1".into()),
            client: ClientIdentity {
                kind: ClientKind::UiReactClient,
                instance_id: "react-client".into(),
                pid: 1,
                origin: "neon-ui-react-client".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.intent.dispatch".into(),
            params: json!(neon_ui_schema::UiIntent::Invoke {
                action: "ui.surface.event".into(),
                params: json!({"schema_version": UI_SURFACE_SCHEMA_VERSION, "surface_id": WORKBENCH_SURFACE_ID, "event": {"type": "DIAGNOSTICS_TOGGLE"}})
            }),
            expected_revision: Some(Revision(0)),
            idempotency_key: Some("surface-intent-key-1".into()),
        };
        let response = runtime.handle_service_request(request);
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.result.unwrap()["value"]["diagnostics"], "expanded");
        assert_eq!(runtime.surface.revision, Revision(1));
    }

    #[test]
    fn text_input_commit_is_typed_and_does_not_accept_preedit() {
        use neon_ui_schema::{
            UiFragmentRevision, UiIntent, UiSemanticEventType, UiTextInputCommit,
        };

        let mut runtime = UiRuntime::new(1, "ui-text-input-test");
        let mut fragment = runtime.static_fragment(Revision(1));
        fragment.effects.push(UiEffect::SemanticIntent {
            intent: UiIntent::Invoke {
                action: "ui.showcase.text.commit".into(),
                params: json!({}),
            },
        });
        runtime.cached_fragment = Some(fragment.clone());
        let event = UiSemanticEvent {
            event: UiSemanticEventType::TextInputCommit,
            event_id: "text-1".into(),
            renderer_epoch: 1,
            composition_revision: Revision(1),
            fragment: UiFragmentRevision {
                id: fragment.fragment_id,
                revision: Revision(1),
            },
            intent: UiIntent::Invoke {
                action: "ui.showcase.text.commit".into(),
                params: json!({}),
            },
            pointer: None,
            focus: None,
            data_grid_cell: None,
            text: Some(UiTextInputCommit {
                value: "composed text".into(),
            }),
            control_value: None,
            drag_drop: None,
        };
        let response = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("text-1".into()),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: "renderer".into(),
                pid: 1,
                origin: "test".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.input.event".into(),
            params: json!(event),
            expected_revision: Some(Revision(1)),
            idempotency_key: Some("text-1".into()),
        });
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.result.unwrap()["value"], "composed text");
    }

    #[test]
    fn generic_host_route_validates_inbound_and_submits_publication_fragment() {
        let (document, program) =
            crate::demo_domain::component_gallery_program(gallery_asset()).unwrap();
        let declaration = program
            .event_records
            .iter()
            .find(|declaration| declaration.intent == "gallery.drag_value.commit")
            .unwrap()
            .clone();
        let rejected_declaration = declaration.clone();
        let fragment = UiFragment {
            fragment_id: UiFragmentId("host-route".into()),
            revision: Revision(1),
            root: document.ir.root.clone(),
            effects: lower_nui_flow_effects(&document),
        };
        let host = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host_endpoint = host.local_addr().unwrap();
        let schema = document.input_schema.clone();
        let host_thread = thread::spawn(move || {
            let mut calls = 0;
            host.serve_until(|request| {
                calls += 1;
                let response = match request.method.as_str() {
                    "ui.host.adapter.get" => RpcResponse {
                        request_id: request.request_id,
                        status: RpcStatus::Accepted,
                        revision: Some(Revision(0)),
                        result: Some(json!(UiHostAdapterConfig {
                            program: program.clone(),
                            input_schema: schema.clone()
                        })),
                        snapshot: None,
                        error: None,
                    },
                    "ui.host.inbound" => {
                        let UiHostInbound::SemanticIntent { event } =
                            serde_json::from_value::<UiHostInbound>(request.params).unwrap()
                        else {
                            unreachable!()
                        };
                        assert_eq!(event.kind, UiProgramSemanticEventKind::ValueCommit);
                        assert_eq!(event.source_node_key, declaration.node_key);
                        assert_eq!(
                            event.payload.get("drag_value"),
                            Some(&UiSemanticPayloadValue::I32 { value: 12 })
                        );
                        assert_eq!(
                            event.requested_value,
                            Some(UiSemanticPayloadValue::I32 { value: 15 })
                        );
                        RpcResponse {
                            request_id: request.request_id,
                            status: RpcStatus::Accepted,
                            revision: Some(Revision(1)),
                            result: Some(json!(UiHostPublication {
                                scalar_frame: UiInputFrame {
                                    program_revision: program.revision.clone(),
                                    expected_input_revision: Revision(0),
                                    request_id: "host-publication".into(),
                                    idempotency_key: "host-publication".into(),
                                    changes: vec![UiInputChange {
                                        key: "drag_value".into(),
                                        value: UiInputValue::I32 { value: 15 }
                                    }]
                                },
                                grid_inputs: Vec::new(),
                                presentation_update: None
                            })),
                            snapshot: None,
                            error: None,
                        }
                    }
                    _ => unreachable!(),
                };
                (response, calls < 2)
            })
        });
        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let renderer_endpoint = renderer.local_addr().unwrap();
        let renderer_thread = thread::spawn(move || {
            renderer.serve_one(|request| {
            let UiCommand::SubmitFragment { submission } = serde_json::from_value(request.params).unwrap() else { unreachable!() };
            assert_eq!(submission.fragment.revision, Revision(2));
            assert!(submission.fragment.effects.iter().any(|effect| matches!(
                effect,
                UiEffect::ControlPresentation { node_id, state: neon_ui_schema::UiControlPresentation::Numeric { value: 15.0, .. } }
                    if node_id.0 == "count-drag"
            )));
            RpcResponse { request_id: request.request_id, status: RpcStatus::Accepted, revision: Some(Revision(2)), result: Some(json!({})), snapshot: None, error: None }
        })
        });
        let mut runtime = UiRuntime::new(7, "generic-host-route");
        runtime.cached_fragment = Some(fragment.clone());
        let event = UiSemanticEvent {
            event: neon_ui_schema::UiSemanticEventType::PointerClick,
            event_id: "host-event".into(),
            renderer_epoch: 7,
            composition_revision: Revision(1),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: fragment.fragment_id,
                revision: Revision(1),
            },
            intent: UiIntent::Invoke {
                action: declaration.intent.clone(),
                params: json!({}),
            },
            pointer: Some(neon_ui_schema::UiPointerMetadata { id: 1, sequence: 1 }),
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: Some(UiSemanticPayloadValue::I32 { value: 15 }),
            drag_drop: None,
        };
        let response = runtime
            .forward_host_request(
                host_endpoint,
                renderer_endpoint,
                RpcRequest {
                    protocol: "neon3.rpc".into(),
                    version: PROTOCOL_VERSION,
                    request_id: RequestId("host-event".into()),
                    client: runtime.client.clone(),
                    target: ServiceName(SERVICE_NAME.into()),
                    method: "ui.host.inbound".into(),
                    params: json!(event),
                    expected_revision: Some(Revision(1)),
                    idempotency_key: Some("host-event".into()),
                },
            )
            .unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.cached_fragment().unwrap().revision, Revision(2));
        let trace = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("trace-get".into()),
            client: runtime.client.clone(),
            target: ServiceName(SERVICE_NAME.into()),
            method: "debug.interaction.get".into(),
            params: json!({"interaction_id": "host-event"}),
            expected_revision: None,
            idempotency_key: None,
        });
        let records: Vec<InteractionTraceRecord> =
            serde_json::from_value(trace.result.unwrap()["records"].clone()).unwrap();
        assert_eq!(
            records.first().unwrap().interaction_id,
            InteractionId("host-event".into())
        );
        assert_eq!(
            records.last().unwrap().stage,
            InteractionTraceStage::WgpuFragmentSubmissionAccepted
        );
        assert!(
            records
                .iter()
                .any(|record| record.stage == InteractionTraceStage::HostForwarded)
        );
        let query = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("trace-query".into()),
            client: runtime.client.clone(),
            target: ServiceName(SERVICE_NAME.into()),
            method: "debug.interaction.query".into(),
            params: json!({"filters": {"interaction_id": "host-event"}}),
            expected_revision: None,
            idempotency_key: None,
        });
        assert_eq!(query.status, RpcStatus::Accepted);
        host_thread.join().unwrap().unwrap();
        renderer_thread.join().unwrap().unwrap();

        let adapter = runtime.host_adapter.as_ref().unwrap();
        let rejected_event = UiProgramSemanticEvent {
            event_id: "rejected-event".into(),
            kind: UiProgramSemanticEventKind::Activate,
            intent: rejected_declaration.intent,
            source_node_key: rejected_declaration.node_key,
            payload: BTreeMap::new(),
            program_revision: adapter.program().revision.clone(),
            input_revision: adapter.snapshot().scalar_inputs.input_revision,
            request_id: "rejected-event".into(),
            idempotency_key: "rejected-event".into(),
            requested_value: None,
            interaction: UiSemanticInteractionMetadata {
                interaction_id: "generic-host-interaction".into(),
                sequence: 1,
                renderer_epoch: 8,
            },
        };
        let rejected = runtime
            .forward_host_request(
                "127.0.0.1:1".parse().unwrap(),
                renderer_endpoint,
                RpcRequest {
                    protocol: "neon3.rpc".into(),
                    version: PROTOCOL_VERSION,
                    request_id: RequestId("rejected-event".into()),
                    client: runtime.client.clone(),
                    target: ServiceName(SERVICE_NAME.into()),
                    method: "ui.host.inbound".into(),
                    params: json!(UiHostInbound::SemanticIntent {
                        event: rejected_event
                    }),
                    expected_revision: None,
                    idempotency_key: Some("rejected-event".into()),
                },
            )
            .unwrap();
        assert_eq!(
            rejected.error.unwrap().code,
            "ui_host_renderer_epoch_mismatch"
        );
        let rejected_records = runtime
            .interaction_traces
            .get(&InteractionId("generic-host-interaction".into()));
        assert_eq!(
            rejected_records.last().unwrap().stage,
            InteractionTraceStage::AdapterValidationRejected
        );
        assert_eq!(
            rejected_records
                .last()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .code,
            "ui_host_renderer_epoch_mismatch"
        );
    }

    #[test]
    fn component_gallery_grid_window_and_cell_use_the_generic_host_route() {
        fn frame(program: &UiProgram, first_row: u64, revision: u64) -> UiDataGridFrame {
            let handle = |id| neon_ui_schema::UiTextHandle {
                id,
                generation: revision as u32,
            };
            let row_count = u64::from(
                program
                    .data_grid_records
                    .iter()
                    .find(|record| record.source_key == "asset_window")
                    .unwrap()
                    .max_window_rows,
            );
            UiDataGridFrame {
                list_revision: Revision(revision),
                total_rows: 10_000,
                first_row,
                window_rows: (first_row..first_row + row_count)
                    .map(|row| neon_ui_schema::UiDataGridWindowRow {
                        stable_row_key: format!("virtual-row-{row}"),
                        cells: std::collections::BTreeMap::from([
                            (
                                "name".into(),
                                neon_ui_schema::UiDataGridCell {
                                    value: UiInputValue::TextHandle {
                                        value: handle(10_000 + row * 5 + 1),
                                    },
                                    display: handle(10_000 + row * 5 + 1),
                                    presentation_override: None,
                                },
                            ),
                            (
                                "status".into(),
                                neon_ui_schema::UiDataGridCell {
                                    value: UiInputValue::Enum {
                                        value: "ready".into(),
                                    },
                                    display: handle(10_000 + row * 5 + 2),
                                    presentation_override: None,
                                },
                            ),
                            (
                                "owner".into(),
                                neon_ui_schema::UiDataGridCell {
                                    value: UiInputValue::Bool { value: true },
                                    display: handle(10_000 + row * 5 + 3),
                                    presentation_override: None,
                                },
                            ),
                            (
                                "notes".into(),
                                neon_ui_schema::UiDataGridCell {
                                    value: UiInputValue::TextHandle {
                                        value: handle(10_000 + row * 5 + 4),
                                    },
                                    display: handle(10_000 + row * 5 + 4),
                                    presentation_override: None,
                                },
                            ),
                        ]),
                    })
                    .collect(),
                expected_program_revision: program.revision.clone(),
            }
        }

        let (document, program) =
            crate::demo_domain::component_gallery_program(gallery_asset()).unwrap();
        let declaration = program.event_records.first().unwrap().clone();
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("component-gallery-host".into()),
            revision: Revision(1),
            root: document.ir.root.clone(),
            effects: lower_nui_flow_effects(&document),
        };
        let record = program
            .data_grid_records
            .iter()
            .find(|record| record.source_key == "asset_window")
            .unwrap();
        fragment.effects.push(UiEffect::DataGridFrame {
            declaration: UiDataGridDeclaration {
                node_key: record.node_key.clone(),
                source_key: record.source_key.clone(),
                max_window_rows: record.max_window_rows,
                row_height: record.row_height,
                overscan: record.overscan,
                columns: record.columns.clone(),
            },
            frame: frame(&program, 0, 1),
        });
        let host = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host_endpoint = host.local_addr().unwrap();
        let schema = document.input_schema.clone();
        let host_program = program.clone();
        let host_thread = thread::spawn(move || {
            let mut calls = 0;
            host.serve_until(|request| {
                calls += 1;
                let response = match request.method.as_str() {
                    "ui.host.adapter.get" => RpcResponse {
                        request_id: request.request_id,
                        status: RpcStatus::Accepted,
                        revision: Some(Revision(0)),
                        result: Some(json!(UiHostAdapterConfig {
                            program: host_program.clone(),
                            input_schema: schema.clone()
                        })),
                        snapshot: None,
                        error: None,
                    },
                    "ui.host.inbound" => {
                        let inbound: UiHostInbound =
                            serde_json::from_value(request.params).unwrap();
                        let (expected, key, grid) = match inbound {
                            UiHostInbound::SemanticIntent { .. } => {
                                (0, "gallery-seed", frame(&host_program, 0, 1))
                            }
                            UiHostInbound::WindowRequest {
                                request: neon_ui_schema::UiWindowRequest::DataGrid { request },
                            } => {
                                assert_eq!(request.source_key, "asset_window");
                                (1, "gallery-window", frame(&host_program, 56, 1))
                            }
                            UiHostInbound::DataGridCell { event } => {
                                assert_eq!(
                                    event.data_grid_cell.unwrap().stable_row_key,
                                    "virtual-row-56"
                                );
                                (2, "gallery-cell", frame(&host_program, 56, 2))
                            }
                            UiHostInbound::DragDrop { .. } => unreachable!(),
                        };
                        RpcResponse {
                            request_id: request.request_id,
                            status: RpcStatus::Accepted,
                            revision: Some(Revision(grid.list_revision.0)),
                            result: Some(json!(UiHostPublication {
                                scalar_frame: UiInputFrame {
                                    program_revision: host_program.revision.clone(),
                                    expected_input_revision: Revision(expected),
                                    request_id: key.into(),
                                    idempotency_key: key.into(),
                                    changes: Vec::new()
                                },
                                grid_inputs: vec![UiDataGridInputFrame {
                                    source_key: "asset_window".into(),
                                    frame: grid
                                }],
                                presentation_update: None,
                            })),
                            snapshot: None,
                            error: None,
                        }
                    }
                    _ => unreachable!(),
                };
                (response, calls < 4)
            })
        });
        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let renderer_endpoint = renderer.local_addr().unwrap();
        let renderer_thread = thread::spawn(move || {
            renderer.serve_until(|request| {
            let UiCommand::SubmitFragment { submission } = serde_json::from_value(request.params).unwrap() else { unreachable!() };
            assert!(submission.fragment.effects.iter().any(|effect| matches!(effect, UiEffect::DataGridFrame { declaration, frame } if declaration.source_key == "asset_window"
                && frame.window_rows.len() == declaration.max_window_rows as usize
                && frame.window_rows.len() as u32 * declaration.row_height >= 2 * (872 - declaration.row_height))));
            (RpcResponse { request_id: request.request_id, status: RpcStatus::Accepted, revision: Some(submission.fragment.revision), result: Some(json!({})), snapshot: None, error: None }, submission.fragment.revision.0 < 4)
        })
        });
        let mut runtime = UiRuntime::new(7, "component-gallery-host-route");
        let initial = runtime
            .forward_fragment(
                renderer_endpoint,
                RpcRequest {
                    protocol: "neon3.rpc".into(),
                    version: PROTOCOL_VERSION,
                    request_id: RequestId("gallery-initial".into()),
                    client: runtime.client.clone(),
                    target: ServiceName(SERVICE_NAME.into()),
                    method: "ui.fragment.submit".into(),
                    params: json!(UiCommand::SubmitFragment {
                        submission: UiFragmentSubmission::new(fragment.clone())
                    }),
                    expected_revision: None,
                    idempotency_key: Some("gallery-initial".into()),
                },
            )
            .unwrap();
        assert_eq!(initial.status, RpcStatus::Accepted);
        let control = UiSemanticEvent {
            event: neon_ui_schema::UiSemanticEventType::PointerClick,
            event_id: "gallery-seed".into(),
            renderer_epoch: 7,
            composition_revision: Revision(1),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: fragment.fragment_id.clone(),
                revision: Revision(1),
            },
            intent: UiIntent::Invoke {
                action: declaration.intent,
                params: json!({}),
            },
            pointer: Some(neon_ui_schema::UiPointerMetadata { id: 1, sequence: 1 }),
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: None,
        };
        let call = |runtime: &mut UiRuntime, request_id: &str, params: Value, key: &str| {
            runtime
                .forward_host_request(
                    host_endpoint,
                    renderer_endpoint,
                    RpcRequest {
                        protocol: "neon3.rpc".into(),
                        version: PROTOCOL_VERSION,
                        request_id: RequestId(request_id.into()),
                        client: runtime.client.clone(),
                        target: ServiceName(SERVICE_NAME.into()),
                        method: "ui.host.inbound".into(),
                        params,
                        expected_revision: None,
                        idempotency_key: Some(key.into()),
                    },
                )
                .unwrap()
        };
        assert_eq!(
            call(&mut runtime, "gallery-seed", json!(control), "gallery-seed").status,
            RpcStatus::Accepted
        );
        let window = neon_ui_schema::UiDataGridWindowRequest {
            renderer_epoch: 7,
            composition_revision: Revision(2),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: fragment.fragment_id.clone(),
                revision: Revision(2),
            },
            source_key: "asset_window".into(),
            expected_list_revision: Revision(1),
            requested_first_row: 56,
            max_window_rows: record.max_window_rows,
            sequence: 2,
        };
        assert_eq!(
            call(
                &mut runtime,
                "gallery-window",
                json!(UiHostInbound::WindowRequest {
                    request: neon_ui_schema::UiWindowRequest::DataGrid { request: window }
                }),
                "gallery-window"
            )
            .status,
            RpcStatus::Accepted
        );
        let cell = UiSemanticEvent {
            event: neon_ui_schema::UiSemanticEventType::SelectionChanged,
            event_id: "gallery-cell".into(),
            renderer_epoch: 7,
            composition_revision: Revision(3),
            fragment: neon_ui_schema::UiFragmentRevision {
                id: fragment.fragment_id,
                revision: Revision(3),
            },
            intent: UiIntent::Invoke {
                action: "virtual_list.status.set".into(),
                params: json!({}),
            },
            pointer: Some(neon_ui_schema::UiPointerMetadata { id: 1, sequence: 3 }),
            focus: None,
            data_grid_cell: Some(UiDataGridCellTarget {
                source_key: "asset_window".into(),
                stable_row_key: "virtual-row-56".into(),
                column_key: "status".into(),
            }),
            text: None,
            control_value: Some(UiSemanticPayloadValue::Enum {
                value: "archived".into(),
            }),
            drag_drop: None,
        };
        assert_eq!(
            call(&mut runtime, "gallery-cell", json!(cell), "gallery-cell").status,
            RpcStatus::Accepted
        );
        host_thread.join().unwrap().unwrap();
        renderer_thread.join().unwrap().unwrap();
    }

    #[test]
    fn forwarder_caches_only_the_renderer_accepted_revision() {
        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = renderer.local_addr().unwrap();
        let receiver = thread::spawn(move || {
            renderer.serve_one(|request| {
                assert_eq!(request.client.kind, ClientKind::UiRuntime);
                assert_eq!(request.method, "wgpu.ui.submit_fragment");
                assert_eq!(request.request_id.0, "forward-1");
                RpcResponse {
                    request_id: request.request_id,
                    status: RpcStatus::Accepted,
                    revision: Some(Revision(9)),
                    result: Some(json!({"state": "ready"})),
                    snapshot: None,
                    error: None,
                }
            })
        });
        let mut runtime = UiRuntime::new(1, "ui-forward-test");
        let fragment = runtime.static_fragment(Revision(4));
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("forward-1".into()),
            client: ClientIdentity {
                kind: ClientKind::UiReactClient,
                instance_id: "react-client".into(),
                pid: 1,
                origin: "neon-ui-react-client".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment)
            }),
            expected_revision: None,
            idempotency_key: Some("forward-key-1".into()),
        };
        let response = runtime.forward_fragment(endpoint, request).unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(runtime.cached_fragment().unwrap().revision, Revision(4));
        assert_eq!(
            runtime
                .command_receipt(&RequestId("forward-1".into()))
                .unwrap()
                .state,
            CommandState::Accepted
        );
        receiver.join().unwrap().unwrap();
    }

    #[test]
    fn renderer_rejection_does_not_advance_the_ui_cache() {
        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = renderer.local_addr().unwrap();
        let receiver = thread::spawn(move || {
            renderer.serve_one(|request| RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Rejected,
                revision: Some(Revision(9)),
                result: None,
                snapshot: None,
                error: Some(RpcError {
                    code: "revision_conflict".into(),
                    message: "renderer has a newer fragment".into(),
                    current_revision: Some(Revision(9)),
                    object_id: None,
                }),
            })
        });
        let mut runtime = UiRuntime::new(1, "ui-forward-test");
        let fragment = runtime.static_fragment(Revision(4));
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("forward-reject-1".into()),
            client: ClientIdentity {
                kind: ClientKind::UiReactClient,
                instance_id: "react-client".into(),
                pid: 1,
                origin: "neon-ui-react-client".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment {
                submission: UiFragmentSubmission::new(fragment)
            }),
            expected_revision: None,
            idempotency_key: Some("forward-reject-key-1".into()),
        };
        let response = runtime.forward_fragment(endpoint, request).unwrap();
        assert_eq!(response.status, RpcStatus::Rejected);
        assert!(runtime.cached_fragment().is_none());
        assert_eq!(
            runtime
                .command_receipt(&RequestId("forward-reject-1".into()))
                .unwrap()
                .state,
            CommandState::Rejected
        );
        receiver.join().unwrap().unwrap();
    }

    #[test]
    fn sends_static_fragment_to_loopback_wgpu_server() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = thread::spawn(move || server.serve_one(accepted));
        let mut runtime = UiRuntime::new(1, "ui-test");
        let response = runtime
            .submit_static_fragment(
                endpoint,
                RequestId("submit-1".into()),
                Revision(1),
                "key-1".into(),
            )
            .unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(
            runtime.cached_fragment().unwrap().fragment_id.0,
            "static-editor-shell"
        );
        assert_eq!(
            runtime.traces(&JournalFilter::default())[0].event,
            EVENT_COMMAND_ACCEPTED
        );
        server_thread.join().unwrap().unwrap();
    }

    #[test]
    fn rejection_is_exposed_and_not_cached_as_success() {
        let server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = server.local_addr().unwrap();
        let server_thread = thread::spawn(move || {
            server.serve_one(|request| RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Rejected,
                revision: Some(Revision(2)),
                result: None,
                snapshot: None,
                error: Some(RpcError {
                    code: "revision_conflict".into(),
                    message: "fragment revision is stale".into(),
                    current_revision: Some(Revision(2)),
                    object_id: None,
                }),
            })
        });
        let mut runtime = UiRuntime::new(1, "ui-test");
        let response = runtime
            .submit_static_fragment(
                endpoint,
                RequestId("reject-1".into()),
                Revision(1),
                "key-2".into(),
            )
            .unwrap();
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(response.error.unwrap().code, "revision_conflict");
        assert!(runtime.cached_fragment().is_none());
        assert_eq!(
            runtime.traces(&JournalFilter::default())[0].event,
            EVENT_COMMAND_REJECTED
        );
        server_thread.join().unwrap().unwrap();
    }

    #[test]
    fn semantic_event_dispatches_the_identical_typed_terrain_command() {
        use neon_ui_schema::{
            UiFragmentRevision, UiIntent, UiPointerMetadata, UiSemanticEventType,
        };

        let terrain_server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = terrain_server.local_addr().unwrap();
        let receiver = thread::spawn(move || {
            terrain_server.serve_one(|request| {
                assert_eq!(request.target.0, "terrain-runtime");
                assert_eq!(request.method, "terrain.tool.select");
                assert_eq!(request.params, json!({"tool": "water_inject"}));
                assert_eq!(request.expected_revision, Some(Revision(3)));
                assert!(request.idempotency_key.is_some());
                RpcResponse {
                    request_id: request.request_id,
                    status: RpcStatus::Accepted,
                    revision: Some(Revision(4)),
                    result: Some(json!({"mode": "water_paint"})),
                    snapshot: None,
                    error: None,
                }
            })
        });
        let mut runtime = UiRuntime::new(7, "ui-test");
        runtime.cached_fragment = Some(runtime.static_fragment(Revision(3)));
        let event = UiSemanticEvent {
            event: UiSemanticEventType::PointerClick,
            event_id: "event-1".into(),
            renderer_epoch: 7,
            composition_revision: Revision(9),
            fragment: UiFragmentRevision {
                id: UiFragmentId("static-editor-shell".into()),
                revision: Revision(3),
            },
            intent: UiIntent::Invoke {
                action: "terrain.tool.select".into(),
                params: json!({"tool": "water_inject"}),
            },
            pointer: Some(UiPointerMetadata { id: 0, sequence: 1 }),
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: None,
        };
        let response = runtime
            .dispatch_semantic_event(
                endpoint,
                event,
                RequestId("terrain-request-1".into()),
                "terrain-key-1".into(),
            )
            .unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(
            runtime
                .command_receipt(&RequestId("terrain-request-1".into()))
                .unwrap()
                .state,
            CommandState::Accepted
        );
        assert_eq!(
            runtime
                .traces(&JournalFilter {
                    request_id: Some(RequestId("terrain-request-1".into())),
                    ..JournalFilter::default()
                })
                .len(),
            2
        );
        receiver.join().unwrap().unwrap();
    }

    #[test]
    fn render_once_event_forwards_one_typed_wgpu_generation_command() {
        use neon_ui_schema::{
            UiFragmentRevision, UiIntent, UiPointerMetadata, UiSemanticEventType,
        };

        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = renderer.local_addr().unwrap();
        let receiver = thread::spawn(move || {
            renderer.serve_one(|request| {
                assert_eq!(request.client.kind, ClientKind::UiRuntime);
                assert_eq!(request.target.0, "wgpu-runtime");
                assert_eq!(request.method, "wgpu.ai.terrain.generate");
                assert_eq!(request.params["condition"]["parent"], 1);
                assert_eq!(request.params["target_id"], "ai.terrain.preview");
                assert_eq!(request.expected_revision, Some(Revision(3)));
                assert_eq!(request.idempotency_key.as_deref(), Some("render-once:7:1"));
                RpcResponse {
                    request_id: request.request_id,
                    status: RpcStatus::Accepted,
                    revision: Some(Revision(4)),
                    result: Some(json!({"job_id": "ai-terrain-render-1", "state": "ready", "seed": 42, "elapsed_ms": 100.0})),
                    snapshot: None,
                    error: None,
                }
            })
        });
        let params = json!({
            "condition": {"sub": 6, "parent": 1, "relief": 3, "texture": 2, "water": 2},
            "guidance": 7.0,
            "steps": 2,
            "seed": 42,
            "size": 32,
            "target_id": "ai.terrain.preview"
        });
        let intent = UiIntent::Invoke {
            action: "ai.terrain.generate".into(),
            params,
        };
        let mut runtime = UiRuntime::new(7, "ui-ai-render-test");
        let mut fragment = runtime.static_fragment(Revision(3));
        fragment.effects.push(UiEffect::SemanticIntent {
            intent: intent.clone(),
        });
        runtime.cached_fragment = Some(fragment);
        let event = UiSemanticEvent {
            event: UiSemanticEventType::PointerClick,
            event_id: "render-1".into(),
            renderer_epoch: 7,
            composition_revision: Revision(3),
            fragment: UiFragmentRevision {
                id: UiFragmentId("static-editor-shell".into()),
                revision: Revision(3),
            },
            intent,
            pointer: Some(UiPointerMetadata { id: 0, sequence: 1 }),
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: None,
        };
        let request = RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("render-1".into()),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: "window-7".into(),
                pid: 1,
                origin: "neon-wgpu-runtime".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.input.event".into(),
            params: json!(event),
            expected_revision: Some(Revision(3)),
            idempotency_key: Some("render-once:7:1".into()),
        };
        let response = runtime.forward_wgpu_event(endpoint, request).unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(
            runtime
                .command_receipt(&RequestId("render-1".into()))
                .unwrap()
                .state,
            CommandState::Accepted
        );
        assert_eq!(runtime.ai_terrain.last_seed, Some(42));
        assert_eq!(runtime.ai_terrain.seed, 43);
        receiver.join().unwrap().unwrap();
    }

    #[test]
    fn compiled_program_control_semantics_route_and_refresh_generically() {
        let (document, program) =
            crate::demo_domain::component_gallery_program(gallery_asset()).unwrap();
        let revision = program.revision.clone();
        let inputs = UiInputStore::activate(revision.clone(), document.input_schema.clone())
            .unwrap()
            .snapshot();
        let frame = evaluate_ui_program(
            &program,
            &inputs,
            UiCpuViewport {
                logical_bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 760.0,
                    height: 680.0,
                },
                revision: Revision(1),
            },
            &UiLocalPresentationState::default(),
        );
        let controls = [
            (
                "action-button",
                UiNodeKind::Button,
                UiProgramSemanticEventKind::Activate,
            ),
            (
                "gallery-text",
                UiNodeKind::TextInput,
                UiProgramSemanticEventKind::TextEditCommit,
            ),
            (
                "feature-toggle",
                UiNodeKind::Checkbox,
                UiProgramSemanticEventKind::SelectionChanged,
            ),
            (
                "mode-radio",
                UiNodeKind::RadioButton,
                UiProgramSemanticEventKind::SelectionChanged,
            ),
            (
                "exposure-slider",
                UiNodeKind::Slider,
                UiProgramSemanticEventKind::ValueCommit,
            ),
            (
                "count-drag",
                UiNodeKind::DragValue,
                UiProgramSemanticEventKind::ValueCommit,
            ),
            (
                "mode-combo",
                UiNodeKind::Combo,
                UiProgramSemanticEventKind::SelectionChanged,
            ),
            (
                "mode-dropdown",
                UiNodeKind::Dropdown,
                UiProgramSemanticEventKind::SelectionChanged,
            ),
            (
                "mode-tabs",
                UiNodeKind::Tabs,
                UiProgramSemanticEventKind::SelectionChanged,
            ),
            (
                "item-selectable",
                UiNodeKind::Selectable,
                UiProgramSemanticEventKind::SelectionChanged,
            ),
            (
                "item-list",
                UiNodeKind::ListBox,
                UiProgramSemanticEventKind::SelectionChanged,
            ),
            (
                "gallery-scroll",
                UiNodeKind::Scrollbar,
                UiProgramSemanticEventKind::ValueCommit,
            ),
        ];
        for (node_key, kind, event_kind) in &controls {
            assert_eq!(
                program
                    .nodes
                    .iter()
                    .find(|node| node.key == *node_key)
                    .unwrap()
                    .kind,
                *kind
            );
            assert!(
                frame
                    .render_primitives
                    .iter()
                    .any(|primitive| primitive.node_key == *node_key)
            );
            assert_eq!(
                program_semantic_event_kind(
                    kind,
                    &neon_ui_schema::UiSemanticEventType::PointerClick
                ),
                *event_kind
            );
        }
        assert_eq!(
            program
                .nodes
                .iter()
                .find(|node| node.key == "gallery-progress")
                .unwrap()
                .kind,
            UiNodeKind::ProgressBar
        );
        assert_eq!(
            frame
                .nodes
                .iter()
                .find(|node| node.node_key == "exposure-slider")
                .unwrap()
                .numeric_value,
            Some(0.5)
        );
        assert_eq!(
            frame
                .nodes
                .iter()
                .find(|node| node.node_key == "mode-combo")
                .unwrap()
                .state_token
                .as_deref(),
            Some("beta")
        );
        let mut router = UiProgramSemanticEventRouter::new(program.clone(), inputs.clone(), 7);
        for (index, (node_key, _, kind)) in controls.iter().enumerate() {
            let node = program
                .nodes
                .iter()
                .find(|node| node.key == *node_key)
                .unwrap();
            assert_eq!(
                program_semantic_event_kind(
                    &node.kind,
                    &neon_ui_schema::UiSemanticEventType::PointerClick
                ),
                *kind,
            );
            let declaration = program
                .event_records
                .iter()
                .find(|event| event.node_key == *node_key)
                .unwrap();
            let payload = declaration
                .bound_input_keys
                .iter()
                .map(|key| {
                    (
                        key.clone(),
                        input_value_as_event_payload(&inputs.values[key].value).unwrap(),
                    )
                })
                .collect();
            let event = UiProgramSemanticEvent {
                event_id: format!("gallery-default-{index}"),
                kind: *kind,
                intent: declaration.intent.clone(),
                source_node_key: (*node_key).into(),
                payload,
                program_revision: revision.clone(),
                input_revision: inputs.input_revision,
                request_id: format!("gallery-request-{index}"),
                idempotency_key: format!("gallery-key-{index}"),
                requested_value: None,
                interaction: neon_ui_schema::UiSemanticInteractionMetadata {
                    interaction_id: format!("gallery-interaction-{index}"),
                    sequence: index as u64 + 1,
                    renderer_epoch: 7,
                },
            };
            assert_eq!(
                router.validate(&event).status,
                UiProgramSemanticEventStatus::Accepted
            );
        }
        assert_eq!(
            program_semantic_event_kind(
                &UiNodeKind::TextInput,
                &neon_ui_schema::UiSemanticEventType::PointerClick
            ),
            UiProgramSemanticEventKind::TextEditCommit,
        );
        assert_eq!(
            program_semantic_event_kind(
                &UiNodeKind::Button,
                &neon_ui_schema::UiSemanticEventType::PointerClick
            ),
            UiProgramSemanticEventKind::Activate,
        );
        let mut refreshed_inputs =
            UiInputStore::activate(revision.clone(), document.input_schema.clone()).unwrap();
        refreshed_inputs
            .apply(
                UiInputWriter::External,
                UiInputFrame {
                    program_revision: revision.clone(),
                    expected_input_revision: Revision(0),
                    request_id: "generic-refresh".into(),
                    idempotency_key: "generic-refresh".into(),
                    changes: vec![
                        UiInputChange {
                            key: "feature_enabled".into(),
                            value: UiInputValue::Bool { value: false },
                        },
                        UiInputChange {
                            key: "slider_value".into(),
                            value: UiInputValue::F32 { value: 0.75 },
                        },
                        UiInputChange {
                            key: "combo_choice".into(),
                            value: UiInputValue::Enum {
                                value: "gamma".into(),
                            },
                        },
                    ],
                },
            )
            .unwrap();
        let mut refreshed_fragment = UiFragment {
            fragment_id: UiFragmentId("generic-refresh".into()),
            revision: Revision(1),
            root: document.ir.root.clone(),
            effects: lower_nui_flow_effects(&document),
        };
        refresh_fragment_from_program(
            &mut refreshed_fragment,
            &program,
            &refreshed_inputs.snapshot(),
            &document.input_schema,
        );
        assert!(refreshed_fragment.effects.iter().any(|effect| matches!(
            effect,
            UiEffect::ControlPresentation { node_id, state: neon_ui_schema::UiControlPresentation::Toggle { selected: false } }
                if node_id.0 == "feature-toggle"
        )));
        assert!(refreshed_fragment.effects.iter().any(|effect| matches!(
            effect,
            UiEffect::ControlPresentation { node_id, state: neon_ui_schema::UiControlPresentation::Numeric { value, min, max } }
                if node_id.0 == "exposure-slider" && *value == 0.75 && *min == 0.0 && *max == 1.0
        )));
        assert!(refreshed_fragment.effects.iter().any(|effect| matches!(
            effect,
            UiEffect::ControlPresentation { node_id, state: neon_ui_schema::UiControlPresentation::Choice { token, .. } }
                if node_id.0 == "mode-combo" && token == "gamma"
        )));
        let mut disabled = inputs.clone();
        disabled.input_revision = Revision(inputs.input_revision.0 + 1);
        disabled.values.get_mut("controls_enabled").unwrap().value =
            UiInputValue::Bool { value: false };
        let disabled_frame = evaluate_ui_program(
            &program,
            &disabled,
            UiCpuViewport {
                logical_bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 760.0,
                    height: 680.0,
                },
                revision: Revision(1),
            },
            &UiLocalPresentationState::default(),
        );
        assert!(
            disabled_frame
                .nodes
                .iter()
                .filter(|node| matches!(
                    node.node_key.as_str(),
                    "feature-toggle"
                        | "mode-radio"
                        | "exposure-slider"
                        | "count-drag"
                        | "mode-combo"
                        | "mode-dropdown"
                        | "item-selectable"
                        | "item-list"
                        | "gallery-scroll"
                ))
                .all(|node| !node.enabled)
        );
        let mut disabled_router =
            UiProgramSemanticEventRouter::new(program.clone(), disabled.clone(), 7);
        for (index, (node_key, _, kind)) in controls.iter().enumerate() {
            let declaration = program
                .event_records
                .iter()
                .find(|event| event.node_key == *node_key)
                .unwrap();
            let payload = declaration
                .bound_input_keys
                .iter()
                .map(|key| {
                    (
                        key.clone(),
                        input_value_as_event_payload(&disabled.values[key].value).unwrap(),
                    )
                })
                .collect();
            let event = UiProgramSemanticEvent {
                event_id: format!("gallery-disabled-{index}"),
                kind: *kind,
                intent: declaration.intent.clone(),
                source_node_key: (*node_key).into(),
                payload,
                program_revision: revision.clone(),
                input_revision: disabled.input_revision,
                request_id: format!("gallery-disabled-request-{index}"),
                idempotency_key: format!("gallery-disabled-key-{index}"),
                requested_value: None,
                interaction: neon_ui_schema::UiSemanticInteractionMetadata {
                    interaction_id: format!("gallery-disabled-interaction-{index}"),
                    sequence: index as u64 + 1,
                    renderer_epoch: 7,
                },
            };
            assert_eq!(
                disabled_router.validate(&event).code.as_deref(),
                Some(ERROR_UI_PROGRAM_EVENT_CONTROL_UNAVAILABLE)
            );
        }
    }

    #[test]
    fn label_selection_updates_only_the_panel_snapshot_without_generation() {
        use neon_ui_schema::{
            UiFragmentRevision, UiIntent, UiPointerMetadata, UiSemanticEventType,
        };

        let intent = UiIntent::Invoke {
            action: "ai.terrain.condition.set".into(),
            params: json!({"dimension": "parent", "index": 7, "label": "volcanic"}),
        };
        let mut runtime = UiRuntime::new(7, "ui-ai-label-test");
        let mut fragment = runtime.static_fragment(Revision(1));
        fragment.effects.push(UiEffect::SemanticIntent {
            intent: intent.clone(),
        });
        runtime.cached_fragment = Some(fragment);
        let event = UiSemanticEvent {
            event: UiSemanticEventType::PointerClick,
            event_id: "label-1".into(),
            renderer_epoch: 7,
            composition_revision: Revision(1),
            fragment: UiFragmentRevision {
                id: UiFragmentId("static-editor-shell".into()),
                revision: Revision(1),
            },
            intent,
            pointer: Some(UiPointerMetadata { id: 0, sequence: 1 }),
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: None,
        };
        let response = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("label-1".into()),
            client: ClientIdentity {
                kind: ClientKind::WgpuRuntime,
                instance_id: "window-7".into(),
                pid: 1,
                origin: "neon-wgpu-runtime".into(),
            },
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.input.event".into(),
            params: json!(event),
            expected_revision: Some(Revision(1)),
            idempotency_key: Some("label:7:1".into()),
        });
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.result.as_ref().unwrap()["condition"]["parent"], 7);
        assert_eq!(response.result.as_ref().unwrap()["state"], "idle");
        assert!(response.result.as_ref().unwrap()["job_id"].is_null());
        assert_eq!(runtime.ai_terrain.revision, Revision(2));
    }

    #[test]
    fn semantic_event_rejection_codes_are_explicit() {
        use neon_ui_schema::{
            UiFragmentRevision, UiIntent, UiPointerMetadata, UiSemanticEventType,
        };
        let mut runtime = UiRuntime::new(7, "ui-test");
        runtime.cached_fragment = Some(runtime.static_fragment(Revision(3)));
        let event = |epoch, revision, action: &str, sequence| UiSemanticEvent {
            event: UiSemanticEventType::PointerClick,
            event_id: "event-reject".into(),
            renderer_epoch: epoch,
            composition_revision: Revision(9),
            fragment: UiFragmentRevision {
                id: UiFragmentId("static-editor-shell".into()),
                revision: Revision(revision),
            },
            intent: UiIntent::Invoke {
                action: action.into(),
                params: json!({"tool": "water_inject"}),
            },
            pointer: Some(UiPointerMetadata { id: 0, sequence }),
            focus: None,
            data_grid_cell: None,
            text: None,
            control_value: None,
            drag_drop: None,
        };
        assert_eq!(
            runtime.validate_semantic_event(&event(8, 3, "terrain.tool.select", 1)),
            Err(ERROR_RENDERER_EPOCH_MISMATCH)
        );
        assert_eq!(
            runtime.validate_semantic_event(&event(7, 2, "terrain.tool.select", 1)),
            Err(ERROR_FRAGMENT_REVISION_STALE)
        );
        assert_eq!(
            runtime.validate_semantic_event(&event(7, 3, "terrain.tool.invalid", 1)),
            Err(ERROR_INTENT_NOT_BOUND)
        );
        runtime
            .validate_semantic_event(&event(7, 3, "terrain.tool.select", 2))
            .unwrap();
        assert_eq!(
            runtime.validate_semantic_event(&event(7, 3, "terrain.tool.select", 2)),
            Err(ERROR_INPUT_SEQUENCE_STALE)
        );
    }

    #[test]
    fn input_store_installs_defaults_and_rejects_external_local_writes() {
        use neon_ui_schema::{
            UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiGpuScalarRepresentation,
            UiInputKind, UiInputPacking, UiInputSlot, UiProgramCapability,
            UiProgramCapabilityOwner, UiProgramCapabilityStatus,
        };
        let program = UiProgramRevision {
            program_id: "surface.editor".into(),
            revision: Revision(3),
            schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: vec![UiProgramCapability {
                name: UI_PROGRAM_CAPABILITY_NAME.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Experimental,
            }],
        };
        let schema = UiInputSchema {
            schema_id: "terrain-inputs".into(),
            version: 1,
            layout_hash: "layout-v1".into(),
            slots: vec![UiInputSlot {
                key: "hovered".into(),
                kind: UiInputKind::Bool,
                default_value: UiInputValue::Bool { value: false },
                update_class: UiInputUpdateClass::LocalPresentation,
                semantic_label: "Hovered".into(),
                packing: UiInputPacking {
                    alignment: 4,
                    lanes: 1,
                    offset: 0,
                    representation: UiGpuScalarRepresentation::Bool32,
                },
            }],
            grid_slots: Vec::new(),
            flow_name: String::new(),
            emit_event_keys: Vec::new(),
        };
        let mut store = UiInputStore::activate(program.clone(), schema).unwrap();
        assert_eq!(
            store.snapshot().values["hovered"].source,
            UiInputValueSource::Default
        );
        let frame = UiInputFrame {
            program_revision: program,
            expected_input_revision: Revision(0),
            request_id: "request-1".into(),
            idempotency_key: "key-1".into(),
            changes: vec![UiInputChange {
                key: "hovered".into(),
                value: UiInputValue::Bool { value: true },
            }],
        };
        assert_eq!(
            store
                .apply(UiInputWriter::External, frame)
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_INPUT_UPDATE_FORBIDDEN
        );
    }

    #[test]
    fn text_registry_tracks_generations_references_and_sanitized_diagnostics() {
        let mut registry = UiTextRegistry::new("surface.editor.text", 1, 64).unwrap();
        let handle = registry
            .insert_dynamic(Revision(0), "Terrain".into())
            .unwrap();
        assert_eq!(registry.resolve(handle).unwrap().text, "Terrain");
        assert!(registry.snapshot(false).records[0].text.is_empty());
        registry.retain(registry.revision(), handle).unwrap();
        registry.release(registry.revision(), handle).unwrap();
        assert_eq!(registry.diagnostic(handle).reference_count, 1);
        registry.release(registry.revision(), handle).unwrap();
        assert_eq!(
            registry.diagnostic(handle).status,
            UiTextHandleStatus::Released
        );
        let reused = registry
            .insert_dynamic(registry.revision(), "\u{5730}\u{5f62}".into())
            .unwrap();
        assert_eq!(reused.id, handle.id);
        assert_ne!(reused.generation, handle.generation);
        assert_eq!(
            registry.validate_handle(handle).unwrap_err().code,
            ERROR_UI_PROGRAM_TEXT_REGISTRY_GENERATION_MISMATCH
        );
        let debug = registry.debug_snapshot();
        assert_eq!(debug.records[0].handle, reused);
        assert_eq!(debug.records[0].byte_length, "地形".len() as u32);
        assert!(!debug.records[0].resident);
    }

    #[test]
    fn text_registry_rejects_overflow_and_input_frames_with_missing_handles() {
        use neon_ui_schema::{
            UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiInputKind, UiInputPacking,
            UiInputSlot, UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
        };
        let mut registry = UiTextRegistry::new("surface.editor.text", 1, 4).unwrap();
        let handle = registry.insert_dynamic(Revision(0), "ok".into()).unwrap();
        assert_eq!(
            registry
                .insert_dynamic(registry.revision(), "next".into())
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_TEXT_REGISTRY_CAPACITY_OVERFLOW
        );
        assert_eq!(
            registry
                .replace_dynamic(registry.revision(), handle, "toolong".into())
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_TEXT_TOO_LONG
        );
        let program = UiProgramRevision {
            program_id: "surface.editor".into(),
            revision: Revision(1),
            schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: vec![UiProgramCapability {
                name: UI_PROGRAM_CAPABILITY_NAME.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Experimental,
            }],
        };
        let kind = UiInputKind::TextHandle;
        let (alignment, lanes, representation) = kind.packing();
        let schema = UiInputSchema {
            schema_id: "text-inputs".into(),
            version: 1,
            layout_hash: "layout-v1".into(),
            slots: vec![UiInputSlot {
                key: "title".into(),
                kind,
                default_value: UiInputValue::TextHandle { value: handle },
                update_class: UiInputUpdateClass::TextRegistryReference,
                semantic_label: "Title".into(),
                packing: UiInputPacking {
                    alignment,
                    lanes,
                    offset: 0,
                    representation,
                },
            }],
            grid_slots: Vec::new(),
            flow_name: String::new(),
            emit_event_keys: Vec::new(),
        };
        let mut store = UiInputStore::activate(program.clone(), schema).unwrap();
        let missing = UiTextHandle {
            id: 999,
            generation: 1,
        };
        let frame = UiInputFrame {
            program_revision: program,
            expected_input_revision: Revision(0),
            request_id: "request-text".into(),
            idempotency_key: "key-text".into(),
            changes: vec![UiInputChange {
                key: "title".into(),
                value: UiInputValue::TextHandle { value: missing },
            }],
        };
        assert_eq!(
            store
                .apply_with_text_registry(UiInputWriter::External, frame, &registry)
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_UNKNOWN_TEXT_HANDLE
        );
    }

    #[test]
    fn text_registry_validates_default_handles_and_literal_revision_before_activation() {
        use neon_ui_schema::{
            UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiInputKind, UiInputPacking,
            UiInputSlot, UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
        };
        let mut registry = UiTextRegistry::new("surface.editor.text", 4, 64).unwrap();
        let literal = registry
            .register_literal(Revision(0), "Terrain".into())
            .unwrap();
        assert_eq!(
            registry
                .register_literal(Revision(0), "Terrain".into())
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_TEXT_REGISTRY_STALE_REVISION
        );
        assert_eq!(
            registry
                .register_literal(registry.revision(), "Terrain".into())
                .unwrap(),
            literal
        );

        let program = UiProgramRevision {
            program_id: "surface.editor".into(),
            revision: Revision(1),
            schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: vec![UiProgramCapability {
                name: UI_PROGRAM_CAPABILITY_NAME.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Experimental,
            }],
        };
        let kind = UiInputKind::TextHandle;
        let (alignment, lanes, representation) = kind.packing();
        let missing = UiTextHandle {
            id: 404,
            generation: 1,
        };
        let schema = UiInputSchema {
            schema_id: "text-inputs".into(),
            version: 1,
            layout_hash: "layout-v1".into(),
            slots: vec![UiInputSlot {
                key: "title".into(),
                kind,
                default_value: UiInputValue::TextHandle { value: missing },
                update_class: UiInputUpdateClass::TextRegistryReference,
                semantic_label: "Title".into(),
                packing: UiInputPacking {
                    alignment,
                    lanes,
                    offset: 0,
                    representation,
                },
            }],
            grid_slots: Vec::new(),
            flow_name: String::new(),
            emit_event_keys: Vec::new(),
        };
        assert_eq!(
            UiInputStore::activate_with_text_registry(program, schema, &registry)
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_UNKNOWN_TEXT_HANDLE
        );
    }

    fn compiler_program_revision() -> UiProgramRevision {
        use neon_ui_schema::{
            UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiProgramCapability,
            UiProgramCapabilityOwner, UiProgramCapabilityStatus,
        };
        UiProgramRevision {
            program_id: "surface.compiler".into(),
            revision: Revision(1),
            schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: vec![UiProgramCapability {
                name: UI_PROGRAM_CAPABILITY_NAME.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Experimental,
            }],
        }
    }

    fn compiler_schema(default_visible: bool) -> UiInputSchema {
        use neon_ui_schema::{UiGpuScalarRepresentation, UiInputKind, UiInputPacking, UiInputSlot};
        UiInputSchema {
            schema_id: "compiler-inputs".into(),
            version: 1,
            layout_hash: "compiler-v1".into(),
            slots: vec![UiInputSlot {
                key: "visible".into(),
                kind: UiInputKind::Bool,
                default_value: UiInputValue::Bool {
                    value: default_visible,
                },
                update_class: UiInputUpdateClass::ReliableExternal,
                semantic_label: "Visible".into(),
                packing: UiInputPacking {
                    alignment: 4,
                    lanes: 1,
                    offset: 0,
                    representation: UiGpuScalarRepresentation::Bool32,
                },
            }],
            grid_slots: vec![neon_ui_schema::UiGridInputSlot {
                key: "assets_window".into(),
            }],
            flow_name: String::new(),
            emit_event_keys: Vec::new(),
        }
    }

    fn compiler_document() -> UiIrDocument {
        UiIrDocument {
            schema_version: 1,
            surface_id: UiSurfaceId("surface.compiler".into()),
            revision: Revision(1),
            root: UiNode {
                node_id: UiNodeId("root".into()),
                kind: UiNodeKind::Panel,
                bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 50.0,
                },
                layout: None,
                visible: true,
                enabled: true,
                text_key: None,
                text: Some(TextRef::Literal {
                    value: "Terrain".into(),
                }),
                image: None,
                surface: None,
                style: UiStyle::default(),
                enter_transition: None,
                world_depth: None,
                children: vec![UiNode {
                    node_id: UiNodeId("commit".into()),
                    kind: UiNodeKind::Button,
                    bounds: UiBounds {
                        x: 4.0,
                        y: 4.0,
                        width: 80.0,
                        height: 24.0,
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
                    world_depth: None,
                    children: Vec::new(),
                }],
            },
            bindings: vec![neon_ui_schema::UiIrBinding {
                input_key: "visible".into(),
                node_key: "commit".into(),
                property: UiBoundProperty::Visible,
            }],
            events: vec![UiProgramEventDeclaration {
                node_key: "commit".into(),
                intent: "terrain.commit".into(),
                allowed_payload_keys: Vec::new(),
                literal_payload: BTreeMap::new(),
                bound_input_keys: Vec::new(),
            }],
            resources: Vec::new(),
            image_resources: BTreeMap::new(),
            branches: Vec::new(),
            templates: Vec::new(),
            data_grids: Vec::new(),
            resource_budget: UiResourceBudget {
                max_nodes: 2,
                max_bindings: 1,
                max_instances: 2,
                max_text_records: 1,
                max_glyph_instances: 7,
                max_events: 1,
                max_clips: 0,
            },
        }
    }

    #[test]
    fn compiler_is_deterministic_and_cpu_evaluator_traces_direct_bindings() {
        let document = compiler_document();
        let schema = compiler_schema(false);
        let revision = compiler_program_revision();
        let first = compile_ui_program(&document, revision.clone(), &schema).unwrap();
        let second = compile_ui_program(&document, revision.clone(), &schema).unwrap();
        assert_eq!(first.layout_hash, second.layout_hash);
        assert_eq!(
            first.nodes.iter().map(|node| &node.key).collect::<Vec<_>>(),
            vec!["root", "commit"]
        );
        assert_eq!(first.dependency_index.input_to_bindings["visible"], vec![0]);
        let store = UiInputStore::activate(revision, schema).unwrap();
        let output = evaluate_ui_program(
            &first,
            &store.snapshot(),
            UiCpuViewport {
                logical_bounds: UiBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 50.0,
                },
                revision: Revision(1),
            },
            &UiLocalPresentationState {
                revision: Revision(0),
                machine_states: BTreeMap::new(),
                drag_offsets: BTreeMap::new(),
            },
        );
        assert!(
            !output
                .nodes
                .iter()
                .find(|node| node.node_key == "commit")
                .unwrap()
                .visible
        );
        assert_eq!(output.semantic_targets[0].node_key, "commit");
    }

    #[test]
    fn data_grid_store_accepts_only_fresh_bounded_windows() {
        use neon_ui_schema::{
            UiDataGridDeclaration, UiDataGridFrame, UiDataGridInputFrame, UiDataGridWindowRow,
        };

        let mut document = compiler_document();
        document.root.children.push(UiNode {
            node_id: UiNodeId("assets".into()),
            kind: UiNodeKind::DataGrid,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 40.0,
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
            world_depth: None,
            children: Vec::new(),
        });
        document.data_grids.push(UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 2,
            row_height: 24,
            overscan: 1,
            columns: vec![neon_ui_schema::UiDataGridColumn {
                key: "name".into(),
                label: "Name".into(),
                width: 120,
                presentation: neon_ui_schema::UiDataGridPresentation::Edit {
                    max_chars: 120,
                    intent: "asset.name.edit".into(),
                },
            }],
        });
        document.resource_budget.max_nodes = 3;
        document.resource_budget.max_instances = 3;
        let revision = compiler_program_revision();
        let program =
            compile_ui_program(&document, revision.clone(), &compiler_schema(true)).unwrap();
        assert_eq!(
            program.data_grid_records[0].columns[0].presentation,
            neon_ui_schema::UiDataGridPresentation::Edit {
                max_chars: 120,
                intent: "asset.name.edit".into()
            }
        );
        let frame = UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 10,
            first_row: 4,
            window_rows: vec![
                UiDataGridWindowRow {
                    stable_row_key: "asset-5".into(),
                    cells: BTreeMap::from([(
                        "name".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: UiInputValue::TextHandle {
                                value: UiTextHandle {
                                    id: 5,
                                    generation: 1,
                                },
                            },
                            display: UiTextHandle {
                                id: 105,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    )]),
                },
                UiDataGridWindowRow {
                    stable_row_key: "asset-6".into(),
                    cells: BTreeMap::from([(
                        "name".into(),
                        neon_ui_schema::UiDataGridCell {
                            value: UiInputValue::TextHandle {
                                value: UiTextHandle {
                                    id: 6,
                                    generation: 1,
                                },
                            },
                            display: UiTextHandle {
                                id: 106,
                                generation: 1,
                            },
                            presentation_override: None,
                        },
                    )]),
                },
            ],
            expected_program_revision: revision,
        };
        let mut store = UiDataGridStore::default();
        let input = |frame| UiDataGridInputFrame {
            source_key: "assets_window".into(),
            frame,
        };
        assert_eq!(
            store
                .apply(&program, input(frame.clone()))
                .unwrap()
                .accepted_rows,
            2
        );
        assert_eq!(store.frame("assets_window").unwrap().first_row, 4);
        let mut same_list_next_window = frame.clone();
        same_list_next_window.first_row = 6;
        assert_eq!(
            store
                .apply(&program, input(same_list_next_window))
                .unwrap()
                .accepted_rows,
            2
        );

        let mut stale_list = frame.clone();
        stale_list.list_revision = Revision(0);
        assert_eq!(
            store.apply(&program, input(stale_list)).unwrap_err().code,
            ERROR_UI_PROGRAM_STALE_INPUT_REVISION
        );

        let mut wrong_program = frame.clone();
        wrong_program.list_revision = Revision(2);
        wrong_program.expected_program_revision.revision = Revision(2);
        assert_eq!(
            store
                .apply(&program, input(wrong_program))
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_STALE_INPUT_REVISION
        );

        let mut out_of_bounds = frame.clone();
        out_of_bounds.list_revision = Revision(2);
        out_of_bounds.first_row = 9;
        assert_eq!(
            store
                .apply(&program, input(out_of_bounds))
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_CAPACITY_OVERFLOW
        );

        let mut duplicate_key = frame;
        duplicate_key.list_revision = Revision(2);
        duplicate_key.window_rows[1].stable_row_key = "asset-5".into();
        assert_eq!(
            store
                .apply(&program, input(duplicate_key))
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE
        );

        let mut missing_cell = store.frame("assets_window").unwrap().clone();
        missing_cell.list_revision = Revision(2);
        missing_cell.window_rows[0].cells.clear();
        assert_eq!(
            store.apply(&program, input(missing_cell)).unwrap_err().code,
            ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE
        );

        let mut invalid_value = store.frame("assets_window").unwrap().clone();
        invalid_value.list_revision = Revision(2);
        invalid_value.window_rows[0]
            .cells
            .get_mut("name")
            .unwrap()
            .value = UiInputValue::F32 { value: f32::NAN };
        assert_eq!(
            store
                .apply(&program, input(invalid_value))
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE
        );

        let mut invalid_override = store.frame("assets_window").unwrap().clone();
        invalid_override.list_revision = Revision(2);
        invalid_override.window_rows[0]
            .cells
            .get_mut("name")
            .unwrap()
            .presentation_override = Some(neon_ui_schema::UiDataGridCellPresentation::Dropdown {
            options: Vec::new(),
        });
        assert_eq!(
            store
                .apply(&program, input(invalid_override))
                .unwrap_err()
                .code,
            ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE
        );
    }

    #[test]
    fn renderer_rejection_rolls_back_staged_presentation_replacement() {
        let document = parse_nui_flow("surface root\n  button source event domain.move\n").unwrap();
        let program = compile_nui_flow_program(
            &document,
            UiProgramRevision {
                program_id: "replacement-contract".into(),
                revision: Revision(1),
                schema_version: 1,
                capabilities: vec![neon_ui_schema::UiProgramCapability {
                    name: "ui.program.v1".into(),
                    version: 1,
                    owner: neon_ui_schema::UiProgramCapabilityOwner::SharedContract,
                    status: neon_ui_schema::UiProgramCapabilityStatus::Supported,
                }],
            },
        )
        .unwrap();
        let schema = document.input_schema.clone();
        let mut runtime = UiRuntime::new(7, "presentation-rollback");
        runtime.cached_fragment = Some(runtime.static_fragment(Revision(1)));
        runtime.host_adapter =
            Some(UiHostAdapter::activate(program.clone(), schema.clone(), 7).unwrap());
        let declaration = program.event_records[0].clone();
        let replacement = neon_ui_schema::UiHostPresentationUpdate {
            expected_fragment_revision: Revision(1),
            replacement_fragment: runtime.static_fragment(Revision(2)),
            replacement_program: program.clone(),
            replacement_input_schema: schema,
        };
        let host = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host_endpoint = host.local_addr().unwrap();
        let event_program_revision = program.revision.clone();
        let expected_program_revision = program.revision.clone();
        let host_thread = thread::spawn(move || {
            host.serve_one(|request| RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Accepted,
                revision: Some(Revision(2)),
                result: Some(json!(UiHostPublication {
                    scalar_frame: UiInputFrame {
                        program_revision: program.revision.clone(),
                        expected_input_revision: Revision(0),
                        request_id: "replace".into(),
                        idempotency_key: "replace".into(),
                        changes: Vec::new()
                    },
                    grid_inputs: Vec::new(),
                    presentation_update: Some(replacement),
                })),
                snapshot: None,
                error: None,
            })
        });
        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let renderer_endpoint = renderer.local_addr().unwrap();
        let renderer_thread = thread::spawn(move || {
            renderer.serve_one(|request| RpcResponse {
                request_id: request.request_id,
                status: RpcStatus::Rejected,
                revision: Some(Revision(1)),
                result: None,
                snapshot: None,
                error: Some(RpcError {
                    code: "renderer_rejected".into(),
                    message: "rejected for rollback test".into(),
                    current_revision: Some(Revision(1)),
                    object_id: None,
                }),
            })
        });
        let event = UiProgramSemanticEvent {
            event_id: "replace-event".into(),
            kind: UiProgramSemanticEventKind::Activate,
            intent: declaration.intent,
            source_node_key: declaration.node_key,
            payload: BTreeMap::new(),
            program_revision: event_program_revision,
            input_revision: Revision(0),
            request_id: "replace-event".into(),
            idempotency_key: "replace-event".into(),
            requested_value: None,
            interaction: UiSemanticInteractionMetadata {
                interaction_id: "replace-interaction".into(),
                sequence: 1,
                renderer_epoch: 7,
            },
        };
        let response = runtime
            .forward_host_request(
                host_endpoint,
                renderer_endpoint,
                RpcRequest {
                    protocol: "neon3.rpc".into(),
                    version: PROTOCOL_VERSION,
                    request_id: RequestId("replace-event".into()),
                    client: runtime.client.clone(),
                    target: ServiceName(SERVICE_NAME.into()),
                    method: "ui.host.inbound".into(),
                    params: json!(UiHostInbound::SemanticIntent { event }),
                    expected_revision: None,
                    idempotency_key: Some("replace-event".into()),
                },
            )
            .unwrap();
        assert_eq!(response.status, RpcStatus::Rejected);
        assert_eq!(runtime.cached_fragment().unwrap().revision, Revision(1));
        assert_eq!(
            runtime.host_adapter.as_ref().unwrap().program().revision,
            expected_program_revision
        );
        assert_eq!(
            runtime
                .host_adapter
                .as_ref()
                .unwrap()
                .snapshot()
                .scalar_inputs
                .input_revision,
            Revision(0)
        );
        host_thread.join().unwrap().unwrap();
        renderer_thread.join().unwrap().unwrap();
    }

    #[test]
    fn data_grid_store_attaches_current_frame_to_fragment() {
        use neon_ui_schema::{
            UiDataGridCell, UiDataGridDeclaration, UiDataGridFrame, UiDataGridInputFrame,
            UiDataGridWindowRow,
        };

        let mut document = compiler_document();
        document.root.children.push(UiNode {
            node_id: UiNodeId("assets".into()),
            kind: UiNodeKind::DataGrid,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 40.0,
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
            world_depth: None,
            children: Vec::new(),
        });
        document.data_grids.push(UiDataGridDeclaration {
            node_key: "assets".into(),
            source_key: "assets_window".into(),
            max_window_rows: 2,
            row_height: 24,
            overscan: 1,
            columns: vec![neon_ui_schema::UiDataGridColumn {
                key: "name".into(),
                label: "Name".into(),
                width: 120,
                presentation: neon_ui_schema::UiDataGridPresentation::Text,
            }],
        });
        document.resource_budget.max_nodes = 3;
        document.resource_budget.max_instances = 3;
        let revision = compiler_program_revision();
        let program =
            compile_ui_program(&document, revision.clone(), &compiler_schema(true)).unwrap();
        let frame = UiDataGridFrame {
            list_revision: Revision(1),
            total_rows: 1,
            first_row: 0,
            window_rows: vec![UiDataGridWindowRow {
                stable_row_key: "asset-1".into(),
                cells: BTreeMap::from([(
                    "name".into(),
                    UiDataGridCell {
                        value: UiInputValue::TextHandle {
                            value: UiTextHandle {
                                id: 1,
                                generation: 1,
                            },
                        },
                        display: UiTextHandle {
                            id: 101,
                            generation: 1,
                        },
                        presentation_override: None,
                    },
                )]),
            }],
            expected_program_revision: revision,
        };
        let mut store = UiDataGridStore::default();
        store
            .apply(
                &program,
                UiDataGridInputFrame {
                    source_key: "assets_window".into(),
                    frame,
                },
            )
            .unwrap();
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("grid-fragment".into()),
            revision: Revision(1),
            root: document.root,
            effects: vec![UiEffect::SemanticAction {
                action: "grid.ready".into(),
            }],
        };

        store.attach_to_fragment(&program, &mut fragment).unwrap();

        assert!(
            matches!(fragment.effects.last(), Some(UiEffect::DataGridFrame { declaration, frame })
            if declaration.node_key == "assets" && frame.window_rows[0].cells["name"].display.id == 101)
        );
        fragment.validate().unwrap();
    }

    #[test]
    fn program_semantic_events_require_current_enabled_declarations_and_are_idempotent() {
        use neon_ui_schema::{
            UiProgramSemanticEvent, UiProgramSemanticEventKind, UiSemanticInteractionMetadata,
        };
        let mut document = compiler_document();
        document.events[0].literal_payload.insert(
            "tool".into(),
            UiSemanticPayloadValue::Enum {
                value: "water".into(),
            },
        );
        let revision = compiler_program_revision();
        let program =
            compile_ui_program(&document, revision.clone(), &compiler_schema(true)).unwrap();
        let inputs = UiInputStore::activate(revision.clone(), compiler_schema(true))
            .unwrap()
            .snapshot();
        let mut router = UiProgramSemanticEventRouter::new(program, inputs.clone(), 7);
        let event = UiProgramSemanticEvent {
            event_id: "event-1".into(),
            kind: UiProgramSemanticEventKind::Activate,
            intent: "terrain.commit".into(),
            source_node_key: "commit".into(),
            payload: BTreeMap::from([(
                "tool".into(),
                UiSemanticPayloadValue::Enum {
                    value: "water".into(),
                },
            )]),
            program_revision: revision,
            input_revision: inputs.input_revision,
            request_id: "request-1".into(),
            idempotency_key: "key-1".into(),
            requested_value: None,
            interaction: UiSemanticInteractionMetadata {
                interaction_id: "interaction-1".into(),
                sequence: 1,
                renderer_epoch: 7,
            },
        };
        assert_eq!(
            router.validate(&event).status,
            UiProgramSemanticEventStatus::Accepted
        );
        assert_eq!(
            router.validate(&event).status,
            UiProgramSemanticEventStatus::Duplicate
        );
        let mut stale = event;
        stale.idempotency_key = "key-2".into();
        stale.input_revision = Revision(99);
        assert_eq!(
            router.validate(&stale).code.as_deref(),
            Some(ERROR_UI_PROGRAM_EVENT_STALE_REVISION)
        );
        assert_eq!(router.trace().len(), 3);
    }

    #[test]
    fn compiler_rejects_invalid_binding_and_budget_overflow() {
        let mut document = compiler_document();
        document.bindings[0].property = UiBoundProperty::Opacity;
        assert_eq!(
            compile_ui_program(
                &document,
                compiler_program_revision(),
                &compiler_schema(true)
            )
            .unwrap_err()
            .code,
            "ui_program_input_type_mismatch"
        );
        let mut document = compiler_document();
        document.resource_budget.max_nodes = 1;
        assert_eq!(
            compile_ui_program(
                &document,
                compiler_program_revision(),
                &compiler_schema(true)
            )
            .unwrap_err()
            .code,
            "ui_program_capacity_overflow"
        );
    }

    #[test]
    fn compiler_rejects_duplicate_keys_and_hashes_default_or_literal_changes() {
        let baseline = compile_ui_program(
            &compiler_document(),
            compiler_program_revision(),
            &compiler_schema(true),
        )
        .unwrap();
        let changed_default = compile_ui_program(
            &compiler_document(),
            compiler_program_revision(),
            &compiler_schema(false),
        )
        .unwrap();
        assert_ne!(baseline.layout_hash, changed_default.layout_hash);
        let mut changed_literal = compiler_document();
        changed_literal.root.text = Some(TextRef::Literal {
            value: "Resources".into(),
        });
        changed_literal.resource_budget.max_glyph_instances = 9;
        assert_ne!(
            baseline.layout_hash,
            compile_ui_program(
                &changed_literal,
                compiler_program_revision(),
                &compiler_schema(true)
            )
            .unwrap()
            .layout_hash
        );
        let mut duplicate = compiler_document();
        duplicate.root.children[0].children.push(UiNode {
            node_id: UiNodeId("root".into()),
            kind: UiNodeKind::Label,
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
            world_depth: None,
            children: Vec::new(),
        });
        assert_eq!(
            compile_ui_program(
                &duplicate,
                compiler_program_revision(),
                &compiler_schema(true)
            )
            .unwrap_err()
            .code,
            "ui_program_duplicate_node_key"
        );
        let mut invalid_schema = compiler_schema(true);
        invalid_schema.slots[0].default_value = UiInputValue::F32 { value: 1.0 };
        assert_eq!(
            compile_ui_program(
                &compiler_document(),
                compiler_program_revision(),
                &invalid_schema
            )
            .unwrap_err()
            .code,
            "ui_program_invalid_schema"
        );
    }

    #[test]
    fn repeat_input_rpc_applies_batched_instances_through_host_adapter() {
        use neon_ui_schema::{UiInputKind, UiTemplateDeclaration};

        // Build a program with a typed "nameplate" template whose row schema is
        // the shape a `NeonWorldUi<V>` instance submits: a stable key plus its
        // typed variable fields (health/level here).
        let mut document = compiler_document();
        document.root.children.push(UiNode {
            node_id: UiNodeId("nameplate".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 10.0,
            },
            layout: None,
            visible: false,
            enabled: false,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            world_depth: None,
            children: Vec::new(),
        });
        document.templates.push(UiTemplateDeclaration {
            template_key: "nameplate".into(),
            root_node_key: "nameplate".into(),
            max_instances: 4,
            row_schema: BTreeMap::from([
                ("row_key".into(), UiInputKind::U32),
                ("health".into(), UiInputKind::F32),
                ("level".into(), UiInputKind::U32),
            ]),
            instance_key_field: "row_key".into(),
            overflow_summary: false,
        });
        document.resource_budget.max_nodes = 3;
        document.resource_budget.max_instances = 4;

        let revision = compiler_program_revision();
        let program = compile_ui_program(&document, revision.clone(), &compiler_schema(true))
            .unwrap();
        assert_eq!(program.template_records.len(), 1);

        let mut runtime = UiRuntime::new(7, "repeat-input");
        runtime.host_adapter =
            Some(UiHostAdapter::activate(program.clone(), compiler_schema(true), 7).unwrap());

        let frame = UiRepeatFrame {
            template_key: "nameplate".into(),
            list_revision: Revision(1),
            rows: vec![
                UiRepeatRow {
                    stable_row_key: "npc.blacksmith".into(),
                    values: BTreeMap::from([
                        ("row_key".into(), UiInputValue::U32 { value: 1 }),
                        ("health".into(), UiInputValue::F32 { value: 82.0 }),
                        ("level".into(), UiInputValue::U32 { value: 12 }),
                    ]),
                    semantic_payload: BTreeMap::new(),
                },
                UiRepeatRow {
                    stable_row_key: "npc.merchant".into(),
                    values: BTreeMap::from([
                        ("row_key".into(), UiInputValue::U32 { value: 2 }),
                        ("health".into(), UiInputValue::F32 { value: 100.0 }),
                        ("level".into(), UiInputValue::U32 { value: 20 }),
                    ]),
                    semantic_payload: BTreeMap::new(),
                },
            ],
            expected_program_revision: revision,
        };

        let response = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("bevy-repeat-1".into()),
            client: runtime.client.clone(),
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.input.repeat".into(),
            params: json!(frame),
            expected_revision: None,
            idempotency_key: Some("bevy-repeat-1".into()),
        });

        assert_eq!(response.status, RpcStatus::Accepted);
        let result = response.result.expect("repeat input returns a result");
        assert_eq!(result["accepted_rows"], 2);
        assert_eq!(result["overflow_rows"], 0);
        assert!(
            runtime
                .host_adapter
                .as_ref()
                .unwrap()
                .repeat_frame("nameplate")
                .is_some()
        );

        // A stale list revision must be rejected instead of silently applied.
        let stale = UiRepeatFrame {
            template_key: "nameplate".into(),
            list_revision: Revision(1),
            rows: Vec::new(),
            expected_program_revision: program.revision.clone(),
        };
        let rejected = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("bevy-repeat-2".into()),
            client: runtime.client.clone(),
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.input.repeat".into(),
            params: json!(stale),
            expected_revision: None,
            idempotency_key: Some("bevy-repeat-2".into()),
        });
        assert_eq!(rejected.status, RpcStatus::Rejected);
        assert_eq!(
            rejected.error.expect("rejection carries an error").code,
            "ui_program_stale_input_revision"
        );

        // A row with a missing/wrong-typed field must be rejected.
        let malformed = UiRepeatFrame {
            template_key: "nameplate".into(),
            list_revision: Revision(2),
            rows: vec![UiRepeatRow {
                stable_row_key: "npc.guard".into(),
                values: BTreeMap::from([(
                    "row_key".into(),
                    UiInputValue::F32 { value: 3.0 },
                )]),
                semantic_payload: BTreeMap::new(),
            }],
            expected_program_revision: program.revision.clone(),
        };
        let rejected = runtime.handle_service_request(RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("bevy-repeat-3".into()),
            client: runtime.client.clone(),
            target: ServiceName(SERVICE_NAME.into()),
            method: "ui.input.repeat".into(),
            params: json!(malformed),
            expected_revision: None,
            idempotency_key: Some("bevy-repeat-3".into()),
        });
        assert_eq!(rejected.status, RpcStatus::Rejected);
        assert_eq!(
            rejected.error.expect("rejection carries an error").code,
            "ui_program_input_type_mismatch"
        );
    }
}
