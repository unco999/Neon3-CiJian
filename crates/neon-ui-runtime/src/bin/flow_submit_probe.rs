use std::time::{Duration, Instant};

use neon_ipc::RpcClient;
use neon_protocol::{ClientIdentity, ClientKind, ProtocolVersion, RequestId, RpcRequest, ServiceName};
use serde_json::json;

const FLOW: &str = "version 1\nsurface surface.probe revision 1\nbudget nodes=16 bindings=4 instances=16 text=8 glyphs=64 events=4 clips=2\nflow probe\nsurface root overlay w 320 h 200 fill #102030\n  text title value \"local flow probe\"\n";

fn request() -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId("flow-submit-probe-1".into()),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "flow-submit-probe".into(),
            pid: std::process::id(),
            origin: "flow-submit-probe".into(),
        },
        target: ServiceName("ui-runtime".into()),
        method: "ui.flow.submit".into(),
        params: json!({ "source": FLOW }),
        expected_revision: None,
        idempotency_key: Some("flow-submit-probe-1".into()),
    }
}

fn main() {
    let endpoint = std::env::args()
        .nth(1)
        .expect("usage: flow_submit_probe <ui-loopback-endpoint>")
        .parse()
        .expect("endpoint must be a socket address");
    let started = Instant::now();
    let result = RpcClient::connect(endpoint)
        .and_then(|client| client.with_timeout(Duration::from_secs(5)))
        .and_then(|mut client| client.call(&request()));
    let elapsed_ms = started.elapsed().as_millis();
    let pass = matches!(&result, Ok(response) if response.status == neon_protocol::RpcStatus::Accepted);
    println!(
        "{}",
        json!({
            "probe": "flow_submit",
            "input": {"endpoint": endpoint.to_string(), "method": "ui.flow.submit", "surface_id": "surface.probe"},
            "intermediate": {"source_bytes": FLOW.len(), "elapsed_ms": elapsed_ms},
            "response": result.as_ref().ok().map(|response| json!({
                "request_id": response.request_id,
                "status": response.status,
                "revision": response.revision,
                "result": response.result,
                "error": response.error,
            })),
            "error": result.as_ref().err().map(ToString::to_string),
            "pass": pass,
        })
    );
    if !pass {
        std::process::exit(1);
    }
}
