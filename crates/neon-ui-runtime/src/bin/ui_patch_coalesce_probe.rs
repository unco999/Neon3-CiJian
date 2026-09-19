//! Deterministic probe for the Phase 3 patch coalescing contract.
//!
//! Plan acceptance mapped to cases:
//!
//! - 50 simultaneous transaction changes produce at most one patch.
//! - A high-frequency token stream emits only a property set (no structural
//!   work that could force a full Flow compile).
//! - Patch `base_revision` values strictly increase; an unacked in-flight
//!   patch blocks the next flush.
//! - Duplicate projection state never produces a patch.
//! - A rejection adopts the authoritative revision and discards the stale
//!   batch.
//!
//! Every flushed patch that claims a live document revision is applied
//! through the public `apply_ui_patch` contract and re-diffed to prove the
//! consumer reaches the producer's target tree. Output is JSONL.

use std::time::Instant;

use neon_ui_runtime::{
    apply_ui_patch, lower_nui_flow, parse_nui_flow,
    ui_keyed_diff::{UiTreeDiff, diff_projection_trees, summarize_operations},
    ui_patch_batcher::UiPatchBatcher,
};
use neon_ui_schema::{UiIrDocument, UiNode, UiPatch, UiPatchOp};
use serde_json::{Value, json};

const SURFACE: &str = "surface.coalesce";
const BASE_HEADER: &str =
    "version 1\nsurface surface.coalesce revision 11\npanel root column w 800 h 600\n";

fn ir_with(source: &str) -> UiIrDocument {
    let document = parse_nui_flow(source).expect("probe flow parses");
    lower_nui_flow(&document)
}

fn fifty_flow(state: &str) -> String {
    let mut source = BASE_HEADER.to_owned();
    for index in 0..50 {
        source.push_str(&format!("  text tx{index} value \"tx{index} {state}\"\n"));
    }
    source
}

fn stream_flow(value: &str) -> String {
    format!("{BASE_HEADER}  text stream value \"{value}\"\n")
}

fn mixed_flow(old_or_new: bool) -> String {
    if old_or_new {
        format!(
            "{BASE_HEADER}  panel list column w 300 h 500\n    text a value \"A\"\n    text b value \"B\"\n    text c value \"C\"\n"
        )
    } else {
        format!(
            "{BASE_HEADER}  panel list column w 300 h 500\n    text a value \"A2\"\n    text c value \"C\"\n    text d value \"D\"\n"
        )
    }
}

fn node_count(node: &UiNode) -> usize {
    1 + node.children.iter().map(node_count).sum::<usize>()
}

fn set_op(node_path: &str, property: &str, value: Value) -> UiPatchOp {
    UiPatchOp::SetProperty {
        node_path: node_path.into(),
        property: property.into(),
        value,
    }
}

fn structural_count(patch: &UiPatch) -> usize {
    patch
        .operations
        .iter()
        .filter(|op| !matches!(op, UiPatchOp::SetProperty { .. }))
        .count()
}

fn diff_ops(old: &UiIrDocument, new: &UiIrDocument) -> UiTreeDiff {
    diff_projection_trees(&old.root, &new.root)
        .unwrap_or_else(|reason| panic!("probe roots must stay patchable, got {reason:?}"))
}

/// Applies a flushed patch against the document it was based on and proves
/// the fixed point: re-diffing the applied tree against `target` is empty.
fn consumer_leg(
    base_document: &UiIrDocument,
    patch: &UiPatch,
    target: &UiIrDocument,
) -> (bool, Value) {
    match apply_ui_patch(base_document, patch) {
        Ok(applied) => {
            let rediff = diff_projection_trees(&applied.root, &target.root)
                .expect("applied root still matches");
            let fixed_point = rediff.is_empty();
            (
                fixed_point,
                json!({
                    "revision_after": applied.revision.0,
                    "node_count": node_count(&applied.root),
                    "fixed_point": fixed_point,
                    "residual_ops": summarize_operations(&rediff),
                }),
            )
        }
        Err(error) => (false, json!({"error": format!("{error:?}")})),
    }
}

fn emit(case: &str, sequence: u64, payload: Value) {
    let mut record = payload;
    if let Some(object) = record.as_object_mut() {
        object.insert("probe".to_owned(), json!("ui-patch-coalesce.v1"));
        object.insert("sequence".to_owned(), json!(sequence));
        object.insert("case".to_owned(), json!(case));
    }
    println!("{record}");
}

/// Case 1: fifty concurrent transaction status changes flush as one patch.
fn case_fifty_one_patch() -> bool {
    let old = ir_with(&fifty_flow("active"));
    let new = ir_with(&fifty_flow("done"));
    let started = Instant::now();
    let diff = diff_ops(&old, &new);
    let mut batcher = UiPatchBatcher::new(SURFACE, old.revision.0);
    // Each domain event arrives on its own tick; the batcher must merge them.
    for operation in diff.operations {
        batcher.enqueue([operation]);
    }
    let patch = batcher.flush().expect("50 events must flush one patch");
    let flush_ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut pass = patch.base_revision == 11
        && patch.operations.len() == 50
        && batcher.flush().is_none()
        && structural_count(&patch) == 0;
    let (applied_pass, consumer) = consumer_leg(&old, &patch, &new);
    pass = pass && applied_pass;
    emit(
        "fifty_events_one_patch",
        1,
        json!({
            "input": {"old_nodes": node_count(&old.root), "new_nodes": node_count(&new.root), "events": 50},
            "batcher": {"patches_emitted": 1, "ops_in_patch": patch.operations.len(), "base_revision": patch.base_revision, "structural_ops": structural_count(&patch)},
            "consumer": consumer,
            "timing_ms": {"enqueue_and_flush": flush_ms},
            "pass": pass,
        }),
    );
    pass
}

/// Case 2: a token stream coalesces to the single final value and never
/// emits structural operations.
fn case_token_stream() -> bool {
    let final_text = "Hello wgpu world";
    let old = ir_with(&stream_flow("H"));
    let new = ir_with(&stream_flow(final_text));
    let mut batcher = UiPatchBatcher::new(SURFACE, old.revision.0);
    for taken in 1..=final_text.chars().count() {
        let partial: String = final_text.chars().take(taken).collect();
        batcher.enqueue([set_op("root/stream", "value", Value::String(partial))]);
    }
    let patch = batcher.flush().expect("token batch flushes once");
    let mut pass = patch.operations.len() == 1 && structural_count(&patch) == 0;
    if let Some(UiPatchOp::SetProperty { value, .. }) = patch.operations.first() {
        pass = pass && value == &Value::String(final_text.into());
    } else {
        pass = false;
    }
    let (applied_pass, consumer) = consumer_leg(&old, &patch, &new);
    pass = pass && applied_pass;
    emit(
        "token_stream_last_value_wins",
        2,
        json!({
            "input": {"token_events": final_text.chars().count()},
            "batcher": {"patches_emitted": 1, "ops_in_patch": patch.operations.len(), "structural_ops": structural_count(&patch), "base_revision": patch.base_revision},
            "consumer": consumer,
            "pass": pass,
        }),
    );
    pass
}

/// Case 3: three rounds where the next change arrives while the previous
/// patch is still unacked; revisions strictly increase and every blocked
/// flush proves the ack gate.
fn case_ack_gate() -> bool {
    let mut doc = ir_with(&stream_flow("V0"));
    let mut batcher = UiPatchBatcher::new(SURFACE, doc.revision.0);
    let mut bases = Vec::new();
    let mut blocked = 0usize;
    let mut pass = true;
    let mut consumer = json!({});
    let mut pending: Option<(UiPatch, UiIrDocument)> = None;
    for round in 1..=3 {
        let target = ir_with(&stream_flow(&format!("V{round}")));
        batcher.enqueue(diff_ops(&doc, &target).operations);
        if pending.is_some() {
            if batcher.flush().is_some() {
                pass = false;
            } else {
                blocked += 1;
            }
            // The renderer now confirms the previous patch; the merged
            // batch becomes releasable.
            let Some((patch, base_doc)) = pending.take() else {
                unreachable!()
            };
            match apply_ui_patch(&base_doc, &patch) {
                Ok(applied) => {
                    batcher.note_ack(applied.revision.0);
                    doc = applied;
                }
                Err(error) => {
                    pass = false;
                    consumer = json!({"error": format!("{error:?}")});
                    break;
                }
            }
        }
        let Some(patch) = batcher.flush() else {
            pass = false;
            break;
        };
        bases.push(patch.base_revision);
        pending = Some((patch, doc.clone()));
    }
    if let Some((patch, base_doc)) = pending.take() {
        let target = ir_with(&stream_flow("V3"));
        let (applied_pass, leg) = consumer_leg(&base_doc, &patch, &target);
        consumer = leg;
        pass = pass && applied_pass;
    }
    let strictly_increasing = bases.windows(2).all(|pair| pair[0] < pair[1]);
    pass = pass && strictly_increasing && bases == [11, 12, 13] && blocked == 2;
    emit(
        "ack_gate_and_strict_revisions",
        3,
        json!({
            "input": {"rounds": 3},
            "batcher": {"base_revisions": bases, "blocked_flushes": blocked, "strictly_increasing": strictly_increasing},
            "consumer": consumer,
            "pass": pass,
        }),
    );
    pass
}

/// Case 4: re-diffing identical projections yields zero operations, so the
/// batcher never emits for duplicate state.
fn case_duplicate_state() -> bool {
    let doc = ir_with(&fifty_flow("active"));
    let same = ir_with(&fifty_flow("active"));
    let mut batcher = UiPatchBatcher::new(SURFACE, doc.revision.0);
    batcher.enqueue(diff_ops(&doc, &same).operations);
    let mut pass = !batcher.is_dirty() && batcher.flush().is_none();
    // A real patch followed by duplicate state still emits nothing new.
    let changed = ir_with(&fifty_flow("done"));
    batcher.enqueue(diff_ops(&doc, &changed).operations);
    let emitted = batcher.flush().map(|patch| patch.operations.len());
    batcher.note_ack(12);
    batcher.enqueue(diff_ops(&changed, &changed).operations);
    let second = batcher.flush();
    pass = pass && emitted == Some(50) && !batcher.is_dirty() && second.is_none();
    emit(
        "duplicate_state_no_patch",
        4,
        json!({
            "input": {"duplicate_diffs": 2},
            "batcher": {"first_flush_ops": emitted.unwrap_or(0), "second_flush_emitted": second.is_some()},
            "pass": pass,
        }),
    );
    pass
}

/// Case 5: events of mixed kinds keep structural arrival order, coalesce
/// property sets to the last value, and converge when applied once.
fn case_mixed_ops_converge() -> bool {
    let old = ir_with(&mixed_flow(true));
    let new = ir_with(&mixed_flow(false));
    let diff = diff_ops(&old, &new);
    let summary = summarize_operations(&diff);
    let mut remove = None;
    let mut insert = None;
    let mut final_set = None;
    for operation in diff.operations {
        match &operation {
            UiPatchOp::RemoveNode { .. } => remove = Some(operation),
            UiPatchOp::InsertNode { .. } => insert = Some(operation),
            UiPatchOp::SetProperty { .. } => final_set = Some(operation),
            other => panic!("probe expects remove/insert/set, got {other:?}"),
        }
    }
    let mut batcher = UiPatchBatcher::new(SURFACE, old.revision.0);
    // Events interleave with a stale property write for the same node.
    batcher.enqueue(remove);
    batcher.enqueue([set_op("root/list/a", "value", Value::String("A1".into()))]);
    batcher.enqueue(insert);
    batcher.enqueue(final_set);
    let patch = batcher.flush().expect("mixed batch flushes");
    let flushed = summarize_operations(&UiTreeDiff {
        operations: patch.operations.clone(),
    });
    let mut pass = flushed
        == [
            "remove root/list/b",
            "insert d@root/list[2]",
            "set root/list/a.value",
        ];
    let (applied_pass, consumer) = consumer_leg(&old, &patch, &new);
    pass = pass && applied_pass;
    emit(
        "mixed_ops_keep_order_and_converge",
        5,
        json!({
            "input": {"producer_summary": summary},
            "batcher": {"flushed_ops": flushed, "base_revision": patch.base_revision},
            "consumer": consumer,
            "pass": pass,
        }),
    );
    pass
}

/// Case 6: a rejection adopts the runtime's authoritative revision and the
/// stale pending batch is discarded, never re-sent.
fn case_rejection_reset() -> bool {
    let old = ir_with(&stream_flow("V0"));
    let new = ir_with(&stream_flow("V1"));
    let mut batcher = UiPatchBatcher::new(SURFACE, old.revision.0);
    batcher.enqueue(diff_ops(&old, &new).operations);
    let first = batcher.flush().expect("first batch flushes");
    batcher.enqueue([set_op(
        "root/stream",
        "value",
        Value::String("stale".into()),
    )]);
    batcher.note_rejected(20);
    let mut pass = first.base_revision == 11
        && !batcher.is_in_flight()
        && !batcher.is_dirty()
        && batcher.flush().is_none();
    let fresh = ir_with(&stream_flow("V9"));
    batcher.enqueue(diff_ops(&ir_with(&stream_flow("V8")), &fresh).operations);
    let patch = batcher.flush().expect("post-rejection batch flushes");
    pass = pass
        && patch.base_revision == 20
        && summarize_operations(&UiTreeDiff {
            operations: patch.operations.clone(),
        }) == ["set root/stream.value"];
    emit(
        "rejection_adopts_authoritative_revision",
        6,
        json!({
            "input": {"rejected_base": 11, "authoritative_revision": 20},
            "batcher": {"stale_batch_discarded": !batcher.is_dirty(), "next_base_revision": patch.base_revision},
            "pass": pass,
        }),
    );
    pass
}

fn main() {
    let mut failures = 0usize;
    let mut check = |name: &str, pass: bool| {
        if !pass {
            failures += 1;
            eprintln!("case {name} failed");
        }
    };
    check("fifty_events_one_patch", case_fifty_one_patch());
    check("token_stream_last_value_wins", case_token_stream());
    check("ack_gate_and_strict_revisions", case_ack_gate());
    check("duplicate_state_no_patch", case_duplicate_state());
    check(
        "mixed_ops_keep_order_and_converge",
        case_mixed_ops_converge(),
    );
    check(
        "rejection_adopts_authoritative_revision",
        case_rejection_reset(),
    );

    println!(
        "{}",
        json!({"probe": "ui-patch-coalesce.v1", "final": true, "pass": failures == 0, "failed_cases": failures})
    );
    if failures != 0 {
        std::process::exit(1);
    }
}
