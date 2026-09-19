//! Phase 6 Stage 6b: state-bound motion feedback probe (plan section 8).
//!
//! Drives the agent workbench through the real `neon-ui-runtime` RPC pipeline
//! and asserts the motion contract: a failed task lands its status text, a
//! visible retry action row, and a one-shot error flash in ONE patch;
//! retrying is a plain state change (remove + set, zero transitions); a
//! completion carries exactly one trailing success sweep and stays
//! `property_only`. One submit for the session, everything else patched.

use std::net::SocketAddr;
use std::time::Duration;

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_runtime::UiRuntime;
use neon_ui_runtime::ide_projection::{
    AgentTask, IdeProjectionUpdate, IdeWorkspaceProjection, TaskStatus, summarize_patch_operations,
};
use serde_json::{Value, json};

const EPOCH: u64 = 1;
const SURFACE: &str = "surface.ide.motion";
const BASE_REVISION: u64 = 41;
const TASK_PATH: &str = "workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build";
const RETRY_PATH: &str =
    "workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.retry";
const PLAN_LIST_PATH: &str = "workspace/agent/agent.section.tasks/agent.plan.list";

fn client_identity() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "motion-feedback-incremental-probe".into(),
        pid: std::process::id(),
        origin: "motion-feedback-incremental-probe".into(),
    }
}

fn request(method: &str, params: Value) -> RpcRequest {
    let id = format!("motion-feedback-incremental-{}", uuid());
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

fn seeded_workspace() -> IdeWorkspaceProjection {
    let mut workspace = IdeWorkspaceProjection::new(SURFACE, BASE_REVISION);
    workspace.agent.add_task(AgentTask {
        plan: "alpha".into(),
        name: "build".into(),
        status: TaskStatus::Running,
        depends_on: None,
    });
    workspace
}

struct CaseOutcome {
    operations: Vec<String>,
    patch_kind: String,
    program_revision: u64,
    flow_document_revision: u64,
    frame_sequence: u64,
    timing_ms: Value,
}

fn drive_case(
    endpoint: SocketAddr,
    workspace: &mut IdeWorkspaceProjection,
    case: &str,
    expected_revision: u64,
) -> Result<CaseOutcome, String> {
    let IdeProjectionUpdate::Patch(patch) = workspace.sync() else {
        return Err(format!("{case}: projection sync produced no patch"));
    };
    let operations = summarize_patch_operations(&patch);
    if patch.base_revision != expected_revision {
        return Err(format!(
            "{case}: base_revision {} != expected {expected_revision}",
            patch.base_revision
        ));
    }
    let response = call(
        endpoint,
        &request(
            "ui.flow.patch",
            json!({
                "surface_id": patch.surface_id,
                "base_revision": patch.base_revision,
                "operations": serde_json::to_value(&patch.operations)
                    .expect("patch operations serialize"),
            }),
        ),
        Duration::from_secs(30),
    )?;
    if response.status != RpcStatus::Accepted {
        return Err(format!(
            "{case}: patch rejected: {:?}",
            response.error.map(|error| error.code)
        ));
    }
    let result = response
        .result
        .clone()
        .ok_or_else(|| format!("{case}: accepted patch without a result"))?;
    let flow_document_revision = result["flow_document_revision"]
        .as_u64()
        .ok_or_else(|| format!("{case}: missing flow_document_revision"))?;
    if flow_document_revision != expected_revision + 1 {
        return Err(format!(
            "{case}: flow_document_revision {flow_document_revision} != expected {}",
            expected_revision + 1
        ));
    }
    Ok(CaseOutcome {
        operations,
        patch_kind: result["patch_kind"].as_str().unwrap_or_default().to_owned(),
        program_revision: result["program_revision"].as_u64().unwrap_or(0),
        flow_document_revision,
        frame_sequence: result["renderer"]["graph_revision"].as_u64().unwrap_or(0),
        timing_ms: result["timing_ms"].clone(),
    })
}

fn has_transition(operations: &[String]) -> bool {
    operations
        .iter()
        .any(|entry| entry == &format!("transition {TASK_PATH}"))
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

    let mut workspace = seeded_workspace();
    let submit = call(
        ui_endpoint,
        &request(
            "ui.flow.submit",
            json!({"source": workspace.initial_source()}),
        ),
        Duration::from_secs(30),
    )?;
    if submit.status != RpcStatus::Accepted {
        return Err(format!(
            "initial submit failed: {:?}",
            submit.error.map(|error| error.code)
        ));
    }
    workspace
        .adopt_baseline()
        .map_err(|error| format!("baseline adoption failed: {error}"))?;

    let mut failures = 0_u64;
    let mut sequence = 0_u64;
    let mut emit = |case: &str, input: Value, record: Value, pass: bool| {
        sequence += 1;
        let mut record = record;
        let object = record.as_object_mut().expect("record object");
        object.insert("probe".to_owned(), json!("motion-feedback-incremental.v1"));
        object.insert("sequence".to_owned(), json!(sequence));
        object.insert("case".to_owned(), json!(case));
        object.insert("input".to_owned(), input);
        if !pass {
            failures += 1;
        }
        object.insert("pass".to_owned(), json!(pass));
        println!("{record}");
    };

    // Case 1: failure is text + action + one-shot flash, all in one patch.
    workspace
        .agent
        .set_task_status("alpha", "build", TaskStatus::Failed);
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "failed_feedback",
        BASE_REVISION,
    ) {
        Ok(outcome) => {
            let pass = outcome.operations.len() == 4
                && outcome.operations[0]
                    == format!("insert task.alpha.build.retry@{PLAN_LIST_PATH}[1]")
                && outcome.operations[1] == format!("set {TASK_PATH}.fill")
                && outcome.operations[2] == format!("set {TASK_PATH}.value")
                && has_transition(&outcome.operations)
                && outcome.patch_kind == "structural";
            emit(
                "failed_feedback",
                json!({"action": "set_task_status", "status": "failed"}),
                json!({
                    "operations": outcome.operations,
                    "program_revision": outcome.program_revision,
                    "flow_document_revision": outcome.flow_document_revision,
                    "frame_sequence": outcome.frame_sequence,
                    "retained": {"submits": 1, "patches": 1},
                    "timing_ms": outcome.timing_ms,
                }),
                pass,
            );
        }
        Err(error) => emit("failed_feedback", json!({}), json!({"error": error}), false),
    }

    // Case 2: retrying is a plain state change — remove + set, zero motion.
    workspace.agent.retry_task("alpha", "build");
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "retry_is_quiet_state",
        BASE_REVISION + 1,
    ) {
        Ok(outcome) => {
            let pass = outcome.operations.len() == 3
                && outcome.operations[0] == format!("remove {RETRY_PATH}")
                && outcome.operations[1] == format!("set {TASK_PATH}.fill")
                && outcome.operations[2] == format!("set {TASK_PATH}.value")
                && !has_transition(&outcome.operations);
            emit(
                "retry_is_quiet_state",
                json!({"action": "retry_task"}),
                json!({
                    "operations": outcome.operations,
                    "program_revision": outcome.program_revision,
                    "flow_document_revision": outcome.flow_document_revision,
                    "frame_sequence": outcome.frame_sequence,
                    "retained": {"submits": 1, "patches": 2},
                    "timing_ms": outcome.timing_ms,
                }),
                pass,
            );
        }
        Err(error) => emit(
            "retry_is_quiet_state",
            json!({}),
            json!({"error": error}),
            false,
        ),
    }

    // Case 3: completion is property-only: one set plus the trailing sweep.
    workspace
        .agent
        .set_task_status("alpha", "build", TaskStatus::Completed);
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "success_sweep_property_only",
        BASE_REVISION + 2,
    ) {
        Ok(outcome) => {
            let pass = outcome.operations.len() == 3
                && outcome.operations[0] == format!("set {TASK_PATH}.fill")
                && outcome.operations[1] == format!("set {TASK_PATH}.value")
                && has_transition(&outcome.operations)
                && outcome.patch_kind == "property_only";
            emit(
                "success_sweep_property_only",
                json!({"action": "set_task_status", "status": "completed"}),
                json!({
                    "operations": outcome.operations,
                    "program_revision": outcome.program_revision,
                    "flow_document_revision": outcome.flow_document_revision,
                    "frame_sequence": outcome.frame_sequence,
                    "retained": {"submits": 1, "patches": 3},
                    "timing_ms": outcome.timing_ms,
                }),
                pass,
            );
        }
        Err(error) => emit(
            "success_sweep_property_only",
            json!({}),
            json!({"error": error}),
            false,
        ),
    }

    let _ = call(
        ui_endpoint,
        &request("service.shutdown", json!({})),
        Duration::from_secs(2),
    );
    server
        .join()
        .map_err(|_| "forwarder panicked".to_owned())??;

    println!(
        "{}",
        json!({
            "probe": "motion-feedback-incremental.v1",
            "final": true,
            "pass": failures == 0,
            "failed_cases": failures,
            "retained": {"submits": 1, "patches": sequence - failures},
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
                "probe": "motion-feedback-incremental.v1",
                "final": true,
                "pass": false,
                "error": {"code": "probe_failed", "message": error},
            })
        );
        std::process::exit(1);
    }
}
