//! Deterministic producer probe for the Phase 2 keyed UI diff.
//!
//! Each case diffs an old Flow IR tree against a new one, applies the emitted
//! operations through the public `apply_ui_patch` contract, and asserts the
//! fixed point: re-diffing the applied tree against the target produces zero
//! operations. Output is JSONL in the shared probe envelope.

use std::time::Instant;

use neon_ui_runtime::{
    apply_ui_patch, lower_nui_flow, parse_nui_flow,
    ui_keyed_diff::{UiTreeDiff, build_ui_patch, diff_projection_trees, summarize_operations},
};
use neon_ui_schema::{UiIrDocument, UiNode, UiPatchOp};
use serde_json::{Value, json};

const BASE: &str = "version 1\nsurface surface.diff revision 1\npanel root column w 800 h 600\n  panel list column w 300 h 500\n    text a value \"A\"\n    text b value \"B\"\n    text c value \"C\"\n  panel detail row w 500 h 500\n    text title value \"T\"\n";

const SWAP_HEADER: &str = "version 1\nsurface surface.diff revision 1\npanel root column w 800 h 600\n  panel list column w 300 h 500\n";

fn ir_with(source: &str) -> UiIrDocument {
    let document = parse_nui_flow(source).expect("probe flow parses");
    lower_nui_flow(&document)
}

fn swap_flow(prefix: &str) -> String {
    let mut source = SWAP_HEADER.to_owned();
    for index in 0..8 {
        source.push_str(&format!(
            "    text {prefix}{index} value \"{prefix}{index}\"\n"
        ));
    }
    source
}

fn node_count(node: &UiNode) -> usize {
    1 + node.children.iter().map(node_count).sum::<usize>()
}

fn ops_by_kind(diff: &UiTreeDiff) -> serde_json::Value {
    let mut counts = serde_json::Map::new();
    for operation in &diff.operations {
        let key = match operation {
            UiPatchOp::SetProperty { .. } => "set",
            UiPatchOp::InsertNode { .. } => "insert",
            UiPatchOp::RemoveNode { .. } => "remove",
            UiPatchOp::ReplaceChildren { .. } => "replace",
            UiPatchOp::MoveNode { .. } => "move",
            UiPatchOp::StartTransition { .. } => "transition",
            UiPatchOp::SetInput { .. } => "input",
        };
        let next = counts.get(key).and_then(Value::as_u64).unwrap_or(0) + 1;
        counts.insert(key.to_owned(), json!(next));
    }
    serde_json::Value::Object(counts)
}

fn emit(case: &str, sequence: u64, payload: serde_json::Value) {
    let mut record = payload;
    if let Some(object) = record.as_object_mut() {
        object.insert("probe".to_owned(), json!("ui-patch-keyed-diff.v1"));
        object.insert("sequence".to_owned(), json!(sequence));
        object.insert("case".to_owned(), json!(case));
    }
    println!("{record}");
}

fn run_case(
    case: &str,
    sequence: u64,
    old_source: &str,
    new_source: &str,
    expected: &[&str],
) -> bool {
    let old = ir_with(old_source);
    let new = ir_with(new_source);
    let started = Instant::now();
    let diff = diff_projection_trees(&old.root, &new.root)
        .unwrap_or_else(|reason| panic!("case {case}: root must stay patchable, got {reason:?}"));
    let diff_ms = started.elapsed().as_secs_f64() * 1000.0;
    let summary = summarize_operations(&diff);
    let mut pass = summary == expected;
    let mut consumer = json!({});
    if pass {
        let patch = build_ui_patch(&old.surface_id, old.revision.0, diff.clone());
        let started = Instant::now();
        match apply_ui_patch(&old, &patch) {
            Ok(applied) => {
                let apply_ms = started.elapsed().as_secs_f64() * 1000.0;
                let rediff = diff_projection_trees(&applied.root, &new.root)
                    .expect("applied root still matches");
                consumer = json!({
                    "revision_after": applied.revision.0,
                    "node_count": node_count(&applied.root),
                    "fixed_point": rediff.is_empty(),
                    "residual_ops": summarize_operations(&rediff),
                    "timing_ms": {"apply": apply_ms},
                });
                pass = rediff.is_empty();
            }
            Err(error) => {
                consumer = json!({"error": format!("{error:?}")});
                pass = false;
            }
        }
    }
    emit(
        case,
        sequence,
        json!({
            "input": {
                "old_nodes": node_count(&old.root),
                "new_nodes": node_count(&new.root),
            },
            "producer": {
                "operations": summary,
                "ops_by_kind": ops_by_kind(&diff),
                "operation_count": diff.operations.len(),
            },
            "consumer": consumer,
            "timing_ms": {"diff": diff_ms},
            "retained": {"expected_ops": expected},
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

    check("no_op", run_case("no_op", 1, BASE, BASE, &[]));
    check(
        "set_only",
        run_case(
            "set_only",
            2,
            BASE,
            &BASE.replace("text a value \"A\"", "text a value \"A2\""),
            &["set root/list/a.value"],
        ),
    );
    check(
        "insert",
        run_case(
            "insert",
            3,
            BASE,
            &BASE.replace(
                "    text c value \"C\"\n",
                "    text c value \"C\"\n    text d value \"D\"\n",
            ),
            &["insert d@root/list[3]"],
        ),
    );
    check(
        "remove",
        run_case(
            "remove",
            4,
            BASE,
            &BASE.replace("    text b value \"B\"\n", ""),
            // Removing a sibling must not ripple into moves for survivors.
            &["remove root/list/b"],
        ),
    );
    check(
        "reorder_moves_only",
        run_case(
            "reorder_moves_only",
            5,
            BASE,
            &BASE.replace(
                "    text a value \"A\"\n    text b value \"B\"\n    text c value \"C\"\n",
                "    text c value \"C\"\n    text a value \"A\"\n    text b value \"B\"\n",
            ),
            &["move root/list/c -> root/list[0]"],
        ),
    );
    check(
        "key_change_remove_insert",
        run_case(
            "key_change_remove_insert",
            6,
            BASE,
            &BASE.replace("text b value \"B\"", "text b2 value \"B\""),
            &["remove root/list/b", "insert b2@root/list[1]"],
        ),
    );
    check(
        "wholesale_swap_replace_children",
        run_case(
            "wholesale_swap_replace_children",
            7,
            &swap_flow("p"),
            &swap_flow("q"),
            &["replace root/list"],
        ),
    );

    println!(
        "{}",
        json!({"probe": "ui-patch-keyed-diff.v1", "final": true, "pass": failures == 0, "failed_cases": failures})
    );
    if failures != 0 {
        std::process::exit(1);
    }
}
