//! JSONL probe for command, trace, and semantic input CLI wrappers.

use std::thread;

use neon_cli::{DebugCommand, execute_debug};
use neon_ipc::RpcServer;
use neon_protocol::{Revision, RpcResponse, RpcStatus};
use serde_json::json;

fn main() {
    let server = RpcServer::bind("127.0.0.1:0".parse().expect("loopback endpoint"))
        .expect("bind wrapper probe");
    let endpoint = server.local_addr().expect("wrapper endpoint");
    let thread = thread::spawn(move || {
        for sequence in 0..3 {
            server
                .serve_one(|request| {
                    let expected = match sequence {
                        0 => "debug.command.get",
                        1 => "debug.trace.query",
                        _ => "debug.window.input.activate_target",
                    };
                    let pass = request.method == expected;
                    println!(
                        "{}",
                        json!({"callback":"wrapper_consumer","sequence":sequence + 1,"method":request.method,"params":request.params,"target":request.target,"pass":pass})
                    );
                    RpcResponse {
                        request_id: request.request_id,
                        status: if pass { RpcStatus::Accepted } else { RpcStatus::Rejected },
                        revision: Some(Revision(1)),
                        result: Some(json!({"accepted": pass, "hit_target": "root/save"})),
                        snapshot: None,
                        error: None,
                    }
                })
                .expect("serve wrapper request");
        }
    });
    let commands = [
        DebugCommand::CommandGet {
            endpoint,
            request_id: "request-1".into(),
        },
        DebugCommand::TraceQuery {
            endpoint,
            query: json!({"request_id":"request-1"}),
        },
        DebugCommand::InputActivateTarget {
            endpoint,
            semantic_node_path: "root/save".into(),
        },
    ];
    let mut failed = false;
    for (sequence, command) in commands.into_iter().enumerate() {
        let output = execute_debug(command).expect("wrapper call");
        let pass = output["response"]["status"] == "accepted";
        println!(
            "{}",
            json!({"callback":"wrapper_producer","sequence":sequence + 1,"request_id":output["response"]["request_id"],"pass":pass})
        );
        failed |= !pass;
    }
    thread.join().expect("wrapper server thread");
    println!("{}", json!({"callback":"wrapper_completed","pass":!failed}));
    if failed {
        std::process::exit(1);
    }
}
