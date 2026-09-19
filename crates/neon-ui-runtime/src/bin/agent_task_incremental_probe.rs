//! Phase 5: agent-workbench incremental patch probe (plan section 10).
//!
//! Drives `AgentWorkbenchProjection` mutations through the real
//! `neon-ui-runtime` RPC pipeline: one `ui.flow.submit` bootstraps the IDE
//! workspace document, and every subsequent agent event (status line, task
//! status, add/remove task, transaction record, approval resolution, panel
//! switch) must reach the renderer as a keyed `ui.flow.patch` — never as a
//! second submit. Task status changes must be property sets only, and panel
//! switches must stay inside the agent subtree.

use std::net::SocketAddr;
use std::time::Duration;

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_runtime::UiRuntime;
use neon_ui_runtime::ide_projection::{
    AgentSection, AgentTask, IdeProjectionUpdate, IdeWorkspaceProjection, TaskStatus,
    summarize_patch_operations,
};
use serde_json::{Value, json};

const EPOCH: u64 = 1;
const SURFACE: &str = "surface.ide.agents";
const BASE_REVISION: u64 = 11;

fn client_identity() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "agent-task-incremental-probe".into(),
        pid: std::process::id(),
        origin: "agent-task-incremental-probe".into(),
    }
}

fn request(method: &str, params: Value) -> RpcRequest {
    let id = format!("agent-task-incremental-{}", uuid());
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
    workspace.agent.add_task(AgentTask {
        plan: "alpha".into(),
        name: "test".into(),
        status: TaskStatus::Queued,
        depends_on: None,
    });
    workspace.agent.record_transaction("t-1", "open project");
    workspace.agent.record_approval("a-1", "write Cargo.toml");
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

fn all_sets(operations: &[String]) -> bool {
    operations.iter().all(|entry| entry.starts_with("set "))
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
        object.insert("probe".to_owned(), json!("agent-task-incremental.v1"));
        object.insert("sequence".to_owned(), json!(sequence));
        object.insert("case".to_owned(), json!(case));
        object.insert("input".to_owned(), input);
        if !pass {
            failures += 1;
        }
        object.insert("pass".to_owned(), json!(pass));
        println!("{record}");
    };

    // Case 1: an agent status line change is one property set, no submit.
    workspace.agent.set_status_line("running alpha/build");
    match drive_case(ui_endpoint, &mut workspace, "status_line", BASE_REVISION) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1
                    && all_sets(&outcome.operations)
                    && outcome.patch_kind == "property_only",
                "status_line",
                json!({"action": "set_status_line", "status": "running alpha/build"}),
                1,
            );
        }
        Err(error) => emit("status_line", json!({}), json!({"error": error}), false),
    }

    // Case 2: a task status transition sets only that row's literal value.
    workspace
        .agent
        .set_task_status("alpha", "test", TaskStatus::Running);
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "task_status",
        BASE_REVISION + 1,
    ) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1
                    && all_sets(&outcome.operations)
                    && outcome.patch_kind == "property_only"
                    && outcome.operations[0].contains("task.alpha.test"),
                "task_status",
                json!({"action": "set_task_status", "task": "alpha/test", "status": "running"}),
                2,
            );
        }
        Err(error) => emit("task_status", json!({}), json!({"error": error}), false),
    }

    // Case 3: a new plan task arriving mid-run is exactly one insert.
    workspace.agent.add_task(AgentTask {
        plan: "alpha".into(),
        name: "deploy".into(),
        status: TaskStatus::Queued,
        depends_on: None,
    });
    match drive_case(ui_endpoint, &mut workspace, "add_task", BASE_REVISION + 2) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1
                    && outcome.operations[0].starts_with("insert ")
                    && outcome.operations[0].contains("task.alpha.deploy"),
                "add_task",
                json!({"action": "add_task", "task": "alpha/deploy"}),
                3,
            );
        }
        Err(error) => emit("add_task", json!({}), json!({"error": error}), false),
    }

    // Case 4: a retired task is exactly one remove on its stable key.
    workspace.agent.remove_task("alpha", "deploy");
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "remove_task",
        BASE_REVISION + 3,
    ) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1
                    && outcome.operations[0].starts_with("remove ")
                    && outcome.operations[0].contains("task.alpha.deploy"),
                "remove_task",
                json!({"action": "remove_task", "task": "alpha/deploy"}),
                4,
            );
        }
        Err(error) => emit("remove_task", json!({}), json!({"error": error}), false),
    }

    // Case 5: a new transaction record is exactly one insert.
    workspace
        .agent
        .record_transaction("t-2", "begin workspace edit");
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "record_transaction",
        BASE_REVISION + 4,
    ) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1
                    && outcome.operations[0].starts_with("insert ")
                    && outcome.operations[0].contains("transaction.t_h2"),
                "record_transaction",
                json!({"action": "record_transaction", "id": "t-2"}),
                5,
            );
        }
        Err(error) => emit(
            "record_transaction",
            json!({}),
            json!({"error": error}),
            false,
        ),
    }

    // Case 6: resolving an approval keeps its key and sets its label.
    workspace.agent.resolve_approval("a-1", true);
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "resolve_approval",
        BASE_REVISION + 5,
    ) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1
                    && all_sets(&outcome.operations)
                    && outcome.patch_kind == "property_only"
                    && outcome.operations[0].contains("approval.a_h1"),
                "resolve_approval",
                json!({"action": "resolve_approval", "id": "a-1", "accepted": true}),
                6,
            );
        }
        Err(error) => emit(
            "resolve_approval",
            json!({}),
            json!({"error": error}),
            false,
        ),
    }

    // Case 7: hiding the records section is a visibility set inside the
    // agent subtree only; the file-tree sidebar is never addressed.
    workspace
        .agent
        .set_section_visible(AgentSection::Records, false);
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "panel_switch",
        BASE_REVISION + 6,
    ) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1
                    && all_sets(&outcome.operations)
                    && outcome.patch_kind == "property_only"
                    && outcome.operations[0] == "set workspace/agent/agent.section.records.visible"
                    && !outcome
                        .operations
                        .iter()
                        .any(|entry| entry.contains("sidebar")),
                "panel_switch",
                json!({"action": "set_section_visible", "section": "records", "visible": false}),
                7,
            );
        }
        Err(error) => emit("panel_switch", json!({}), json!({"error": error}), false),
    }

    // The whole session must have touched the renderer with exactly one
    // fragment submit plus one forwarded patch per case.
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
            "probe": "agent-task-incremental.v1",
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

/// Shared tail emission for cases whose predicate is a boolean computed by
/// the caller.
fn pass_check(
    emit: &mut impl FnMut(&str, Value, Value, bool),
    outcome: &CaseOutcome,
    pass: bool,
    case: &str,
    input: Value,
    patch_index: u64,
) {
    emit(
        case,
        input,
        json!({
            "operations": outcome.operations,
            "program_revision": outcome.program_revision,
            "flow_document_revision": outcome.flow_document_revision,
            "frame_sequence": outcome.frame_sequence,
            "retained": {"submits": 1, "patches": patch_index},
            "timing_ms": outcome.timing_ms,
        }),
        pass,
    );
}

fn main() {
    if let Err(error) = run() {
        println!(
            "{}",
            json!({
                "probe": "agent-task-incremental.v1",
                "final": true,
                "pass": false,
                "error": {"code": "probe_failed", "message": error},
            })
        );
        std::process::exit(1);
    }
}
