//! Phase 2: generic keyed UI diff.
//!
//! `diff_projection_trees` compares two canonical `UiNode` trees that share
//! stable keys and emits the minimal ordered [`UiPatchOp`] list that turns the
//! old tree into the new one. The operations plug directly into the existing
//! `apply_ui_patch` contract, so a projection owner can stop re-submitting the
//! whole Flow program for localized changes.
//!
//! Ordering follows the plan's canonical sequence:
//! `RemoveNode` (deepest first) -> structural pre-order (`MoveNode` /
//! `InsertNode` / `ReplaceChildren` interleaved per parent in new-tree order)
//! -> `SetProperty`. Structural operations resolve nodes by their globally
//! unique stable key (guaranteed by the Phase 1 IR contract), so paths never
//! depend on array indexes surviving an earlier operation.
//! `StartTransition` and `SetInput` are presentation intents a projection
//! owner appends explicitly; the diff never infers them.
//!
//! Only properties the IR `Set` contract supports (`enabled`, `visible`,
//! `value`, `w`, `h`, `fill`, `opacity`) are patched as `SetProperty`. Any
//! other field change (kind, layout, text source shape, x/y, border, clip,
//! world projection) is a key-compatible but unpatchable identity change and
//! forces an explicit remove + full insert. `enter_transition` is excluded
//! from identity comparison because it is play-once presentation metadata
//! asserted via `StartTransition` instead.

use std::collections::{BTreeMap, HashMap, HashSet};

use neon_ui_schema::{TextRef, UiNode, UiPatchOp};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Lower bound on old child count before a wholesale child-list swap is
/// collapsed into one `ReplaceChildren` instead of many removes and inserts.
pub const REPLACE_CHILDREN_MIN_STRUCTURAL: usize = 8;

/// Keyed projection view of a node tree. This is the abstraction a projection
/// owner (file tree, task graph, workbench panel) builds from domain state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiProjectionTree {
    pub key: String,
    pub kind: String,
    pub properties: BTreeMap<String, Value>,
    pub children: Vec<UiProjectionTree>,
}

impl UiProjectionTree {
    pub fn from_node(node: &UiNode) -> Self {
        Self {
            key: node.node_id.0.clone(),
            kind: kind_label(node),
            properties: patchable_properties(node),
            children: node.children.iter().map(Self::from_node).collect(),
        }
    }
}

/// Ordered, coalesced patch operations produced by the keyed diff.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiTreeDiff {
    pub operations: Vec<UiPatchOp>,
}

impl UiTreeDiff {
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }
}

/// Reasons the diff refuses to patch: the caller must fall back to a full
/// Flow submit because no patch operation can address the root itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiDiffRootMismatch {
    KeyChanged { old: String, new: String },
    IdentityChanged,
}

/// Wraps a diff into the public patch contract. Revision enforcement stays
/// inside `apply_ui_patch`; this only binds the identity fields.
pub fn build_ui_patch(
    surface_id: &neon_ui_schema::UiSurfaceId,
    base_revision: u64,
    diff: UiTreeDiff,
) -> neon_ui_schema::UiPatch {
    neon_ui_schema::UiPatch {
        surface_id: surface_id.0.clone(),
        base_revision,
        operations: diff.operations,
    }
}

/// Last-value-wins coalescing for `SetProperty` on the same node path and
/// property; every other operation kind passes through unchanged.
pub fn merge_operations(operations: &mut Vec<UiPatchOp>) {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    operations.reverse();
    operations.retain(|operation| match operation {
        UiPatchOp::SetProperty {
            node_path,
            property,
            ..
        } => seen.insert((node_path.clone(), property.clone())),
        _ => true,
    });
    operations.reverse();
}

pub fn diff_projection_trees(
    old_root: &UiNode,
    new_root: &UiNode,
) -> Result<UiTreeDiff, UiDiffRootMismatch> {
    if old_root.node_id.0 != new_root.node_id.0 {
        return Err(UiDiffRootMismatch::KeyChanged {
            old: old_root.node_id.0.clone(),
            new: new_root.node_id.0.clone(),
        });
    }
    if node_identity(old_root) != node_identity(new_root) {
        return Err(UiDiffRootMismatch::IdentityChanged);
    }
    let mut diff = Diff::default();
    diff.index_old(old_root, old_root.node_id.0.clone(), None, 0, &Vec::new());
    diff.analyze_new(new_root, new_root.node_id.0.clone(), None, &Vec::new());
    diff.emit_walk(new_root, new_root.node_id.0.clone());
    diff.finish()
}

struct OldEntry<'a> {
    node: &'a UiNode,
    path: String,
    parent_key: Option<String>,
    index: usize,
    ancestors: Vec<String>,
}

#[derive(Default)]
struct OldIndex<'a> {
    entries: HashMap<String, OldEntry<'a>>,
    order: Vec<String>,
}

#[derive(Default)]
struct Diff<'a> {
    old: OldIndex<'a>,
    /// Stable key of every old node a patch operation already accounts for
    /// (kept in place, consumed by a replace, or deleted inside an inserted
    /// subtree). Anything left unconsumed becomes a `RemoveNode`.
    consumed: HashSet<String>,
    /// Old keys from which at least one kept node was reparented away; the
    /// subtrees under these keys are never eligible for `ReplaceChildren`.
    escaped: HashSet<String>,
    removed: Vec<(usize, UiPatchOp)>,
    structural: Vec<UiPatchOp>,
    sets: Vec<UiPatchOp>,
}

impl<'a> Diff<'a> {
    fn index_old(
        &mut self,
        node: &'a UiNode,
        path: String,
        parent_key: Option<String>,
        index: usize,
        ancestors: &[String],
    ) {
        let mut ancestors = ancestors.to_vec();
        ancestors.push(node.node_id.0.clone());
        let key = node.node_id.0.clone();
        if let std::collections::hash_map::Entry::Vacant(vacant) =
            self.old.entries.entry(key.clone())
        {
            self.old.order.push(key);
            vacant.insert(OldEntry {
                node,
                path: path.clone(),
                parent_key,
                index,
                ancestors: ancestors.clone(),
            });
        }
        for (child_index, child) in node.children.iter().enumerate() {
            self.index_old(
                child,
                format!("{path}/{}", child.node_id.0),
                Some(node.node_id.0.clone()),
                child_index,
                &ancestors,
            );
        }
    }

    fn kept_entry(&self, node: &UiNode) -> Option<&OldEntry<'a>> {
        self.old
            .entries
            .get(&node.node_id.0)
            .filter(|entry| node_identity(entry.node) == node_identity(node))
    }

    /// First pass: record globally which new nodes keep an old key, marking
    /// reparenting escapes before any emission decides on `ReplaceChildren`.
    fn analyze_new(
        &mut self,
        node: &'a UiNode,
        path: String,
        parent_key: Option<&str>,
        ancestors: &[String],
    ) {
        let reparented = self
            .kept_entry(node)
            .is_some_and(|entry| entry.parent_key.as_deref() != parent_key);
        if reparented {
            let ancestors = self
                .old
                .entries
                .get(&node.node_id.0)
                .map(|entry| entry.ancestors.clone())
                .expect("kept node has an old entry");
            for escaped_key in ancestors.iter().take(ancestors.len() - 1) {
                self.escaped.insert(escaped_key.clone());
            }
        }
        let mut ancestors = ancestors.to_vec();
        ancestors.push(node.node_id.0.clone());
        for child in &node.children {
            self.analyze_new(
                child,
                format!("{path}/{}", child.node_id.0),
                Some(node.node_id.0.as_str()),
                &ancestors,
            );
        }
    }

    /// Second pass in new-tree pre-order: emits structural operations and
    /// property sets so every destination parent exists before its children
    /// are touched.
    fn emit_walk(&mut self, new_node: &'a UiNode, path: String) {
        if let Some((old_node, _)) = self
            .kept_entry(new_node)
            .map(|entry| (entry.node, entry.path.clone()))
        {
            self.consumed.insert(new_node.node_id.0.clone());
            emit_property_sets(&mut self.sets, old_node, new_node, &path);
        }
        let keep_children: Vec<bool> = new_node
            .children
            .iter()
            .map(|child| self.kept_entry(child).is_some())
            .collect();
        let entry = self.old.entries.get(&new_node.node_id.0);
        let replace = !new_node.children.is_empty()
            && !keep_children.iter().any(|keep| *keep)
            && entry.is_some_and(|entry| {
                node_identity(entry.node) == node_identity(new_node)
                    && entry.path == path
                    && entry.node.children.len() >= REPLACE_CHILDREN_MIN_STRUCTURAL
                    && !subtree_has_key_below_root(entry.node, &self.escaped)
            });
        if replace {
            let old_node = entry.expect("replace requires a matched parent").node;
            mark_consumed_subtree(&mut self.consumed, old_node);
            self.structural.push(UiPatchOp::ReplaceChildren {
                parent_path: path,
                children: new_node.children.clone(),
            });
            return;
        }
        for (index, child) in new_node.children.iter().enumerate() {
            let child_path = format!("{path}/{}", child.node_id.0);
            let kept = self
                .kept_entry(child)
                .map(|entry| (entry.parent_key.clone(), entry.index));
            match kept {
                Some((parent_key, old_index)) => {
                    if parent_key.as_deref() != Some(new_node.node_id.0.as_str())
                        || old_index != index
                    {
                        self.structural.push(UiPatchOp::MoveNode {
                            node_path: child_path.clone(),
                            parent_path: path.clone(),
                            index,
                        });
                    }
                    self.emit_walk(child, child_path);
                }
                None => {
                    self.structural.push(UiPatchOp::InsertNode {
                        parent_path: path.clone(),
                        index,
                        node: child.clone(),
                    });
                }
            }
        }
    }

    fn finish(mut self) -> Result<UiTreeDiff, UiDiffRootMismatch> {
        // Pre-order over the old tree: only subtree roots that no operation
        // otherwise accounts for get an explicit RemoveNode.
        let mut under_removed: HashSet<String> = HashSet::new();
        for key in &self.old.order {
            let entry = &self.old.entries[key];
            let Some(parent_key) = entry.parent_key.clone() else {
                continue; // the root itself is never removable via patch
            };
            if self.consumed.contains(key) || under_removed.contains(&parent_key) {
                continue;
            }
            under_removed.insert(key.clone());
            self.removed.push((
                entry.ancestors.len(),
                UiPatchOp::RemoveNode {
                    node_path: entry.path.clone(),
                },
            ));
        }
        self.removed.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        let mut operations = self
            .removed
            .into_iter()
            .map(|(_, operation)| operation)
            .collect::<Vec<_>>();
        operations.append(&mut self.structural);
        operations.append(&mut self.sets);
        merge_operations(&mut operations);
        Ok(UiTreeDiff { operations })
    }
}

/// Keys whose subtree must not be wholesale replaced because a kept node
/// escaped out of them. The queried node itself is exempt: only keys strictly
/// below a replace candidate matter.
fn subtree_has_key_below_root(node: &UiNode, escaped: &HashSet<String>) -> bool {
    node.children.iter().any(|child| {
        escaped.contains(&child.node_id.0) || subtree_has_key_below_root(child, escaped)
    })
}

fn mark_consumed_subtree(consumed: &mut HashSet<String>, node: &UiNode) {
    consumed.insert(node.node_id.0.clone());
    for child in &node.children {
        mark_consumed_subtree(consumed, child);
    }
}

fn emit_property_sets(sets: &mut Vec<UiPatchOp>, old: &UiNode, new: &UiNode, path: &str) {
    let old_properties = patchable_properties(old);
    let new_properties = patchable_properties(new);
    for (property, value) in &new_properties {
        if old_properties.get(property) != Some(value) {
            sets.push(UiPatchOp::SetProperty {
                node_path: path.to_owned(),
                property: property.clone(),
                value: value.clone(),
            });
        }
    }
}

fn patchable_properties(node: &UiNode) -> BTreeMap<String, Value> {
    let mut properties = BTreeMap::new();
    properties.insert("enabled".to_owned(), json!(node.enabled));
    properties.insert("visible".to_owned(), json!(node.visible));
    properties.insert("w".to_owned(), json!(node.bounds.width));
    properties.insert("h".to_owned(), json!(node.bounds.height));
    properties.insert("opacity".to_owned(), json!(node.style.opacity));
    properties.insert(
        "fill".to_owned(),
        json!(hex_color(node.style.background_color)),
    );
    if let Some(TextRef::Literal { value }) = &node.text {
        properties.insert("value".to_owned(), json!(value));
    }
    properties
}

fn kind_label(node: &UiNode) -> String {
    serde_json::to_value(&node.kind)
        .expect("kind serializes")
        .as_str()
        .unwrap_or("unknown")
        .to_owned()
}

/// Normalized comparison snapshot of every non-patchable node field. A
/// difference here means the node cannot be brought up to date with `Set`
/// operations and must be removed and re-inserted whole.
fn node_identity(node: &UiNode) -> Value {
    let mut value = serde_json::to_value(node).expect("UiNode serializes");
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    object.remove("node_id");
    object.remove("children");
    object.remove("enabled");
    object.remove("visible");
    object.remove("enter_transition");
    if let Some(bounds) = object.remove("bounds").and_then(|b| b.as_object().cloned()) {
        object.insert(
            "bounds".to_owned(),
            json!({ "x": bounds.get("x"), "y": bounds.get("y") }),
        );
    }
    if let Some(style) = object.remove("style") {
        let mut style = style;
        if let Some(object) = style.as_object_mut() {
            object.remove("background_color");
            object.remove("opacity");
        }
        object.insert("style".to_owned(), style);
    }
    if let Some(text) = object.remove("text") {
        let keep = match serde_json::from_value::<TextRef>(text.clone()) {
            Ok(TextRef::Literal { .. }) => json!({ "kind": "literal" }),
            _ => text,
        };
        if !keep.is_null() {
            object.insert("text".to_owned(), keep);
        }
    }
    value
}

fn hex_color(color: [f32; 4]) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    let (red, green, blue, alpha) = (
        channel(color[0]),
        channel(color[1]),
        channel(color[2]),
        channel(color[3]),
    );
    if alpha == 255 {
        format!("#{red:02x}{green:02x}{blue:02x}")
    } else {
        format!("#{red:02x}{green:02x}{blue:02x}{alpha:02x}")
    }
}

/// Compact stable rendering of a diff for assertions and probe output.
pub fn summarize_operations(diff: &UiTreeDiff) -> Vec<String> {
    diff.operations
        .iter()
        .map(|operation| match operation {
            UiPatchOp::RemoveNode { node_path } => format!("remove {node_path}"),
            UiPatchOp::MoveNode {
                node_path,
                parent_path,
                index,
            } => format!("move {node_path} -> {parent_path}[{index}]"),
            UiPatchOp::InsertNode {
                parent_path,
                index,
                node,
            } => format!("insert {}@{parent_path}[{index}]", node.node_id.0),
            UiPatchOp::ReplaceChildren { parent_path, .. } => {
                format!("replace {parent_path}")
            }
            UiPatchOp::SetProperty {
                node_path,
                property,
                ..
            } => format!("set {node_path}.{property}"),
            UiPatchOp::StartTransition { node_path, .. } => format!("transition {node_path}"),
            UiPatchOp::SetInput { key, .. } => format!("input {key}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use neon_ui_schema::{
        UI_FRAGMENT_SCHEMA_VERSION, UiBounds, UiClipShape, UiNodeId, UiNodeKind, UiStyle,
    };

    fn node(key: &str, kind: UiNodeKind, children: Vec<UiNode>) -> UiNode {
        UiNode {
            node_id: UiNodeId(key.into()),
            kind,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 20.0,
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
            world_scale: None,
            clip_shape: UiClipShape::Rect,
            children,
        }
    }

    fn text(key: &str, value: &str) -> UiNode {
        let mut item = node(key, UiNodeKind::Label, Vec::new());
        item.text = Some(TextRef::Literal {
            value: value.into(),
        });
        item
    }

    fn fixture(name: &str) -> UiNode {
        let source = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .expect("keyed diff fixture exists");
        let value: serde_json::Value =
            serde_json::from_str(&source).expect("fixture is valid JSON");
        assert_eq!(
            value.get("schema_version").and_then(Value::as_u64),
            Some(u64::from(UI_FRAGMENT_SCHEMA_VERSION)),
            "fixture schema_version must match the fragment schema"
        );
        serde_json::from_value(value.get("root").expect("fixture has a root node").clone())
            .expect("fixture root deserializes")
    }

    #[test]
    fn identical_trees_produce_no_operations() {
        let old = node(
            "root",
            UiNodeKind::Panel,
            vec![text("a", "A"), text("b", "B")],
        );
        let new = node(
            "root",
            UiNodeKind::Panel,
            vec![text("a", "A"), text("b", "B")],
        );
        let diff = diff_projection_trees(&old, &new).expect("root matches");
        assert!(diff.is_empty(), "identical trees must not emit ops");
    }

    #[test]
    fn single_property_change_emits_one_set() {
        let old = node("root", UiNodeKind::Panel, vec![text("a", "A")]);
        let mut new_child = text("a", "A");
        new_child.enabled = false;
        let new = node("root", UiNodeKind::Panel, vec![new_child]);
        let diff = diff_projection_trees(&old, &new).expect("root matches");
        assert_eq!(summarize_operations(&diff), ["set root/a.enabled"]);
        match &diff.operations[0] {
            UiPatchOp::SetProperty { value, .. } => assert_eq!(value, &json!(false)),
            other => panic!("unexpected op {other:?}"),
        }
    }

    #[test]
    fn insert_remove_reorder_and_reparent_follow_keys() {
        // old:  root -> [ x[a, b], y[c] ]
        // new:  root -> [ y[b], x[a, d] ]   (y/x reorder, b moves x->y, c removed, d inserted)
        let old = node(
            "root",
            UiNodeKind::Panel,
            vec![
                node("x", UiNodeKind::Panel, vec![text("a", "A"), text("b", "B")]),
                node("y", UiNodeKind::Panel, vec![text("c", "C")]),
            ],
        );
        let new = node(
            "root",
            UiNodeKind::Panel,
            vec![
                node("y", UiNodeKind::Panel, vec![text("b", "B")]),
                node("x", UiNodeKind::Panel, vec![text("a", "A"), text("d", "D")]),
            ],
        );
        let diff = diff_projection_trees(&old, &new).expect("root matches");
        assert_eq!(
            summarize_operations(&diff),
            [
                "remove root/y/c",
                "move root/y -> root[0]",
                "move root/y/b -> root/y[0]",
                "move root/x -> root[1]",
                "insert d@root/x[1]",
            ]
        );
    }

    #[test]
    fn key_change_is_remove_plus_insert_never_reuse() {
        let old = node("root", UiNodeKind::Panel, vec![text("task-1", "one")]);
        let new = node("root", UiNodeKind::Panel, vec![text("task-2", "one")]);
        let diff = diff_projection_trees(&old, &new).expect("root matches");
        assert_eq!(
            summarize_operations(&diff),
            ["remove root/task-1", "insert task-2@root[0]"]
        );
    }

    #[test]
    fn merge_keeps_last_value_per_node_and_property() {
        let mut operations = vec![
            UiPatchOp::SetProperty {
                node_path: "root/a".into(),
                property: "value".into(),
                value: json!("\"running\""),
            },
            UiPatchOp::RemoveNode {
                node_path: "root/b".into(),
            },
            UiPatchOp::SetProperty {
                node_path: "root/a".into(),
                property: "value".into(),
                value: json!("\"done\""),
            },
            UiPatchOp::SetProperty {
                node_path: "root/a".into(),
                property: "enabled".into(),
                value: json!(false),
            },
        ];
        merge_operations(&mut operations);
        assert_eq!(operations.len(), 3);
        assert!(matches!(
            &operations[0],
            UiPatchOp::RemoveNode { node_path } if node_path == "root/b"
        ));
        // The surviving set is the last write, kept at its final position.
        match &operations[1] {
            UiPatchOp::SetProperty { value, .. } => assert_eq!(value, &json!("\"done\"")),
            other => panic!("unexpected op {other:?}"),
        }
    }

    #[test]
    fn unpatchable_identity_change_replaces_only_the_subtree_root() {
        let mut old_child = node(
            "a",
            UiNodeKind::Panel,
            vec![node("deep", UiNodeKind::Label, Vec::new())],
        );
        old_child.bounds.x = 0.0;
        let mut new_child = node(
            "a",
            UiNodeKind::Panel,
            vec![node("deep", UiNodeKind::Label, Vec::new())],
        );
        new_child.bounds.x = 12.0;
        let old = node("root", UiNodeKind::Panel, vec![old_child]);
        let new = node("root", UiNodeKind::Panel, vec![new_child]);
        let diff = diff_projection_trees(&old, &new).expect("root matches");
        assert_eq!(
            summarize_operations(&diff),
            ["remove root/a", "insert a@root[0]"]
        );
    }

    #[test]
    fn root_mismatch_requests_full_remount() {
        let old = node("root", UiNodeKind::Panel, Vec::new());
        let renamed = node("root2", UiNodeKind::Panel, Vec::new());
        assert_eq!(
            diff_projection_trees(&old, &renamed),
            Err(UiDiffRootMismatch::KeyChanged {
                old: "root".into(),
                new: "root2".into(),
            })
        );
        let restyled = node("root", UiNodeKind::Modal, Vec::new());
        assert_eq!(
            diff_projection_trees(&old, &restyled),
            Err(UiDiffRootMismatch::IdentityChanged)
        );
    }

    #[test]
    fn wholesale_child_list_swap_collapses_to_replace_children() {
        let old_children: Vec<UiNode> = (0..10).map(|i| text(&format!("o{i}"), "old")).collect();
        let new_children: Vec<UiNode> = (0..10).map(|i| text(&format!("n{i}"), "new")).collect();
        let old = node("root", UiNodeKind::Panel, old_children);
        let new = node("root", UiNodeKind::Panel, new_children);
        let diff = diff_projection_trees(&old, &new).expect("root matches");
        assert_eq!(summarize_operations(&diff), ["replace root"]);
        match &diff.operations[0] {
            UiPatchOp::ReplaceChildren {
                parent_path,
                children,
            } => {
                assert_eq!(parent_path, "root");
                assert_eq!(children.len(), 10);
            }
            other => panic!("unexpected op {other:?}"),
        }
    }

    #[test]
    fn replace_children_is_blocked_when_a_key_escapes_the_subtree() {
        // old:  root -> [ stage[ a1..a7, deep[o8] ] ]
        // new:  root -> [ stage[ holder[o8], b1, b2 ] ]
        // The kept key o8 escapes stage's old subtree, so stage must fall back
        // to explicit ops instead of ReplaceChildren.
        let mut stage_children: Vec<UiNode> =
            (0..7).map(|i| text(&format!("a{i}"), "old")).collect();
        stage_children.push(node("deep", UiNodeKind::Panel, vec![text("o8", "old")]));
        let old = node(
            "root",
            UiNodeKind::Panel,
            vec![node("stage", UiNodeKind::Panel, stage_children)],
        );
        let new = node(
            "root",
            UiNodeKind::Panel,
            vec![node(
                "stage",
                UiNodeKind::Panel,
                vec![
                    node("holder", UiNodeKind::Panel, vec![text("o8", "old")]),
                    text("b1", "new"),
                    text("b2", "new"),
                ],
            )],
        );
        let diff = diff_projection_trees(&old, &new).expect("root matches");
        let summary = summarize_operations(&diff);
        assert!(
            !summary.iter().any(|entry| entry.starts_with("replace ")),
            "escaped keys must force explicit ops: {summary:?}"
        );
        // o8's old position is removed with its container subtree, and the
        // new holder subtree arrives as one explicit insert.
        assert!(
            summary
                .iter()
                .any(|entry| entry == "remove root/stage/deep")
        );
        assert!(
            summary
                .iter()
                .any(|entry| entry == "insert holder@root/stage[0]")
        );
    }

    #[test]
    fn json_fixture_diff_is_stable_and_replays_to_a_fixed_point() {
        let old = fixture("ui_keyed_diff_old.json");
        let new = fixture("ui_keyed_diff_new.json");
        let diff = diff_projection_trees(&old, &new).expect("root matches");
        let summary = summarize_operations(&diff);
        let raw = std::fs::read_to_string(format!(
            "{}/tests/fixtures/ui_keyed_diff_expected_ops.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("expected ops fixture exists");
        let expected: Vec<String> =
            serde_json::from_str(&raw).expect("expected ops fixture is valid JSON");
        assert_eq!(summary, expected, "keyed diff output drifted");

        // Replay: applying the ops to the old tree yields the new tree, so a
        // second diff is empty (fixed point = ops are complete).
        let applied = apply_ops(&old, &diff.operations);
        assert_eq!(
            serde_json::to_value(&applied).expect("serialize applied"),
            serde_json::to_value(&new).expect("serialize new")
        );
        let second = diff_projection_trees(&applied, &new).expect("root still matches");
        assert!(second.is_empty(), "re-diff must be empty: {second:?}");
    }

    #[test]
    fn diff_ops_apply_through_the_public_patch_contract() {
        let old_source = "version 1\nsurface surface.diff.test revision 3\npanel workspace column gap 4\n  text a value \"A\"\n  text b value \"B\"\n";
        let new_source = "version 1\nsurface surface.diff.test revision 3\npanel workspace column gap 4\n  text c value \"C\"\n  text a value \"A2\"\n";
        let old = crate::nui_flow::parse_nui_flow(old_source).expect("old flow parses");
        let new = crate::nui_flow::parse_nui_flow(new_source).expect("new flow parses");
        let diff = diff_projection_trees(&old.ir.root, &new.ir.root).expect("root matches");
        assert_eq!(
            summarize_operations(&diff),
            [
                "remove workspace/b",
                "insert c@workspace[0]",
                "move workspace/a -> workspace[1]",
                "set workspace/a.value",
            ]
        );
        let patch = build_ui_patch(&old.ir.surface_id, old.ir.revision.0, diff);
        let result = crate::nui_flow::apply_ui_patch(&old.ir, &patch).expect("patch applies");
        let second = diff_projection_trees(&result.root, &new.ir.root).expect("root matches");
        assert!(
            second.is_empty(),
            "public contract replay must reach the new tree: {:?}",
            summarize_operations(&second)
        );
    }

    /// Replays structural+property ops the way the patch pipeline will, but
    /// on bare trees so fixtures stay dependency-free.
    fn apply_ops(root: &UiNode, operations: &[UiPatchOp]) -> UiNode {
        let mut tree = root.clone();
        for operation in operations {
            match operation {
                UiPatchOp::RemoveNode { node_path } => {
                    remove_at(&mut tree, &split_path(node_path));
                }
                UiPatchOp::InsertNode {
                    parent_path,
                    index,
                    node,
                } => {
                    let parent = find(&mut tree, &split_path(parent_path));
                    parent
                        .children
                        .insert((*index).min(parent.children.len()), node.clone());
                }
                UiPatchOp::MoveNode {
                    node_path,
                    parent_path,
                    index,
                } => {
                    let mut path = split_path(node_path);
                    let key = path.pop().expect("move path has a key");
                    let mut taken = None;
                    take(&mut tree, &key, &mut taken);
                    let moved = taken.expect("move source exists");
                    let parent = find(&mut tree, &split_path(parent_path));
                    if let Some(existing) = parent
                        .children
                        .iter()
                        .position(|child| child.node_id.0 == key)
                    {
                        parent.children.remove(existing);
                    }
                    parent
                        .children
                        .insert((*index).min(parent.children.len()), moved);
                }
                UiPatchOp::ReplaceChildren {
                    parent_path,
                    children,
                } => {
                    let parent = find(&mut tree, &split_path(parent_path));
                    parent.children = children.clone();
                }
                UiPatchOp::SetProperty {
                    node_path,
                    property,
                    value,
                } => {
                    let target = find(&mut tree, &split_path(node_path));
                    match property.as_str() {
                        "enabled" => target.enabled = value.as_bool().expect("bool"),
                        "visible" => target.visible = value.as_bool().expect("bool"),
                        "w" => target.bounds.width = value.as_f64().expect("number") as f32,
                        "h" => target.bounds.height = value.as_f64().expect("number") as f32,
                        "opacity" => target.style.opacity = value.as_f64().expect("number") as f32,
                        "value" => {
                            target.text = Some(TextRef::Literal {
                                value: value.as_str().expect("string literal").to_owned(),
                            })
                        }
                        other => panic!("unsupported fixture property {other}"),
                    }
                }
                other => panic!("fixture replay does not handle {other:?}"),
            }
        }
        tree
    }

    fn split_path(path: &str) -> Vec<String> {
        path.split('/').map(str::to_owned).collect()
    }

    fn find<'b>(tree: &'b mut UiNode, segments: &[String]) -> &'b mut UiNode {
        let segments = if segments.first().map(String::as_str) == Some(tree.node_id.0.as_str()) {
            &segments[1..]
        } else {
            segments
        };
        match segments {
            [] => tree,
            [key] => find_key(tree, key).expect("target exists"),
            [key, rest @ ..] => {
                let parent = find_key(tree, key).expect("parent exists");
                find(parent, rest)
            }
        }
    }

    fn find_key<'b>(tree: &'b mut UiNode, key: &str) -> Option<&'b mut UiNode> {
        if tree.node_id.0 == key {
            return Some(tree);
        }
        tree.children
            .iter_mut()
            .find_map(|child| find_key(child, key))
    }

    fn take(tree: &mut UiNode, key: &str, taken: &mut Option<UiNode>) {
        if let Some(index) = tree
            .children
            .iter()
            .position(|child| child.node_id.0 == key)
        {
            *taken = Some(tree.children.remove(index));
            return;
        }
        for child in &mut tree.children {
            if taken.is_none() {
                take(child, key, taken);
            }
        }
    }

    fn remove_at(tree: &mut UiNode, segments: &[String]) -> bool {
        let segments = if segments.first().map(String::as_str) == Some(tree.node_id.0.as_str()) {
            &segments[1..]
        } else {
            segments
        };
        match segments {
            [] => false,
            [key] => {
                if let Some(index) = tree
                    .children
                    .iter()
                    .position(|child| child.node_id.0 == *key)
                {
                    tree.children.remove(index);
                    return true;
                }
                false
            }
            [key, rest @ ..] => {
                let Some(parent) = find_key(tree, key) else {
                    return false;
                };
                remove_at(parent, rest)
            }
        }
    }
}
