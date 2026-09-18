//! Deterministic contract probe for the versioned Agents workbench UiPatch.

use neon_ui_runtime::{apply_ui_patch, lower_nui_flow, parse_nui_flow};
use neon_ui_schema::{UiPatch, UiPatchOp};
use serde_json::json;

const FLOW: &str = "version 1\nsurface surface.agents revision 7\nsurface root column w 800 h 600\n  panel chat column w 700 h 500\n    text transcript value \"hello\"\n";

fn emit(
    sequence: u64,
    method: &str,
    producer: serde_json::Value,
    consumer: serde_json::Value,
    pass: bool,
    error: Option<String>,
) {
    println!(
        "{}",
        json!({
            "probe": "ui-patch-contract.v1",
            "sequence": sequence,
            "method": method,
            "producer": producer,
            "consumer": consumer,
            "error": error,
            "pass_result": pass,
        })
    );
}

fn main() {
    let document = match parse_nui_flow(FLOW) {
        Ok(document) => document,
        Err(error) => {
            emit(
                1,
                "parse",
                json!({"source_bytes": FLOW.len()}),
                json!({}),
                false,
                Some(format!("{error:?}")),
            );
            std::process::exit(1);
        }
    };
    let ir = lower_nui_flow(&document);
    emit(
        1,
        "parse",
        json!({"surface_id": "surface.agents", "revision": ir.revision}),
        json!({"node_count": count_nodes(&ir.root)}),
        true,
        None,
    );

    let patch = UiPatch {
        surface_id: "surface.agents".into(),
        base_revision: 7,
        operations: vec![
            UiPatchOp::SetProperty {
                node_path: "root/chat/transcript".into(),
                property: "value".into(),
                value: json!("updated transcript"),
            },
            UiPatchOp::InsertNode {
                parent_path: "chat".into(),
                index: 1,
                node: neon_ui_schema::UiNode {
                    node_id: neon_ui_schema::UiNodeId("tool-call-1".into()),
                    kind: neon_ui_schema::UiNodeKind::Panel,
                    bounds: neon_ui_schema::UiBounds {
                        x: 0.0,
                        y: 0.0,
                        width: 640.0,
                        height: 48.0,
                    },
                    layout: Some(neon_ui_schema::UiLayout::default()),
                    visible: true,
                    enabled: true,
                    text_key: None,
                    text: None,
                    image: None,
                    surface: None,
                    style: neon_ui_schema::UiStyle::default(),
                    enter_transition: None,
                    world_depth: None,
                    world_scale: None,
                    clip_shape: neon_ui_schema::UiClipShape::default(),
                    children: Vec::new(),
                },
            },
        ],
    };
    let patched = apply_ui_patch(&ir, &patch);
    let pass = patched
        .as_ref()
        .is_ok_and(|value| value.revision.0 == 8 && count_nodes(&value.root) == 4);
    emit(
        2,
        "ui.patch.apply",
        json!({"base_revision": 7, "operation_count": 2}),
        patched
            .as_ref()
            .map(
                |value| json!({"revision": value.revision, "node_count": count_nodes(&value.root)}),
            )
            .unwrap_or_default(),
        pass,
        patched.as_ref().err().map(|error| format!("{error:?}")),
    );

    let stale = apply_ui_patch(
        &ir,
        &UiPatch {
            base_revision: 6,
            ..patch
        },
    );
    let stale_pass = stale.is_err();
    emit(
        3,
        "ui.patch.stale",
        json!({"base_revision": 6, "current_revision": 7}),
        json!({"rejected": stale_pass}),
        stale_pass,
        stale.err().map(|error| format!("{error:?}")),
    );

    let final_pass = pass && stale_pass;
    println!(
        "{}",
        json!({"probe": "ui-patch-contract.v1", "final": true, "pass_result": final_pass})
    );
    if !final_pass {
        std::process::exit(1);
    }
}

fn count_nodes(node: &neon_ui_schema::UiNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}
