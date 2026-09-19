//! Phase 3: patch coalescing and frame batching.
//!
//! Domain events arrive one at a time, but the renderer wants few, ordered
//! patches. `UiPatchBatcher` accumulates the operations produced by the
//! keyed diff (or any typed producer) into a single pending batch and emits
//! at most one [`UiPatch`] per flush.
//!
//! Coalescing rules from the plan:
//!
//! - Repeated `SetProperty` writes to the same `(node_path, property)`
//!   collapse to the last value (`RUNNING -> WAITING -> DONE` sends `DONE`).
//! - Many node changes flush as one patch under one revision.
//! - Structural operations keep their arrival order because remove/move/
//!   insert/replace are not commutative.
//! - A duplicate state adds nothing, and an empty batch flushes to `None`.
//!
//! Revision discipline is enforced by the ack gate: while a flushed patch is
//! in flight, `flush` refuses to emit another one, so the producer can never
//! advance past a revision the renderer has not confirmed. Strictly
//! increasing `base_revision` values follow from `note_ack` /
//! `note_rejected` being the only way to move the base forward.

use std::collections::BTreeMap;

use neon_ui_schema::{UiPatch, UiPatchOp};
use serde_json::Value;

/// Single-producer coalescer from domain operations to revisioned `UiPatch`
/// batches.
#[derive(Debug)]
pub struct UiPatchBatcher {
    surface_id: String,
    /// Revision the runtime has confirmed as applied. The next emitted patch
    /// always uses this as its `base_revision`.
    acked_revision: u64,
    /// True while a flushed patch has not been acked or rejected.
    in_flight: bool,
    /// Property sets keyed by `(node_path, property)`; last value wins.
    pending_sets: BTreeMap<(String, String), Value>,
    /// Structural, transition, and input operations in arrival order.
    pending_ordered: Vec<UiPatchOp>,
}

impl UiPatchBatcher {
    /// Starts a batcher for the surface whose last confirmed revision is
    /// `acked_revision` (the value the consumer's IR holds right now).
    pub fn new(surface_id: impl Into<String>, acked_revision: u64) -> Self {
        Self {
            surface_id: surface_id.into(),
            acked_revision,
            in_flight: false,
            pending_sets: BTreeMap::new(),
            pending_ordered: Vec::new(),
        }
    }

    pub fn surface_id(&self) -> &str {
        &self.surface_id
    }

    pub fn acked_revision(&self) -> u64 {
        self.acked_revision
    }

    pub fn is_in_flight(&self) -> bool {
        self.in_flight
    }

    /// True when at least one operation is waiting to be flushed.
    pub fn is_dirty(&self) -> bool {
        !self.pending_sets.is_empty() || !self.pending_ordered.is_empty()
    }

    /// Absorbs one producer update. `SetProperty` operations coalesce by
    /// `(node_path, property)`; every other operation kind is appended to
    /// the ordered tail. Enqueuing an empty slice (for example, the result
    /// of diffing two identical projections) marks nothing dirty.
    pub fn enqueue(&mut self, operations: impl IntoIterator<Item = UiPatchOp>) {
        for operation in operations {
            match operation {
                UiPatchOp::SetProperty {
                    node_path,
                    property,
                    value,
                } => {
                    self.pending_sets.insert((node_path, property), value);
                }
                other => self.pending_ordered.push(other),
            }
        }
    }

    /// Builds the next patch when the batch is non-empty and no previous
    /// patch is awaiting acknowledgement. Emptiness alone (a duplicate
    /// state) yields `None` without touching revision state.
    pub fn flush(&mut self) -> Option<UiPatch> {
        if self.in_flight || !self.is_dirty() {
            return None;
        }
        let mut operations =
            Vec::with_capacity(self.pending_ordered.len() + self.pending_sets.len());
        operations.append(&mut self.pending_ordered);
        // `into_iter` keeps BTreeMap order, so coalesced sets land grouped by
        // node path and then property.
        for ((node_path, property), value) in std::mem::take(&mut self.pending_sets) {
            operations.push(UiPatchOp::SetProperty {
                node_path,
                property,
                value,
            });
        }
        self.in_flight = true;
        Some(UiPatch {
            surface_id: self.surface_id.clone(),
            base_revision: self.acked_revision,
            operations,
        })
    }

    /// Confirms the in-flight patch applied at `revision` (the runtime's new
    /// revision). The next flush will build on that revision.
    pub fn note_ack(&mut self, revision: u64) {
        self.in_flight = false;
        self.acked_revision = revision;
    }

    /// Confirms the in-flight patch was rejected. `current_revision` is the
    /// runtime's authoritative revision; the batcher adopts it as its base,
    /// discards the stale pending batch, and becomes ready for the producer
    /// to re-diff from the fresh snapshot.
    pub fn note_rejected(&mut self, current_revision: u64) {
        self.in_flight = false;
        self.acked_revision = current_revision;
        self.pending_sets.clear();
        self.pending_ordered.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_keyed_diff::{diff_projection_trees, summarize_operations};
    use neon_ui_schema::{TextRef, UiBounds, UiClipShape, UiNode, UiNodeId, UiNodeKind, UiStyle};

    fn node(key: &str, children: Vec<UiNode>) -> UiNode {
        UiNode {
            node_id: UiNodeId(key.into()),
            kind: UiNodeKind::Panel,
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
        let mut item = node(key, Vec::new());
        item.kind = UiNodeKind::Label;
        item.text = Some(TextRef::Literal {
            value: value.into(),
        });
        item
    }

    fn set(node_path: &str, property: &str, value: Value) -> UiPatchOp {
        UiPatchOp::SetProperty {
            node_path: node_path.into(),
            property: property.into(),
            value,
        }
    }

    #[test]
    fn fifty_state_changes_flush_as_one_patch() {
        let mut batcher = UiPatchBatcher::new("surface.agents", 11);
        for index in 0..50 {
            batcher.enqueue([set(
                &format!("root/tx{index}"),
                "fill",
                Value::String("success".into()),
            )]);
        }
        let patch = batcher.flush().expect("50 changes must flush a patch");
        assert_eq!(patch.base_revision, 11);
        assert_eq!(patch.operations.len(), 50);
        assert_eq!(patch.surface_id, "surface.agents");
        // The ack gates everything until the renderer confirms.
        batcher.enqueue([set("root/tx0", "fill", Value::String("failed".into()))]);
        assert!(batcher.flush().is_none(), "no second patch before ack");
        batcher.note_ack(12);
        let retry = batcher.flush().expect("queued change flushes after ack");
        assert_eq!(retry.base_revision, 12, "revision strictly increases");
        assert_eq!(retry.operations.len(), 1);
    }

    #[test]
    fn repeated_property_writes_coalesce_to_the_last_value() {
        let mut batcher = UiPatchBatcher::new("surface.agents", 3);
        for status in ["active", "warning", "success"] {
            batcher.enqueue([set(
                "root/transaction-1",
                "fill",
                Value::String(status.into()),
            )]);
        }
        let patch = batcher.flush().expect("pending set flushes");
        assert_eq!(patch.operations.len(), 1, "only the last value survives");
        match &patch.operations[0] {
            UiPatchOp::SetProperty { value, .. } => {
                assert_eq!(value, &Value::String("success".into()))
            }
            other => panic!("unexpected op {other:?}"),
        }
    }

    #[test]
    fn duplicate_projection_state_never_produces_a_patch() {
        let old = node("root", vec![text("a", "A"), text("b", "B")]);
        let same = node("root", vec![text("a", "A"), text("b", "B")]);
        let mut batcher = UiPatchBatcher::new("surface.agents", 7);
        // Producers feed diff output, never raw assertions: an identical
        // projection contributes zero operations and marks nothing dirty.
        let diff = diff_projection_trees(&old, &same).expect("root matches");
        assert!(diff.is_empty());
        batcher.enqueue(diff.operations);
        assert!(!batcher.is_dirty());
        assert!(batcher.flush().is_none(), "duplicate state must not emit");
    }

    #[test]
    fn structural_ops_keep_arrival_order_and_sets_sort_last() {
        let mut batcher = UiPatchBatcher::new("surface.agents", 0);
        batcher.enqueue([set("root/b", "value", Value::String("\"B2\"".into()))]);
        batcher.enqueue([UiPatchOp::RemoveNode {
            node_path: "root/a".into(),
        }]);
        batcher.enqueue([set("root/c", "value", Value::String("\"C2\"".into()))]);
        let patch = batcher.flush().expect("mixed batch flushes");
        let summary = summarize_operations(&crate::ui_keyed_diff::UiTreeDiff {
            operations: patch.operations,
        });
        assert_eq!(
            summary,
            ["remove root/a", "set root/b.value", "set root/c.value"],
            "structural ops keep arrival order; coalesced sets land last"
        );
    }

    #[test]
    fn rejection_resets_base_and_discards_the_stale_batch() {
        let mut batcher = UiPatchBatcher::new("surface.agents", 4);
        batcher.enqueue([set("root/x", "visible", Value::Bool(true))]);
        assert!(batcher.flush().is_some());
        batcher.enqueue([set("root/y", "visible", Value::Bool(true))]);
        batcher.note_rejected(9);
        assert!(!batcher.is_in_flight());
        assert!(
            !batcher.is_dirty(),
            "a rejected batch is stale and must be re-diffed from the fresh snapshot"
        );
        assert!(
            batcher.flush().is_none(),
            "a rejection never re-sends the stale batch"
        );
        batcher.enqueue([set("root/z", "visible", Value::Bool(true))]);
        let patch = batcher
            .flush()
            .expect("post-rejection batches build on the authoritative revision");
        assert_eq!(patch.base_revision, 9);
        let summary = summarize_operations(&crate::ui_keyed_diff::UiTreeDiff {
            operations: patch.operations,
        });
        assert_eq!(summary, ["set root/z.visible"]);
    }
}
