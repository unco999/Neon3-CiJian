//! GPU-independent UI declaration schema types.
//! This crate must not create GPU or window objects.

use neon_protocol::Revision;
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiNode {
    pub node_id: UiNodeId,
    pub kind: UiNodeKind,
    pub bounds: UiBounds,
    pub visible: bool,
    pub enabled: bool,
    pub text_key: Option<String>,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
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
    pub logical_position: [f32; 2],
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
        fragment: UiFragment,
    },
    RemoveFragment {
        fragment_id: UiFragmentId,
        revision: Revision,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiSchemaError {
    EmptyFragmentId,
    EmptyNodeId,
    EmptyAction,
    InvalidBounds,
    InvalidStyle,
    InvalidTransition,
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
    fn semantic_event_serialization_has_no_render_or_node_identifier() {
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
                logical_position: [12.5, 8.0],
            }),
            focus: None,
        };
        let encoded = serde_json::to_value(event).unwrap();
        assert!(encoded.get("render_hit_id").is_none());
        assert!(encoded.get("node_id").is_none());
        assert!(encoded.get("pixel_position").is_none());
    }
}
