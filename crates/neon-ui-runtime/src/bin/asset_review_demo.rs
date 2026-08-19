use std::net::SocketAddr;

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, ServiceName,
};
use neon_ui_runtime::{lower_nui_flow_effects, parse_nui_flow};
use neon_ui_schema::{TextRef, UiCommand, UiFragment, UiFragmentId, UiFragmentSubmission, UiNode};
use serde_json::json;

const SOURCE: &str = include_str!("../../tests/fixtures/ui/asset-review-workbench.nui");

fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:40101".into())
        .parse::<SocketAddr>()
        .expect("endpoint must be a socket address");
    let document = parse_nui_flow(SOURCE).expect("asset review fixture must parse");
    let effects = lower_nui_flow_effects(&document);
    let mut root = document.ir.root;
    prepare_demo_tree(&mut root);
    let fragment = UiFragment {
        fragment_id: UiFragmentId("asset-review-demo".into()),
        revision: Revision(1),
        root,
        effects,
    };
    let request = RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId("asset-review-demo-submit".into()),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "asset-review-demo".into(),
            pid: std::process::id(),
            origin: "asset-review-demo".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: "wgpu.ui.submit_fragment".into(),
        params: json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission::new(fragment),
        }),
        expected_revision: None,
        idempotency_key: Some("asset-review-demo-submit-v4".into()),
    };
    let mut client = RpcClient::connect(endpoint).expect("connect to window server");
    let response = client.call(&request).expect("submit complex Flow fragment");
    if !matches!(response.status, neon_protocol::RpcStatus::Accepted) {
        panic!(
            "window server rejected complex Flow fragment: {:?}",
            response.error
        );
    }
}

fn prepare_demo_tree(node: &mut UiNode) {
    match node.node_id.0.as_str() {
        "workspace-title" => set_text(node, "Asset Review / Riverside District"),
        "selected-asset-title" => set_text(node, "Selected asset: Granite cliff material"),
        "selected-asset-summary" => set_text(node, "Revision 18 is ready for editorial review."),
        "activity-row" => set_text(node, "Texture preview regenerated 2 minutes ago"),
        "diagnostics-row" => set_text(node, "1 capacity warning retained for review"),
        "loading-navigation" | "error-navigation" => {
            hide_tree(node);
            return;
        }
        "ready-review" => node.visible = true,
        _ => {}
    }
    for child in &mut node.children {
        prepare_demo_tree(child);
    }
}

fn hide_tree(node: &mut UiNode) {
    node.visible = false;
    for child in &mut node.children {
        hide_tree(child);
    }
}

fn set_text(node: &mut UiNode, value: &str) {
    node.text = Some(TextRef::Literal {
        value: value.into(),
    });
}
