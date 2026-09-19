//! B0-4 fixed-scenario producer baseline probe.
//!
//! Drives the real `ui.flow.submit` / `ui.flow.patch` RPC pipeline through
//! `UiRuntime::serve_forwarder` against a scripted fake renderer and emits one
//! JSONL record per case with the B0-1/B0-2 `timing_ms` stage breakdown. The
//! consumer-side retained counters come from the companion
//! `ui_reconcile_baseline_probe` in `neon-wgpu-runtime`.

use std::net::SocketAddr;
use std::time::Duration;

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_runtime::UiRuntime;
use serde_json::{Value, json};

const EPOCH: u64 = 1;

fn flow_source(node_count: usize, revision: u64) -> String {
    let mut source = String::from(
        "version 1\nsurface surface.baseline revision \
         __REV__\nbudget nodes=8192 bindings=8192 instances=8192 text=8192 glyphs=131072 events=64 clips=8192\nflow baseline\nsurface root column w 1200 h 900 fill #102030\n",
    );
    source = source.replace("__REV__", &revision.to_string());
    for row in 0..node_count {
        source.push_str(&format!("  text row-{row:04} value \"row {row}\"\n"));
    }
    source
}

fn client_identity() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "ui-patch-baseline-probe".into(),
        pid: std::process::id(),
        origin: "ui-patch-baseline-probe".into(),
    }
}

fn request(method: &str, params: Value) -> RpcRequest {
    let id = format!("ui-patch-baseline-{}", uuid());
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: PROTOCOL_VERSION,
        request_id: RequestId(id.clone()),
        client: client_identity(),
        target: ServiceName("ui-runtime".into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(id),
    }
}

fn uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

fn call(
    endpoint: SocketAddr,
    request: &RpcRequest,
    timeout: Duration,
) -> Result<RpcResponse, String> {
    RpcClient::connect(endpoint)
        .and_then(|client| client.with_timeout(timeout))
        .and_then(|mut client| client.call(request))
        .map_err(|error: neon_ipc::TransportError| error.to_string())
}

fn set_op(path: &str, property: &str, value: Value) -> Value {
    json!({"kind": "set", "path": path, "property": property, "value": value})
}

fn scenario_cases() -> Vec<(&'static str, usize, Vec<Vec<Value>>)> {
    vec![
        // A: 1 text SetProperty on a 100-node tree.
        (
            "A_property_set_1",
            100,
            vec![vec![set_op("root/row-0000", "value", json!("updated"))]],
        ),
        // B: 50 transaction state changes in ONE coalesced patch on a 100-node tree.
        (
            "B_transaction_batch_50",
            100,
            vec![
                (0..50)
                    .map(|i| set_op(&format!("root/row-{i:04}"), "opacity", json!("0.5")))
                    .collect(),
            ],
        ),
        // C: task-list insert, update and delete on a 100-node tree.
        (
            "C_task_list_lifecycle_100",
            100,
            vec![
                vec![
                    json!({"kind": "insert", "path": "root", "kind_name": "text", "node_key": "task-extra"}),
                ],
                vec![set_op("root/task-extra", "value", json!("task"))],
                vec![json!({"kind": "remove", "path": "root/task-extra"})],
            ],
        ),
        // D: selection-state-only update on a 500-node tree.
        (
            "D_selection_only_500",
            500,
            vec![vec![set_op("root/row-0250", "opacity", json!("1"))]],
        ),
        // E: one insert into a 100-node tree.
        (
            "E_insert_one_100",
            100,
            vec![vec![
                json!({"kind": "insert", "path": "root", "kind_name": "text", "node_key": "row-extra"}),
            ]],
        ),
        // F: one delete from a 100-node tree.
        (
            "F_remove_one_100",
            100,
            vec![vec![json!({"kind": "remove", "path": "root/row-0042"})]],
        ),
        // G: batch reorder of ten rows in one patch inside a 100-node tree.
        (
            "G_reorder_batch_100",
            100,
            vec![(0..10)
                .map(|i| json!({"kind": "move", "path": format!("row-{i:04}"), "parent": "root"}))
                .collect()],
        ),
        // H: 1000-node tree, 100 status properties changed in one patch.
        (
            "H_large_batch_1000",
            1000,
            vec![
                (0..100)
                    .map(|i| set_op(&format!("root/row-{i:04}"), "opacity", json!("0.25")))
                    .collect(),
            ],
        ),
    ]
}

fn emit(record: Value) {
    println!("{record}");
}

fn fake_renderer() -> Result<SocketAddr, String> {
    let server = RpcServer::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .map_err(|error| error.to_string())?;
    let endpoint = server.local_addr().map_err(|error| error.to_string())?;
    std::thread::spawn(move || {
        let mut revision = 0_u64;
        let _ = server.serve_until(|request| {
            revision += 1;
            let accepted = RpcResponse {
                request_id: request.request_id.clone(),
                status: RpcStatus::Accepted,
                revision: Some(Revision(revision)),
                result: Some(json!({
                    "graph_revision": revision,
                    "fragment_count": 1,
                    "mode": "headless",
                    "hit_target_generation": revision,
                })),
                snapshot: None,
                error: None,
            };
            (accepted, true)
        });
    });
    Ok(endpoint)
}

fn run() -> Result<(), String> {
    let renderer_endpoint = fake_renderer()?;
    let any_port: SocketAddr = "127.0.0.1:0".parse().expect("static address");
    let ui_endpoint = {
        let reservation = RpcServer::bind(any_port).map_err(|error| error.to_string())?;
        reservation
            .local_addr()
            .map_err(|error| error.to_string())?
    };
    let wgpu_target = renderer_endpoint;
    let server = std::thread::spawn(move || {
        UiRuntime::serve_forwarder(
            ui_endpoint,
            wgpu_target,
            "127.0.0.1:9".parse().expect("discarded domain endpoint"),
            None,
            EPOCH,
        )
        .map_err(|error| error.to_string())
    });
    // Wait for the forwarder to accept connections.
    let mut ready = false;
    for _ in 0..50 {
        match call(
            ui_endpoint,
            &request("service.health", json!({})),
            Duration::from_millis(200),
        ) {
            Ok(_) => {
                ready = true;
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    if !ready {
        return Err("forwarder never became ready".into());
    }

    let mut failures = 0_u64;
    for (case, node_count, patches) in scenario_cases() {
        const BASE_REVISION: u64 = 3;
        let submit = call(
            ui_endpoint,
            &request(
                "ui.flow.submit",
                json!({"source": flow_source(node_count, BASE_REVISION)}),
            ),
            Duration::from_secs(30),
        )?;
        let submit_timing = submit
            .result
            .as_ref()
            .and_then(|result| result.get("timing_ms"))
            .cloned();
        let submit_pass = submit.status == RpcStatus::Accepted && submit_timing.is_some();
        emit(json!({
            "probe": "ui_patch_baseline.v1",
            "case": format!("{case}/submit"),
            "input": {"node_count": node_count, "operation_count": 0},
            "producer": {"base_revision": BASE_REVISION},
            "consumer": {"status": submit.status, "revision": submit.revision, "error": submit.error},
            "timing_ms": submit_timing,
            "pass": submit_pass,
        }));
        if !submit_pass {
            failures += 1;
            continue;
        }
        for (ir_revision, (sequence, operations)) in
            (BASE_REVISION..).zip(patches.iter().enumerate())
        {
            let patch = call(
                ui_endpoint,
                &request(
                    "ui.flow.patch",
                    json!({
                        "revision": ir_revision,
                        "operations": operations,
                    }),
                ),
                Duration::from_secs(30),
            )?;
            let timing = patch
                .result
                .as_ref()
                .and_then(|result| result.get("timing_ms"))
                .cloned();
            let pass = patch.status == RpcStatus::Accepted && timing.is_some();
            emit(json!({
                "probe": "ui_patch_baseline.v1",
                "case": format!("{case}/patch_{}", sequence + 1),
                "input": {"node_count": node_count, "operation_count": operations.len()},
                "producer": {"patch_sequence": sequence + 1, "base_revision": ir_revision},
                "consumer": {"status": patch.status, "revision": patch.revision, "error": patch.error},
                "timing_ms": timing,
                "pass": pass,
            }));
            if !pass {
                failures += 1;
                break;
            }
        }
    }

    let _ = call(
        ui_endpoint,
        &request("service.shutdown", json!({})),
        Duration::from_secs(2),
    );
    server
        .join()
        .map_err(|_| "forwarder panicked".to_owned())??;

    emit(json!({
        "probe": "ui_patch_baseline.v1",
        "final": true,
        "status": if failures == 0 { "passed" } else { "failed" },
        "failures": failures,
        "pass": failures == 0,
    }));
    if failures != 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        emit(json!({
            "probe": "ui_patch_baseline.v1",
            "final": true,
            "status": "failed",
            "error": {"code": "probe_failed", "message": error},
            "pass": false,
        }));
        std::process::exit(1);
    }
}
