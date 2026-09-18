//! Deterministic JSONL probe for the AI observation loop: diff and wait.

use std::fs;
use std::thread;
use std::time::Duration;

use neon_cli::{RevisionTarget, execute_snapshot_diff, execute_wait_revision};
use neon_ipc::RpcServer;
use neon_protocol::{Revision, RpcResponse, RpcStatus};
use serde_json::json;

fn response(
    request: neon_protocol::RpcRequest,
    revision: u64,
    value: serde_json::Value,
) -> RpcResponse {
    RpcResponse {
        request_id: request.request_id,
        status: RpcStatus::Accepted,
        revision: Some(Revision(revision)),
        result: Some(value),
        snapshot: None,
        error: None,
    }
}

fn main() {
    let server = RpcServer::bind("127.0.0.1:0".parse().expect("loopback endpoint"))
        .expect("bind observation server");
    let endpoint = server.local_addr().expect("observation endpoint");
    let thread = thread::spawn(move || {
        for sequence in 0..3 {
            server
                .serve_one(|request| {
                    let revision = if sequence == 1 { 1 } else { 2 };
                    println!(
                        "{}",
                        json!({
                            "callback": "snapshot_producer",
                            "sequence": sequence + 1,
                            "method": request.method,
                            "revision": revision,
                            "frame_pair": format!("producer-{}-consumer-{}", sequence + 1, sequence + 1),
                        })
                    );
                    response(request, revision, json!({"revision": revision, "label": if revision == 1 { "before" } else { "after" }}))
                })
                .expect("serve observation request");
        }
    });
    let path = std::env::temp_dir().join(format!("neon-ui-observe-{}.json", std::process::id()));
    fs::write(&path, r#"{"revision":1,"label":"before"}"#).expect("write before snapshot");
    let diff = execute_snapshot_diff(endpoint, Some("wgpu-runtime"), path.to_str().unwrap())
        .expect("diff request");
    println!(
        "{}",
        json!({"callback":"diff_completed","changed_paths":diff["diff"]["changed_paths"],"pass":diff["status"] == "passed" && diff["diff"]["changed_paths"].as_array().unwrap().len() == 2})
    );
    let wait = execute_wait_revision(
        endpoint,
        Some("wgpu-runtime"),
        RevisionTarget::Absolute(2),
        Duration::from_secs(1),
    )
    .expect("wait request");
    println!(
        "{}",
        json!({"callback":"wait_completed","matched_revision":wait["matched_revision"],"timeout":wait["timeout"],"pass":wait["matched"] == true && wait["timeout"] == false})
    );
    let _ = fs::remove_file(path);
    thread.join().expect("observation server thread");
}
