//! Phase 5: plan-dependency gating probe (plan section 10).
//!
//! Task rows are visibility-gated by their `depends_on` edge: a queued task
//! with an unsatisfied dependency must not exist in the UI tree at all, so
//! domain changes to it produce zero cross-process traffic. Completing a
//! task must then release exactly one dependent row as a single insert plus
//! one property set on the completed row — never a full Flow submit.

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
const SURFACE: &str = "surface.ide.plan";
const BASE_REVISION: u64 = 21;

fn client_identity() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "plan-dependency-incremental-probe".into(),
        pid: std::process::id(),
        origin: "plan-dependency-incremental-probe".into(),
    }
}

fn request(method: &str, params: Value) -> RpcRequest {
    let id = format!("plan-dependency-incremental-{}", uuid());
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
        name: "deploy".into(),
        status: TaskStatus::Queued,
        depends_on: Some("build".into()),
    });
    workspace.agent.add_task(AgentTask {
        plan: "alpha".into(),
        name: "package".into(),
        status: TaskStatus::Queued,
        depends_on: Some("deploy".into()),
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
        object.insert("probe".to_owned(), json!("plan-dependency-incremental.v1"));
        object.insert("sequence".to_owned(), json!(sequence));
        object.insert("case".to_owned(), json!(case));
        object.insert("input".to_owned(), input);
        if !pass {
            failures += 1;
        }
        object.insert("pass".to_owned(), json!(pass));
        println!("{record}");
    };

    // Case 1: the gated chain must be collapsed to the build row only, so
    // mutating a hidden dependent task is a pure no-change domain event.
    workspace
        .agent
        .set_task_status("alpha", "package", TaskStatus::Running);
    let quiet = matches!(workspace.sync(), IdeProjectionUpdate::NoChange);
    emit(
        "gated_change_is_quiet",
        json!({"action": "set_task_status", "task": "alpha/package", "status": "running"}),
        json!({
            "operations": [],
            "retained": {"submits": 1, "patches": 0},
        }),
        quiet,
    );

    // Case 2: completing the build releases its direct dependent with one
    // insert, one property set, and the one-shot success sweep transition on
    // the completed row.
    workspace.agent.complete_task("alpha", "build");
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "complete_releases_direct",
        BASE_REVISION,
    ) {
        Ok(outcome) => {
            let pass = outcome.operations.len() == 4
                && outcome.operations[0].starts_with("insert task.alpha.deploy")
                && outcome.operations[1].starts_with(
                    "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.fill",
                )
                && outcome.operations[2].starts_with(
                    "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build.value",
                )
                && outcome.operations[3]
                    == "transition workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.build"
                && outcome.patch_kind == "structural";
            emit(
                "complete_releases_direct",
                json!({"action": "complete_task", "task": "alpha/build"}),
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
        Err(error) => emit(
            "complete_releases_direct",
            json!({}),
            json!({"error": error}),
            false,
        ),
    }

    // Case 3: completing the released task cascades to the next dependent,
    // still as one insert plus one set plus the trailing sweep.
    workspace.agent.complete_task("alpha", "deploy");
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "complete_releases_chain",
        BASE_REVISION + 1,
    ) {
        Ok(outcome) => {
            let pass = outcome.operations.len() == 4
                && outcome.operations[0].starts_with("insert task.alpha.package")
                && outcome.operations[1].starts_with(
                    "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.deploy.fill",
                )
                && outcome.operations[2].starts_with(
                    "set workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.deploy.value",
                )
                && outcome.operations[3]
                    == "transition workspace/agent/agent.section.tasks/agent.plan.list/task.alpha.deploy"
                && outcome.patch_kind == "structural";
            emit(
                "complete_releases_chain",
                json!({"action": "complete_task", "task": "alpha/deploy"}),
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
            "complete_releases_chain",
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
            "probe": "plan-dependency-incremental.v1",
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
                "probe": "plan-dependency-incremental.v1",
                "final": true,
                "pass": false,
                "error": {"code": "probe_failed", "message": error},
            })
        );
        std::process::exit(1);
    }
}
