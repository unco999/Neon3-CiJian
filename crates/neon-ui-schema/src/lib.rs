//! GPU-independent UI declaration schema types.
//! This crate must not create GPU or window objects.

use neon_protocol::Revision;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiFragmentId(pub String);

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiEffect {
    SemanticAction { action: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiIntent {
    Invoke { action: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiCommand {
    SubmitFragment { fragment: UiFragment },
    RemoveFragment { fragment_id: UiFragmentId, revision: Revision },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiSchemaError {
    EmptyFragmentId,
    EmptyNodeId,
    EmptyAction,
    InvalidBounds,
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

impl UiEffect {
    fn validate(&self) -> Result<(), UiSchemaError> {
        match self {
            Self::SemanticAction { action } if action.trim().is_empty() => Err(UiSchemaError::EmptyAction),
            Self::SemanticAction { .. } => Ok(()),
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
            assert!(!encoded.contains(forbidden), "serialized declaration contains {forbidden}");
        }
    }
}
