//! Real loopback RPC probe for manifest-based page-state aggregation.

use std::fs;
use std::thread;

use neon_cli::execute_snapshot_aggregate;
use neon_ipc::RpcServer;
use neon_protocol::{RpcResponse, RpcStatus};
use serde_json::json;

fn main() {
    let service_cases = [
        (
            "ui-runtime",
            "debug.snapshot.get",
            json!({"fragment_count": 2}),
        ),
        (
            "wgpu-runtime",
            "debug.snapshot.get",
            json!({"graph_revision": 7}),
        ),
    ];
    let mut entries = Vec::new();
    let mut workers = Vec::new();

    for (name, expected_method, result) in service_cases {
        let server = RpcServer::bind("127.0.0.1:0".parse().expect("loopback endpoint"))
            .expect("bind service probe");
        let endpoint = server.local_addr().expect("service endpoint");
        entries.push(format!(
            "\"{name}\":{{\"endpoint\":\"{endpoint}\",\"epoch\":1}}"
        ));
        let name = name.to_owned();
        let expected_method = expected_method.to_owned();
        workers.push(thread::spawn(move || {
            for sequence in 0..3 {
                server
                    .serve_one(|request| {
                        let pass = request.target.0 == name
                            && matches!(
                                request.method.as_str(),
                                "service.health" | "service.describe"
                            )
                            || (request.method == expected_method && request.target.0 == name);
                        println!(
                            "{}",
                            json!({
                                "callback": "service_received",
                                "service": name,
                                "sequence": sequence + 1,
                                "method": request.method,
                                "target": request.target.0,
                                "pass": pass,
                            })
                        );
                        RpcResponse {
                            request_id: request.request_id,
                            status: RpcStatus::Accepted,
                            revision: None,
                            result: Some(if request.method == expected_method {
                                result.clone()
                            } else {
                                json!({"status": "healthy"})
                            }),
                            snapshot: None,
                            error: None,
                        }
                    })
                    .expect("serve aggregate request");
            }
        }));
    }

    let path = std::env::temp_dir().join(format!(
        "neon-cli-snapshot-probe-{}.json",
        std::process::id()
    ));
    let manifest = format!("{{\"services\":{{{}}}}}", entries.join(","));
    fs::write(&path, manifest).expect("write deterministic manifest");
    let output = execute_snapshot_aggregate(path.to_str().expect("manifest path"), None)
        .expect("aggregate query must complete");
    let pass = output["services"]["ui-runtime"]["snapshot"]["result"]["fragment_count"] == 2
        && output["services"]["wgpu-runtime"]["snapshot"]["result"]["graph_revision"] == 7;
    println!(
        "{}",
        json!({
            "callback": "aggregate_completed",
            "service_count": output["services"].as_object().map_or(0, |services| services.len()),
            "input_manifest": path,
            "pass": pass && output["status"] == "passed",
        })
    );
    let _ = fs::remove_file(path);
    for worker in workers {
        worker.join().expect("probe service thread");
    }
    if !pass || output["status"] != "passed" {
        std::process::exit(1);
    }
}
