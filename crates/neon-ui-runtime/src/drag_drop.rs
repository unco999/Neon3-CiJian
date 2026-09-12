//! Built-in drag & drop fragment mutation.
//!
//! This module implements the fixed interaction logic for drag & drop:
//! validating declared bindings, moving source nodes, instantiating target
//! templates, and updating the fragment revision. It is not demo-specific —
//! any host that accepts drag/drop can call [`apply_drag_drop`].

use neon_protocol::Revision;
use neon_ui_schema::{
    TextRef, UiDropPlacement, UiEffect, UiFragment, UiNode, UiNodeId, UiSemanticEvent,
    UiSemanticEventType,
};

use crate::instantiate_ui_template;

/// Errors produced by [`apply_drag_drop`].
#[derive(Debug, Clone)]
pub enum DragDropError {
    NotDragDropEvent,
    MissingPayload,
    MissingTemplate,
    NotDeclared,
    SourceNotFound,
    TargetNotFound,
    TemplateNotOwned,
    RelativeTargetHasNoParent,
    InvalidFragment,
}

impl DragDropError {
    pub fn code(&self) -> &'static str {
        match self {
            DragDropError::NotDragDropEvent => "invalid_drag_drop",
            DragDropError::MissingPayload => "invalid_drag_drop",
            DragDropError::MissingTemplate => "presentation_template_required",
            DragDropError::NotDeclared => "drag_drop_not_declared",
            DragDropError::SourceNotFound => "source_not_found",
            DragDropError::TargetNotFound => "target_not_found",
            DragDropError::TemplateNotOwned => "template_not_owned",
            DragDropError::RelativeTargetHasNoParent => "relative_target_has_no_parent",
            DragDropError::InvalidFragment => "invalid_fragment",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            DragDropError::NotDragDropEvent => "a drag/drop semantic event is required",
            DragDropError::MissingPayload => "drag/drop payload is required",
            DragDropError::MissingTemplate => "accepted drops require a target-owned template",
            DragDropError::NotDeclared => {
                "source, target, placement, or template is not declared"
            }
            DragDropError::SourceNotFound => "drag source is not present",
            DragDropError::TargetNotFound => "drop target is not present",
            DragDropError::TemplateNotOwned => {
                "presentation template is not owned by the drop target"
            }
            DragDropError::RelativeTargetHasNoParent => {
                "before and after targets must have a parent"
            }
            DragDropError::InvalidFragment => "accepted domain revision is invalid",
        }
    }
}

/// Apply a drag/drop semantic event to a fragment in place.
///
/// This validates that the source/target/placement/template combination is
/// declared in the fragment, removes the source node and its drag/drop
/// bindings, instantiates the target-owned template, and inserts it at the
/// requested placement. The fragment revision is bumped on success.
pub fn apply_drag_drop(
    fragment: &mut UiFragment,
    event: &UiSemanticEvent,
) -> Result<(), DragDropError> {
    if event.event != UiSemanticEventType::DragDrop {
        return Err(DragDropError::NotDragDropEvent);
    }
    let drop = event.drag_drop.as_ref().ok_or(DragDropError::MissingPayload)?;
    let template_key = drop
        .presentation_template_key
        .as_ref()
        .ok_or(DragDropError::MissingTemplate)?;

    // Resolve the declared drag key for the source.
    let source_drag_key = fragment.effects.iter().find_map(|effect| match effect {
        UiEffect::DragBinding { binding } if binding.source_node_id.0 == drop.source_key => {
            Some(binding.key.clone())
        }
        _ => None,
    });

    // Verify the drop is declared in the fragment.
    let declared = fragment.effects.iter().any(|effect| match effect {
        UiEffect::DropBinding { binding } => {
            binding.intent == event.intent
                && binding.target_node_id.0 == drop.target_key
                && source_drag_key.as_deref() == Some(binding.accepts_drag_key.as_str())
                && binding.placement == drop.placement
                && binding.presentation_template_key.as_deref() == Some(template_key.as_str())
        }
        _ => false,
    });
    if !declared {
        return Err(DragDropError::NotDeclared);
    }

    let source = find_node(&fragment.root, &drop.source_key)
        .cloned()
        .ok_or(DragDropError::SourceNotFound)?;
    let target = find_node(&fragment.root, &drop.target_key)
        .ok_or(DragDropError::TargetNotFound)?;
    let template = target
        .children
        .iter()
        .find(|node| node.node_id.0 == *template_key)
        .cloned()
        .ok_or(DragDropError::TemplateNotOwned)?;

    if drop.placement != UiDropPlacement::Into
        && find_parent(&fragment.root, &drop.target_key).is_none()
    {
        return Err(DragDropError::RelativeTargetHasNoParent);
    }

    let label = first_literal(&source).unwrap_or_else(|| source.node_id.0.clone());
    let next_revision = Revision(fragment.revision.0 + 1);
    let representation = instantiate_ui_template(
        &template,
        (drop.placement == UiDropPlacement::Into).then_some(target),
        &format!("{}-{}-r{}", template_key, drop.source_key, next_revision.0),
        Some(TextRef::Literal { value: label }),
    );

    remove_node(&mut fragment.root, &drop.source_key);
    fragment.effects.retain(|effect| match effect {
        UiEffect::DragBinding { binding } => binding.source_node_id.0 != drop.source_key,
        UiEffect::DropBinding { binding } => {
            source_drag_key.as_deref() != Some(binding.accepts_drag_key.as_str())
        }
        _ => true,
    });

    match drop.placement {
        UiDropPlacement::Into => find_node_mut(&mut fragment.root, &drop.target_key)
            .expect("validated target remains present")
            .children
            .push(representation),
        UiDropPlacement::Before | UiDropPlacement::After => {
            let parent = find_parent_mut(&mut fragment.root, &drop.target_key)
                .expect("validated relative target retains its parent");
            let target_index = parent
                .children
                .iter()
                .position(|child| child.node_id.0 == drop.target_key)
                .expect("validated relative target remains present");
            let insertion_index = if drop.placement == UiDropPlacement::Before {
                target_index
            } else {
                target_index + 1
            };
            parent.children.insert(insertion_index, representation);
        }
    }

    fragment.revision = next_revision;
    if fragment.validate().is_err() {
        return Err(DragDropError::InvalidFragment);
    }
    Ok(())
}

// --- tree helpers (private to this module) ---

fn find_node<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if node.node_id.0 == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, key))
}

fn find_node_mut<'a>(node: &'a mut UiNode, key: &str) -> Option<&'a mut UiNode> {
    if node.node_id.0 == key {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, key))
}

fn find_parent<'a>(node: &'a UiNode, child_key: &str) -> Option<&'a UiNode> {
    if node.children.iter().any(|child| child.node_id.0 == child_key) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_parent(child, child_key))
}

fn find_parent_mut<'a>(node: &'a mut UiNode, child_key: &str) -> Option<&'a mut UiNode> {
    if node.children.iter().any(|child| child.node_id.0 == child_key) {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_parent_mut(child, child_key))
}

fn remove_node(node: &mut UiNode, key: &str) -> bool {
    if let Some(index) = node
        .children
        .iter()
        .position(|child| child.node_id.0 == key)
    {
        node.children.remove(index);
        return true;
    }
    node.children
        .iter_mut()
        .any(|child| remove_node(child, key))
}

fn first_literal(node: &UiNode) -> Option<String> {
    if let Some(TextRef::Literal { value }) = &node.text {
        return Some(value.clone());
    }
    node.children.iter().find_map(first_literal)
}

// Suppress unused import warning for UiNodeId (used in type clarity).
#[allow(dead_code)]
fn _type_hint(_id: UiNodeId) {}
