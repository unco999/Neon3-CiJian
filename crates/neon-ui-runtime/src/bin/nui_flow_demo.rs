//! Submits an open NUI Flow fixture through the headless UI runtime.

use std::net::SocketAddr;
use std::time::Duration;

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, ServiceName,
};
use neon_ui_runtime::{
    UiDataGridStore, UiInputStore, UiLocalPresentationState, compile_nui_flow_program,
    evaluate_ui_program, lower_nui_flow_effects, parse_nui_flow,
};
use neon_ui_schema::{
    NuiFlowDocument, UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_CAPABILITY_NAME,
    UI_PROGRAM_SCHEMA_VERSION, UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
    UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME, UiBounds, UiCommand, UiCpuViewport, UiDataGridCell,
    UiDataGridFrame, UiDataGridInputFrame, UiDataGridWindowRow, UiFragment, UiFragmentId,
    UiFragmentSubmission, UiInputValue, UiNode, UiNodeKind, UiProgramCapability,
    UiProgramCapabilityOwner, UiProgramCapabilityStatus, UiProgramResource, UiProgramResourceKind,
    UiProgramRevision, UiTextHandle,
};
use serde_json::json;

const ASSET_REVIEW_SOURCE: &str =
    include_str!("../../../../tests/fixtures/ui/asset-review-workbench.nui");
const KANBAN_REPARENT_SOURCE: &str =
    include_str!("../../../../tests/fixtures/ui/kanban-reparent-workbench.nui");
const COMPONENT_GALLERY_SOURCE: &str =
    include_str!("../../../../tests/fixtures/ui/imgui-component-gallery.nui");
const DATA_GRID_SOURCE: &str = include_str!("../../../../tests/fixtures/ui/data-grid-demo.nui");
const SCROLL_VIEW_SOURCE: &str = include_str!("../../../../tests/fixtures/ui/scroll-view-demo.nui");
const VIRTUAL_LIST_SOURCE: &str =
    include_str!("../../../../tests/fixtures/ui/virtual-list-demo.nui");

fn main() {
    let mut args = std::env::args().skip(1);
    let case = args.next().expect("case name is required");
    let endpoint = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:40102".into())
        .parse::<SocketAddr>()
        .expect("UI runtime endpoint must be a socket address");
    let source = match case.as_str() {
        "asset-review" => ASSET_REVIEW_SOURCE,
        "kanban-reparent" => KANBAN_REPARENT_SOURCE,
        "component-gallery" => COMPONENT_GALLERY_SOURCE,
        "data-grid" => DATA_GRID_SOURCE,
        "scroll-view" => SCROLL_VIEW_SOURCE,
        "virtual-list" => VIRTUAL_LIST_SOURCE,
        _ => panic!("unsupported NUI Flow case: {case}"),
    };
    let document = parse_nui_flow(source).expect("NUI Flow fixture must parse");
    let effects = lower_nui_flow_effects(&document);
    let mut fragment = UiFragment {
        fragment_id: UiFragmentId(format!("nui-flow-case-{case}")),
        revision: neon_protocol::Revision(1),
        root: initial_visible_root(&document),
        effects,
    };
    if case == "component-gallery" {
        let (_, program) = neon_ui_runtime::demo_domain::component_gallery_program()
            .expect("component Gallery program must compile");
        let domain = neon_ui_runtime::demo_domain::DemoInputDomain::new(
            program,
            document.input_schema.clone(),
        )
        .expect("component Gallery defaults must activate");
        neon_ui_runtime::demo_domain::apply_visible_status_to_fragment(
            &mut fragment,
            &domain.snapshot(),
        );
        attach_demo_virtual_list_frame(&document, &mut fragment);
    }
    if case == "data-grid" {
        attach_demo_data_grid_frame(&document, &mut fragment);
    }
    if case == "virtual-list" {
        attach_demo_virtual_list_frame(&document, &mut fragment);
    }
    fragment
        .validate()
        .expect("NUI Flow demo fragment must validate before submission");
    let request = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("nui-flow-case-{case}-submit")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "nui-flow-demo".into(),
            pid: std::process::id(),
            origin: "nui-flow-demo".into(),
        },
        target: ServiceName("ui-runtime".into()),
        method: "ui.fragment.submit".into(),
        params: json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment)
        }),
        expected_revision: None,
        idempotency_key: Some(format!("nui-flow-case-{case}-submit-v1")),
    };
    for attempt in 1..=10 {
        match RpcClient::connect(endpoint).and_then(|mut client| client.call(&request)) {
            Ok(response) if matches!(response.status, neon_protocol::RpcStatus::Accepted) => return,
            Ok(response)
                if response.error.as_ref().is_some_and(|error| {
                    error.code == "window_compositor_stale"
                        && error.current_revision == Some(neon_protocol::Revision(1))
                }) =>
            {
                return;
            }
            Ok(response) if attempt == 10 => panic!("NUI Flow case rejected: {:?}", response.error),
            Err(error) if attempt == 10 => panic!("submit NUI Flow case: {error}"),
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn attach_demo_data_grid_frame(document: &NuiFlowDocument, fragment: &mut UiFragment) {
    let program =
        compile_nui_flow_program(document, demo_program_revision(&document.ir.surface_id.0))
            .expect("DataGrid demo fixture must compile");
    let frame = UiDataGridFrame {
        list_revision: Revision(1),
        total_rows: 3,
        first_row: 0,
        window_rows: [
            ("asset-wood", 1, 101, 2, 102),
            ("asset-stone", 3, 103, 4, 104),
            ("asset-water", 5, 105, 6, 106),
        ]
        .into_iter()
        .map(
            |(stable_row_key, name_id, name_display, status_id, status_display)| {
                UiDataGridWindowRow {
                    stable_row_key: stable_row_key.into(),
                    cells: std::collections::BTreeMap::from([
                        (
                            "name".into(),
                            UiDataGridCell {
                                value: UiInputValue::TextHandle {
                                    value: UiTextHandle {
                                        id: name_id,
                                        generation: 1,
                                    },
                                },
                                display: UiTextHandle {
                                    id: name_display,
                                    generation: 1,
                                },
                                presentation_override: None,
                            },
                        ),
                        (
                            "status".into(),
                            UiDataGridCell {
                                value: UiInputValue::TextHandle {
                                    value: UiTextHandle {
                                        id: status_id,
                                        generation: 1,
                                    },
                                },
                                display: UiTextHandle {
                                    id: status_display,
                                    generation: 1,
                                },
                                presentation_override: None,
                            },
                        ),
                    ]),
                }
            },
        )
        .collect(),
        expected_program_revision: program.revision.clone(),
    };
    let mut store = UiDataGridStore::default();
    store
        .apply(
            &program,
            UiDataGridInputFrame {
                source_key: "asset_window".into(),
                frame,
            },
        )
        .expect("DataGrid demo frame must be valid");
    store
        .attach_to_fragment(&program, fragment)
        .expect("DataGrid demo frame must attach");
}

fn attach_demo_virtual_list_frame(document: &NuiFlowDocument, fragment: &mut UiFragment) {
    let program =
        compile_nui_flow_program(document, demo_program_revision(&document.ir.surface_id.0))
            .expect("Virtual list demo fixture must compile");
    let column_keys = program.data_grid_records[0]
        .columns
        .iter()
        .map(|column| column.key.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let row_count = u64::from(program.data_grid_records[0].max_window_rows);
    let first_row = 0_u64;
    let window_rows = (0_u64..row_count)
        .map(|row_index| {
            let handle = |id: u64| UiTextHandle { id, generation: 1 };
            let base = 10_000 + row_index * 5;
            let mut cells: std::collections::BTreeMap<String, UiDataGridCell> =
                std::collections::BTreeMap::from([
                    (
                        "id".into(),
                        UiDataGridCell {
                            value: UiInputValue::I32 {
                                value: row_index as i32,
                            },
                            display: handle(base),
                            presentation_override: None,
                        },
                    ),
                    (
                        "name".into(),
                        UiDataGridCell {
                            value: UiInputValue::TextHandle {
                                value: handle(base + 1),
                            },
                            display: handle(base + 1),
                            presentation_override: None,
                        },
                    ),
                    (
                        "status".into(),
                        UiDataGridCell {
                            value: UiInputValue::Enum {
                                value: "ready".into(),
                            },
                            display: handle(base + 2),
                            presentation_override: None,
                        },
                    ),
                    (
                        "owner".into(),
                        UiDataGridCell {
                            value: UiInputValue::Bool {
                                value: row_index % 2 == 0,
                            },
                            display: handle(base + 3),
                            presentation_override: None,
                        },
                    ),
                    (
                        "notes".into(),
                        UiDataGridCell {
                            value: UiInputValue::TextHandle {
                                value: handle(base + 4),
                            },
                            display: handle(base + 4),
                            presentation_override: None,
                        },
                    ),
                ]);
            cells.retain(|key, _| column_keys.contains(key.as_str()));
            UiDataGridWindowRow {
                stable_row_key: format!("virtual-row-{row_index}"),
                cells,
            }
        })
        .collect();
    let frame = UiDataGridFrame {
        list_revision: Revision(1),
        total_rows: 10_000,
        first_row,
        window_rows,
        expected_program_revision: program.revision.clone(),
    };
    let mut store = UiDataGridStore::default();
    store
        .apply(
            &program,
            UiDataGridInputFrame {
                source_key: "asset_window".into(),
                frame,
            },
        )
        .expect("Virtual list demo frame must be valid");
    store
        .attach_to_fragment(&program, fragment)
        .expect("Virtual list demo frame must attach");
}

fn initial_visible_root(document: &NuiFlowDocument) -> UiNode {
    let mut compile_document = document.clone();
    declare_preview_fallbacks(
        &compile_document.ir.root,
        &mut compile_document.ir.resources,
    );
    let revision = demo_program_revision(&document.ir.surface_id.0);
    let program = compile_nui_flow_program(&compile_document, revision.clone())
        .expect("NUI Flow fixture must compile for initial visibility evaluation");
    let inputs = UiInputStore::activate(revision, document.input_schema.clone())
        .expect("NUI Flow defaults must activate");
    let frame = evaluate_ui_program(
        &program,
        &inputs.snapshot(),
        UiCpuViewport {
            logical_bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
            },
            revision: Revision(1),
        },
        &UiLocalPresentationState::default(),
    );
    let visibility = frame
        .nodes
        .into_iter()
        .map(|node| (node.node_key, node.visible))
        .collect();
    let mut root = document.ir.root.clone();
    apply_evaluated_visibility(&mut root, &visibility);
    root
}

fn declare_preview_fallbacks(node: &UiNode, resources: &mut Vec<UiProgramResource>) {
    let kind = match node.kind {
        UiNodeKind::Image => Some(UiProgramResourceKind::Image),
        UiNodeKind::RenderSurface => Some(UiProgramResourceKind::RenderSurface),
        _ => None,
    };
    if let Some(kind) = kind {
        if !resources
            .iter()
            .any(|resource| resource.key == node.node_id.0)
        {
            resources.push(UiProgramResource {
                key: node.node_id.0.clone(),
                kind,
                has_fallback: true,
            });
        }
    }
    for child in &node.children {
        declare_preview_fallbacks(child, resources);
    }
}

fn apply_evaluated_visibility(
    node: &mut UiNode,
    visibility: &std::collections::BTreeMap<String, bool>,
) {
    node.visible &= visibility.get(&node.node_id.0).copied().unwrap_or(false);
    for child in &mut node.children {
        apply_evaluated_visibility(child, visibility);
    }
}

fn demo_program_revision(surface_id: &str) -> UiProgramRevision {
    UiProgramRevision {
        program_id: format!("{surface_id}.demo"),
        revision: Revision(1),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: [
            UI_PROGRAM_CAPABILITY_NAME,
            UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
            UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
            UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
        ]
        .into_iter()
        .map(|name| UiProgramCapability {
            name: name.into(),
            version: 1,
            owner: UiProgramCapabilityOwner::SharedContract,
            status: UiProgramCapabilityStatus::Supported,
        })
        .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_visibility_hides_inactive_kanban_status_branches() {
        let document = parse_nui_flow(KANBAN_REPARENT_SOURCE).unwrap();
        let root = initial_visible_root(&document);
        let mut visible = std::collections::BTreeMap::new();
        fn collect(node: &UiNode, visible: &mut std::collections::BTreeMap<String, bool>) {
            visible.insert(node.node_id.0.clone(), node.visible);
            for child in &node.children {
                collect(child, visible);
            }
        }
        collect(&root, &mut visible);
        assert_eq!(visible["reparent-pending"], false);
        assert_eq!(visible["reparent-accepted"], false);
        assert_eq!(visible["reparent-rejected"], false);
    }

    #[test]
    fn data_grid_case_attaches_a_bounded_frame() {
        let document = parse_nui_flow(DATA_GRID_SOURCE).unwrap();
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("data-grid-demo-test".into()),
            revision: Revision(1),
            root: initial_visible_root(&document),
            effects: lower_nui_flow_effects(&document),
        };

        attach_demo_data_grid_frame(&document, &mut fragment);

        assert!(fragment.effects.iter().any(|effect| matches!(effect,
            neon_ui_schema::UiEffect::DataGridFrame { declaration, frame }
                if declaration.node_key == "asset-grid" && frame.window_rows.len() == 3
        )));
    }

    #[test]
    fn virtual_list_case_attaches_the_asset_window_frame() {
        let document = parse_nui_flow(VIRTUAL_LIST_SOURCE).unwrap();
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("virtual-list-demo-test".into()),
            revision: Revision(1),
            root: initial_visible_root(&document),
            effects: lower_nui_flow_effects(&document),
        };

        attach_demo_virtual_list_frame(&document, &mut fragment);

        assert!(fragment.effects.iter().any(|effect| matches!(effect,
            neon_ui_schema::UiEffect::DataGridFrame { declaration, frame }
                if declaration.node_key == "virtual-list"
                    && declaration.source_key == "asset_window"
                    && frame.window_rows.len() == 24
        )));
    }

    #[test]
    fn component_gallery_initializes_the_asset_window_through_its_grid_input() {
        let document = parse_nui_flow(COMPONENT_GALLERY_SOURCE).unwrap();
        let mut fragment = UiFragment {
            fragment_id: UiFragmentId("component-gallery-demo-test".into()),
            revision: Revision(1),
            root: initial_visible_root(&document),
            effects: lower_nui_flow_effects(&document),
        };

        attach_demo_virtual_list_frame(&document, &mut fragment);

        assert!(fragment.effects.iter().any(|effect| matches!(effect,
            neon_ui_schema::UiEffect::DataGridFrame { declaration, frame }
                if declaration.node_key == "asset-grid"
                    && declaration.source_key == "asset_window"
                    && frame.window_rows.len() == declaration.max_window_rows as usize
                    && frame.window_rows.len() as u32 * declaration.row_height
                        >= 2 * (872 - declaration.row_height)
        )));
    }
}
