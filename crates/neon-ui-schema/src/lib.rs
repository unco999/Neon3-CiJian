//! GPU-independent UI declaration schema types.
//! This crate must not create GPU or window objects.

use neon_protocol::{AssetRef, Revision};
use serde::{Deserialize, Serialize};

/// Version of the renderer-independent UI declaration contract.
/// This is deliberately separate from the RPC transport version.
pub const UI_FRAGMENT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiFragmentId(pub String);

/// Fragment-local declaration identity. This is never a public input or domain identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiNodeId(pub String);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiFragment {
    pub fragment_id: UiFragmentId,
    pub revision: Revision,
    pub root: UiNode,
    pub effects: Vec<UiEffect>,
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
        Self { schema_version: UI_FRAGMENT_SCHEMA_VERSION, fragment }
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
}

/// Source-independent text declaration. Renderers receive resolved immutable content only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextRef {
    Key { key: String, arguments: serde_json::Value },
    Literal { value: String },
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
pub enum UiLayoutMode { Absolute, Overlay, Row, Column }

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
    pub clip: bool,
    pub scroll_offset: [f32; 2],
}

impl Default for UiLayout {
    fn default() -> Self { Self { mode: UiLayoutMode::Absolute, padding: [0.0; 4], margin: [0.0; 4], gap: 0.0, min_size: None, max_size: None, preferred_size: None, clip: false, scroll_offset: [0.0; 2] } }
}

pub fn resolve_layout(node: &UiNode, parent: UiBounds, scale_factor: f32) -> Result<Vec<UiBounds>, UiSchemaError> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 { return Err(UiSchemaError::InvalidLayout); }
    let layout = node.layout.unwrap_or_default();
    if !layout.is_valid() { return Err(UiSchemaError::InvalidLayout); }
    let own = clamp_bounds(UiBounds { x: parent.x + node.bounds.x, y: parent.y + node.bounds.y, width: node.bounds.width, height: node.bounds.height }, layout);
    let mut output = vec![physical_bounds(own, scale_factor)];
    let inner = UiBounds { x: own.x + layout.padding[3], y: own.y + layout.padding[0], width: (own.width - layout.padding[1] - layout.padding[3]).max(0.0), height: (own.height - layout.padding[0] - layout.padding[2]).max(0.0) };
    let mut cursor = [inner.x - layout.scroll_offset[0], inner.y - layout.scroll_offset[1]];
    for child in &node.children {
        let child_layout = child.layout.unwrap_or_default();
        let mut child_bounds = UiBounds { x: inner.x + child.bounds.x, y: inner.y + child.bounds.y, width: child.bounds.width, height: child.bounds.height };
        if matches!(layout.mode, UiLayoutMode::Row | UiLayoutMode::Column) {
            child_bounds.x = cursor[0] + child_layout.margin[3]; child_bounds.y = cursor[1] + child_layout.margin[0];
            if layout.mode == UiLayoutMode::Row { cursor[0] += child_bounds.width + child_layout.margin[1] + layout.gap; } else { cursor[1] += child_bounds.height + child_layout.margin[2] + layout.gap; }
        }
        output.extend(resolve_layout(child, child_bounds, scale_factor)?);
    }
    Ok(output)
}

fn clamp_bounds(mut bounds: UiBounds, layout: UiLayout) -> UiBounds {
    if let Some([width, height]) = layout.preferred_size { bounds.width = width; bounds.height = height; }
    if let Some([width, height]) = layout.min_size { bounds.width = bounds.width.max(width); bounds.height = bounds.height.max(height); }
    if let Some([width, height]) = layout.max_size { bounds.width = bounds.width.min(width); bounds.height = bounds.height.min(height); }
    bounds
}

fn physical_bounds(bounds: UiBounds, scale: f32) -> UiBounds { UiBounds { x: bounds.x * scale, y: bounds.y * scale, width: bounds.width * scale, height: bounds.height * scale } }

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
    SemanticAction { action: String },
    SemanticIntent { intent: UiIntent },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiIntent {
    Invoke { action: String, params: serde_json::Value },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiSemanticEventType {
    PointerClick,
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
}

pub const ERROR_RENDERER_EPOCH_MISMATCH: &str = "renderer_epoch_mismatch";
pub const ERROR_FRAGMENT_REVISION_STALE: &str = "fragment_revision_stale";
pub const ERROR_INTENT_NOT_BOUND: &str = "intent_not_bound";
pub const ERROR_INTERACTION_CANCELLED: &str = "interaction_cancelled";
pub const ERROR_FOCUS_INVALID: &str = "focus_invalid";
pub const ERROR_INPUT_SEQUENCE_STALE: &str = "input_sequence_stale";

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
    EmptyFragmentId,
    EmptyNodeId,
    EmptyAction,
    InvalidBounds,
    InvalidStyle,
    InvalidTransition,
    InvalidLayout,
    DuplicateNodeId,
    MissingImageAsset,
    InvalidText,
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
        if self.text.as_ref().is_some_and(|text| !text.is_valid()) { return Err(UiSchemaError::InvalidText); }
        if self.layout.is_some_and(|layout| !layout.is_valid()) { return Err(UiSchemaError::InvalidLayout); }
        let mut child_ids = std::collections::HashSet::new();
        for child in &self.children {
            if !child_ids.insert(child.node_id.0.as_str()) { return Err(UiSchemaError::DuplicateNodeId); }
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
        self.padding.iter().chain(self.margin.iter()).all(|value| value.is_finite() && *value >= 0.0)
            && self.gap.is_finite() && self.gap >= 0.0 && self.scroll_offset.iter().all(|value| value.is_finite())
            && self.min_size.is_none_or(|size| size.iter().all(|value| value.is_finite() && *value >= 0.0))
            && self.max_size.is_none_or(|size| size.iter().all(|value| value.is_finite() && *value >= 0.0))
            && self.preferred_size.is_none_or(|size| size.iter().all(|value| value.is_finite() && *value >= 0.0))
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
        }
    }
}

impl UiIntent {
    pub fn validate(&self) -> Result<(), UiSchemaError> {
        match self {
            Self::Invoke { action, .. } if action.trim().is_empty() => Err(UiSchemaError::EmptyAction),
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
    fn submission_is_versioned_declarative_data() {
        let fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        let submission = UiFragmentSubmission::new(fragment);
        submission.validate().unwrap();
        let encoded = serde_json::to_value(&submission).unwrap().to_string();
        assert!(encoded.contains("schema_version"));
        for forbidden in ["react", "jsx", "callback", "wgpu", "window", "handle"] {
            assert!(!encoded.contains(forbidden), "submission contains {forbidden}");
        }
    }

    #[test]
    fn submission_rejects_unknown_schema_version() {
        let fragment: UiFragment = serde_json::from_str(STATIC_FRAGMENT).unwrap();
        let submission = UiFragmentSubmission { schema_version: UI_FRAGMENT_SCHEMA_VERSION + 1, fragment };
        assert_eq!(submission.validate(), Err(UiSchemaError::UnsupportedSchemaVersion));
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
            pointer: Some(UiPointerMetadata {
                id: 0,
                sequence: 4,
            }),
            focus: None,
        };
        let encoded = serde_json::to_value(event).unwrap();
        assert!(encoded.get("render_hit_id").is_none());
        assert!(encoded.get("node_id").is_none());
        assert!(encoded.get("pixel_position").is_none());
        assert!(encoded.get("logical_position").is_none());
    }

    #[test]
    fn public_schema_has_no_renderer_or_web_runtime_dependency() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in ["wgpu", "winit", "react", "typescript", "tauri", "webview"] {
            assert!(
                !manifest.lines().any(|line| line.trim_start().starts_with(&format!("{forbidden} ="))),
                "public UI schema must not depend on {forbidden}"
            );
        }
    }

    fn layout_node(mode: UiLayoutMode) -> UiNode {
        UiNode {
            node_id: UiNodeId("root".into()), kind: UiNodeKind::Panel,
            bounds: UiBounds { x: 0.0, y: 0.0, width: 100.0, height: 60.0 },
            layout: Some(UiLayout { mode, padding: [4.0, 4.0, 4.0, 4.0], gap: 3.0, ..UiLayout::default() }),
            visible: true, enabled: true, text_key: None, text: None, image: None, style: UiStyle::default(), enter_transition: None,
            children: vec![
                UiNode { node_id: UiNodeId("a".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 0.0, y: 0.0, width: 20.0, height: 10.0 }, layout: Some(UiLayout { margin: [1.0, 2.0, 3.0, 4.0], ..UiLayout::default() }), visible: true, enabled: true, text_key: None, text: None, image: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
                UiNode { node_id: UiNodeId("b".into()), kind: UiNodeKind::Button, bounds: UiBounds { x: 0.0, y: 0.0, width: 20.0, height: 10.0 }, layout: None, visible: true, enabled: true, text_key: None, text: None, image: None, style: UiStyle::default(), enter_transition: None, children: Vec::new() },
            ],
        }
    }

    #[test]
    fn row_column_dpi_and_scroll_layout_are_deterministic() {
        let row = resolve_layout(&layout_node(UiLayoutMode::Row), UiBounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, 2.0).unwrap();
        assert_eq!(row[1], UiBounds { x: 16.0, y: 10.0, width: 40.0, height: 20.0 });
        assert_eq!(row[2].x, 58.0);
        let column = resolve_layout(&layout_node(UiLayoutMode::Column), UiBounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, 1.0).unwrap();
        assert_eq!(column[1].y, 5.0);
        assert_eq!(column[2].y, 20.0);
        let mut scrolled = layout_node(UiLayoutMode::Column);
        scrolled.layout.as_mut().unwrap().scroll_offset = [0.0, 2.0];
        assert_eq!(resolve_layout(&scrolled, UiBounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, 1.0).unwrap()[1].y, 3.0);
    }

    #[test]
    fn layout_clamps_sizes_and_rejects_invalid_values() {
        let mut node = layout_node(UiLayoutMode::Absolute);
        node.layout.as_mut().unwrap().min_size = Some([120.0, 70.0]);
        node.layout.as_mut().unwrap().max_size = Some([130.0, 80.0]);
        assert_eq!(resolve_layout(&node, UiBounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, 1.0).unwrap()[0].width, 120.0);
        node.layout.as_mut().unwrap().gap = -1.0;
        assert_eq!(resolve_layout(&node, UiBounds { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }, 1.0), Err(UiSchemaError::InvalidLayout));
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
    fn text_ref_is_source_independent_and_rejects_empty_content() {
        let mut node = layout_node(UiLayoutMode::Absolute);
        node.text = Some(TextRef::Key { key: "ui.fixture.label".into(), arguments: serde_json::json!({"count": 2}) });
        node.validate().unwrap();
        let encoded = serde_json::to_value(&node).unwrap().to_string();
        for forbidden in ["path", "font_file", "wgpu", "handle"] {
            assert!(!encoded.contains(forbidden), "text declaration contains {forbidden}");
        }
        node.text = Some(TextRef::Literal { value: String::new() });
        assert_eq!(node.validate(), Err(UiSchemaError::InvalidText));
    }
}
