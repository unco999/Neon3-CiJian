use std::time::{Duration, Instant};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, ServiceName,
};
use neon_ui_schema::{
    UI_FRAGMENT_SCHEMA_VERSION, UiBounds, UiCommand, UiFragment, UiFragmentId,
    UiFragmentSubmission, UiNode, UiNodeId, UiNodeKind, UiStyle,
};
use serde_json::json;

fn request(sequence: u64) -> RpcRequest {
    let fragment = UiFragment {
        fragment_id: UiFragmentId("probe.fragment".into()),
        revision: Revision(sequence),
        root: UiNode {
            node_id: UiNodeId("probe.root".into()),
            kind: UiNodeKind::Panel,
            bounds: UiBounds {
                x: 0.0,
                y: 0.0,
                width: 32.0,
                height: 32.0,
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
            children: Vec::new(),
        },
        effects: Vec::new(),
    };
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("latency-probe-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "interaction-latency-probe".into(),
            pid: std::process::id(),
            origin: "interaction-latency-probe".into(),
        },
        target: ServiceName("wgpu-runtime".into()),
        method: "wgpu.ui.submit_fragment".into(),
        params: json!(UiCommand::SubmitFragment {
            submission: UiFragmentSubmission {
                schema_version: UI_FRAGMENT_SCHEMA_VERSION,
                fragment,
            },
        }),
        expected_revision: None,
        idempotency_key: Some(format!("latency-probe-{sequence}")),
    }
}

fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .expect("usage: interaction_latency_probe <loopback-endpoint>")
        .parse()
        .expect("endpoint must be a socket address");
    let mut failures = 0_u32;
    for sequence in 1..=3 {
        let started = Instant::now();
        let result = RpcClient::connect(endpoint)
            .and_then(|client| client.with_timeout(Duration::from_millis(500)))
            .and_then(|mut client| client.call(&request(sequence)));
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let passed = matches!(&result, Ok(response) if response.status == neon_protocol::RpcStatus::Accepted)
            && elapsed_ms < 250.0;
        if !passed {
            failures += 1;
        }
        println!(
            "{}",
            json!({
                "probe": "interaction_latency",
                "sequence": sequence,
                "input": {"method": "wgpu.ui.submit_fragment", "fragment_revision": sequence},
                "intermediate": {"elapsed_ms": elapsed_ms},
                "response": result.as_ref().ok().map(|response| json!({
                    "request_id": response.request_id,
                    "status": response.status,
                    "revision": response.revision,
                })),
                "pass": passed,
            })
        );
    }
    if failures != 0 {
        std::process::exit(1);
    }
}
