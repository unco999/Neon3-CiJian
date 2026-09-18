//! Deterministic IPC probe for the public CLI method router.
//! It emits one JSONL record per request and validates the received envelope.

use std::thread;

use neon_cli::{RpcCommand, default_target, execute_rpc};
use neon_ipc::RpcServer;
use neon_protocol::{Revision, RpcStatus};
use serde_json::json;

fn main() {
    let cases = [
        ("ui.flow.compile", None, json!({"source": "version 1"})),
        ("debug.trace.query", None, json!({"limit": 1})),
        ("event.snapshot", Some("eventd"), json!({})),
        ("terrain.preview.begin", None, json!({"terrain_id": 12})),
    ];
    let mut failed = false;

    for (sequence, (method, explicit_service, params)) in cases.into_iter().enumerate() {
        let server = RpcServer::bind("127.0.0.1:0".parse().expect("loopback endpoint"))
            .expect("bind probe server");
        let endpoint = server.local_addr().expect("probe endpoint");
        let expected = explicit_service.unwrap_or_else(|| default_target(method));
        let expected_target = expected.to_owned();
        let thread = thread::spawn(move || {
            server
                .serve_one(|request| {
                    let passed = request.target.0 == expected_target;
                    println!(
                        "{}",
                        json!({
                            "callback": "consumer_received",
                            "frame_sequence": sequence + 1,
                            "input": {"method": request.method, "params": request.params},
                            "producer_target": expected_target,
                            "consumer_target": request.target.0,
                            "pass": passed,
                        })
                    );
                    neon_protocol::RpcResponse {
                        request_id: request.request_id,
                        status: if passed {
                            RpcStatus::Accepted
                        } else {
                            RpcStatus::Rejected
                        },
                        revision: Some(Revision(1)),
                        result: Some(json!({"target_verified": passed})),
                        snapshot: None,
                        error: None,
                    }
                })
                .expect("serve probe request");
        });

        let command = RpcCommand {
            endpoint,
            method: method.to_owned(),
            service: explicit_service.map(str::to_owned),
            params,
            idempotency_key: Some(format!("probe-{sequence}")),
        };
        let output = execute_rpc(command).expect("CLI RPC must complete");
        let pass = output["response"]["status"] == "accepted" && output["target"] == expected;
        println!(
            "{}",
            json!({
                "callback": "producer_completed",
                "frame_sequence": sequence + 1,
                "method": method,
                "producer_target": output["target"],
                "consumer_status": output["response"]["status"],
                "pass": pass,
            })
        );
        if !pass {
            failed = true;
        }
        thread.join().expect("probe server thread");
    }

    println!(
        "{}",
        json!({"callback": "probe_completed", "pass": !failed})
    );
    if failed {
        std::process::exit(1);
    }
}
