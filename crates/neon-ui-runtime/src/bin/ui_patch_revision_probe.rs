//! Phase 4 runtime patch pipeline contract probe (plan section 6).
//!
//! Drives the real `ui.flow.submit` / `ui.flow.patch` RPC path against a
//! scriptable fake renderer and asserts the R1-R6 acceptance behavior:
//!
//! - R1: dry runs report the impacted nodes only and mutate nothing.
//! - R2: `patch_kind` distinguishes property-only from structural patches.
//! - R4: stale revision, invalid patch, and compile failures reject with the
//!   previous program explicitly retained, and the next healthy patch works.
//! - R5: renderer rejection yields an explicit `patch_render_fallback`
//!   reason and the same patch is retryable at the unchanged revision.
//! - R6: every patch response carries split `timing_ms` stage statistics.
//!
//! Output is JSONL in the shared probe envelope.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, Revision, RpcError, RpcRequest,
    RpcResponse, RpcStatus, ServiceName,
};
use neon_ui_runtime::UiRuntime;
use serde_json::{Value, json};

const EPOCH: u64 = 1;
const BASE_REVISION: u64 = 3;

fn flow_source(node_count: usize) -> String {
    let mut source = String::from(
        "version 1\nsurface surface.revision revision \
         __REV__\nbudget nodes=8192 bindings=8192 instances=8192 text=8192 glyphs=131072 events=64 clips=8192\nflow revision\nsurface root column w 1200 h 900 fill #102030\n",
    );
    source = source.replace("__REV__", &BASE_REVISION.to_string());
    for row in 0..node_count {
        source.push_str(&format!("  text row-{row:04} value \"row {row}\"\n"));
    }
    source
}

fn client_identity() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "ui-patch-revision-probe".into(),
        pid: std::process::id(),
        origin: "ui-patch-revision-probe".into(),
    }
}

fn uuid() -> String {
    use std::sync::atomic::AtomicU64;
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

fn request(method: &str, params: Value) -> RpcRequest {
    let id = format!("ui-patch-revision-{}", uuid());
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

fn call(endpoint: SocketAddr, req: &RpcRequest) -> RpcResponse {
    RpcClient::connect(endpoint)
        .and_then(|client| client.with_timeout(Duration::from_secs(30)))
        .and_then(|mut client| client.call(req))
        .expect("probe RPC round trip")
}

fn set_op(path: &str, property: &str, value: Value) -> Value {
    json!({"kind": "set", "path": path, "property": property, "value": value})
}

fn result_of(response: &RpcResponse) -> Value {
    response.result.clone().unwrap_or(Value::Null)
}

fn error_code_of(response: &RpcResponse) -> String {
    response
        .error
        .as_ref()
        .map(|error| error.code.clone())
        .unwrap_or_default()
}

fn fake_renderer(reject_next: Arc<AtomicUsize>) -> Result<SocketAddr, String> {
    let server = RpcServer::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .map_err(|error| error.to_string())?;
    let endpoint = server.local_addr().map_err(|error| error.to_string())?;
    std::thread::spawn(move || {
        let mut revision = 0_u64;
        let _ = server.serve_until(move |request| {
            if reject_next.load(Ordering::SeqCst) > 0 {
                reject_next.fetch_sub(1, Ordering::SeqCst);
                let rejected = RpcResponse {
                    request_id: request.request_id.clone(),
                    status: RpcStatus::Rejected,
                    revision: None,
                    result: None,
                    snapshot: None,
                    error: Some(RpcError {
                        code: "renderer_simulated_failure".into(),
                        message: "probe scripted renderer rejection".into(),
                        current_revision: None,
                        object_id: None,
                        details: None,
                    }),
                };
                return (rejected, true);
            }
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

fn submit(ui_endpoint: SocketAddr) -> bool {
    let response = call(
        ui_endpoint,
        &request("ui.flow.submit", json!({"source": flow_source(10)})),
    );
    response.status == RpcStatus::Accepted
}

fn patch(ui_endpoint: SocketAddr, params: Value) -> RpcResponse {
    call(ui_endpoint, &request("ui.flow.patch", params))
}

/// R1 + R2: dry run reports the impacted nodes without consuming the
/// revision; the identical real patch then applies cleanly.
fn case_dry_run(ui_endpoint: SocketAddr) -> bool {
    if !submit(ui_endpoint) {
        return false;
    }
    let operations = json!([
        set_op("root/row-0000", "value", json!("dry")),
        {"kind": "insert", "path": "root", "kind_name": "text", "node_key": "task-x"},
    ]);
    let dry = patch(
        ui_endpoint,
        json!({"revision": BASE_REVISION, "dry_run": true, "operations": operations}),
    );
    let result = result_of(&dry);
    let mut pass = dry.status == RpcStatus::Accepted
        && result.get("state").and_then(Value::as_str) == Some("dry_run")
        && result.get("patch_kind").and_then(Value::as_str) == Some("structural")
        && result.get("impacted_nodes").cloned()
            == Some(json!(["root", "root/row-0000", "root/task-x"]));
    // The dry run must not have advanced or mutated anything: the same
    // operations at the same revision must still apply.
    let real = patch(
        ui_endpoint,
        json!({"revision": BASE_REVISION, "operations": operations}),
    );
    pass = pass
        && real.status == RpcStatus::Accepted
        && result_of(&real).get("state").and_then(Value::as_str) == Some("patched");
    emit(
        "dry_run_reports_impacted_nodes",
        1,
        json!({
            "dry_run": {"status": dry.status, "result": result},
            "real_patch": {"status": real.status},
            "pass": pass,
        }),
    );
    pass
}

/// R4: a stale revision is rejected with a stable code and the previous
/// program retained; the healthy patch at the current revision still works.
fn case_stale_revision(ui_endpoint: SocketAddr) -> bool {
    if !submit(ui_endpoint) {
        return false;
    }
    let stale = patch(
        ui_endpoint,
        json!({
            "revision": BASE_REVISION + 42,
            "operations": [set_op("root/row-0000", "value", json!("stale"))],
        }),
    );
    let result = result_of(&stale);
    let mut pass = stale.status == RpcStatus::Rejected
        && error_code_of(&stale) == "ui_flow_patch_stale_revision"
        && result.get("state").and_then(Value::as_str) == Some("patch_rejected")
        && result.get("fallback").and_then(Value::as_str) == Some("previous_program_retained")
        && result.get("retained_revision").and_then(Value::as_u64) == Some(BASE_REVISION);
    let healthy = patch(
        ui_endpoint,
        json!({
            "revision": BASE_REVISION,
            "operations": [set_op("root/row-0001", "value", json!("ok"))],
        }),
    );
    pass = pass && healthy.status == RpcStatus::Accepted;
    emit(
        "stale_revision_rejects_with_retained_program",
        2,
        json!({
            "stale": {"status": stale.status, "error_code": error_code_of(&stale), "result": result},
            "healthy_after": {"status": healthy.status},
            "pass": pass,
        }),
    );
    pass
}

/// R4: a patch whose result breaks the compile budget rejects with the old
/// program retained; the surface keeps accepting later healthy patches.
fn case_compile_failure(ui_endpoint: SocketAddr) -> bool {
    if !submit(ui_endpoint) {
        return false;
    }
    let huge = "x".repeat(140_000);
    let failed = patch(
        ui_endpoint,
        json!({
            "revision": BASE_REVISION,
            "operations": [set_op("root/row-0000", "value", json!(huge))],
        }),
    );
    let result = result_of(&failed);
    let mut pass = failed.status == RpcStatus::Rejected
        && error_code_of(&failed) == "nui_flow_compile"
        && result.get("state").and_then(Value::as_str) == Some("patch_rejected")
        && result.get("fallback").and_then(Value::as_str) == Some("previous_program_retained");
    let healthy = patch(
        ui_endpoint,
        json!({
            "revision": BASE_REVISION,
            "operations": [set_op("root/row-0002", "value", json!("after"))],
        }),
    );
    pass = pass && healthy.status == RpcStatus::Accepted;
    emit(
        "compile_failure_keeps_old_program",
        3,
        json!({
            "failed": {"status": failed.status, "error_code": error_code_of(&failed), "result": result},
            "healthy_after": {"status": healthy.status},
            "pass": pass,
        }),
    );
    pass
}

/// R5: when the renderer refuses the replacement fragment the patch reports
/// an explicit fallback reason and the identical patch is retryable.
fn case_render_rejection(ui_endpoint: SocketAddr, reject_next: Arc<AtomicUsize>) -> bool {
    if !submit(ui_endpoint) {
        return false;
    }
    reject_next.store(1, Ordering::SeqCst);
    let operations = json!([set_op("root/row-0003", "value", json!("retry me"))]);
    let failed = patch(
        ui_endpoint,
        json!({"revision": BASE_REVISION, "operations": operations}),
    );
    let result = result_of(&failed);
    let mut pass = failed.status == RpcStatus::Rejected
        && result.get("state").and_then(Value::as_str) == Some("patch_render_fallback")
        && result.get("fallback_reason").and_then(Value::as_str)
            == Some("renderer_simulated_failure")
        && result.get("retained_revision").and_then(Value::as_u64) == Some(BASE_REVISION);
    let retry = patch(
        ui_endpoint,
        json!({"revision": BASE_REVISION, "operations": operations}),
    );
    pass = pass
        && retry.status == RpcStatus::Accepted
        && result_of(&retry).get("state").and_then(Value::as_str) == Some("patched");
    emit(
        "render_rejection_reports_fallback_and_retries",
        4,
        json!({
            "failed": {"status": failed.status, "result": result},
            "retry": {"status": retry.status, "state": result_of(&retry).get("state")},
            "pass": pass,
        }),
    );
    pass
}

/// R2 + R6: property-only patches are classified as such, the document
/// revision strictly increases per accepted patch, and every accepted
/// response carries the split stage timings.
fn case_revision_progression(ui_endpoint: SocketAddr) -> bool {
    if !submit(ui_endpoint) {
        return false;
    }
    let mut revisions = Vec::new();
    let mut pass = true;
    for round in 0..3 {
        let response = patch(
            ui_endpoint,
            json!({
                "revision": BASE_REVISION + round,
                "operations": [set_op("root/row-0004", "value", json!(format!("v{round}")))],
            }),
        );
        let result = result_of(&response);
        let timing = result.get("timing_ms").cloned().unwrap_or(Value::Null);
        let documented = result.get("flow_document_revision").and_then(Value::as_u64);
        pass = pass
            && response.status == RpcStatus::Accepted
            && result.get("patch_kind").and_then(Value::as_str) == Some("property_only")
            && timing.get("patch_apply").is_some()
            && timing.get("compile").is_some()
            && timing.get("fragment").is_some()
            && timing.get("forward").is_some()
            && result.get("renderer").is_some();
        if let Some(revision) = documented {
            revisions.push(revision);
        } else {
            pass = false;
        }
    }
    pass = pass && revisions == [BASE_REVISION + 1, BASE_REVISION + 2, BASE_REVISION + 3];
    emit(
        "property_only_progression_with_split_timing",
        5,
        json!({
            "flow_document_revisions": revisions,
            "expected": [BASE_REVISION + 1, BASE_REVISION + 2, BASE_REVISION + 3],
            "pass": pass,
        }),
    );
    pass
}

fn emit(case: &str, sequence: u64, payload: Value) {
    let mut record = payload;
    if let Some(object) = record.as_object_mut() {
        object.insert("probe".to_owned(), json!("ui-patch-revision.v1"));
        object.insert("sequence".to_owned(), json!(sequence));
        object.insert("case".to_owned(), json!(case));
    }
    println!("{record}");
}

fn run() -> Result<(), String> {
    let reject_next = Arc::new(AtomicUsize::new(0));
    let renderer_endpoint = fake_renderer(Arc::clone(&reject_next))?;
    let any_port: SocketAddr = "127.0.0.1:0".parse().expect("static address");
    let ui_endpoint = {
        let reservation = RpcServer::bind(any_port).map_err(|error| error.to_string())?;
        reservation
            .local_addr()
            .map_err(|error| error.to_string())?
    };
    let server = std::thread::spawn(move || {
        UiRuntime::serve_forwarder(
            ui_endpoint,
            renderer_endpoint,
            "127.0.0.1:9".parse().expect("discarded domain endpoint"),
            None,
            EPOCH,
        )
        .map_err(|error| error.to_string())
    });
    let mut ready = false;
    for _ in 0..50 {
        if call(ui_endpoint, &request("service.health", json!({}))).status == RpcStatus::Accepted {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !ready {
        return Err("forwarder never became ready".into());
    }

    let mut failures = 0_u64;
    for (name, pass) in [
        ("dry_run", case_dry_run(ui_endpoint)),
        ("stale_revision", case_stale_revision(ui_endpoint)),
        ("compile_failure", case_compile_failure(ui_endpoint)),
        (
            "render_rejection",
            case_render_rejection(ui_endpoint, reject_next.clone()),
        ),
        (
            "revision_progression",
            case_revision_progression(ui_endpoint),
        ),
    ] {
        if !pass {
            failures += 1;
            eprintln!("case {name} failed");
        }
    }

    let _ = call(ui_endpoint, &request("service.shutdown", json!({})));
    server
        .join()
        .map_err(|_| "forwarder panicked".to_owned())??;

    println!(
        "{}",
        json!({
            "probe": "ui-patch-revision.v1",
            "final": true,
            "status": if failures == 0 { "passed" } else { "failed" },
            "failures": failures,
            "pass": failures == 0,
        })
    );
    if failures != 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        println!(
            "{}",
            json!({
                "probe": "ui-patch-revision.v1",
                "final": true,
                "status": "failed",
                "error": {"code": "probe_failed", "message": error},
                "pass": false,
            })
        );
        std::process::exit(1);
    }
}
