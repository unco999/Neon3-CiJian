//! Headless UI declaration runtime. It must not create windows or GPU objects.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
};

use neon_ipc::{RpcClient, RpcServer, TransportError};
use neon_observability::{
    CommandJournal, CommandReceipt, CommandState, DebugSnapshot, JournalFilter, TraceLevel,
    TraceRecord, EVENT_COMMAND_ACCEPTED, EVENT_COMMAND_RECEIVED, EVENT_COMMAND_REJECTED,
};
use neon_protocol::{
    AiTerrainCondition, ClientIdentity, ClientKind, HealthStatus, ProtocolVersion, RequestId,
    Revision, RpcError, RpcRequest, RpcResponse, RpcStatus, ServiceDescription, ServiceHealth,
    ServiceName, PROTOCOL_VERSION,
};
use neon_ui_schema::{
    TextRef, UiBinding, UiBoundProperty, UiBounds, UiCommand, UiCpuFrameOutput, UiCpuNodeState,
    UiCpuRenderPrimitive, UiCpuSemanticTarget, UiCpuViewport, UiDependencyIndex, UiDiagnostic,
    UiDiagnosticSeverity, UiDiagnosticsState, UiEffect, UiFragment, UiFragmentId,
    UiFragmentSubmission, UiInputChange, UiInputFrame, UiInputSchema, UiInputUpdateClass,
    UiInputValue, UiInputValueSource, UiInspectorState, UiInspectorTab, UiIrDocument, UiNode,
    UiNodeId, UiNodeKind, UiProgram, UiProgramEventDeclaration, UiProgramLayoutRecord,
    UiProgramLiteralText, UiProgramNode, UiProgramResourceKind, UiProgramRevision,
    UiBranchPredicate, UiBranchRecord, UiTemplateRecord, UiRepeatFrame, UiDataGridCellTarget, UiDataGridDeclaration,
    UiDataGridFrame, UiDataGridRecord, UiDataGridWindowRequest,
    UiResolvedInputValue, UiResolvedInputs, UiResourceBudget, UiSchemaError, UiSemanticEvent, UiIntent,
    UiProgramSemanticEvent, UiProgramSemanticEventKind, UiProgramSemanticEventResult,
    UiProgramSemanticEventStatus, UiSemanticPayloadValue, UiEventTraceRecord,
    UiStyle, UiSurfaceEvent, UiSurfaceEventKind, UiSurfaceEventRequest, UiSurfaceId,
    UiSurfaceSnapshot, UiSurfaceState, UiTextHandle, UiTextHandleDiagnostic, UiTextHandleStatus,
    UiSemanticInteractionMetadata,
    UiTextRecord, UiTextRegistryDebugSnapshot, UiTextRegistryEntryMetadata, UiTextRegistrySnapshot,
    UiTextSourceCategory, UiTransition, UiTransitionState, ERROR_FRAGMENT_REVISION_STALE,
    ERROR_DATA_GRID_CELL_INVALID, ERROR_INPUT_SEQUENCE_STALE, ERROR_INTENT_NOT_BOUND, ERROR_RENDERER_EPOCH_MISMATCH,
    ERROR_UI_PROGRAM_DUPLICATE_INPUT_CHANGE, ERROR_UI_PROGRAM_INPUT_TYPE_MISMATCH,
    ERROR_UI_PROGRAM_INPUT_UPDATE_FORBIDDEN, ERROR_UI_PROGRAM_STALE_INPUT_REVISION,
    ERROR_UI_PROGRAM_TEXT_REGISTRY_CAPACITY_OVERFLOW,
    ERROR_UI_PROGRAM_TEXT_REGISTRY_GENERATION_MISMATCH,
    ERROR_UI_PROGRAM_TEXT_REGISTRY_STALE_REVISION, ERROR_UI_PROGRAM_TEXT_TOO_LONG,
    ERROR_UI_PROGRAM_UNKNOWN_INPUT_KEY, ERROR_UI_PROGRAM_UNKNOWN_TEXT_HANDLE,
    UI_SURFACE_SCHEMA_VERSION, ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE,
    ERROR_UI_PROGRAM_CAPACITY_OVERFLOW,
    ERROR_UI_PROGRAM_EVENT_CONTROL_UNAVAILABLE, ERROR_UI_PROGRAM_EVENT_DUPLICATE_IDEMPOTENCY_KEY,
    ERROR_UI_PROGRAM_EVENT_INTERACTION_EPOCH_MISMATCH, ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE,
    ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, ERROR_UI_PROGRAM_EVENT_STALE_REVISION,
};
use serde_json::{json, Value};

pub mod nui_flow;
pub mod nui_state_machine;
pub mod debug;
pub mod terrain_workbench;
pub mod demo_domain;
pub use nui_flow::{
    apply_nui_ir_patch, compile_nui_flow_program, format_nui_flow, lower_nui_flow, lower_nui_flow_effects, parse_nui_flow,
    parse_nui_flow_patch, NuiFlowError,
};
pub use nui_state_machine::{NuiFlowDragController, NuiFlowDragUpdate, NuiFlowDropResult, NuiFlowStateMachineRuntime, NuiFlowStateTransitionResult};

pub const SERVICE_NAME: &str = "ui-runtime";
pub const WORKBENCH_SURFACE_ID: &str = "surface.ui-workbench";
pub const AI_TERRAIN_SURFACE_ID: &str = "surface.ai.terrain-generator";

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
            if values
                .get(&key)
                .is_none_or(|current| current.value != value)
            {
                changed_slots.push(key.clone());
                self.dirty_slots.insert(key.clone());
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
            Some(entry) if entry.record.handle.generation == handle.generation => UiTextHandleDiagnostic { handle, status: UiTextHandleStatus::Ready, reference_count: entry.reference_count, resident: false, message: "text handle is registered".into() },
            Some(_) => UiTextHandleDiagnostic { handle, status: UiTextHandleStatus::GenerationMismatch, reference_count: 0, resident: false, message: "text handle generation is stale".into() },
            None if self.released_generations.contains_key(&handle.id) => UiTextHandleDiagnostic { handle, status: UiTextHandleStatus::Released, reference_count: 0, resident: false, message: "text handle has been released".into() },
            None => UiTextHandleDiagnostic { handle, status: UiTextHandleStatus::Missing, reference_count: 0, resident: false, message: "text handle is not registered".into() },
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
    fn default() -> Self { Self { revision: Revision(0), machine_states: BTreeMap::new(), drag_offsets: BTreeMap::new() } }
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
        Self { program, inputs, renderer_epoch, next_trace_sequence: 0, idempotent_results: HashMap::new(), trace: Vec::new() }
    }

    pub fn replace_resolved_inputs(&mut self, inputs: UiResolvedInputs) { self.inputs = inputs; }
    pub fn set_renderer_epoch(&mut self, renderer_epoch: u64) { self.renderer_epoch = renderer_epoch; }
    pub fn trace(&self) -> &[UiEventTraceRecord] { &self.trace }

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
        self.idempotent_results.insert(event.idempotency_key.clone(), result.clone());
        self.record(event, &result);
        result
    }

    fn validate_fresh(&self, event: &UiProgramSemanticEvent) -> Result<(), (&'static str, &'static str)> {
        if event.event_id.trim().is_empty() || event.request_id.trim().is_empty() || event.idempotency_key.trim().is_empty() || event.interaction.interaction_id.trim().is_empty() {
            return Err((ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE, "event identity and interaction metadata are required"));
        }
        if event.program_revision != self.program.revision || event.input_revision != self.inputs.input_revision {
            return Err((ERROR_UI_PROGRAM_EVENT_STALE_REVISION, "program or input revision is stale"));
        }
        if event.interaction.renderer_epoch != self.renderer_epoch {
            return Err((ERROR_UI_PROGRAM_EVENT_INTERACTION_EPOCH_MISMATCH, "renderer epoch does not match the active event gate"));
        }
        let node = self.program.nodes.iter().find(|node| node.key == event.source_node_key)
            .ok_or((ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE, "event source node is not declared by this program"))?;
        let declaration = self.program.event_records.iter().find(|declaration| declaration.node_key == node.key && declaration.intent == event.intent)
            .ok_or((ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE, "event intent is not declared by the source node"))?;
        let state = evaluate_ui_program(&self.program, &self.inputs, UiCpuViewport { logical_bounds: UiBounds { x: 0.0, y: 0.0, width: f32::MAX, height: f32::MAX }, revision: Revision(0) }, &UiLocalPresentationState::default())
            .nodes.into_iter().find(|state| state.node_key == event.source_node_key)
            .ok_or((ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE, "event source has no evaluated state"))?;
        if !state.visible || !state.enabled {
            return Err((ERROR_UI_PROGRAM_EVENT_CONTROL_UNAVAILABLE, "event source is hidden or disabled"));
        }
        if matches!(event.kind, UiProgramSemanticEventKind::TextEditCommit) && !event.payload.values().any(|value| matches!(value, UiSemanticPayloadValue::TextHandle { .. })) {
            return Err((ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, "text edit commits require a bounded text handle payload"));
        }
        let mut expected = declaration.literal_payload.clone();
        for key in &declaration.bound_input_keys {
            let value = self.inputs.values.get(key).ok_or((ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, "event references an absent bound input"))?;
            expected.insert(key.clone(), input_value_as_event_payload(&value.value).ok_or((ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, "bound input kind cannot cross the semantic event boundary"))?);
        }
        if event.payload != expected {
            return Err((ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, "event payload differs from declared literals or resolved bound inputs"));
        }
        if let Some(requested) = &event.requested_value {
            let Some(key) = declaration.bound_input_keys.first() else {
                return Err((ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, "requested values require a bound input"));
            };
            let Some(current) = expected.get(key) else {
                return Err((ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, "requested value input is absent"));
            };
            if !same_payload_kind(current, requested) {
                return Err((ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED, "requested value does not match the bound input kind"));
            }
        }
        Ok(())
    }

    fn record(&mut self, event: &UiProgramSemanticEvent, result: &UiProgramSemanticEventResult) {
        self.next_trace_sequence += 1;
        self.trace.push(UiEventTraceRecord { sequence: self.next_trace_sequence, event_id: event.event_id.clone(), intent: event.intent.clone(), source_node_key: event.source_node_key.clone(), program_revision: event.program_revision.revision, input_revision: event.input_revision, renderer_epoch: event.interaction.renderer_epoch, result: result.status, code: result.code.clone(), timestamp_unix_ms: 0 });
    }
}

fn input_value_as_event_payload(value: &UiInputValue) -> Option<UiSemanticPayloadValue> {
    Some(match value {
        UiInputValue::Bool { value } => UiSemanticPayloadValue::Bool { value: *value },
        UiInputValue::I32 { value } => UiSemanticPayloadValue::I32 { value: *value },
        UiInputValue::U32 { value } => UiSemanticPayloadValue::U32 { value: *value },
        UiInputValue::F32 { value } => UiSemanticPayloadValue::F32 { value: *value },
        UiInputValue::Enum { value } => UiSemanticPayloadValue::Enum { value: value.clone() },
        UiInputValue::TextHandle { value } => UiSemanticPayloadValue::TextHandle { value: *value },
        UiInputValue::AssetHandle { id, generation } => UiSemanticPayloadValue::AssetHandle { id: *id, generation: *generation },
        UiInputValue::Vec2 { .. } | UiInputValue::Vec4 { .. } | UiInputValue::Color { .. } => return None,
    })
}

fn apply_program_visibility(
    node: &mut UiNode,
    visibility: &std::collections::BTreeMap<String, bool>,
) {
    node.visible = visibility.get(&node.node_id.0).copied().unwrap_or(false);
    for child in &mut node.children {
        apply_program_visibility(child, visibility);
    }
}

fn same_payload_kind(left: &UiSemanticPayloadValue, right: &UiSemanticPayloadValue) -> bool {
    matches!(
        (left, right),
        (UiSemanticPayloadValue::Bool { .. }, UiSemanticPayloadValue::Bool { .. })
            | (UiSemanticPayloadValue::I32 { .. }, UiSemanticPayloadValue::I32 { .. })
            | (UiSemanticPayloadValue::U32 { .. }, UiSemanticPayloadValue::U32 { .. })
            | (UiSemanticPayloadValue::F32 { .. }, UiSemanticPayloadValue::F32 { .. })
            | (UiSemanticPayloadValue::Enum { .. }, UiSemanticPayloadValue::Enum { .. })
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
pub struct UiDataGridApplyResult { pub accepted_rows: u32 }

impl UiDataGridStore {
    pub fn frame(&self, data_grid_key: &str) -> Option<&UiDataGridFrame> { self.frames.get(data_grid_key) }

    /// Attaches the current bounded grid windows to a fragment produced from the
    /// same compiled program. Frames are presentation data, not fragment topology.
    pub fn attach_to_fragment(&self, program: &UiProgram, fragment: &mut UiFragment) -> Result<(), UiProgramCompileError> {
        let mut effects = fragment.effects.iter().filter(|effect| !matches!(effect, UiEffect::DataGridFrame { .. })).cloned().collect::<Vec<_>>();
        for grid in &program.data_grid_records {
            let Some(frame) = self.frame(&grid.node_key) else { continue; };
            if frame.expected_program_revision != program.revision {
                return Err(compile_error(ERROR_UI_PROGRAM_STALE_INPUT_REVISION, "DataGrid frame belongs to a different program revision"));
            }
            effects.push(UiEffect::DataGridFrame {
                declaration: UiDataGridDeclaration {
                    node_key: grid.node_key.clone(), max_window_rows: grid.max_window_rows,
                    row_height: grid.row_height, overscan: grid.overscan, columns: grid.columns.clone(),
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

    pub fn apply(&mut self, program: &UiProgram, frame: UiDataGridFrame) -> Result<UiDataGridApplyResult, UiProgramCompileError> {
        if frame.expected_program_revision != program.revision {
            return Err(compile_error(ERROR_UI_PROGRAM_STALE_INPUT_REVISION, "DataGrid frame belongs to a different program revision"));
        }
        let grid = program.data_grid_records.iter().find(|record| record.node_key == frame.data_grid_key)
            .ok_or_else(|| compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "DataGrid frame references an unknown grid"))?;
        if let Some(previous) = self.frames.get(&frame.data_grid_key)
            && frame.list_revision.0 < previous.list_revision.0 {
            return Err(compile_error(ERROR_UI_PROGRAM_STALE_INPUT_REVISION, "DataGrid frame revision is stale"));
        }
        let row_count = u64::try_from(frame.window_rows.len()).map_err(|_| compile_error(ERROR_UI_PROGRAM_CAPACITY_OVERFLOW, "DataGrid window row count overflows u64"))?;
        if row_count > u64::from(grid.max_window_rows) || frame.first_row > frame.total_rows
            || row_count > frame.total_rows - frame.first_row {
            return Err(compile_error(ERROR_UI_PROGRAM_CAPACITY_OVERFLOW, "DataGrid window exceeds its declared bounds"));
        }
        let mut keys = HashSet::new();
        if frame.window_rows.iter().any(|row| row.stable_row_key.trim().is_empty() || !keys.insert(&row.stable_row_key)) {
            return Err(compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "DataGrid window rows require unique nonempty stable row keys"));
        }
        let column_keys = grid.columns.iter().map(|column| column.key.as_str()).collect::<HashSet<_>>();
        if frame.window_rows.iter().any(|row| {
            row.cells.len() != column_keys.len()
                || row.cells.keys().any(|key| !column_keys.contains(key.as_str()))
                || row.cells.values().any(|cell| !cell.validate())
        }) {
            return Err(compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "DataGrid rows must contain one valid typed/display cell for every declared column"));
        }
        let accepted_rows = frame.window_rows.len() as u32;
        self.frames.insert(frame.data_grid_key.clone(), frame);
        Ok(UiDataGridApplyResult { accepted_rows })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiRepeatApplyResult {
    pub accepted_rows: u32,
    pub overflow_rows: u32,
    pub diagnostics: Vec<UiDiagnostic>,
}

impl UiRepeatStore {
    pub fn frame(&self, template_key: &str) -> Option<&UiRepeatFrame> { self.frames.get(template_key) }

    pub fn apply(
        &mut self,
        program: &UiProgram,
        frame: UiRepeatFrame,
    ) -> Result<UiRepeatApplyResult, UiProgramCompileError> {
        if frame.expected_program_revision != program.revision {
            return Err(compile_error(ERROR_UI_PROGRAM_STALE_INPUT_REVISION, "repeat frame belongs to a different program revision"));
        }
        let template = program.template_records.iter().find(|record| record.template_key == frame.template_key)
            .ok_or_else(|| compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "repeat frame references an unknown template"))?;
        if let Some(previous) = self.frames.get(&frame.template_key) {
            if frame.list_revision.0 <= previous.list_revision.0 {
                return Err(compile_error(ERROR_UI_PROGRAM_STALE_INPUT_REVISION, "repeat frame revision is stale"));
            }
        }
        let mut keys = HashSet::new();
        for row in &frame.rows {
            if row.stable_row_key.trim().is_empty() || !keys.insert(&row.stable_row_key) {
                return Err(compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "repeat rows require unique nonempty stable row keys"));
            }
            if row.values.len() != template.row_schema.len() || template.row_schema.iter().any(|(key, kind)| !row.values.get(key).is_some_and(|value| kind.accepts(value))) {
                return Err(compile_error(ERROR_UI_PROGRAM_INPUT_TYPE_MISMATCH, "repeat row values do not match the declared template row schema"));
            }
        }
        let overflow_rows = frame.rows.len().saturating_sub(template.max_instances as usize) as u32;
        if overflow_rows != 0 && !template.overflow_summary {
            return Err(compile_error(ERROR_UI_PROGRAM_CAPACITY_OVERFLOW, "repeat rows exceed capacity and this template declares no overflow summary"));
        }
        let accepted_rows = frame.rows.len().min(template.max_instances as usize) as u32;
        let mut accepted = frame;
        accepted.rows.truncate(accepted_rows as usize);
        self.frames.insert(accepted.template_key.clone(), accepted);
        let diagnostics = if overflow_rows == 0 { Vec::new() } else { vec![cpu_diagnostic(
                ERROR_UI_PROGRAM_CAPACITY_OVERFLOW,
                "repeat rows exceed capacity; the declared overflow summary must be rendered",
            Some(&template.template_key), None, program.revision.revision,
        )] };
        Ok(UiRepeatApplyResult { accepted_rows, overflow_rows, diagnostics })
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
        .filter(|node| node.layout.as_ref().is_some_and(|layout| layout.clip != neon_ui_schema::UiClipPolicy::None))
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
    let data_grid_records = compile_data_grid_records(document, &nodes)?;
    let template_instances = template_records.iter().try_fold(0u32, |total, template| {
            let count = template.node_range.len() as u32;
        total.checked_add(count.saturating_mul(template.max_instances)).ok_or(())
    }).map_err(|_| compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "template instance count overflows the declared resource budget"))?;
    if template_instances > document.resource_budget.max_instances {
        return Err(compile_error(ERROR_UI_PROGRAM_CAPACITY_OVERFLOW, "preallocated template instances exceed the declared program budget"));
    }
    for node in &templates {
        if matches!(node.kind, UiNodeKind::Image | UiNodeKind::RenderSurface) {
            let kind = if node.kind == UiNodeKind::Image {
                UiProgramResourceKind::Image
            } else {
                UiProgramResourceKind::RenderSurface
            };
            if !document.resources.iter().any(|resource| {
                resource.key == node.node_id.0 && (resource.kind == kind || resource.has_fallback)
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
        for key in event.literal_payload.keys().chain(event.bound_input_keys.iter()) {
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
        event_records: document.events.clone(),
        resource_budget: document.resource_budget.clone(),
        dependency_index: dependencies,
        layout_hash,
    })
}

fn compile_data_grid_records(document: &UiIrDocument, nodes: &[neon_ui_schema::UiProgramNode]) -> Result<Vec<UiDataGridRecord>, UiProgramCompileError> {
    document.data_grids.iter().map(|grid| {
        if grid.max_window_rows == 0 || grid.row_height == 0 || grid.overscan > grid.max_window_rows || grid.columns.is_empty()
            || grid.columns.iter().any(|column| !column.validate())
            || grid.columns.iter().map(|column| &column.key).collect::<HashSet<_>>().len() != grid.columns.len()
            || !nodes.iter().any(|node| node.key == grid.node_key && node.kind == UiNodeKind::DataGrid) {
            Err(compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "DataGrid declaration must target a DataGrid node with positive bounded metrics and columns"))
        } else {
            Ok(UiDataGridRecord {
                node_key: grid.node_key.clone(), max_window_rows: grid.max_window_rows,
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
                if let Some(state) = states.get_mut(node_key) { state.visible = false; }
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

fn cumulative_drag_offset(node_key: &str, nodes: &[UiProgramNode], offsets: &BTreeMap<String, [f32; 2]>) -> [f32; 2] {
    let mut key = Some(node_key);
    let mut total = [0.0; 2];
    while let Some(current) = key {
        if let Some(offset) = offsets.get(current) { total[0] += offset[0]; total[1] += offset[1]; }
        key = nodes.iter().find(|node| node.key == current).and_then(|node| node.parent_key.as_deref());
    }
    total
}

fn branch_predicate_matches(predicate: &UiBranchPredicate, inputs: &UiResolvedInputs, local: &UiLocalPresentationState) -> bool {
    match predicate {
        UiBranchPredicate::Bool { input_key, expected } => matches!(inputs.values.get(input_key).map(|value| &value.value), Some(UiInputValue::Bool { value }) if value == expected),
        UiBranchPredicate::EnumEquals { input_key, variant } => matches!(inputs.values.get(input_key).map(|value| &value.value), Some(UiInputValue::Enum { value }) if value == variant),
        UiBranchPredicate::MachineState { machine_key, state } => local.machine_states.get(machine_key).is_some_and(|active| active == state),
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
    document.templates.iter().map(|template| {
        if template.row_schema.values().any(|kind| matches!(kind, neon_ui_schema::UiInputKind::AssetHandle)) {
            return Err(compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "template row schema cannot contain renderer resource handles"));
            }
            let node_range = subtree_keys(nodes, &template.root_node_key);
        if node_range.is_empty() { return Err(compile_error(ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE, "template root must identify a compiled subtree")); }
        Ok(UiTemplateRecord { template_key: template.template_key.clone(), node_range, max_instances: template.max_instances, row_schema: template.row_schema.clone(), instance_key_field: template.instance_key_field.clone(), overflow_summary: template.overflow_summary })
    }).collect()
}

fn subtree_keys(nodes: &[UiProgramNode], root: &str) -> Vec<String> {
    let mut result = vec![root.to_owned()];
    let mut index = 0;
    while index < result.len() {
        let parent = result[index].clone();
        result.extend(nodes.iter().filter(|node| node.parent_key.as_deref() == Some(parent.as_str())).map(|node| node.key.clone()));
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
            matches!(kind, I32 | U32 | F32 | I32Range { .. } | U32Range { .. })
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
        (UiBoundProperty::NumericValue, UiInputValue::I32 { value }) => state.numeric_value = Some(*value as f32),
        (UiBoundProperty::NumericValue, UiInputValue::U32 { value }) => state.numeric_value = Some(*value as f32),
        (UiBoundProperty::NumericValue, UiInputValue::F32 { value }) => state.numeric_value = Some(*value),
        (UiBoundProperty::StateToken, UiInputValue::Enum { value }) => state.state_token = Some(value.clone()),
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
    last_data_grid_sequence: HashMap<String, u64>,
    idempotent_responses: HashMap<String, RpcResponse>,
    surface: UiSurfaceMachine,
    ai_terrain: AiTerrainPanelState,
    showcase_text: String,
    gallery: Option<GalleryForwarder>,
}

/// Live program-domain state mirror for the component gallery. The domain
/// controller process remains the authority; this mirror only tracks the
/// resolved inputs so bound payloads and revisions stay consistent between
/// renderer clicks and domain acceptance.
struct GalleryForwarder {
    program: UiProgram,
    inputs: UiResolvedInputs,
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
            last_data_grid_sequence: HashMap::new(),
            idempotent_responses: HashMap::new(),
            surface: UiSurfaceMachine::new(UiSurfaceId(WORKBENCH_SURFACE_ID.into())),
            ai_terrain: AiTerrainPanelState::default(),
            showcase_text: String::new(),
            gallery: None,
        }
    }

    pub fn service_health(&self) -> ServiceHealth {
        ServiceHealth {
            service: ServiceName(SERVICE_NAME.into()),
            status: HealthStatus::Healthy,
            epoch: self.epoch,
        }
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
                "ui.data_grid.window.v1".into(),
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
            "debug.command.get" => return self.handle_debug_command(request),
            "debug.trace.query" => return self.handle_debug_trace(request),
            "ui.fragment.submit" => return self.handle_fragment_submit(request),
            "ui.surface.snapshot.get" => Some(self.surface_value()),
            "ui.ai.terrain.snapshot.get" => Some(self.ai_terrain.snapshot()),
            "ui.surface.event" => return self.handle_surface_event(request),
            "ui.input.event" => return self.handle_input_event(request),
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

    /// Runs the UI declaration control plane. A React client can only submit to this
    /// service; the service forwards a validated declaration to the sole renderer.
    /// When `program_domain` is set, renderer pointer/selection/value events whose
    /// intent is declared by the component gallery program are routed through the
    /// program domain endpoint and their accepted snapshots republished to the
    /// renderer as revisioned fragments.
    pub fn serve_forwarder(
        endpoint: SocketAddr,
        wgpu_endpoint: SocketAddr,
        domain_endpoint: SocketAddr,
        epoch: u64,
        program_domain: bool,
    ) -> Result<(), TransportError> {
        let server = RpcServer::bind(endpoint)?;
        let mut runtime = Self::new(epoch, "ui-runtime-forwarder");
        runtime.gallery = if program_domain {
            let (document, program) = demo_domain::component_gallery_program()
                .map_err(|error| TransportError::Io(std::io::Error::other(error)))?;
            let inputs = UiInputStore::activate(program.revision.clone(), document.input_schema)
                .map_err(|error| TransportError::Io(std::io::Error::other(error.message)))?
                .snapshot();
            Some(GalleryForwarder { program, inputs })
        } else {
            None
        };
        server.serve_until(|request| {
            let shutdown = request.method == "service.shutdown";
            let request_id = request.request_id.clone();
            let response = if request.method == "ui.fragment.submit" {
                runtime
                    .forward_fragment(wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else if request.method == "ui.data_grid.window.request" {
                runtime
                    .forward_data_grid_window_request(domain_endpoint, wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else if request.method == "ui.input.event" && renderer_event_targets_wgpu(&request) {
                runtime
                    .forward_wgpu_event(wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else if request.method == "ui.input.event" && renderer_event_targets_domain(&request) {
                runtime
                    .forward_drag_drop_event(domain_endpoint, wgpu_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else if request.method == "ui.input.event" && renderer_event_targets_data_grid(&request) {
                runtime
                    .forward_data_grid_cell_event(domain_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else if request.method == "ui.input.event" && runtime.gallery.is_some() {
                runtime
                    .forward_gallery_program_event(wgpu_endpoint, domain_endpoint, request)
                    .unwrap_or_else(|error| {
                        runtime.rejected(request_id, "service_unavailable", &error.to_string())
                    })
            } else {
                runtime.handle_service_request(request)
            };
            (response, !shutdown)
        })
    }

    /// Applies a domain-owned replacement frame to the cached presentation
    /// fragment. The request contains only revisioned DataGrid identity, never
    /// renderer-local pointer or hit-test data.
    fn forward_data_grid_window_request(
        &mut self,
        domain_endpoint: SocketAddr,
        wgpu_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let response_request_id = request.request_id.clone();
        let window_request: UiDataGridWindowRequest = serde_json::from_value(request.params.clone())
            .map_err(|_| TransportError::Io(std::io::Error::other("invalid DataGrid window request")))?;
        if window_request.renderer_epoch != self.epoch {
            return Ok(self.rejected(response_request_id, ERROR_RENDERER_EPOCH_MISMATCH, "renderer epoch is stale"));
        }
        let fragment = self.cached_fragment.clone().ok_or_else(|| {
            TransportError::Io(std::io::Error::other("no UI fragment has been submitted"))
        })?;
        if fragment.fragment_id != window_request.fragment.id || fragment.revision != window_request.fragment.revision {
            return Ok(self.rejected(response_request_id, ERROR_FRAGMENT_REVISION_STALE, "DataGrid fragment revision is stale"));
        }
        let sequence_key = format!("{}/{}", window_request.fragment.id.0, window_request.data_grid_key);
        if self.last_data_grid_sequence.get(&sequence_key).is_some_and(|last| window_request.sequence <= *last) {
            return Ok(self.rejected(response_request_id, ERROR_INPUT_SEQUENCE_STALE, "DataGrid request sequence is stale"));
        }
        let current = fragment.effects.iter().find_map(|effect| match effect {
            UiEffect::DataGridFrame { declaration, frame }
                if declaration.node_key == window_request.data_grid_key => Some((declaration.clone(), frame.clone())),
            _ => None,
        }).ok_or_else(|| TransportError::Io(std::io::Error::other("DataGrid is not declared by the submitted fragment")))?;
        if current.1.list_revision != window_request.expected_list_revision
            || window_request.max_window_rows != current.0.max_window_rows {
            return Ok(self.rejected(response_request_id, "revision_conflict", "DataGrid list revision or window capacity is stale"));
        }
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION,
            request_id: RequestId(format!("{}-domain", response_request_id.0)),
            client: self.client.clone(), target: ServiceName("demo-domain".into()),
            method: "ui.data_grid.window.request".into(), params: json!(window_request),
            expected_revision: Some(current.1.list_revision),
            idempotency_key: Some(format!("data-grid-window:{}:{}", sequence_key, window_request.sequence)),
        };
        let domain_response = RpcClient::connect(domain_endpoint)?.call(&forwarded)?;
        if domain_response.status != RpcStatus::Accepted {
            return Ok(domain_response);
        }
        let frame: UiDataGridFrame = domain_response.result.clone()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| TransportError::Io(std::io::Error::other("DataGrid domain accepted without a frame")))?;
        if frame.data_grid_key != current.0.node_key
            || frame.expected_program_revision != current.1.expected_program_revision
            || frame.window_rows.len() > current.0.max_window_rows as usize
            || frame.first_row > frame.total_rows
            || frame.window_rows.len() as u64 > frame.total_rows - frame.first_row {
            return Ok(self.rejected(response_request_id, "invalid_data_grid_frame", "domain returned an invalid DataGrid frame"));
        }
        let mut updated = fragment.clone();
        updated.revision = Revision(updated.revision.0 + 1);
        for effect in &mut updated.effects {
            if let UiEffect::DataGridFrame { declaration, frame: target } = effect
                && declaration.node_key == window_request.data_grid_key {
                *target = frame.clone();
            }
        }
        if updated.validate().is_err() {
            return Ok(self.rejected(response_request_id, "invalid_data_grid_frame", "domain returned an invalid DataGrid frame"));
        }
        let submit = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION,
            request_id: RequestId(format!("{}-fragment", response_request_id.0)),
            client: self.client.clone(), target: ServiceName(SERVICE_NAME.into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(updated) }),
            expected_revision: Some(fragment.revision),
            idempotency_key: Some(format!("data-grid-frame:{}:{}", sequence_key, window_request.sequence)),
        };
        let mut response = self.forward_fragment(wgpu_endpoint, submit)?;
        if response.status == RpcStatus::Accepted {
            self.last_data_grid_sequence.insert(sequence_key, window_request.sequence);
        }
        response.request_id = response_request_id;
        Ok(response)
    }

    /// Routes a renderer-resolved component gallery event through the program
    /// domain and republishes the accepted input snapshot to the renderer.
    fn forward_gallery_program_event(
        &mut self,
        wgpu_endpoint: SocketAddr,
        domain_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let request_id = request.request_id.clone();
        let event: UiSemanticEvent = serde_json::from_value(request.params.clone())
            .map_err(|_| TransportError::Io(std::io::Error::other("invalid UI semantic event")))?;
        self.validate_semantic_event(&event)
            .map_err(|code| TransportError::Io(std::io::Error::other(code)))?;
        let (program_event, domain_key) = {
            let gallery = self.gallery.as_mut().ok_or_else(|| {
                TransportError::Io(std::io::Error::other("gallery program domain is unavailable"))
            })?;
            let neon_ui_schema::UiIntent::Invoke { action, .. } = event.intent.clone();
            let Some(declaration) = gallery
                .program
                .event_records
                .iter()
                .find(|declaration| declaration.intent == action)
            else {
                return Err(TransportError::Io(std::io::Error::other(
                    "event intent is not declared by the gallery program",
                )));
            };
            let mut payload = declaration.literal_payload.clone();
            for key in &declaration.bound_input_keys {
                let Some(value) = gallery.inputs.values.get(key) else {
                    return Err(TransportError::Io(std::io::Error::other(
                        "event bound input is absent from the gallery domain snapshot",
                    )));
                };
                let Some(payload_value) = input_value_as_event_payload(&value.value) else {
                    return Err(TransportError::Io(std::io::Error::other(
                        "bound input kind cannot cross the program event boundary",
                    )));
                };
                payload.insert(key.clone(), payload_value);
            }
            let sequence = event
                .pointer
                .as_ref()
                .map(|pointer| pointer.sequence)
                .unwrap_or(1);
            let domain_key = request
                .idempotency_key
                .clone()
                .unwrap_or_else(|| format!("gallery-live:{}", event.event_id));
            let program_event = UiProgramSemanticEvent {
                event_id: format!("gallery-live:{}", event.event_id),
                kind: demo_domain::gallery_event_kind(&declaration.node_key),
                intent: action,
                source_node_key: declaration.node_key.clone(),
                payload,
                program_revision: gallery.program.revision.clone(),
                input_revision: gallery.inputs.input_revision,
                request_id: event.event_id.clone(),
                idempotency_key: domain_key.clone(),
                requested_value: event.control_value.clone(),
                interaction: UiSemanticInteractionMetadata {
                    interaction_id: event.event_id.clone(),
                    sequence,
                    renderer_epoch: event.renderer_epoch,
                },
            };
            (program_event, domain_key)
        };
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION,
            request_id: RequestId(format!("{}-domain", request_id.0)),
            client: self.client.clone(), target: ServiceName("demo-domain".into()),
            method: "ui.program.event".into(), params: json!(program_event),
            expected_revision: Some(program_event.input_revision),
            idempotency_key: Some(program_event.idempotency_key.clone()),
        };
        let domain_response = RpcClient::connect(domain_endpoint)?.call(&forwarded)?;
        if domain_response.status != RpcStatus::Accepted {
            return Ok(domain_response);
        }
        let snapshot: demo_domain::DemoInputDomainSnapshot = domain_response
            .result
            .as_ref()
            .and_then(|result| result.get("snapshot"))
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| {
                TransportError::Io(std::io::Error::other(
                    "gallery domain accepted without a snapshot",
                ))
            })?;
        self.gallery
            .as_mut()
            .map(|gallery| gallery.inputs = snapshot.inputs.clone());
        let Some(fragment) = self.cached_fragment.clone() else {
            return Err(TransportError::Io(std::io::Error::other(
                "no UI fragment has been submitted",
            )));
        };
        let mut updated = fragment.clone();
        updated.revision = Revision(updated.revision.0 + 1);
        if let Some(gallery) = self.gallery.as_ref() {
            let frame = evaluate_ui_program(
                &gallery.program,
                &snapshot.inputs,
                UiCpuViewport {
                    logical_bounds: UiBounds { x: 0.0, y: 0.0, width: 1280.0, height: 800.0 },
                    revision: updated.revision,
                },
                &UiLocalPresentationState::default(),
            );
            let visibility = frame.nodes.into_iter()
                .map(|node| (node.node_key, node.visible))
                .collect::<std::collections::BTreeMap<_, _>>();
            apply_program_visibility(&mut updated.root, &visibility);
        }
        demo_domain::apply_visible_status_to_fragment(&mut updated, &snapshot);
        let submit = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION,
            request_id: RequestId(format!("{}-fragment", request_id.0)),
            client: self.client.clone(), target: ServiceName(SERVICE_NAME.into()),
            method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(updated) }),
            expected_revision: Some(fragment.revision),
            idempotency_key: Some(format!("gallery-live-fragment:{}", domain_key)),
        };
        let mut response = self.forward_fragment(wgpu_endpoint, submit)?;
        response.request_id = request_id;
        Ok(response)
    }

    fn forward_drag_drop_event(
        &mut self,
        domain_endpoint: SocketAddr,
        wgpu_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let response_request_id = request.request_id.clone();
        let event: UiSemanticEvent = serde_json::from_value(request.params.clone())
            .map_err(|_| TransportError::Io(std::io::Error::other("invalid UI semantic event")))?;
        self.validate_semantic_event(&event)
            .map_err(|code| TransportError::Io(std::io::Error::other(code)))?;
        let fragment = self.cached_fragment.clone().expect("validated semantic event has a fragment");
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: request.request_id.clone(),
            client: self.client.clone(), target: ServiceName("demo-domain".into()), method: "ui.drag_drop.apply".into(),
            params: json!({"event": event, "fragment": fragment}), expected_revision: Some(event.fragment.revision), idempotency_key: request.idempotency_key.clone(),
        };
        let domain_response = RpcClient::connect(domain_endpoint)?.call(&forwarded)?;
        if domain_response.status != RpcStatus::Accepted { return Ok(domain_response); }
        let fragment: UiFragment = domain_response.result.as_ref().and_then(|result| result.get("fragment")).cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or_else(|| TransportError::Io(std::io::Error::other("domain accepted without a UI fragment")))?;
        let submit = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId(format!("{}-fragment", request.request_id.0)),
            client: self.client.clone(), target: ServiceName(SERVICE_NAME.into()), method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment) }), expected_revision: Some(event.fragment.revision),
            idempotency_key: Some(format!("domain-fragment:{}", event.event_id)),
        };
        let mut response = self.forward_fragment(wgpu_endpoint, submit)?;
        response.request_id = response_request_id;
        Ok(response)
    }

    /// Forwards only a UI-runtime validated cell intent. The target and value are
    /// semantic schema values; renderer hit IDs, paths, and coordinates never leave WGPU.
    fn forward_data_grid_cell_event(
        &mut self,
        domain_endpoint: SocketAddr,
        request: RpcRequest,
    ) -> Result<RpcResponse, TransportError> {
        let response_request_id = request.request_id.clone();
        let event: UiSemanticEvent = serde_json::from_value(request.params.clone())
            .map_err(|_| TransportError::Io(std::io::Error::other("invalid UI semantic event")))?;
        self.validate_semantic_event(&event)
            .map_err(|code| TransportError::Io(std::io::Error::other(code)))?;
        let target = event.data_grid_cell.clone().ok_or_else(|| {
            TransportError::Io(std::io::Error::other(ERROR_DATA_GRID_CELL_INVALID))
        })?;
        let neon_ui_schema::UiIntent::Invoke { action, mut params } = event.intent.clone();
        let Some(object) = params.as_object_mut() else {
            return Ok(self.rejected(
                response_request_id,
                ERROR_DATA_GRID_CELL_INVALID,
                "DataGrid intents require object parameters",
            ));
        };
        object.insert("data_grid_cell".into(), json!(target));
        if let Some(value) = event.control_value {
            object.insert("control_value".into(), json!(value));
        }
        if let Some(text) = event.text {
            object.insert("text".into(), json!(text));
        }
        let forwarded = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION,
            request_id: RequestId(format!("{}-domain", response_request_id.0)),
            client: self.client.clone(), target: ServiceName("demo-domain".into()), method: action,
            params, expected_revision: Some(event.fragment.revision),
            idempotency_key: request.idempotency_key,
        };
        let mut response = RpcClient::connect(domain_endpoint)?.call(&forwarded)?;
        response.request_id = response_request_id;
        Ok(response)
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
                )
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
                        return self.rejected(request.request_id, code, message)
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

    fn handle_intent_dispatch(&mut self, mut request: RpcRequest) -> RpcResponse {
        let intent: neon_ui_schema::UiIntent = match serde_json::from_value(request.params.clone())
        {
            Ok(intent) => intent,
            Err(_) => {
                return self.rejected(request.request_id, "invalid_request", "invalid UI intent")
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
        if event.event == neon_ui_schema::UiSemanticEventType::DragDrop && event.drag_drop.is_none() {
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

fn renderer_event_targets_domain(request: &RpcRequest) -> bool {
    serde_json::from_value::<UiSemanticEvent>(request.params.clone())
        .ok()
        .is_some_and(|event| event.event == neon_ui_schema::UiSemanticEventType::DragDrop)
}

fn renderer_event_targets_data_grid(request: &RpcRequest) -> bool {
    serde_json::from_value::<UiSemanticEvent>(request.params.clone())
        .ok()
        .is_some_and(|event| event.data_grid_cell.is_some())
}

fn validate_data_grid_cell_event(
    fragment: &UiFragment,
    event: &UiSemanticEvent,
    target: &UiDataGridCellTarget,
) -> Result<(), &'static str> {
    if target.data_grid_key.trim().is_empty()
        || target.stable_row_key.trim().is_empty()
        || target.column_key.trim().is_empty()
        || event.drag_drop.is_some()
    {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    }
    let Some((declaration, frame)) = fragment.effects.iter().find_map(|effect| match effect {
        UiEffect::DataGridFrame { declaration, frame }
            if declaration.node_key == target.data_grid_key && frame.data_grid_key == target.data_grid_key => {
                Some((declaration, frame))
            }
        _ => None,
    }) else {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    };
    let Some(column) = declaration.columns.iter().find(|column| column.key == target.column_key) else {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    };
    let Some(cell) = frame.window_rows.iter()
        .find(|row| row.stable_row_key == target.stable_row_key)
        .and_then(|row| row.cells.get(&target.column_key))
    else {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    };
    let (intent, effective) = match (&column.presentation, cell.presentation_override.as_ref()) {
        (neon_ui_schema::UiDataGridPresentation::Text, _) => return Err(ERROR_DATA_GRID_CELL_INVALID),
        (neon_ui_schema::UiDataGridPresentation::Select { intent }, None) => (intent, None),
        (neon_ui_schema::UiDataGridPresentation::Dropdown { intent, .. }, None)
        | (neon_ui_schema::UiDataGridPresentation::Edit { intent, .. }, None)
        | (neon_ui_schema::UiDataGridPresentation::Select { intent }, Some(_))
        | (neon_ui_schema::UiDataGridPresentation::Dropdown { intent, .. }, Some(_))
        | (neon_ui_schema::UiDataGridPresentation::Edit { intent, .. }, Some(_)) => (intent, cell.presentation_override.as_ref()),
    };
    let UiIntent::Invoke { action, params } = &event.intent;
    if action != intent || params.as_object().is_none_or(|params| !params.is_empty()) {
        return Err(ERROR_DATA_GRID_CELL_INVALID);
    }
    match effective {
        None => match &column.presentation {
            neon_ui_schema::UiDataGridPresentation::Select { .. } => match (&cell.value, &event.control_value) {
                (UiInputValue::Bool { value: current }, Some(UiSemanticPayloadValue::Bool { value }))
                    if event.event == neon_ui_schema::UiSemanticEventType::SelectionChanged
                        && *value == !*current
                        && event.text.is_none() => Ok(()),
                _ => Err(ERROR_DATA_GRID_CELL_INVALID),
            },
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
                && event.text.is_none() => Ok(()),
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
            && text.value.chars().count() <= max_chars as usize => Ok(()),
        _ => Err(ERROR_DATA_GRID_CELL_INVALID),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_ipc::RpcServer;
    use neon_protocol::RpcError;
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
    fn drag_drop_forwards_to_domain_and_submits_its_accepted_fragment() {
        let document = parse_nui_flow(include_str!("../../../tests/fixtures/ui/kanban-reparent-workbench.nui")).unwrap();
        let effects = lower_nui_flow_effects(&document);
        let intent = effects.iter().find_map(|effect| match effect {
            UiEffect::DropBinding { binding } if binding.key == "progress-drop" => Some(binding.intent.clone()),
                _ => None,
        }).unwrap();
        let fragment = UiFragment {
            fragment_id: UiFragmentId("forwarded-demo".into()), revision: Revision(1), root: document.ir.root, effects,
        };
        let domain = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let domain_endpoint = domain.local_addr().unwrap();
        let domain_thread = thread::spawn(move || {
            let mut controller = crate::demo_domain::DemoDragDropDomain::new();
            domain.serve_until(|request| {
                let shutdown = request.method == "service.shutdown";
                (controller.handle(request), !shutdown)
            })
        });
        let renderer = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let renderer_endpoint = renderer.local_addr().unwrap();
        let renderer_thread = thread::spawn(move || renderer.serve_one(|request| {
                let command: UiCommand = serde_json::from_value(request.params.clone()).unwrap();
            let UiCommand::SubmitFragment { submission } = command else { unreachable!() };
                assert_eq!(submission.fragment.revision, Revision(2));
            assert!(submission.fragment.root.children.iter().all(|node| node.node_id.0 != "backlog-card-01"));
                accepted(request)
        }));
        let event = UiSemanticEvent {
            event: neon_ui_schema::UiSemanticEventType::DragDrop, event_id: "drag-forward-1".into(), renderer_epoch: 7,
            composition_revision: Revision(1), fragment: neon_ui_schema::UiFragmentRevision { id: fragment.fragment_id.clone(), revision: Revision(1) },
            intent, pointer: Some(neon_ui_schema::UiPointerMetadata { id: 0, sequence: 1 }), focus: None, data_grid_cell: None, text: None, control_value: None,
            drag_drop: Some(neon_ui_schema::UiDragDropPayload { source_key: "backlog-card-01".into(), target_key: "in-progress-panel".into(), placement: neon_ui_schema::UiDropPlacement::Into, presentation_template_key: Some("progress-template".into()) }),
        };
        let mut runtime = UiRuntime::new(7, "ui-drag-forward-test");
        runtime.cached_fragment = Some(fragment);
        let response = runtime.forward_drag_drop_event(domain_endpoint, renderer_endpoint, RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId("drag-forward-1".into()),
            client: ClientIdentity { kind: ClientKind::WgpuRuntime, instance_id: "renderer".into(), pid: 1, origin: "test".into() },
            target: ServiceName(SERVICE_NAME.into()), method: "ui.input.event".into(), params: json!(event), expected_revision: Some(Revision(1)), idempotency_key: Some("drag-forward-1".into()),
        }).unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.request_id, RequestId("drag-forward-1".into()));
        assert_eq!(runtime.cached_fragment().unwrap().revision, Revision(2));
        let mut client = RpcClient::connect(domain_endpoint).unwrap();
        let shutdown = RpcRequest { protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId("domain-stop".into()), client: runtime.client.clone(), target: ServiceName("demo-domain".into()), method: "service.shutdown".into(), params: json!({}), expected_revision: None, idempotency_key: None };
        assert_eq!(client.call(&shutdown).unwrap().status, RpcStatus::Accepted);
        renderer_thread.join().unwrap().unwrap();
        domain_thread.join().unwrap().unwrap();
    }

    #[test]
    fn component_gallery_contract_covers_declarations_defaults_events_and_disabled_controls() {
        let document = parse_nui_flow(include_str!("../../../tests/fixtures/ui/imgui-component-gallery.nui")).unwrap();
        let revision = compiler_program_revision();
        let program = compile_nui_flow_program(&document, revision.clone()).unwrap();
        let inputs = UiInputStore::activate(revision.clone(), document.input_schema.clone()).unwrap().snapshot();
        let frame = evaluate_ui_program(&program, &inputs, UiCpuViewport { logical_bounds: UiBounds { x: 0.0, y: 0.0, width: 760.0, height: 680.0 }, revision: Revision(1) }, &UiLocalPresentationState::default());
        let controls = [
            ("feature-toggle", UiNodeKind::Checkbox, UiProgramSemanticEventKind::SelectionChanged),
            ("mode-radio", UiNodeKind::RadioButton, UiProgramSemanticEventKind::SelectionChanged),
            ("exposure-slider", UiNodeKind::Slider, UiProgramSemanticEventKind::ValueCommit),
            ("count-drag", UiNodeKind::DragValue, UiProgramSemanticEventKind::ValueCommit),
            ("mode-combo", UiNodeKind::Combo, UiProgramSemanticEventKind::SelectionChanged),
            ("mode-dropdown", UiNodeKind::Dropdown, UiProgramSemanticEventKind::SelectionChanged),
            ("item-selectable", UiNodeKind::Selectable, UiProgramSemanticEventKind::SelectionChanged),
            ("item-list", UiNodeKind::ListBox, UiProgramSemanticEventKind::SelectionChanged),
            ("gallery-scroll", UiNodeKind::Scrollbar, UiProgramSemanticEventKind::ValueCommit),
        ];
        for (node_key, kind, _) in &controls {
            assert_eq!(program.nodes.iter().find(|node| node.key == *node_key).unwrap().kind, *kind);
            assert!(frame.render_primitives.iter().any(|primitive| primitive.node_key == *node_key));
        }
        assert_eq!(frame.nodes.iter().find(|node| node.node_key == "exposure-slider").unwrap().numeric_value, Some(0.5));
        assert_eq!(frame.nodes.iter().find(|node| node.node_key == "mode-combo").unwrap().state_token.as_deref(), Some("beta"));
        let mut router = UiProgramSemanticEventRouter::new(program.clone(), inputs.clone(), 7);
        for (index, (node_key, _, kind)) in controls.iter().enumerate() {
            let declaration = program.event_records.iter().find(|event| event.node_key == *node_key).unwrap();
            let payload = declaration.bound_input_keys.iter().map(|key| (key.clone(), input_value_as_event_payload(&inputs.values[key].value).unwrap())).collect();
            let event = UiProgramSemanticEvent { event_id: format!("gallery-default-{index}"), kind: *kind, intent: declaration.intent.clone(), source_node_key: (*node_key).into(), payload, program_revision: revision.clone(), input_revision: inputs.input_revision, request_id: format!("gallery-request-{index}"), idempotency_key: format!("gallery-key-{index}"), requested_value: None, interaction: neon_ui_schema::UiSemanticInteractionMetadata { interaction_id: format!("gallery-interaction-{index}"), sequence: index as u64 + 1, renderer_epoch: 7 } };
            assert_eq!(router.validate(&event).status, UiProgramSemanticEventStatus::Accepted);
        }
        let mut disabled = inputs.clone();
        disabled.input_revision = Revision(inputs.input_revision.0 + 1);
        disabled.values.get_mut("controls_enabled").unwrap().value = UiInputValue::Bool { value: false };
        let disabled_frame = evaluate_ui_program(&program, &disabled, UiCpuViewport { logical_bounds: UiBounds { x: 0.0, y: 0.0, width: 760.0, height: 680.0 }, revision: Revision(1) }, &UiLocalPresentationState::default());
        assert!(disabled_frame.nodes.iter().filter(|node| matches!(
                    node.node_key.as_str(),
            "feature-toggle" | "mode-radio" | "exposure-slider" | "count-drag" | "mode-combo"
                | "mode-dropdown" | "item-selectable" | "item-list" | "gallery-scroll"
        )).all(|node| !node.enabled));
        let mut disabled_router = UiProgramSemanticEventRouter::new(program.clone(), disabled.clone(), 7);
        for (index, (node_key, _, kind)) in controls.iter().enumerate() {
            let declaration = program.event_records.iter().find(|event| event.node_key == *node_key).unwrap();
            let payload = declaration.bound_input_keys.iter().map(|key| (key.clone(), input_value_as_event_payload(&disabled.values[key].value).unwrap())).collect();
            let event = UiProgramSemanticEvent { event_id: format!("gallery-disabled-{index}"), kind: *kind, intent: declaration.intent.clone(), source_node_key: (*node_key).into(), payload, program_revision: revision.clone(), input_revision: disabled.input_revision, request_id: format!("gallery-disabled-request-{index}"), idempotency_key: format!("gallery-disabled-key-{index}"), requested_value: None, interaction: neon_ui_schema::UiSemanticInteractionMetadata { interaction_id: format!("gallery-disabled-interaction-{index}"), sequence: index as u64 + 1, renderer_epoch: 7 } };
            assert_eq!(disabled_router.validate(&event).code.as_deref(), Some(ERROR_UI_PROGRAM_EVENT_CONTROL_UNAVAILABLE));
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
            UiGpuScalarRepresentation, UiInputKind, UiInputPacking, UiInputSlot,
            UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
            UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION,
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
            UiGpuScalarRepresentation, UiInputKind, UiInputPacking, UiInputSlot,
            UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
            UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION,
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
            UiGpuScalarRepresentation, UiInputKind, UiInputPacking, UiInputSlot,
            UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
            UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION,
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
            UiProgramCapability, UiProgramCapabilityOwner, UiProgramCapabilityStatus,
            UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION,
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
        use neon_ui_schema::{UiDataGridDeclaration, UiDataGridFrame, UiDataGridWindowRow};

        let mut document = compiler_document();
        document.root.children.push(UiNode {
            node_id: UiNodeId("assets".into()), kind: UiNodeKind::DataGrid,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 100.0, height: 40.0 }, layout: None,
            visible: true, enabled: true, text_key: None, text: None, image: None, surface: None,
            style: UiStyle::default(), enter_transition: None, children: Vec::new(),
        });
        document.data_grids.push(UiDataGridDeclaration {
            node_key: "assets".into(), max_window_rows: 2, row_height: 24, overscan: 1,
            columns: vec![neon_ui_schema::UiDataGridColumn { key: "name".into(), label: "Name".into(), width: 120, presentation: neon_ui_schema::UiDataGridPresentation::Edit { max_chars: 120, intent: "asset.name.edit".into() } }],
        });
        document.resource_budget.max_nodes = 3;
        document.resource_budget.max_instances = 3;
        let revision = compiler_program_revision();
        let program = compile_ui_program(&document, revision.clone(), &compiler_schema(true)).unwrap();
        assert_eq!(program.data_grid_records[0].columns[0].presentation, neon_ui_schema::UiDataGridPresentation::Edit { max_chars: 120, intent: "asset.name.edit".into() });
        let frame = UiDataGridFrame {
            data_grid_key: "assets".into(), list_revision: Revision(1), total_rows: 10, first_row: 4,
            window_rows: vec![
                UiDataGridWindowRow { stable_row_key: "asset-5".into(), cells: BTreeMap::from([("name".into(), neon_ui_schema::UiDataGridCell { value: UiInputValue::TextHandle { value: UiTextHandle { id: 5, generation: 1 } }, display: UiTextHandle { id: 105, generation: 1 }, presentation_override: None })]) },
                UiDataGridWindowRow { stable_row_key: "asset-6".into(), cells: BTreeMap::from([("name".into(), neon_ui_schema::UiDataGridCell { value: UiInputValue::TextHandle { value: UiTextHandle { id: 6, generation: 1 } }, display: UiTextHandle { id: 106, generation: 1 }, presentation_override: None })]) },
            ],
            expected_program_revision: revision,
        };
        let mut store = UiDataGridStore::default();
        assert_eq!(store.apply(&program, frame.clone()).unwrap().accepted_rows, 2);
        assert_eq!(store.frame("assets").unwrap().first_row, 4);
        let mut same_list_next_window = frame.clone();
        same_list_next_window.first_row = 6;
        assert_eq!(store.apply(&program, same_list_next_window).unwrap().accepted_rows, 2);

        let mut stale_list = frame.clone();
        stale_list.list_revision = Revision(0);
        assert_eq!(store.apply(&program, stale_list).unwrap_err().code, ERROR_UI_PROGRAM_STALE_INPUT_REVISION);

        let mut wrong_program = frame.clone();
        wrong_program.list_revision = Revision(2);
        wrong_program.expected_program_revision.revision = Revision(2);
        assert_eq!(store.apply(&program, wrong_program).unwrap_err().code, ERROR_UI_PROGRAM_STALE_INPUT_REVISION);

        let mut out_of_bounds = frame.clone();
        out_of_bounds.list_revision = Revision(2);
        out_of_bounds.first_row = 9;
        assert_eq!(store.apply(&program, out_of_bounds).unwrap_err().code, ERROR_UI_PROGRAM_CAPACITY_OVERFLOW);

        let mut duplicate_key = frame;
        duplicate_key.list_revision = Revision(2);
        duplicate_key.window_rows[1].stable_row_key = "asset-5".into();
        assert_eq!(store.apply(&program, duplicate_key).unwrap_err().code, ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE);

        let mut missing_cell = store.frame("assets").unwrap().clone();
        missing_cell.list_revision = Revision(2);
        missing_cell.window_rows[0].cells.clear();
        assert_eq!(store.apply(&program, missing_cell).unwrap_err().code, ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE);

        let mut invalid_value = store.frame("assets").unwrap().clone();
        invalid_value.list_revision = Revision(2);
        invalid_value.window_rows[0].cells.get_mut("name").unwrap().value = UiInputValue::F32 { value: f32::NAN };
        assert_eq!(store.apply(&program, invalid_value).unwrap_err().code, ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE);

        let mut invalid_override = store.frame("assets").unwrap().clone();
        invalid_override.list_revision = Revision(2);
        invalid_override.window_rows[0].cells.get_mut("name").unwrap().presentation_override = Some(neon_ui_schema::UiDataGridCellPresentation::Dropdown { options: Vec::new() });
        assert_eq!(store.apply(&program, invalid_override).unwrap_err().code, ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE);
    }

    #[test]
    fn data_grid_store_attaches_current_frame_to_fragment() {
        use neon_ui_schema::{UiDataGridCell, UiDataGridDeclaration, UiDataGridFrame, UiDataGridWindowRow};

        let mut document = compiler_document();
        document.root.children.push(UiNode {
            node_id: UiNodeId("assets".into()), kind: UiNodeKind::DataGrid,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 100.0, height: 40.0 }, layout: None,
            visible: true, enabled: true, text_key: None, text: None, image: None, surface: None,
            style: UiStyle::default(), enter_transition: None, children: Vec::new(),
        });
        document.data_grids.push(UiDataGridDeclaration {
            node_key: "assets".into(), max_window_rows: 2, row_height: 24, overscan: 1,
            columns: vec![neon_ui_schema::UiDataGridColumn { key: "name".into(), label: "Name".into(), width: 120, presentation: neon_ui_schema::UiDataGridPresentation::Text }],
        });
        document.resource_budget.max_nodes = 3;
        document.resource_budget.max_instances = 3;
        let revision = compiler_program_revision();
        let program = compile_ui_program(&document, revision.clone(), &compiler_schema(true)).unwrap();
        let frame = UiDataGridFrame {
            data_grid_key: "assets".into(), list_revision: Revision(1), total_rows: 1, first_row: 0,
            window_rows: vec![UiDataGridWindowRow {
                stable_row_key: "asset-1".into(),
                cells: BTreeMap::from([("name".into(), UiDataGridCell {
                    value: UiInputValue::TextHandle { value: UiTextHandle { id: 1, generation: 1 } },
                    display: UiTextHandle { id: 101, generation: 1 },
                    presentation_override: None,
                })]),
            }],
            expected_program_revision: revision,
        };
        let mut store = UiDataGridStore::default();
        store.apply(&program, frame).unwrap();
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("grid-fragment".into()), revision: Revision(1),
            root: document.root, effects: vec![UiEffect::SemanticAction { action: "grid.ready".into() }],
        };

        store.attach_to_fragment(&program, &mut fragment).unwrap();

        assert!(matches!(fragment.effects.last(), Some(UiEffect::DataGridFrame { declaration, frame })
            if declaration.node_key == "assets" && frame.window_rows[0].cells["name"].display.id == 101));
        fragment.validate().unwrap();
    }

    #[test]
    fn data_grid_cell_events_require_current_targets_typed_values_and_edit_bounds() {
        let root = UiNode {
            node_id: UiNodeId("assets".into()), kind: UiNodeKind::DataGrid,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 320.0, height: 80.0 }, layout: None,
            visible: true, enabled: true, text_key: None, text: None, image: None, surface: None,
            style: UiStyle::default(), enter_transition: None, children: Vec::new(),
        };
        let declaration = UiDataGridDeclaration {
            node_key: "assets".into(), max_window_rows: 1, row_height: 24, overscan: 0,
            columns: vec![
                neon_ui_schema::UiDataGridColumn { key: "selected".into(), label: "Selected".into(), width: 80, presentation: neon_ui_schema::UiDataGridPresentation::Select { intent: "asset.selected.set".into() } },
                neon_ui_schema::UiDataGridColumn { key: "state".into(), label: "State".into(), width: 80, presentation: neon_ui_schema::UiDataGridPresentation::Dropdown { options: vec!["ready".into(), "review".into()], intent: "asset.state.set".into() } },
                neon_ui_schema::UiDataGridColumn { key: "name".into(), label: "Name".into(), width: 160, presentation: neon_ui_schema::UiDataGridPresentation::Edit { max_chars: 5, intent: "asset.name.set".into() } },
            ],
        };
        let text_handle = UiTextHandle { id: 3, generation: 1 };
        let frame = UiDataGridFrame {
            data_grid_key: "assets".into(), list_revision: Revision(1), total_rows: 1, first_row: 0,
            window_rows: vec![neon_ui_schema::UiDataGridWindowRow {
                stable_row_key: "asset-42".into(),
                cells: BTreeMap::from([
                    ("selected".into(), neon_ui_schema::UiDataGridCell { value: UiInputValue::Bool { value: false }, display: UiTextHandle { id: 1, generation: 1 }, presentation_override: None }),
                    ("state".into(), neon_ui_schema::UiDataGridCell { value: UiInputValue::Enum { value: "ready".into() }, display: UiTextHandle { id: 2, generation: 1 }, presentation_override: None }),
                    ("name".into(), neon_ui_schema::UiDataGridCell { value: UiInputValue::TextHandle { value: text_handle }, display: text_handle, presentation_override: None }),
                ]),
            }],
            expected_program_revision: compiler_program_revision(),
        };
        let mut runtime = UiRuntime::new(7, "grid-event-test");
        runtime.cached_fragment = Some(UiFragment {
            fragment_id: UiFragmentId("grid".into()), revision: Revision(1), root,
            effects: vec![UiEffect::DataGridFrame { declaration, frame }],
        });
        let event = |event, action: &str, column: &str, control_value, text| UiSemanticEvent {
            event, event_id: format!("event-{column}"), renderer_epoch: 7, composition_revision: Revision(1),
            fragment: neon_ui_schema::UiFragmentRevision { id: UiFragmentId("grid".into()), revision: Revision(1) },
            intent: UiIntent::Invoke { action: action.into(), params: json!({}) }, pointer: None, focus: None,
            data_grid_cell: Some(UiDataGridCellTarget { data_grid_key: "assets".into(), stable_row_key: "asset-42".into(), column_key: column.into() }),
            text, control_value, drag_drop: None,
        };
        assert!(runtime.validate_semantic_event(&event(
            neon_ui_schema::UiSemanticEventType::SelectionChanged, "asset.selected.set", "selected",
            Some(UiSemanticPayloadValue::Bool { value: true }), None,
        )).is_ok());
        assert!(runtime.validate_semantic_event(&event(
            neon_ui_schema::UiSemanticEventType::SelectionChanged, "asset.state.set", "state",
            Some(UiSemanticPayloadValue::Enum { value: "review".into() }), None,
        )).is_ok());
        assert!(runtime.validate_semantic_event(&event(
            neon_ui_schema::UiSemanticEventType::TextInputCommit, "asset.name.set", "name",
            Some(UiSemanticPayloadValue::TextHandle { value: text_handle }),
            Some(neon_ui_schema::UiTextInputCommit { value: "short".into() }),
        )).is_ok());
        let mut stale = event(
            neon_ui_schema::UiSemanticEventType::SelectionChanged, "asset.selected.set", "selected",
            Some(UiSemanticPayloadValue::Bool { value: true }), None,
        );
        stale.data_grid_cell.as_mut().unwrap().stable_row_key = "off-window".into();
        assert_eq!(runtime.validate_semantic_event(&stale), Err(ERROR_DATA_GRID_CELL_INVALID));
        assert_eq!(runtime.validate_semantic_event(&event(
            neon_ui_schema::UiSemanticEventType::TextInputCommit, "asset.name.set", "name",
            Some(UiSemanticPayloadValue::TextHandle { value: text_handle }),
            Some(neon_ui_schema::UiTextInputCommit { value: "too-long".into() }),
        )), Err(ERROR_DATA_GRID_CELL_INVALID));

        let domain = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let endpoint = domain.local_addr().unwrap();
        let domain_thread = thread::spawn(move || domain.serve_one(|request| {
            assert_eq!(request.method, "asset.selected.set");
            assert_eq!(request.params["data_grid_cell"]["stable_row_key"], "asset-42");
            assert_eq!(request.params["control_value"], json!({"kind": "bool", "value": true}));
            assert!(request.params.get("node_path").is_none());
            accepted(request)
        }));
        let outbound = event(
            neon_ui_schema::UiSemanticEventType::SelectionChanged, "asset.selected.set", "selected",
            Some(UiSemanticPayloadValue::Bool { value: true }), None,
        );
        let response = runtime.forward_data_grid_cell_event(endpoint, RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId("grid-forward".into()),
            client: runtime.client.clone(), target: ServiceName(SERVICE_NAME.into()), method: "ui.input.event".into(),
            params: json!(outbound), expected_revision: Some(Revision(1)), idempotency_key: Some("grid-forward".into()),
        }).unwrap();
        assert_eq!(response.status, RpcStatus::Accepted);
        assert_eq!(response.request_id, RequestId("grid-forward".into()));
        domain_thread.join().unwrap().unwrap();
    }

    #[test]
    fn program_semantic_events_require_current_enabled_declarations_and_are_idempotent() {
        use neon_ui_schema::{UiProgramSemanticEvent, UiProgramSemanticEventKind, UiSemanticInteractionMetadata};
        let mut document = compiler_document();
        document.events[0].literal_payload.insert("tool".into(), UiSemanticPayloadValue::Enum { value: "water".into() });
        let revision = compiler_program_revision();
        let program = compile_ui_program(&document, revision.clone(), &compiler_schema(true)).unwrap();
        let inputs = UiInputStore::activate(revision.clone(), compiler_schema(true)).unwrap().snapshot();
        let mut router = UiProgramSemanticEventRouter::new(program, inputs.clone(), 7);
        let event = UiProgramSemanticEvent {
            event_id: "event-1".into(), kind: UiProgramSemanticEventKind::Activate,
            intent: "terrain.commit".into(), source_node_key: "commit".into(),
            payload: BTreeMap::from([("tool".into(), UiSemanticPayloadValue::Enum { value: "water".into() })]),
            program_revision: revision, input_revision: inputs.input_revision,
            request_id: "request-1".into(), idempotency_key: "key-1".into(), requested_value: None,
            interaction: UiSemanticInteractionMetadata { interaction_id: "interaction-1".into(), sequence: 1, renderer_epoch: 7 },
        };
        assert_eq!(router.validate(&event).status, UiProgramSemanticEventStatus::Accepted);
        assert_eq!(router.validate(&event).status, UiProgramSemanticEventStatus::Duplicate);
        let mut stale = event;
        stale.idempotency_key = "key-2".into();
        stale.input_revision = Revision(99);
        assert_eq!(router.validate(&stale).code.as_deref(), Some(ERROR_UI_PROGRAM_EVENT_STALE_REVISION));
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
    fn forwarder_routes_gallery_pointer_clicks_through_program_domain() {
        use std::time::Duration;

        fn gallery_client() -> ClientIdentity {
            ClientIdentity { kind: ClientKind::UiReactClient, instance_id: "gallery-live-test".into(), pid: 0, origin: "neon-ui-runtime-test".into() }
        }

        fn literal_text(root: &UiNode, node_id: &str) -> Option<String> {
            fn visit(node: &UiNode, node_id: &str) -> Option<String> {
                if node.node_id.0 == node_id {
                    if let Some(TextRef::Literal { value }) = &node.text { return Some(value.clone()); }
                }
                node.children.iter().find_map(|child| visit(child, node_id))
            }
            visit(root, node_id)
        }

        fn shutdown_request(endpoint: SocketAddr) -> RpcRequest {
            RpcRequest {
                protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId(format!("shutdown-{}", endpoint.port())),
                client: gallery_client(), target: ServiceName("ui-runtime".into()), method: "service.shutdown".into(),
                params: json!({}), expected_revision: None, idempotency_key: None,
            }
        }

        let domain_server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let domain_addr = domain_server.local_addr().unwrap();
        drop(domain_server);
        let domain_thread = thread::spawn(move || {
            let _ = demo_domain::DemoInputDomain::serve_component_gallery(domain_addr);
        });

        let wgpu_server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let wgpu_addr = wgpu_server.local_addr().unwrap();
        let (fragments_tx, fragments_rx) = std::sync::mpsc::channel::<UiFragment>();
        let wgpu_thread = thread::spawn(move || {
            let _ = wgpu_server.serve_until(|request| {
                let shutdown = request.method == "service.shutdown";
                let response = if shutdown {
                    RpcResponse { request_id: request.request_id, status: RpcStatus::Accepted, revision: None, result: Some(json!({"state": "accepted"})), snapshot: None, error: None }
                } else if request.method == "wgpu.ui.submit_fragment" {
                    match serde_json::from_value::<UiCommand>(request.params.clone()) {
                        Ok(UiCommand::SubmitFragment { submission }) => {
                            let _ = fragments_tx.send(submission.fragment.clone());
                            RpcResponse { request_id: request.request_id, status: RpcStatus::Accepted, revision: Some(submission.fragment.revision), result: Some(json!({"fragment_revision": submission.fragment.revision.0})), snapshot: None, error: None }
                            }
                        _ => RpcResponse { request_id: request.request_id, status: RpcStatus::Rejected, revision: None, result: None, snapshot: None, error: Some(RpcError { code: "invalid_request".into(), message: "expected submit fragment command".into(), current_revision: None, object_id: None }) },
                    }
                } else {
                    RpcResponse { request_id: request.request_id, status: RpcStatus::Rejected, revision: None, result: None, snapshot: None, error: Some(RpcError { code: "unsupported_method".into(), message: "method is not supported".into(), current_revision: None, object_id: None }) }
                };
                (response, !shutdown)
            });
        });

        let ui_server = RpcServer::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let ui_addr = ui_server.local_addr().unwrap();
        drop(ui_server);
        let forwarder_thread = thread::spawn(move || {
            let _ = UiRuntime::serve_forwarder(ui_addr, wgpu_addr, domain_addr, 1, true);
        });

        let call = |endpoint: SocketAddr, request: RpcRequest| {
            let mut last_error = None;
            for _ in 0..40 {
                match RpcClient::connect(endpoint).and_then(|mut client| client.call(&request)) {
                    Ok(response) => return response,
                    Err(error) => {
                        last_error = Some(error);
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            panic!("RPC call failed after retries: {last_error:?}");
        };

        let (document, _program) = demo_domain::component_gallery_program().unwrap();
        let fragment_id = UiFragmentId("component-gallery-live".into());
        let fragment = UiFragment { fragment_id: fragment_id.clone(), revision: Revision(1), root: document.ir.root.clone(), effects: lower_nui_flow_effects(&document) };
        let submit = RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId("gallery-live-submit-1".into()),
            client: gallery_client(), target: ServiceName("ui-runtime".into()), method: "ui.fragment.submit".into(),
            params: json!(UiCommand::SubmitFragment { submission: UiFragmentSubmission::new(fragment.clone()) }),
            expected_revision: None, idempotency_key: Some("gallery-live-submit-1".into()),
        };
        assert_eq!(call(ui_addr, submit).status, RpcStatus::Accepted);
        assert_eq!(fragments_rx.recv_timeout(Duration::from_secs(5)).unwrap().revision, Revision(1));

        let click = |sequence: u64, event_revision: Revision, idempotency: &str| RpcRequest {
            protocol: "neon3.rpc".into(), version: PROTOCOL_VERSION, request_id: RequestId(format!("wgpu-click-{sequence}")),
            client: gallery_client(), target: ServiceName("ui-runtime".into()), method: "ui.input.event".into(),
            params: json!(UiSemanticEvent {
                event: neon_ui_schema::UiSemanticEventType::PointerClick,
                event_id: format!("wgpu-pointer-click-{sequence}"),
                renderer_epoch: 1,
                composition_revision: event_revision,
                fragment: neon_ui_schema::UiFragmentRevision { id: fragment_id.clone(), revision: event_revision },
                intent: neon_ui_schema::UiIntent::Invoke { action: "gallery.checkbox.toggle".into(), params: json!({}) },
                pointer: Some(neon_ui_schema::UiPointerMetadata { id: 0, sequence }),
                focus: None, data_grid_cell: None, text: None, control_value: None, drag_drop: None,
            }),
            expected_revision: Some(event_revision),
            idempotency_key: Some(idempotency.into()),
        };

        let first = call(ui_addr, click(1, Revision(1), "wgpu-pointer-click:1:1"));
        assert_eq!(first.status, RpcStatus::Accepted, "first gallery click must be accepted: {first:?}");
        let fragment_two = fragments_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(fragment_two.revision, Revision(2));
        assert_eq!(literal_text(&fragment_two.root, "status-feature_enabled").as_deref(), Some("feature_enabled: false"));

        let second = call(ui_addr, click(2, Revision(2), "wgpu-pointer-click:1:2"));
        assert_eq!(second.status, RpcStatus::Accepted, "second gallery click must be accepted: {second:?}");
        let fragment_three = fragments_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(fragment_three.revision, Revision(3));
        assert_eq!(literal_text(&fragment_three.root, "status-feature_enabled").as_deref(), Some("feature_enabled: true"));

        for endpoint in [ui_addr, domain_addr, wgpu_addr] {
            let _ = call(endpoint, shutdown_request(endpoint));
        }
        forwarder_thread.join().unwrap();
        domain_thread.join().unwrap();
        wgpu_thread.join().unwrap();
    }
}
