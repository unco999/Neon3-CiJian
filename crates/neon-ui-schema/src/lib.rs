//! GPU-independent UI declaration schema types.
//! This crate must not create GPU or window objects.

use neon_protocol::{AssetRef, Revision};
use serde::{Deserialize, Serialize};

/// Version of the renderer-independent UI declaration contract.
/// This is deliberately separate from the RPC transport version.
pub const UI_FRAGMENT_SCHEMA_VERSION: u16 = 1;
pub const UI_SURFACE_SCHEMA_VERSION: u16 = 1;
/// Baseline schema for the static, GPU-reactive UI program contract.
///
/// This is additive to `UiFragment`. Existing fragment clients stay on their
/// current compatibility contract until they explicitly negotiate this capability.
pub const UI_PROGRAM_SCHEMA_VERSION: u16 = 1;
pub const UI_PROGRAM_CAPABILITY_NAME: &str = "ui.program.v1";
pub const UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME: &str = "ui.program.text_registry.v1";
pub const UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME: &str = "ui.program.bounded_structure.v1";
pub const UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME: &str = "ui.program.semantic_event.v1";

pub const ERROR_UI_PROGRAM_UNSUPPORTED_SCHEMA: &str = "ui_program_unsupported_schema";
pub const ERROR_UI_PROGRAM_UNSUPPORTED_CAPABILITY: &str = "ui_program_unsupported_capability";
pub const ERROR_UI_PROGRAM_DUPLICATE_INPUT_KEY: &str = "ui_program_duplicate_input_key";
pub const ERROR_UI_PROGRAM_INVALID_DEFAULT: &str = "ui_program_invalid_default";
pub const ERROR_UI_PROGRAM_INPUT_TYPE_MISMATCH: &str = "ui_program_input_type_mismatch";
pub const ERROR_UI_PROGRAM_STALE_INPUT_REVISION: &str = "ui_program_stale_input_revision";
pub const ERROR_UI_PROGRAM_UNKNOWN_TEXT_HANDLE: &str = "ui_program_unknown_text_handle";
pub const ERROR_UI_PROGRAM_TEXT_REGISTRY_GENERATION_MISMATCH: &str =
    "ui_program_text_registry_generation_mismatch";
pub const ERROR_UI_PROGRAM_UNKNOWN_BINDING_TARGET: &str = "ui_program_unknown_binding_target";
pub const ERROR_UI_PROGRAM_CAPACITY_OVERFLOW: &str = "ui_program_capacity_overflow";
pub const ERROR_UI_PROGRAM_INVALID_BRANCH_TEMPLATE: &str = "ui_program_invalid_branch_template";
pub const ERROR_UI_PROGRAM_FORBIDDEN_FLOW_FEATURE: &str = "ui_program_forbidden_flow_feature";
pub const ERROR_UI_PROGRAM_EVENT_STALE_REVISION: &str = "ui_program_event_stale_revision";
pub const ERROR_UI_PROGRAM_EVENT_INVALID_SOURCE: &str = "ui_program_event_invalid_source";
pub const ERROR_UI_PROGRAM_EVENT_CONTROL_UNAVAILABLE: &str = "ui_program_event_control_unavailable";
pub const ERROR_UI_PROGRAM_EVENT_PAYLOAD_REJECTED: &str = "ui_program_event_payload_rejected";
pub const ERROR_UI_PROGRAM_EVENT_DUPLICATE_IDEMPOTENCY_KEY: &str =
    "ui_program_event_duplicate_idempotency_key";
pub const ERROR_UI_PROGRAM_EVENT_INTERACTION_EPOCH_MISMATCH: &str =
    "ui_program_event_interaction_epoch_mismatch";
pub const ERROR_UI_PROGRAM_UNKNOWN_INPUT_KEY: &str = "ui_program_unknown_input_key";
pub const ERROR_UI_PROGRAM_INPUT_UPDATE_FORBIDDEN: &str = "ui_program_input_update_forbidden";
pub const ERROR_UI_PROGRAM_DUPLICATE_INPUT_CHANGE: &str = "ui_program_duplicate_input_change";
pub const ERROR_UI_PROGRAM_TEXT_REGISTRY_STALE_REVISION: &str =
    "ui_program_text_registry_stale_revision";
pub const ERROR_UI_PROGRAM_TEXT_REGISTRY_CAPACITY_OVERFLOW: &str =
    "ui_program_text_registry_capacity_overflow";
pub const ERROR_UI_PROGRAM_TEXT_TOO_LONG: &str = "ui_program_text_too_long";
pub const ERROR_NUI_FLOW_PARSE: &str = "nui_flow_parse";
pub const ERROR_NUI_FLOW_FORBIDDEN_FEATURE: &str = "nui_flow_forbidden_feature";
pub const ERROR_NUI_FLOW_INVALID_PATCH: &str = "nui_flow_invalid_patch";
pub const ERROR_NUI_FLOW_STALE_PATCH_REVISION: &str = "nui_flow_stale_patch_revision";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiGpuScalarRepresentation {
    Bool32,
    I32,
    U32,
    F32,
    Vec2F32,
    Vec4F32,
    HandleUvec2,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInputPacking {
    pub alignment: u32,
    pub lanes: u8,
    pub offset: u32,
    pub representation: UiGpuScalarRepresentation,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiInputKind {
    Bool,
    I32,
    U32,
    F32,
    Vec2,
    Vec4,
    Color,
    Enum { variants: Vec<String> },
    TextHandle,
    AssetHandle,
    I32Range { minimum: i32, maximum: i32 },
    U32Range { minimum: u32, maximum: u32 },
    F32Range { minimum: f32, maximum: f32 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiInputUpdateClass {
    StaticAtProgramActivation,
    ReliableExternal,
    LocalPresentation,
    TextRegistryReference,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiInputValue {
    Bool { value: bool },
    I32 { value: i32 },
    U32 { value: u32 },
    F32 { value: f32 },
    Vec2 { value: [f32; 2] },
    Vec4 { value: [f32; 4] },
    Color { value: [f32; 4] },
    Enum { value: String },
    TextHandle { value: UiTextHandle },
    AssetHandle { id: u64, generation: u32 },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInputSlot {
    pub key: String,
    pub kind: UiInputKind,
    pub default_value: UiInputValue,
    pub update_class: UiInputUpdateClass,
    pub semantic_label: String,
    pub packing: UiInputPacking,
}
/// A control-plane input which supplies a bounded DataGrid window. Grid inputs
/// deliberately have no scalar value or GPU packing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiGridInputSlot {
    pub key: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInputSchema {
    pub schema_id: String,
    pub version: u16,
    pub slots: Vec<UiInputSlot>,
    #[serde(default)]
    pub grid_slots: Vec<UiGridInputSlot>,
    pub layout_hash: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInputChange {
    pub key: String,
    pub value: UiInputValue,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInputFrame {
    pub program_revision: UiProgramRevision,
    pub expected_input_revision: Revision,
    pub request_id: String,
    pub idempotency_key: String,
    pub changes: Vec<UiInputChange>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiInputValueSource {
    Default,
    ReliableExternal,
    LocalPresentation,
    TextRegistryReference,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiResolvedInputValue {
    pub value: UiInputValue,
    pub source: UiInputValueSource,
    pub last_update_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiResolvedInputs {
    pub program_revision: UiProgramRevision,
    pub input_revision: Revision,
    pub values: std::collections::BTreeMap<String, UiResolvedInputValue>,
    pub changed_slots: Vec<String>,
}

/// Complete host-visible input state for one active UI program. Scalar inputs
/// and bounded grid windows are kept separate because grids have no GPU scalar
/// packing and are replaced as whole windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramInputSnapshot {
    pub scalar_inputs: UiResolvedInputs,
    pub grid_inputs: Vec<UiDataGridInputFrame>,
}

impl UiInputKind {
    pub fn packing(&self) -> (u32, u8, UiGpuScalarRepresentation) {
        match self {
            Self::Bool => (4, 1, UiGpuScalarRepresentation::Bool32),
            Self::I32 | Self::I32Range { .. } => (4, 1, UiGpuScalarRepresentation::I32),
            Self::U32 | Self::U32Range { .. } | Self::Enum { .. } => {
                (4, 1, UiGpuScalarRepresentation::U32)
            }
            Self::F32 | Self::F32Range { .. } => (4, 1, UiGpuScalarRepresentation::F32),
            Self::Vec2 => (8, 2, UiGpuScalarRepresentation::Vec2F32),
            Self::Vec4 | Self::Color => (16, 4, UiGpuScalarRepresentation::Vec4F32),
            Self::TextHandle | Self::AssetHandle => (8, 2, UiGpuScalarRepresentation::HandleUvec2),
        }
    }
    pub fn accepts(&self, value: &UiInputValue) -> bool {
        let finite = |values: &[f32]| values.iter().all(|value| value.is_finite());
        match (self, value) {
            (Self::Bool, UiInputValue::Bool { .. })
            | (Self::I32, UiInputValue::I32 { .. })
            | (Self::U32, UiInputValue::U32 { .. })
            | (Self::TextHandle, UiInputValue::TextHandle { .. })
            | (Self::AssetHandle, UiInputValue::AssetHandle { .. }) => true,
            (Self::F32, UiInputValue::F32 { value }) => value.is_finite(),
            (Self::Vec2, UiInputValue::Vec2 { value }) => finite(value),
            (Self::Vec4, UiInputValue::Vec4 { value })
            | (Self::Color, UiInputValue::Color { value }) => finite(value),
            (Self::Enum { variants }, UiInputValue::Enum { value }) => {
                variants.iter().any(|variant| variant == value)
            }
            (Self::I32Range { minimum, maximum }, UiInputValue::I32 { value }) => {
                minimum <= maximum && (*minimum..=*maximum).contains(value)
            }
            (Self::U32Range { minimum, maximum }, UiInputValue::U32 { value }) => {
                minimum <= maximum && (*minimum..=*maximum).contains(value)
            }
            (Self::F32Range { minimum, maximum }, UiInputValue::F32 { value }) => {
                minimum.is_finite()
                    && maximum.is_finite()
                    && value.is_finite()
                    && minimum <= maximum
                    && *value >= *minimum
                    && *value <= *maximum
            }
            _ => false,
        }
    }
}

/// Opaque session-stable text identity. Its generation prevents a released ID
/// from resolving to unrelated replacement text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTextHandle {
    pub id: u64,
    pub generation: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTextSourceCategory {
    Literal,
    Dynamic,
}

/// Bounded UTF-8 text held by the text registry, never by a scalar input frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTextRecord {
    pub handle: UiTextHandle,
    pub text: String,
    pub category: UiTextSourceCategory,
    pub revision: Revision,
    pub byte_length: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTextRegistrySnapshot {
    pub registry_id: String,
    pub revision: Revision,
    pub capacity: u32,
    pub used: u32,
    pub records: Vec<UiTextRecord>,
}

/// Sanitized text-registry inspection data. This is the default diagnostic
/// surface; text content is returned only by an explicitly approved query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTextRegistryEntryMetadata {
    pub handle: UiTextHandle,
    pub category: UiTextSourceCategory,
    pub revision: Revision,
    pub byte_length: u32,
    pub reference_count: u32,
    pub resident: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTextRegistryDebugSnapshot {
    pub registry_id: String,
    pub revision: Revision,
    pub capacity: u32,
    pub used: u32,
    pub records: Vec<UiTextRegistryEntryMetadata>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTextHandleStatus {
    Ready,
    Missing,
    GenerationMismatch,
    Released,
    CapacityExceeded,
}

/// Sanitized metadata for default diagnostics. Text content is deliberately not included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTextHandleDiagnostic {
    pub handle: UiTextHandle,
    pub status: UiTextHandleStatus,
    pub reference_count: u32,
    pub resident: bool,
    pub message: String,
}
impl UiInputSchema {
    pub fn validate(&self) -> Result<(), UiSchemaError> {
        if self.schema_id.trim().is_empty()
            || self.version == 0
            || self.layout_hash.trim().is_empty()
        {
            return Err(UiSchemaError::InvalidInputSchema);
        }
        let mut keys = std::collections::HashSet::new();
        let mut offsets = std::collections::HashSet::new();
        for slot in &self.slots {
            if slot.key.trim().is_empty() || slot.semantic_label.trim().is_empty() {
                return Err(UiSchemaError::InvalidInputSlot);
            }
            if !keys.insert(slot.key.as_str()) {
                return Err(UiSchemaError::DuplicateInputKey);
            }
            if !slot.kind.accepts(&slot.default_value) {
                return Err(UiSchemaError::InvalidInputDefault);
            }
            let (alignment, lanes, representation) = slot.kind.packing();
            if slot.packing.alignment != alignment
                || slot.packing.lanes != lanes
                || slot.packing.representation != representation
                || slot.packing.offset % alignment != 0
                || !offsets.insert(slot.packing.offset)
            {
                return Err(UiSchemaError::InvalidInputPacking);
            }
            if let UiInputKind::Enum { variants } = &slot.kind {
                if variants.is_empty()
                    || variants.iter().any(|variant| variant.trim().is_empty())
                    || variants
                        .iter()
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                        != variants.len()
                {
                    return Err(UiSchemaError::InvalidInputSlot);
                }
            }
        }
        for slot in &self.grid_slots {
            if slot.key.trim().is_empty() || !keys.insert(slot.key.as_str()) {
                return Err(UiSchemaError::DuplicateInputKey);
            }
        }
        Ok(())
    }
    pub fn validate_evolution_from(&self, previous: &Self) -> Result<(), UiSchemaError> {
        if self.schema_id != previous.schema_id || self.version <= previous.version {
            return Err(UiSchemaError::IncompatibleInputSchemaEvolution);
        }
        for old_slot in &previous.slots {
            let Some(new_slot) = self.slots.iter().find(|slot| slot.key == old_slot.key) else {
                return Err(UiSchemaError::IncompatibleInputSchemaEvolution);
            };
            if new_slot.kind != old_slot.kind
                || new_slot.update_class != old_slot.update_class
                || new_slot.packing != old_slot.packing
            {
                return Err(UiSchemaError::IncompatibleInputSchemaEvolution);
            }
        }
        if previous.grid_slots.iter().any(|old_slot| {
            !self
                .grid_slots
                .iter()
                .any(|new_slot| new_slot.key == old_slot.key)
        }) {
            return Err(UiSchemaError::IncompatibleInputSchemaEvolution);
        }
        self.validate()
    }
}

/// Declares a versioned UI program capability and its authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramCapability {
    pub name: String,
    pub version: u16,
    pub owner: UiProgramCapabilityOwner,
    pub status: UiProgramCapabilityStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiProgramCapabilityOwner {
    UiRuntime,
    WgpuRuntime,
    SharedContract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiProgramCapabilityStatus {
    Experimental,
    Supported,
    Deprecated,
}

/// Identity and compatibility information for one immutable UI program upload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramRevision {
    pub program_id: String,
    pub revision: Revision,
    pub schema_version: u16,
    pub capabilities: Vec<UiProgramCapability>,
}

impl UiProgramRevision {
    pub fn validate_baseline(&self) -> Result<(), UiSchemaError> {
        if self.program_id.trim().is_empty() {
            return Err(UiSchemaError::EmptyProgramId);
        }
        if self.schema_version != UI_PROGRAM_SCHEMA_VERSION {
            return Err(UiSchemaError::UnsupportedProgramSchemaVersion);
        }
        if !self.capabilities.iter().any(|capability| {
            capability.name == UI_PROGRAM_CAPABILITY_NAME && capability.version == 1
        }) {
            return Err(UiSchemaError::UnsupportedProgramCapability);
        }
        let mut names = std::collections::HashSet::new();
        for capability in &self.capabilities {
            if capability.name.trim().is_empty() || capability.version == 0 {
                return Err(UiSchemaError::InvalidProgramCapability);
            }
            if !names.insert((capability.name.as_str(), capability.version)) {
                return Err(UiSchemaError::DuplicateProgramCapability);
            }
            if !matches!(
                capability.name.as_str(),
                UI_PROGRAM_CAPABILITY_NAME
                    | UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME
                    | UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME
                    | UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME
            ) || capability.version != 1
            {
                return Err(UiSchemaError::UnsupportedProgramCapability);
            }
        }
        Ok(())
    }
}

/// A source location in NUI Flow or an equivalent authoring representation.
/// It is semantic debug metadata, never a renderer-local identifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSourceSpan {
    pub source_id: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

/// Machine-readable diagnostic for program validation, execution, or inspection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDiagnostic {
    pub code: String,
    pub severity: UiDiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<UiSourceSpan>,
    pub revision: Revision,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiFragmentId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiSurfaceId(pub String);

/// Fragment-local declaration identity. This is never a public input or domain identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiNodeId(pub String);

/// Opaque lookup key for a render target owned by the destination WGPU runtime.
/// This is not a GPU handle, project asset identifier, or persistent reference.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderSurfaceRef {
    pub target_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiFragment {
    pub fragment_id: UiFragmentId,
    pub revision: Revision,
    pub root: UiNode,
    pub effects: Vec<UiEffect>,
}

/// Public statechart wire contract for a revisioned UI surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSurfaceSnapshot {
    pub schema_version: u16,
    pub surface_id: UiSurfaceId,
    pub revision: Revision,
    pub value: UiSurfaceState,
    pub available_events: Vec<UiSurfaceEventKind>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSurfaceState {
    pub diagnostics: UiDiagnosticsState,
    pub inspector: UiInspectorState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiDiagnosticsState {
    Collapsed,
    Expanded,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInspectorState {
    pub tab: UiInspectorTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiInspectorTab {
    Overview,
    Materials,
    History,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UiSurfaceEventKind {
    DiagnosticsToggle,
    InspectorTabSelect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UiSurfaceEvent {
    DiagnosticsToggle,
    InspectorTabSelect { tab: UiInspectorTab },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSurfaceEventRequest {
    pub schema_version: u16,
    pub surface_id: UiSurfaceId,
    pub event: UiSurfaceEvent,
}

/// The only payload accepted by `wgpu.ui.submit_fragment`.
/// It contains declarative UI data only; React/TS component state and render handles
/// are intentionally outside this contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiFragmentSubmission {
    pub schema_version: u16,
    pub fragment: UiFragment,
}

impl UiFragmentSubmission {
    pub fn new(fragment: UiFragment) -> Self {
        Self {
            schema_version: UI_FRAGMENT_SCHEMA_VERSION,
            fragment,
        }
    }

    pub fn validate(&self) -> Result<(), UiSchemaError> {
        if self.schema_version != UI_FRAGMENT_SCHEMA_VERSION {
            return Err(UiSchemaError::UnsupportedSchemaVersion);
        }
        self.fragment.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiNode {
    pub node_id: UiNodeId,
    pub kind: UiNodeKind,
    pub bounds: UiBounds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<UiLayout>,
    pub visible: bool,
    pub enabled: bool,
    pub text_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<AssetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<RenderSurfaceRef>,
    #[serde(default, skip_serializing_if = "UiStyle::is_default")]
    pub style: UiStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enter_transition: Option<UiTransition>,
    pub children: Vec<UiNode>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiNodeKind {
    Panel,
    Label,
    Button,
    Image,
    RenderSurface,
    TextInput,
    Checkbox,
    RadioButton,
    Slider,
    DragValue,
    Combo,
    Dropdown,
    /// A non-modal top-level presentation layer, normally anchored by its bounds.
    Tooltip,
    /// A modal top-level presentation layer with a renderer-owned backdrop.
    Modal,
    /// Alias of modal presentation for document-like dialog content.
    Dialog,
    Selectable,
    ListBox,
    Scrollbar,
    ProgressBar,
    /// A declarative, virtualized tabular viewport. Row data is supplied by
    /// bounded `UiDataGridFrame` windows rather than by runtime topology.
    DataGrid,
}

/// Source-independent text declaration. Renderers receive resolved immutable content only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextRef {
    Key {
        key: String,
        arguments: serde_json::Value,
    },
    Literal {
        value: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLayoutMode {
    Absolute,
    Overlay,
    Row,
    Column,
}

/// Child-overflow policy for a layout container. `Rounded` uses the node's
/// declared corner radius; `Scroll` clips identically to bounds while applying
/// the layout scroll offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiClipPolicy {
    None,
    #[default]
    Bounds,
    Rounded,
    Scroll,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiJustifyContent {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

impl UiJustifyContent {
    fn is_start(value: &Self) -> bool {
        *value == Self::Start
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAlignItems {
    #[default]
    Start,
    Center,
    End,
    Stretch,
}

impl UiAlignItems {
    fn is_start(value: &Self) -> bool {
        *value == Self::Start
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiLayout {
    pub mode: UiLayoutMode,
    pub padding: [f32; 4],
    pub margin: [f32; 4],
    pub gap: f32,
    pub min_size: Option<[f32; 2]>,
    pub max_size: Option<[f32; 2]>,
    pub preferred_size: Option<[f32; 2]>,
    /// Participates only when the parent uses row or column layout. A missing basis is auto:
    /// the declared size is used when nonzero, otherwise WGPU derives intrinsic text size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flex_basis: Option<f32>,
    /// Defaults preserve v1's fixed-size row and column behavior.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flex_grow: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub flex_shrink: f32,
    #[serde(default, skip_serializing_if = "UiJustifyContent::is_start")]
    pub justify_content: UiJustifyContent,
    #[serde(default, skip_serializing_if = "UiAlignItems::is_start")]
    pub align_items: UiAlignItems,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align_self: Option<UiAlignItems>,
    pub clip: UiClipPolicy,
    pub scroll_offset: [f32; 2],
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

impl Default for UiLayout {
    fn default() -> Self {
        Self {
            mode: UiLayoutMode::Absolute,
            padding: [0.0; 4],
            margin: [0.0; 4],
            gap: 0.0,
            min_size: None,
            max_size: None,
            preferred_size: None,
            flex_basis: None,
            flex_grow: 0.0,
            flex_shrink: 0.0,
            justify_content: UiJustifyContent::Start,
            align_items: UiAlignItems::Start,
            align_self: None,
            clip: UiClipPolicy::Bounds,
            scroll_offset: [0.0; 2],
        }
    }
}

/// Renderer-independent visual properties for a screen-space UI node.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiStyle {
    pub background_color: [f32; 4],
    pub border_color: [f32; 4],
    pub border_width: f32,
    pub corner_radius: f32,
    pub opacity: f32,
}

impl Default for UiStyle {
    fn default() -> Self {
        Self {
            background_color: [0.12, 0.14, 0.18, 1.0],
            border_color: [0.34, 0.42, 0.52, 0.7],
            border_width: 1.0,
            corner_radius: 4.0,
            opacity: 1.0,
        }
    }
}

/// A transition starts from the supplied overrides and ends at the node's bounds and style.
/// Omitted properties sample the currently rendered node state, making updates concise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTransition {
    #[serde(default)]
    pub delay_ms: u32,
    pub duration_ms: u32,
    #[serde(default)]
    pub easing: UiEasing,
    #[serde(default)]
    pub from: UiTransitionState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiEasing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Default for UiEasing {
    fn default() -> Self {
        Self::EaseOut
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiTransitionState {
    pub bounds: Option<UiBounds>,
    pub background_color: Option<[f32; 4]>,
    pub border_color: Option<[f32; 4]>,
    pub border_width: Option<f32>,
    pub corner_radius: Option<f32>,
    pub opacity: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiEffect {
    SemanticAction {
        action: String,
    },
    SemanticIntent {
        intent: UiIntent,
    },
    BoundSemanticIntent {
        node_id: UiNodeId,
        intent: UiIntent,
    },
    ControlPresentation {
        node_id: UiNodeId,
        state: UiControlPresentation,
    },
    DataGridFrame {
        declaration: UiDataGridDeclaration,
        frame: UiDataGridFrame,
    },
    DragBinding {
        binding: UiDragBinding,
    },
    DropBinding {
        binding: UiDropBinding,
    },
}

/// Domain-prepared visual value for a declared control. It contains only
/// renderer presentation data and cannot encode actions or business rules.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiControlPresentation {
    Toggle {
        selected: bool,
    },
    Numeric {
        value: f32,
        min: f32,
        max: f32,
    },
    Choice {
        token: String,
        options: Vec<String>,
        selected: bool,
    },
    Scroll {
        position: f32,
    },
}

/// Renderer-local pointer interaction policy. The keys name declared UI
/// semantics; renderer hit IDs and pointer coordinates never leave WGPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiDragAxis {
    Horizontal,
    Vertical,
    Both,
}

/// Bounds used by the renderer-local drag preview. This is presentation policy,
/// not a domain placement rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiDragBoundary {
    Parent,
    Surface,
    Free,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDragBinding {
    pub key: String,
    pub source_node_id: UiNodeId,
    pub axis: UiDragAxis,
    pub snap: f32,
    pub threshold: f32,
    pub boundary: UiDragBoundary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDropBinding {
    pub key: String,
    pub target_node_id: UiNodeId,
    pub accepts_drag_key: String,
    #[serde(default)]
    pub placement: UiDropPlacement,
    /// A bounded template owned by the `into` target. The domain uses it when
    /// constructing an accepted revision; the renderer never applies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_template_key: Option<String>,
    pub intent: UiIntent,
}

/// Semantic placement relative to a declared drop target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiDropPlacement {
    #[default]
    Into,
    Before,
    After,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiIntent {
    Invoke {
        action: String,
        params: serde_json::Value,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiSemanticEventType {
    PointerClick,
    ValuePreview,
    ValueCommit,
    SelectionChanged,
    TextInputCommit,
    DragDrop,
    FocusChanged,
    InteractionCancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiFragmentRevision {
    pub id: UiFragmentId,
    pub revision: Revision,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiPointerMetadata {
    pub id: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiFocusMetadata {
    pub focused: bool,
}

/// A committed input value. IME preedit never enters the UI runtime contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTextInputCommit {
    pub value: String,
}

/// Stable identity for a cell in the current bounded DataGrid window. This is
/// semantic identity only; row indices, renderer paths, and hit IDs stay local.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridCellTarget {
    pub source_key: String,
    pub stable_row_key: String,
    pub column_key: String,
}

/// The sole drag/drop data crossing the renderer boundary on release. The
/// presentation template is target policy for an accepted domain patch, not a
/// renderer instruction to mutate the canonical tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDragDropPayload {
    pub source_key: String,
    pub target_key: String,
    #[serde(default)]
    pub placement: UiDropPlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_template_key: Option<String>,
}

/// A renderer-resolved semantic event. It intentionally contains no render hit ID or node key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSemanticEvent {
    pub event: UiSemanticEventType,
    pub event_id: String,
    pub renderer_epoch: u64,
    pub composition_revision: Revision,
    pub fragment: UiFragmentRevision,
    pub intent: UiIntent,
    pub pointer: Option<UiPointerMetadata>,
    pub focus: Option<UiFocusMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_grid_cell: Option<UiDataGridCellTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<UiTextInputCommit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_value: Option<UiSemanticPayloadValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drag_drop: Option<UiDragDropPayload>,
}

pub const ERROR_RENDERER_EPOCH_MISMATCH: &str = "renderer_epoch_mismatch";
pub const ERROR_FRAGMENT_REVISION_STALE: &str = "fragment_revision_stale";
pub const ERROR_INTENT_NOT_BOUND: &str = "intent_not_bound";
pub const ERROR_INTERACTION_CANCELLED: &str = "interaction_cancelled";
pub const ERROR_FOCUS_INVALID: &str = "focus_invalid";
pub const ERROR_INPUT_SEQUENCE_STALE: &str = "input_sequence_stale";
pub const ERROR_DATA_GRID_CELL_INVALID: &str = "data_grid_cell_invalid";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiCommand {
    SubmitFragment {
        submission: UiFragmentSubmission,
    },
    RemoveFragment {
        fragment_id: UiFragmentId,
        revision: Revision,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiSchemaError {
    UnsupportedSchemaVersion,
    UnsupportedProgramSchemaVersion,
    UnsupportedProgramCapability,
    EmptyProgramId,
    InvalidProgramCapability,
    DuplicateProgramCapability,
    EmptyFragmentId,
    EmptyNodeId,
    EmptyAction,
    InvalidBounds,
    InvalidStyle,
    InvalidTransition,
    InvalidLayout,
    DuplicateNodeId,
    MissingImageAsset,
    MissingRenderSurfaceRef,
    InvalidRenderSurfaceRef,
    InvalidText,
    InvalidInputSchema,
    InvalidInputSlot,
    DuplicateInputKey,
    InvalidInputDefault,
    InvalidInputPacking,
    IncompatibleInputSchemaEvolution,
    InvalidTextRegistry,
    InvalidTextRecord,
    InvalidIrDocument,
    DuplicateIrNodeKey,
    InvalidBinding,
    InvalidBindingTarget,
    InvalidBindingType,
    InvalidProgramBudget,
    InvalidProgramEvent,
    MissingProgramResource,
}

/// Canonical, versioned authoring document. It remains data-only: bindings are
/// declared separately and can only target the finite property vocabulary below.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiIrDocument {
    pub schema_version: u16,
    pub surface_id: UiSurfaceId,
    pub revision: Revision,
    pub root: UiNode,
    #[serde(default)]
    pub bindings: Vec<UiIrBinding>,
    #[serde(default)]
    pub events: Vec<UiProgramEventDeclaration>,
    #[serde(default)]
    pub resources: Vec<UiProgramResource>,
    /// Finite subtrees selected by one direct input predicate. The subtree is
    /// already present in `root`; this table only supplies its runtime rule.
    #[serde(default)]
    pub branches: Vec<UiBranchDeclaration>,
    /// Precompiled row subtrees. A repeat frame selects at most `max_instances`
    /// copies; it can never create arbitrary topology.
    #[serde(default)]
    pub templates: Vec<UiTemplateDeclaration>,
    /// Bounded virtual-grid declarations attached to `DataGrid` nodes.
    #[serde(default)]
    pub data_grids: Vec<UiDataGridDeclaration>,
    pub resource_budget: UiResourceBudget,
}

/// A stable source location in the line-oriented NUI Flow notation. It is
/// authoring metadata only and never identifies a renderer hit target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NuiSourceSpan {
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NuiFlowParseDiagnostic {
    pub code: String,
    pub severity: UiDiagnosticSeverity,
    pub message: String,
    pub span: NuiSourceSpan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

/// Parsed Flow source and its deterministic lowering result. JSON IR remains
/// the canonical persisted representation; Flow is an authoring notation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NuiFlowDocument {
    pub version: u16,
    pub source: String,
    pub source_map: std::collections::BTreeMap<String, NuiSourceSpan>,
    pub ir: UiIrDocument,
    pub input_schema: UiInputSchema,
    #[serde(default)]
    pub state_machines: Vec<NuiFlowStateMachine>,
    #[serde(default)]
    pub drags: Vec<NuiFlowDragDeclaration>,
    #[serde(default)]
    pub drops: Vec<NuiFlowDropDeclaration>,
}

/// Finite UI-local statechart declared by NUI Flow. It may only control
/// presentation; domain mutations leave through declared semantic intents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NuiFlowStateMachine {
    pub key: String,
    pub initial_state: String,
    pub states: Vec<String>,
    #[serde(default)]
    pub transitions: Vec<NuiFlowStateTransition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NuiFlowStateTrigger {
    Sync,
    Intent { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NuiFlowStateTransition {
    pub from_state: String,
    pub trigger: NuiFlowStateTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<UiBranchPredicate>,
    pub target_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emit_intent: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NuiFlowDragAxis {
    Horizontal,
    Vertical,
    Both,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NuiFlowDragDeclaration {
    pub key: String,
    pub source_node_key: String,
    pub axis: NuiFlowDragAxis,
    pub snap: f32,
    pub threshold: f32,
    pub boundary: UiDragBoundary,
}

/// Declarative drop target. It proposes a revisioned semantic reparent command;
/// it never moves a running program node directly. An optional presentation
/// template is owned by an `into` target and guides domain patch construction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NuiFlowDropDeclaration {
    pub key: String,
    pub target_node_key: String,
    pub accepts_drag_key: String,
    #[serde(default)]
    pub placement: UiDropPlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_template_key: Option<String>,
    pub emit_intent: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiIrPatchOperationKind {
    Insert,
    Remove,
    Set,
    Move,
}

/// Revisioned canonical patch operation originating from Flow. Targets use
/// stable semantic keys/paths, never array indexes or renderer identities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiIrPatchOperation {
    pub kind: UiIrPatchOperationKind,
    pub target_path: String,
    pub expected_revision: Revision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    pub source_span: NuiSourceSpan,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiIrPatch {
    pub expected_revision: Revision,
    pub operations: Vec<UiIrPatchOperation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiBoundProperty {
    TextValue,
    Visible,
    Enabled,
    Selected,
    Active,
    NumericValue,
    ImageAsset,
    Opacity,
    StateToken,
    ScrollOffset,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiIrBinding {
    pub input_key: String,
    pub node_key: String,
    pub property: UiBoundProperty,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiBranchPredicate {
    Bool {
        input_key: String,
        #[serde(default = "default_true")]
        expected: bool,
    },
    EnumEquals {
        input_key: String,
        variant: String,
    },
    MachineState {
        machine_key: String,
        state: String,
    },
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiBranchLayoutParticipation {
    HiddenSubtree,
    RetainLayout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiBranchDeclaration {
    pub branch_key: String,
    pub root_node_key: String,
    pub predicate: UiBranchPredicate,
    pub layout_participation: UiBranchLayoutParticipation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTemplateDeclaration {
    pub template_key: String,
    pub root_node_key: String,
    pub max_instances: u32,
    pub row_schema: std::collections::BTreeMap<String, UiInputKind>,
    pub instance_key_field: String,
    #[serde(default)]
    pub overflow_summary: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiRepeatRow {
    pub stable_row_key: String,
    pub values: std::collections::BTreeMap<String, UiInputValue>,
    #[serde(default)]
    pub semantic_payload: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiRepeatFrame {
    pub template_key: String,
    pub list_revision: Revision,
    pub rows: Vec<UiRepeatRow>,
    pub expected_program_revision: UiProgramRevision,
}

/// Declarative bound for one virtualized grid viewport. The grid node owns its
/// stable key; a frame can replace only the currently visible row window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiDataGridPresentation {
    Text,
    Select {
        intent: String,
    },
    Dropdown {
        options: Vec<String>,
        intent: String,
    },
    Edit {
        max_chars: u32,
        intent: String,
    },
}

impl Default for UiDataGridPresentation {
    fn default() -> Self {
        Self::Text
    }
}

impl UiDataGridPresentation {
    pub fn validate(&self) -> bool {
        match self {
            Self::Text => true,
            Self::Select { intent } => !intent.trim().is_empty(),
            Self::Dropdown { options, intent } => {
                valid_data_grid_options(options) && !intent.trim().is_empty()
            }
            Self::Edit { max_chars, intent } => *max_chars > 0 && !intent.trim().is_empty(),
        }
    }
}

impl UiDataGridColumn {
    pub fn validate(&self) -> bool {
        !self.key.trim().is_empty()
            && !self.label.trim().is_empty()
            && self.width > 0
            && self.presentation.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiDataGridCellPresentation {
    Text,
    Dropdown { options: Vec<String> },
    Edit { max_chars: u32 },
}

impl UiDataGridCellPresentation {
    pub fn validate(&self) -> bool {
        match self {
            Self::Text => true,
            Self::Dropdown { options } => valid_data_grid_options(options),
            Self::Edit { max_chars } => *max_chars > 0,
        }
    }
}

fn valid_data_grid_options(options: &[String]) -> bool {
    !options.is_empty()
        && options.iter().all(|option| !option.trim().is_empty())
        && options
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            == options.len()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridColumn {
    pub key: String,
    pub label: String,
    pub width: u32,
    #[serde(default)]
    pub presentation: UiDataGridPresentation,
}

/// Declarative geometry and bounded data contract for one virtual grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridDeclaration {
    pub node_key: String,
    pub source_key: String,
    pub max_window_rows: u32,
    pub row_height: u32,
    pub overscan: u32,
    pub columns: Vec<UiDataGridColumn>,
}

impl UiDataGridDeclaration {
    pub fn validate(&self) -> bool {
        self.node_key.trim() != ""
            && self.source_key.trim() != ""
            && self.max_window_rows > 0
            && self.row_height > 0
            && self.overscan <= self.max_window_rows
            && !self.columns.is_empty()
            && self.columns.iter().all(UiDataGridColumn::validate)
            && self
                .columns
                .iter()
                .map(|column| &column.key)
                .collect::<std::collections::HashSet<_>>()
                .len()
                == self.columns.len()
    }
}

/// One domain-prepared row in a virtual grid window. Its key is stable across
/// window changes; row position is deliberately not an identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridWindowRow {
    pub stable_row_key: String,
    pub cells: std::collections::BTreeMap<String, UiDataGridCell>,
}

/// A bounded cell contains a typed domain value and its domain-provided display
/// handle. The UI runtime never formats or derives either value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridCell {
    pub value: UiInputValue,
    pub display: UiTextHandle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_override: Option<UiDataGridCellPresentation>,
}

impl UiDataGridCell {
    pub fn validate(&self) -> bool {
        data_grid_value_is_valid(&self.value)
            && self
                .presentation_override
                .as_ref()
                .is_none_or(UiDataGridCellPresentation::validate)
    }
}

fn data_grid_value_is_valid(value: &UiInputValue) -> bool {
    match value {
        UiInputValue::F32 { value } => value.is_finite(),
        UiInputValue::Vec2 { value } => value.iter().all(|value| value.is_finite()),
        UiInputValue::Vec4 { value } | UiInputValue::Color { value } => {
            value.iter().all(|value| value.is_finite())
        }
        UiInputValue::Enum { value } => !value.trim().is_empty(),
        _ => true,
    }
}

/// Revisioned bounded window for a declared virtual DataGrid. `first_row` is
/// the zero-based logical row offset and `total_rows` is the full domain count.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridFrame {
    pub list_revision: Revision,
    pub total_rows: u64,
    pub first_row: u64,
    pub window_rows: Vec<UiDataGridWindowRow>,
    pub expected_program_revision: UiProgramRevision,
}

/// One typed update for a declared control-plane grid input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridInputFrame {
    pub source_key: String,
    pub frame: UiDataGridFrame,
}

/// Renderer-to-UI-runtime demand for a bounded replacement window. This carries
/// only revisioned semantic identity; pointer coordinates and renderer hit IDs
/// remain local to the renderer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridWindowRequest {
    pub renderer_epoch: u64,
    pub composition_revision: Revision,
    pub fragment: UiFragmentRevision,
    pub source_key: String,
    pub expected_list_revision: Revision,
    pub requested_first_row: u64,
    pub max_window_rows: u32,
    pub sequence: u64,
}

/// A renderer request for a bounded window. The tagged wrapper leaves room for
/// additional windowed input kinds without changing the host inbound envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiWindowRequest {
    DataGrid { request: UiDataGridWindowRequest },
}

/// Generic data entering a UI host. Semantic events remain semantic data: a
/// host validates them but does not choose a domain destination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiHostInbound {
    WindowRequest {
        request: UiWindowRequest,
    },
    SemanticIntent {
        event: UiProgramSemanticEvent,
    },
    /// A completed renderer-local drag resolved against declared program keys.
    DragDrop {
        event: UiProgramDragDropEvent,
        active_fragment: UiHostFragmentContext,
    },
    /// A renderer-validated mutation for a currently published DataGrid cell.
    /// This preserves the cell target and typed payload for a generic host.
    DataGridCell {
        event: UiSemanticEvent,
    },
}

/// The active presentation supplied to a host for a structural update. Keeping
/// revisioned identity separate from the tree makes replacement identity an
/// explicit part of the host contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiHostFragmentContext {
    pub fragment: UiFragmentRevision,
    pub root: UiNode,
    pub effects: Vec<UiEffect>,
}

impl UiHostFragmentContext {
    pub fn from_fragment(fragment: &UiFragment) -> Self {
        Self {
            fragment: UiFragmentRevision {
                id: fragment.fragment_id.clone(),
                revision: fragment.revision,
            },
            root: fragment.root.clone(),
            effects: fragment.effects.clone(),
        }
    }

    pub fn into_fragment(self) -> UiFragment {
        UiFragment {
            fragment_id: self.fragment.id,
            revision: self.fragment.revision,
            root: self.root,
            effects: self.effects,
        }
    }
}

/// An externally accepted input publication. The host applies its scalar and
/// grid portions together or leaves the active input state unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiHostPublication {
    pub scalar_frame: UiInputFrame,
    #[serde(default)]
    pub grid_inputs: Vec<UiDataGridInputFrame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_update: Option<UiHostPresentationUpdate>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Replaces presentation only. The active adapter owns and carries forward all
/// scalar and grid inputs; hosts cannot include input state in this update.
pub struct UiHostPresentationUpdate {
    pub expected_fragment_revision: Revision,
    pub replacement_fragment: UiFragment,
    pub replacement_program: UiProgram,
    pub replacement_input_schema: UiInputSchema,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramEventDeclaration {
    pub node_key: String,
    pub intent: String,
    /// Compatibility list retained for existing IR documents. New declarations
    /// must use the typed literal or bound payload tables below.
    #[serde(default)]
    pub allowed_payload_keys: Vec<String>,
    #[serde(default)]
    pub literal_payload: std::collections::BTreeMap<String, UiSemanticPayloadValue>,
    #[serde(default)]
    pub bound_input_keys: Vec<String>,
}

/// The finite payload vocabulary accepted at the program semantic boundary.
/// It deliberately excludes arbitrary JSON, raw text, coordinates, GPU data,
/// and renderer-local identifiers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiSemanticPayloadValue {
    Bool { value: bool },
    I32 { value: i32 },
    U32 { value: u32 },
    F32 { value: f32 },
    Enum { value: String },
    TextHandle { value: UiTextHandle },
    AssetHandle { id: u64, generation: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiProgramSemanticEventKind {
    Activate,
    ValueTentative,
    ValueCommit,
    SelectionChanged,
    TextEditCommit,
    InteractionCancel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSemanticInteractionMetadata {
    pub interaction_id: String,
    pub sequence: u64,
    pub renderer_epoch: u64,
}

/// Program-native semantic event. Unlike the legacy fragment event, this is
/// revisioned against the compiled program and resolved input snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramSemanticEvent {
    pub event_id: String,
    pub kind: UiProgramSemanticEventKind,
    pub intent: String,
    pub source_node_key: String,
    pub payload: std::collections::BTreeMap<String, UiSemanticPayloadValue>,
    pub program_revision: UiProgramRevision,
    pub input_revision: Revision,
    pub request_id: String,
    pub idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_value: Option<UiSemanticPayloadValue>,
    pub interaction: UiSemanticInteractionMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiProgramSemanticEventStatus {
    Accepted,
    Rejected,
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramSemanticEventResult {
    pub event_id: String,
    pub status: UiProgramSemanticEventStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_input_revision: Option<Revision>,
    pub message: String,
}

/// Cross-process safe trace data. Renderer-local hit IDs and physical pointer
/// coordinates are intentionally absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiEventTraceRecord {
    pub sequence: u64,
    pub event_id: String,
    pub intent: String,
    pub source_node_key: String,
    pub program_revision: Revision,
    pub input_revision: Revision,
    pub renderer_epoch: u64,
    pub result: UiProgramSemanticEventStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub timestamp_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiProgramResourceKind {
    Image,
    RenderSurface,
    ThemeToken,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramResource {
    pub key: String,
    pub kind: UiProgramResourceKind,
    #[serde(default)]
    pub has_fallback: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiResourceBudget {
    pub max_nodes: u32,
    pub max_bindings: u32,
    pub max_instances: u32,
    pub max_text_records: u32,
    pub max_glyph_instances: u32,
    pub max_events: u32,
    pub max_clips: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramNode {
    pub key: String,
    pub parent_key: Option<String>,
    pub kind: UiNodeKind,
    pub source_span: Option<UiSourceSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramLiteralText {
    pub node_key: String,
    pub handle: UiTextHandle,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramLayoutRecord {
    pub node_key: String,
    pub bounds: UiBounds,
    pub layout: Option<UiLayout>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiBinding {
    pub binding_id: u32,
    pub input_key: String,
    pub node_key: String,
    pub property: UiBoundProperty,
    pub expected_kind: UiInputKind,
    pub default_resolved_value: UiInputValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDependencyIndex {
    pub input_to_bindings: std::collections::BTreeMap<String, Vec<u32>>,
    pub node_to_source_span: std::collections::BTreeMap<String, Option<UiSourceSpan>>,
    pub node_to_dependents: std::collections::BTreeMap<String, Vec<u32>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiBranchRecord {
    pub branch_key: String,
    pub predicate: UiBranchPredicate,
    pub node_range: Vec<String>,
    pub layout_participation: UiBranchLayoutParticipation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTemplateRecord {
    pub template_key: String,
    pub node_range: Vec<String>,
    pub max_instances: u32,
    pub row_schema: std::collections::BTreeMap<String, UiInputKind>,
    pub instance_key_field: String,
    pub overflow_summary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDataGridRecord {
    pub node_key: String,
    pub source_key: String,
    pub max_window_rows: u32,
    pub row_height: u32,
    pub overscan: u32,
    pub columns: Vec<UiDataGridColumn>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramDragRecord {
    pub key: String,
    pub source_node_key: String,
    pub axis: UiDragAxis,
    pub snap: f32,
    pub threshold: f32,
    pub boundary: UiDragBoundary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramDropRecord {
    pub key: String,
    pub target_node_key: String,
    pub accepts_drag_key: String,
    pub placement: UiDropPlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_template_key: Option<String>,
    pub intent: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgram {
    pub revision: UiProgramRevision,
    pub nodes: Vec<UiProgramNode>,
    pub node_templates: Vec<UiNode>,
    pub literal_texts: Vec<UiProgramLiteralText>,
    pub layout_records: Vec<UiProgramLayoutRecord>,
    pub binding_records: Vec<UiBinding>,
    pub branch_records: Vec<UiBranchRecord>,
    pub template_records: Vec<UiTemplateRecord>,
    pub data_grid_records: Vec<UiDataGridRecord>,
    #[serde(default)]
    pub drag_records: Vec<UiProgramDragRecord>,
    #[serde(default)]
    pub drop_records: Vec<UiProgramDropRecord>,
    pub event_records: Vec<UiProgramEventDeclaration>,
    pub resource_budget: UiResourceBudget,
    pub dependency_index: UiDependencyIndex,
    pub layout_hash: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiCpuNodeState {
    pub node_key: String,
    pub visible: bool,
    pub enabled: bool,
    pub selected: bool,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_value: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_token: Option<String>,
    pub text: Option<UiTextHandle>,
    pub image: Option<UiInputValue>,
    pub opacity: f32,
    pub scroll_offset: [f32; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiCpuRenderPrimitive {
    pub node_key: String,
    pub kind: UiNodeKind,
    pub bounds: UiBounds,
    pub clip: Option<UiBounds>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiCpuSemanticTarget {
    pub node_key: String,
    pub intents: Vec<String>,
    pub enabled: bool,
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiCpuFrameOutput {
    pub program_revision: UiProgramRevision,
    pub input_revision: Revision,
    pub nodes: Vec<UiCpuNodeState>,
    pub logical_layout: Vec<UiProgramLayoutRecord>,
    pub clips: std::collections::BTreeMap<String, UiBounds>,
    pub render_primitives: Vec<UiCpuRenderPrimitive>,
    pub semantic_targets: Vec<UiCpuSemanticTarget>,
    pub diagnostics: Vec<UiDiagnostic>,
}

/// Compact, semantic inspection data. These records deliberately use stable
/// node keys and logical bounds; render hit IDs and physical pixels are absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiIrOutlineEntry {
    pub node_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_key: Option<String>,
    pub kind: UiNodeKind,
    pub static_properties: std::collections::BTreeMap<String, serde_json::Value>,
    pub binding_summary: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<UiSourceSpan>,
    pub diagnostic_count: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiIrOutlinePage {
    pub entries: Vec<UiIrOutlineEntry>,
    pub offset: u32,
    pub limit: u32,
    pub total: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiNodeInspection {
    pub node: UiProgramNode,
    pub declared_properties: serde_json::Value,
    pub effective_properties: serde_json::Value,
    pub provenance: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<UiProgramLayoutRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<UiBounds>,
    pub visibility_reason: String,
    pub resources: Vec<UiProgramResource>,
    pub events: Vec<UiProgramEventDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_span: Option<UiSourceSpan>,
    pub diagnostics: Vec<UiDiagnostic>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiPatchDryRun {
    pub accepted: bool,
    pub base_revision: Revision,
    pub resulting_revision: Revision,
    pub diff: serde_json::Value,
    pub impacted_nodes: Vec<String>,
    pub required_input_schema_changes: Vec<String>,
    pub budget: UiResourceBudget,
    pub diagnostics: Vec<UiDiagnostic>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiLayoutDiagnosticSnapshot {
    pub program_revision: UiProgramRevision,
    pub input_revision: Revision,
    pub logical_layout: Vec<UiProgramLayoutRecord>,
    pub clips: std::collections::BTreeMap<String, UiBounds>,
    pub visibility_reasons: std::collections::BTreeMap<String, String>,
    pub diagnostics: Vec<UiDiagnostic>,
    #[serde(default)]
    pub gpu_differential_mismatches: Vec<UiDiagnostic>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramDescription {
    pub revision: UiProgramRevision,
    pub layout_hash: String,
    pub active_capabilities: Vec<UiProgramCapability>,
    pub resource_budget: UiResourceBudget,
    pub runtime_high_water_marks: std::collections::BTreeMap<String, u32>,
    pub overflow_counters: std::collections::BTreeMap<String, u64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDebugBundle {
    pub version: u16,
    pub flow_source_hash: String,
    pub ir_hash: String,
    pub program: UiProgram,
    pub schema: UiInputSchema,
    pub initial_inputs: UiResolvedInputs,
    pub input_timeline: Vec<UiInputFrame>,
    pub repeat_timeline: Vec<UiRepeatFrame>,
    pub text_registry: UiTextRegistryDebugSnapshot,
    pub event_timeline: Vec<UiProgramSemanticEvent>,
    pub viewport: UiCpuViewport,
    pub expected_frames: Vec<UiCpuFrameOutput>,
    pub diagnostics: Vec<UiDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_readbacks: Option<Vec<UiGpuLayoutReadback>>,
}

/// Renderer-owned GPU state exposed only as revisioned diagnostics. Buffer
/// handles deliberately do not cross this contract boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiGpuFrameState {
    pub renderer_epoch: u64,
    pub program_revision: UiProgramRevision,
    pub input_revision: Revision,
    pub dirty_slots: Vec<String>,
    pub frame_sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiGpuLayoutNode {
    pub node_key: String,
    pub bounds: UiBounds,
    pub clip: Option<UiBounds>,
    pub visible: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiGpuLayoutReadback {
    pub renderer_epoch: u64,
    pub program_revision: UiProgramRevision,
    pub input_revision: Revision,
    pub nodes: Vec<UiGpuLayoutNode>,
    pub diagnostics: Vec<UiDiagnostic>,
    pub sampled_frame: u64,
    pub asynchronous: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiGpuPassTiming {
    pub program_upload_us: u64,
    pub input_upload_us: u64,
    pub binding_us: u64,
    pub layout_us: u64,
    pub instance_us: u64,
    pub render_us: u64,
    pub readback_us: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiGpuUploadStatus {
    Empty,
    Staged,
    Active,
    RejectedCapacity,
}
/// Public adapter summary. GPU objects remain private to the renderer process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiGpuBackendAdapter {
    pub renderer_epoch: u64,
    pub program_revision: Option<UiProgramRevision>,
    pub input_revision: Option<Revision>,
    pub upload_status: UiGpuUploadStatus,
    pub capacity: UiResourceBudget,
    pub diagnostics: Vec<UiDiagnostic>,
    pub last_timing: UiGpuPassTiming,
}

/// Domain-prepared values consumed by the terrain workbench. These groups are
/// an inspection convenience only; the program still receives individual,
/// typed input slots and bounded repeat frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainWorkbenchUiInputs {
    pub text_handles: std::collections::BTreeMap<String, UiTextHandle>,
    pub tool_selection: String,
    pub eligibility: std::collections::BTreeMap<String, bool>,
    pub view_state: String,
    pub controlled_values: std::collections::BTreeMap<String, UiInputValue>,
    pub bounded_rows: std::collections::BTreeMap<String, u32>,
    pub diagnostic_state: String,
}

/// Logical CPU/GPU comparison evidence for one named, headless scenario.
/// `differences` contains diagnostics instead of renderer-private identities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDifferentialScenarioResult {
    pub scenario: String,
    pub program_revision: UiProgramRevision,
    pub input_revision: Revision,
    pub cpu_snapshot: UiCpuFrameOutput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_snapshot: Option<UiGpuLayoutReadback>,
    pub differences: Vec<UiDiagnostic>,
    pub status: String,
}

/// Persistable acceptance metadata. It records automated evidence and leaves
/// interactive acceptance explicitly user-owned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiGpuReactiveAcceptanceRecord {
    pub environment: std::collections::BTreeMap<String, String>,
    pub program_hash: String,
    pub capacities: UiResourceBudget,
    pub timings: UiGpuPassTiming,
    pub automated_results: Vec<UiDifferentialScenarioResult>,
    pub manual_results: String,
    pub residual_risks: Vec<String>,
}

/// CPU evaluator input expressed exclusively in logical layout units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiCpuViewport {
    pub logical_bounds: UiBounds,
    pub revision: Revision,
}

impl UiFragment {
    pub fn validate(&self) -> Result<(), UiSchemaError> {
        if self.fragment_id.0.trim().is_empty() {
            return Err(UiSchemaError::EmptyFragmentId);
        }
        self.root.validate()?;
        for effect in &self.effects {
            effect.validate()?;
        }
        let mut nodes = std::collections::HashSet::new();
        fn collect(node: &UiNode, nodes: &mut std::collections::HashSet<String>) {
            nodes.insert(node.node_id.0.clone());
            for child in &node.children {
                collect(child, nodes);
            }
        }
        collect(&self.root, &mut nodes);
        let drags = self
            .effects
            .iter()
            .filter_map(|effect| match effect {
                UiEffect::DragBinding { binding } => Some(binding),
                _ => None,
            })
            .collect::<Vec<_>>();
        for effect in &self.effects {
            match effect {
                UiEffect::DragBinding { binding } if !nodes.contains(&binding.source_node_id.0) => {
                    return Err(UiSchemaError::InvalidProgramEvent);
                }
                UiEffect::DropBinding { binding }
                    if !nodes.contains(&binding.target_node_id.0)
                        || !drags
                            .iter()
                            .any(|drag| drag.key == binding.accepts_drag_key) =>
                {
                    return Err(UiSchemaError::InvalidProgramEvent);
                }
                UiEffect::ControlPresentation { node_id, .. } if !nodes.contains(&node_id.0) => {
                    return Err(UiSchemaError::InvalidProgramEvent);
                }
                UiEffect::DataGridFrame { declaration, .. }
                    if !nodes.contains(&declaration.node_key) =>
                {
                    return Err(UiSchemaError::InvalidProgramEvent);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl UiNode {
    fn validate(&self) -> Result<(), UiSchemaError> {
        if self.node_id.0.trim().is_empty() {
            return Err(UiSchemaError::EmptyNodeId);
        }
        if !self.bounds.is_valid() {
            return Err(UiSchemaError::InvalidBounds);
        }
        if !self.style.is_valid() {
            return Err(UiSchemaError::InvalidStyle);
        }
        if self.kind == UiNodeKind::Image && self.image.is_none() {
            return Err(UiSchemaError::MissingImageAsset);
        }
        if self.kind == UiNodeKind::RenderSurface && self.surface.is_none() {
            return Err(UiSchemaError::MissingRenderSurfaceRef);
        }
        if self
            .surface
            .as_ref()
            .is_some_and(|surface| surface.target_id.trim().is_empty())
        {
            return Err(UiSchemaError::InvalidRenderSurfaceRef);
        }
        if self.text.as_ref().is_some_and(|text| !text.is_valid()) {
            return Err(UiSchemaError::InvalidText);
        }
        if self.layout.is_some_and(|layout| !layout.is_valid()) {
            return Err(UiSchemaError::InvalidLayout);
        }
        let mut child_ids = std::collections::HashSet::new();
        for child in &self.children {
            if !child_ids.insert(child.node_id.0.as_str()) {
                return Err(UiSchemaError::DuplicateNodeId);
            }
        }
        if let Some(transition) = &self.enter_transition
            && !transition.is_valid()
        {
            return Err(UiSchemaError::InvalidTransition);
        }
        for child in &self.children {
            child.validate()?;
        }
        Ok(())
    }
}

impl UiBounds {
    pub fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= 0.0
            && self.height >= 0.0
    }
}

impl UiLayout {
    fn is_valid(self) -> bool {
        self.padding
            .iter()
            .chain(self.margin.iter())
            .all(|value| value.is_finite() && *value >= 0.0)
            && self.gap.is_finite()
            && self.gap >= 0.0
            && self.scroll_offset.iter().all(|value| value.is_finite())
            && self
                .flex_basis
                .is_none_or(|value| value.is_finite() && value >= 0.0)
            && self.flex_grow.is_finite()
            && self.flex_grow >= 0.0
            && self.flex_shrink.is_finite()
            && self.flex_shrink >= 0.0
            && self
                .min_size
                .is_none_or(|size| size.iter().all(|value| value.is_finite() && *value >= 0.0))
            && self
                .max_size
                .is_none_or(|size| size.iter().all(|value| value.is_finite() && *value >= 0.0))
            && self
                .preferred_size
                .is_none_or(|size| size.iter().all(|value| value.is_finite() && *value >= 0.0))
    }
}

impl UiStyle {
    pub fn is_default(value: &Self) -> bool {
        *value == Self::default()
    }

    pub fn is_valid(self) -> bool {
        self.background_color.iter().all(|value| value.is_finite())
            && self.border_color.iter().all(|value| value.is_finite())
            && self.border_width.is_finite()
            && self.corner_radius.is_finite()
            && self.opacity.is_finite()
            && self.border_width >= 0.0
            && self.corner_radius >= 0.0
            && (0.0..=1.0).contains(&self.opacity)
    }
}

impl UiTransition {
    pub fn is_valid(&self) -> bool {
        self.duration_ms > 0 && self.from.is_valid()
    }
}

impl UiTransitionState {
    fn is_valid(self) -> bool {
        self.bounds.is_none_or(UiBounds::is_valid)
            && self
                .background_color
                .is_none_or(|color| color.iter().all(|value| value.is_finite()))
            && self
                .border_color
                .is_none_or(|color| color.iter().all(|value| value.is_finite()))
            && self
                .border_width
                .is_none_or(|value| value.is_finite() && value >= 0.0)
            && self
                .corner_radius
                .is_none_or(|value| value.is_finite() && value >= 0.0)
            && self
                .opacity
                .is_none_or(|value| value.is_finite() && (0.0..=1.0).contains(&value))
    }
}

impl UiEffect {
    fn validate(&self) -> Result<(), UiSchemaError> {
        match self {
            Self::SemanticAction { action } if action.trim().is_empty() => {
                Err(UiSchemaError::EmptyAction)
            }
            Self::SemanticAction { .. } => Ok(()),
            Self::SemanticIntent { intent } => intent.validate(),
            Self::BoundSemanticIntent { intent, .. } => intent.validate(),
            Self::ControlPresentation { node_id, state } => {
                if node_id.0.trim().is_empty() {
                    return Err(UiSchemaError::InvalidProgramEvent);
                }
                match state {
                    UiControlPresentation::Toggle { .. } | UiControlPresentation::Choice { .. } => {
                        Ok(())
                    }
                    UiControlPresentation::Numeric { value, min, max } => {
                        if value.is_finite() && min.is_finite() && max.is_finite() && min < max {
                            Ok(())
                        } else {
                            Err(UiSchemaError::InvalidProgramEvent)
                        }
                    }
                    UiControlPresentation::Scroll { position } => {
                        if position.is_finite() && (0.0..=1.0).contains(position) {
                            Ok(())
                        } else {
                            Err(UiSchemaError::InvalidProgramEvent)
                        }
                    }
                }
            }
            Self::DataGridFrame { declaration, frame } => {
                if !declaration.validate()
                    || frame.window_rows.len() > declaration.max_window_rows as usize
                    || frame
                        .first_row
                        .saturating_add(frame.window_rows.len() as u64)
                        > frame.total_rows
                    || frame
                        .window_rows
                        .iter()
                        .any(|row| row.cells.values().any(|cell| !cell.validate()))
                {
                    Err(UiSchemaError::InvalidProgramEvent)
                } else {
                    Ok(())
                }
            }
            Self::DragBinding { binding } => {
                if binding.key.trim().is_empty()
                    || binding.source_node_id.0.trim().is_empty()
                    || !binding.snap.is_finite()
                    || !binding.threshold.is_finite()
                    || binding.snap < 0.0
                    || binding.threshold < 0.0
                {
                    Err(UiSchemaError::InvalidProgramEvent)
                } else {
                    Ok(())
                }
            }
            Self::DropBinding { binding } => {
                if binding.key.trim().is_empty()
                    || binding.target_node_id.0.trim().is_empty()
                    || binding.accepts_drag_key.trim().is_empty()
                {
                    Err(UiSchemaError::InvalidProgramEvent)
                } else {
                    binding.intent.validate()
                }
            }
        }
    }
}

impl UiIntent {
    pub fn validate(&self) -> Result<(), UiSchemaError> {
        match self {
            Self::Invoke { action, .. } if action.trim().is_empty() => {
                Err(UiSchemaError::EmptyAction)
            }
            Self::Invoke { .. } => Ok(()),
        }
    }
}

impl TextRef {
    fn is_valid(&self) -> bool {
        match self {
            Self::Key { key, .. } => !key.trim().is_empty(),
            Self::Literal { value } => !value.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const STATIC_FRAGMENT: &str = include_str!("../../../tests/fixtures/ui/static-fragment.json");

    #[test]
    fn static_fragment_fixture_round_trips() {
        let fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        fragment.validate().unwrap();
        assert_eq!(
            serde_json::to_value(fragment).unwrap(),
            serde_json::from_str::<Value>(STATIC_FRAGMENT).unwrap()
        );
    }

    #[test]
    fn data_grid_presentations_default_for_legacy_json_and_reject_invalid_values() {
        let column: UiDataGridColumn = serde_json::from_value(serde_json::json!({
            "key": "name", "label": "Name", "width": 120
        }))
        .unwrap();
        assert_eq!(column.presentation, UiDataGridPresentation::Text);
        let cell: UiDataGridCell = serde_json::from_value(serde_json::json!({
            "value": { "kind": "i32", "value": 1 },
            "display": { "id": 1, "generation": 1 }
        }))
        .unwrap();
        assert_eq!(cell.presentation_override, None);
        assert!(
            !UiDataGridPresentation::Dropdown {
                options: vec!["same".into(), "same".into()],
                intent: "set".into()
            }
            .validate()
        );
        assert!(!UiDataGridCellPresentation::Edit { max_chars: 0 }.validate());
    }

    #[test]
    fn drag_drop_payload_carries_only_target_presentation_policy() {
        let payload = UiDragDropPayload {
            source_key: "backlog-card-01".into(),
            target_key: "done-panel".into(),
            placement: UiDropPlacement::Into,
            presentation_template_key: Some("accepted-template".into()),
        };
        assert_eq!(
            serde_json::to_value(payload).unwrap(),
            serde_json::json!({
                    "source_key": "backlog-card-01",
                    "target_key": "done-panel",
                    "placement": "into",
                    "presentation_template_key": "accepted-template"
            })
        );
    }

    #[test]
    fn surface_machine_event_and_snapshot_use_stable_json_contracts() {
        let request: UiSurfaceEventRequest = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "surface_id": "surface.ui-workbench",
            "event": { "type": "INSPECTOR_TAB_SELECT", "tab": "materials" }
        }))
        .unwrap();
        assert_eq!(
            request.event,
            UiSurfaceEvent::InspectorTabSelect {
                tab: UiInspectorTab::Materials
            }
        );
        let snapshot = UiSurfaceSnapshot {
            schema_version: UI_SURFACE_SCHEMA_VERSION,
            surface_id: UiSurfaceId("surface.ui-workbench".into()),
            revision: Revision(7),
            value: UiSurfaceState {
                diagnostics: UiDiagnosticsState::Expanded,
                inspector: UiInspectorState {
                    tab: UiInspectorTab::Materials,
                },
            },
            available_events: vec![
                UiSurfaceEventKind::DiagnosticsToggle,
                UiSurfaceEventKind::InspectorTabSelect,
            ],
        };
        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            serde_json::json!({
                    "schema_version": 1,
                    "surface_id": "surface.ui-workbench",
                    "revision": 7,
                    "value": { "diagnostics": "expanded", "inspector": { "tab": "materials" } },
                    "available_events": ["DIAGNOSTICS_TOGGLE", "INSPECTOR_TAB_SELECT"]
            })
        );
    }

    #[test]
    fn submission_is_versioned_declarative_data() {
        let fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        let submission = UiFragmentSubmission::new(fragment);
        submission.validate().unwrap();
        let encoded = serde_json::to_value(&submission).unwrap().to_string();
        assert!(encoded.contains("schema_version"));
        for forbidden in ["react", "jsx", "callback", "wgpu", "window", "handle"] {
            assert!(
                !encoded.contains(forbidden),
                "submission contains {forbidden}"
            );
        }
    }

    #[test]
    fn submission_rejects_unknown_schema_version() {
        let fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        let submission = UiFragmentSubmission {
            schema_version: UI_FRAGMENT_SCHEMA_VERSION + 1,
            fragment,
        };
        assert_eq!(
            submission.validate(),
            Err(UiSchemaError::UnsupportedSchemaVersion)
        );
    }

    #[test]
    fn empty_children_multiple_children_and_disabled_button_are_valid() {
        let fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        assert_eq!(fragment.root.children.len(), 2);
        assert!(fragment.root.children[0].children.is_empty());
        assert!(!fragment.root.children[1].enabled);
        fragment.validate().unwrap();
    }

    #[test]
    fn invalid_bounds_are_rejected() {
        let mut fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        fragment.root.bounds.width = -1.0;
        assert_eq!(fragment.validate(), Err(UiSchemaError::InvalidBounds));
    }

    #[test]
    fn transition_is_validated_without_renderer_dependencies() {
        let mut fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        fragment.root.enter_transition = Some(UiTransition {
            delay_ms: 0,
            duration_ms: 180,
            easing: UiEasing::EaseOut,
            from: UiTransitionState {
                opacity: Some(0.0),
                bounds: Some(UiBounds {
                    x: 0.0,
                    y: 8.0,
                    width: 320.0,
                    height: 160.0,
                }),
                ..UiTransitionState::default()
            },
        });
        fragment.validate().unwrap();
        fragment.root.enter_transition.as_mut().unwrap().duration_ms = 0;
        assert_eq!(fragment.validate(), Err(UiSchemaError::InvalidTransition));
    }

    #[test]
    fn empty_fragment_id_is_rejected() {
        let mut fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        fragment.fragment_id = UiFragmentId(" ".into());
        assert_eq!(fragment.validate(), Err(UiSchemaError::EmptyFragmentId));
    }

    #[test]
    fn unknown_node_kind_is_rejected_by_deserialization() {
        let invalid = STATIC_FRAGMENT.replace("\"panel\"", "\"unknown\"");
        assert!(serde_json::from_str::<UiFragment>(&invalid).is_err());
    }

    #[test]
    fn serialized_fragment_has_no_renderer_or_local_path_handle() {
        let fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        let encoded = serde_json::to_value(fragment).unwrap().to_string();
        for forbidden in ["wgpu", "winit", "window", "local_path", "handle"] {
            assert!(
                !encoded.contains(forbidden),
                "serialized declaration contains {forbidden}"
            );
        }
    }

    #[test]
    fn semantic_event_serialization_has_no_renderer_local_or_coordinate_data() {
        let event = UiSemanticEvent {
            event: UiSemanticEventType::PointerClick,
            event_id: "event-1".into(),
            renderer_epoch: 7,
            composition_revision: Revision(3),
            fragment: UiFragmentRevision {
                id: UiFragmentId("terrain-tools".into()),
                revision: Revision(2),
            },
            intent: UiIntent::Invoke {
                action: "terrain.tool.select".into(),
                params: serde_json::json!({"tool": "water_inject"}),
            },
            pointer: Some(UiPointerMetadata { id: 0, sequence: 4 }),
            focus: None,
            data_grid_cell: Some(UiDataGridCellTarget {
                source_key: "assets_window".into(),
                stable_row_key: "asset-42".into(),
                column_key: "status".into(),
            }),
            text: None,
            control_value: None,
            drag_drop: None,
        };
        let encoded = serde_json::to_value(event).unwrap();
        assert!(encoded.get("render_hit_id").is_none());
        assert!(encoded.get("node_id").is_none());
        assert!(encoded.get("node_path").is_none());
        assert!(encoded.get("pixel_position").is_none());
        assert!(encoded.get("logical_position").is_none());
        assert_eq!(encoded["data_grid_cell"]["source_key"], "assets_window");
        assert!(encoded["data_grid_cell"].get("data_grid_key").is_none());
        assert_eq!(encoded["data_grid_cell"]["stable_row_key"], "asset-42");
    }

    #[test]
    fn data_grid_window_request_uses_source_identity_without_renderer_paths() {
        let request = UiDataGridWindowRequest {
            renderer_epoch: 7,
            composition_revision: Revision(3),
            fragment: UiFragmentRevision {
                id: UiFragmentId("asset-list".into()),
                revision: Revision(2),
            },
            source_key: "asset_window".into(),
            expected_list_revision: Revision(5),
            requested_first_row: 96,
            max_window_rows: 24,
            sequence: 4,
        };
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["source_key"], "asset_window");
        assert!(encoded.get("data_grid_key").is_none());
        assert!(encoded.get("node_key").is_none());
        assert!(encoded.get("node_path").is_none());
        assert!(encoded.get("pointer").is_none());
    }

    #[test]
    fn public_schema_has_no_renderer_or_web_runtime_dependency() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in ["wgpu", "winit", "react", "typescript", "tauri", "webview"] {
            assert!(
                !manifest
                    .lines()
                    .any(|line| line.trim_start().starts_with(&format!("{forbidden} ="))),
                "public UI schema must not depend on {forbidden}"
            );
        }
    }

    fn layout_node(mode: UiLayoutMode) -> UiNode {
        UiNode {
            node_id: UiNodeId("root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 60.0,
            },
            layout: Some(UiLayout {
                mode,
                padding: [4.0, 4.0, 4.0, 4.0],
                gap: 3.0,
                ..UiLayout::default()
            }),
            visible: true,
            enabled: true,
            text_key: None,
            text: None,
            image: None,
            surface: None,
            style: UiStyle::default(),
            enter_transition: None,
            children: vec![
                UiNode {
                    node_id: UiNodeId("a".into()),
                    kind: UiNodeKind::Button,
                    bounds: UiBounds {
                        x: 0.0,
                        y: 0.0,
                        width: 20.0,
                        height: 10.0,
                    },
                    layout: Some(UiLayout {
                        margin: [1.0, 2.0, 3.0, 4.0],
                        ..UiLayout::default()
                    }),
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
                UiNode {
                    node_id: UiNodeId("b".into()),
                    kind: UiNodeKind::Button,
                    bounds: UiBounds {
                        x: 0.0,
                        y: 0.0,
                        width: 20.0,
                        height: 10.0,
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
            ],
        }
    }

    #[test]
    fn layout_contract_validates_flex_values_without_resolving_renderer_layout() {
        let mut node = layout_node(UiLayoutMode::Absolute);
        node.layout.as_mut().unwrap().flex_grow = 1.0;
        node.layout.as_mut().unwrap().flex_basis = Some(32.0);
        node.validate().unwrap();
        node.layout.as_mut().unwrap().flex_shrink = -1.0;
        assert_eq!(node.validate(), Err(UiSchemaError::InvalidLayout));
    }

    #[test]
    fn duplicate_sibling_node_identity_is_rejected_as_layout_error() {
        let mut node = layout_node(UiLayoutMode::Overlay);
        node.children[1].node_id = node.children[0].node_id.clone();
        assert_eq!(node.validate(), Err(UiSchemaError::DuplicateNodeId));
    }

    #[test]
    fn image_nodes_require_a_stable_asset_ref_without_a_path() {
        let mut node = layout_node(UiLayoutMode::Absolute);
        node.kind = UiNodeKind::Image;
        assert_eq!(node.validate(), Err(UiSchemaError::MissingImageAsset));
        node.image = Some(AssetRef {
            project_id: "fixture-project".into(),
            asset_id: 81,
            revision: Revision(5),
            kind: "image".into(),
        });
        node.validate().unwrap();
        let value = serde_json::to_value(node).unwrap();
        assert!(value["image"].get("path").is_none());
        assert_eq!(value["image"]["asset_id"], 81);
    }

    #[test]
    fn render_surface_nodes_require_an_opaque_runtime_target() {
        let mut node = layout_node(UiLayoutMode::Absolute);
        node.kind = UiNodeKind::RenderSurface;
        assert_eq!(node.validate(), Err(UiSchemaError::MissingRenderSurfaceRef));
        node.surface = Some(RenderSurfaceRef {
            target_id: " ".into(),
        });
        assert_eq!(node.validate(), Err(UiSchemaError::InvalidRenderSurfaceRef));
        node.surface = Some(RenderSurfaceRef {
            target_id: "ai.terrain.preview".into(),
        });
        node.validate().unwrap();
        let value = serde_json::to_value(node).unwrap();
        assert_eq!(value["kind"], "render_surface");
        assert_eq!(value["surface"]["target_id"], "ai.terrain.preview");
        for forbidden in ["wgpu", "texture", "handle", "project_id", "asset_id"] {
            assert!(value["surface"].get(forbidden).is_none());
        }
    }

    #[test]
    fn text_ref_is_source_independent_and_rejects_empty_content() {
        let mut node = layout_node(UiLayoutMode::Absolute);
        node.text = Some(TextRef::Key {
            key: "ui.fixture.label".into(),
            arguments: serde_json::json!({"count": 2}),
        });
        node.validate().unwrap();
        let encoded = serde_json::to_value(&node).unwrap().to_string();
        for forbidden in ["path", "font_file", "wgpu", "handle"] {
            assert!(
                !encoded.contains(forbidden),
                "text declaration contains {forbidden}"
            );
        }
        node.text = Some(TextRef::Literal {
            value: String::new(),
        });
        assert_eq!(node.validate(), Err(UiSchemaError::InvalidText));
    }

    fn baseline_program_revision() -> UiProgramRevision {
        UiProgramRevision {
            program_id: "surface.editor.terrain-workbench".into(),
            revision: Revision(12),
            schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: vec![UiProgramCapability {
                name: UI_PROGRAM_CAPABILITY_NAME.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Experimental,
            }],
        }
    }

    #[test]
    fn program_revision_capability_round_trips_without_changing_fragment_v1() {
        let revision = baseline_program_revision();
        revision.validate_baseline().unwrap();
        let encoded = serde_json::to_value(&revision).unwrap();
        assert_eq!(encoded["schema_version"], UI_PROGRAM_SCHEMA_VERSION);
        assert_eq!(
            encoded["capabilities"][0]["name"],
            UI_PROGRAM_CAPABILITY_NAME
        );
        assert_eq!(
            serde_json::from_value::<UiProgramRevision>(encoded).unwrap(),
            revision
        );
        assert_eq!(UI_FRAGMENT_SCHEMA_VERSION, 1);
    }

    #[test]
    fn program_revision_rejects_unknown_schema_and_capability_versions() {
        let mut revision = baseline_program_revision();
        revision.schema_version += 1;
        assert_eq!(
            revision.validate_baseline(),
            Err(UiSchemaError::UnsupportedProgramSchemaVersion)
        );

        let mut revision = baseline_program_revision();
        revision.capabilities[0].version += 1;
        assert_eq!(
            revision.validate_baseline(),
            Err(UiSchemaError::UnsupportedProgramCapability)
        );

        let mut revision = baseline_program_revision();
        revision.capabilities.clear();
        assert_eq!(
            revision.validate_baseline(),
            Err(UiSchemaError::UnsupportedProgramCapability)
        );
    }

    #[test]
    fn program_revision_rejects_duplicate_capabilities_and_unknown_fields() {
        let mut revision = baseline_program_revision();
        revision.capabilities.push(revision.capabilities[0].clone());
        assert_eq!(
            revision.validate_baseline(),
            Err(UiSchemaError::DuplicateProgramCapability)
        );

        let invalid = serde_json::json!({
            "program_id": "surface.editor",
            "revision": 1,
            "schema_version": 1,
            "capabilities": [],
            "gpu_handle": 42
        });
        assert!(serde_json::from_value::<UiProgramRevision>(invalid).is_err());
    }

    #[test]
    fn diagnostic_round_trip_exposes_only_semantic_debug_identity() {
        let diagnostic = UiDiagnostic {
            code: ERROR_UI_PROGRAM_UNKNOWN_BINDING_TARGET.into(),
            severity: UiDiagnosticSeverity::Error,
            message: "binding target is not declared by this program".into(),
            node_key: Some("terrain-inspector.apply".into()),
            input_key: Some("can_commit".into()),
            source_span: Some(UiSourceSpan {
                source_id: "workbench.nui".into(),
                line: 18,
                column: 5,
                end_line: 18,
                end_column: 24,
            }),
            revision: Revision(12),
        };
        let encoded = serde_json::to_value(&diagnostic).unwrap();
        assert_eq!(
            serde_json::from_value::<UiDiagnostic>(encoded.clone()).unwrap(),
            diagnostic
        );
        for private_field in ["render_hit_id", "gpu_instance_index", "physical_pixels"] {
            assert!(encoded.get(private_field).is_none());
        }
    }

    #[test]
    fn input_schema_round_trips_and_rejects_invalid_defaults() {
        let kind = UiInputKind::Bool;
        let (alignment, lanes, representation) = kind.packing();
        let schema = UiInputSchema {
            schema_id: "terrain-inputs".into(),
            version: 1,
            layout_hash: "layout-v1".into(),
            slots: vec![UiInputSlot {
                key: "can_commit".into(),
                kind,
                default_value: UiInputValue::Bool { value: false },
                update_class: UiInputUpdateClass::ReliableExternal,
                semantic_label: "Can commit".into(),
                packing: UiInputPacking {
                    alignment,
                    lanes,
                    offset: 0,
                    representation,
                },
            }],
            grid_slots: Vec::new(),
        };
        schema.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<UiInputSchema>(serde_json::to_value(&schema).unwrap())
                .unwrap(),
            schema
        );
        let mut invalid = schema;
        invalid.slots[0].default_value = UiInputValue::F32 { value: f32::NAN };
        assert_eq!(invalid.validate(), Err(UiSchemaError::InvalidInputDefault));
    }

    #[test]
    fn float_ranges_reject_non_finite_bounds_and_out_of_range_values() {
        let kind = UiInputKind::F32Range {
            minimum: 0.0,
            maximum: 1.0,
        };
        assert!(kind.accepts(&UiInputValue::F32 { value: 0.5 }));
        assert!(!kind.accepts(&UiInputValue::F32 { value: 1.5 }));
        assert!(
            !UiInputKind::F32Range {
                minimum: 0.0,
                maximum: f32::INFINITY
            }
            .accepts(&UiInputValue::F32 { value: 0.5 })
        );
        assert_eq!(kind.packing().2, UiGpuScalarRepresentation::F32);
        assert_eq!(
            serde_json::from_value::<UiInputKind>(serde_json::to_value(&kind).unwrap()).unwrap(),
            kind
        );
    }

    #[test]
    fn text_handles_and_registry_snapshots_round_trip_without_raw_input_text() {
        let handle = UiTextHandle {
            id: 7,
            generation: 3,
        };
        let value = UiInputValue::TextHandle { value: handle };
        let encoded = serde_json::to_value(&value).unwrap();
        assert_eq!(encoded["kind"], "text_handle");
        assert!(encoded.get("text").is_none());
        assert_eq!(
            serde_json::from_value::<UiInputValue>(encoded).unwrap(),
            value
        );
        let snapshot = UiTextRegistrySnapshot {
            registry_id: "surface.editor.text".into(),
            revision: Revision(4),
            capacity: 32,
            used: 1,
            records: vec![UiTextRecord {
                handle,
                text: "Terrain \u{5730}\u{5f62}".into(),
                category: UiTextSourceCategory::Dynamic,
                revision: Revision(4),
                byte_length: 14,
            }],
        };
        assert_eq!(
            serde_json::from_value::<UiTextRegistrySnapshot>(
                serde_json::to_value(&snapshot).unwrap()
            )
            .unwrap(),
            snapshot
        );
        let debug = UiTextRegistryDebugSnapshot {
            registry_id: "surface.editor.text".into(),
            revision: Revision(4),
            capacity: 32,
            used: 1,
            records: vec![UiTextRegistryEntryMetadata {
                handle,
                category: UiTextSourceCategory::Dynamic,
                revision: Revision(4),
                byte_length: 14,
                reference_count: 2,
                resident: false,
            }],
        };
        let encoded_debug = serde_json::to_value(&debug).unwrap();
        assert!(encoded_debug["records"][0].get("text").is_none());
        assert_eq!(
            serde_json::from_value::<UiTextRegistryDebugSnapshot>(encoded_debug).unwrap(),
            debug
        );
    }
}

impl UiIrDocument {
    pub fn validate(&self) -> Result<(), UiSchemaError> {
        if self.schema_version != 1 || self.surface_id.0.trim().is_empty() {
            return Err(UiSchemaError::InvalidIrDocument);
        }
        self.root.validate()?;
        if self.resource_budget.max_nodes == 0 || self.resource_budget.max_instances == 0 {
            return Err(UiSchemaError::InvalidProgramBudget);
        }
        let mut keys = std::collections::HashSet::new();
        collect_ir_keys(&self.root, &mut keys);
        let mut branch_keys = std::collections::HashSet::new();
        for branch in &self.branches {
            if branch.branch_key.trim().is_empty()
                || !branch_keys.insert(&branch.branch_key)
                || !keys.contains(&branch.root_node_key)
            {
                return Err(UiSchemaError::InvalidIrDocument);
            }
            match &branch.predicate {
                UiBranchPredicate::Bool { input_key, .. } if !input_key.trim().is_empty() => {}
                UiBranchPredicate::EnumEquals { input_key, variant }
                    if !input_key.trim().is_empty() && !variant.trim().is_empty() => {}
                UiBranchPredicate::MachineState { machine_key, state }
                    if !machine_key.trim().is_empty() && !state.trim().is_empty() => {}
                _ => return Err(UiSchemaError::InvalidIrDocument),
            }
        }
        let mut template_keys = std::collections::HashSet::new();
        for template in &self.templates {
            if template.template_key.trim().is_empty()
                || !template_keys.insert(&template.template_key)
                || !keys.contains(&template.root_node_key)
                || template.max_instances == 0
                || template.instance_key_field.trim().is_empty()
                || template.row_schema.is_empty()
                || !template
                    .row_schema
                    .contains_key(&template.instance_key_field)
            {
                return Err(UiSchemaError::InvalidIrDocument);
            }
        }
        let mut data_grid_node_keys = std::collections::HashSet::new();
        let mut data_grid_sources = std::collections::HashSet::new();
        for data_grid in &self.data_grids {
            if data_grid.node_key.trim().is_empty()
                || data_grid.source_key.trim().is_empty()
                || data_grid.max_window_rows == 0
                || data_grid.row_height == 0
                || data_grid.overscan > data_grid.max_window_rows
                || data_grid.columns.is_empty()
                || !data_grid_node_keys.insert(data_grid.node_key.clone())
                || !data_grid_sources.insert(data_grid.source_key.clone())
                || !matches!(find_ir_node(&self.root, &data_grid.node_key), Some(node) if node.kind == UiNodeKind::DataGrid)
            {
                return Err(UiSchemaError::InvalidIrDocument);
            }
            let mut column_keys = std::collections::HashSet::new();
            if data_grid
                .columns
                .iter()
                .any(|column| !column.validate() || !column_keys.insert(&column.key))
            {
                return Err(UiSchemaError::InvalidIrDocument);
            }
        }
        let mut declared_grid_nodes = std::collections::HashSet::new();
        collect_data_grid_node_keys(&self.root, &mut declared_grid_nodes);
        if declared_grid_nodes != data_grid_node_keys {
            return Err(UiSchemaError::InvalidIrDocument);
        }
        Ok(())
    }
}

fn collect_ir_keys(node: &UiNode, keys: &mut std::collections::HashSet<String>) {
    keys.insert(node.node_id.0.clone());
    for child in &node.children {
        collect_ir_keys(child, keys);
    }
}

fn find_ir_node<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if node.node_id.0 == key {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_ir_node(child, key))
}

fn collect_data_grid_node_keys(node: &UiNode, keys: &mut std::collections::HashSet<String>) {
    if node.kind == UiNodeKind::DataGrid {
        keys.insert(node.node_id.0.clone());
    }
    for child in &node.children {
        collect_data_grid_node_keys(child, keys);
    }
}

/// Program-native drag/drop commit using only stable declaration identities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProgramDragDropEvent {
    pub event_id: String,
    pub drag_key: String,
    pub drop_key: String,
    pub intent: String,
    pub payload: UiDragDropPayload,
    pub program_revision: UiProgramRevision,
    pub input_revision: Revision,
    pub request_id: String,
    pub idempotency_key: String,
    pub interaction: UiSemanticInteractionMetadata,
}
