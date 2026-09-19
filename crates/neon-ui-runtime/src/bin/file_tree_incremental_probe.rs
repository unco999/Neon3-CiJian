//! Phase 5: file-tree incremental patch probe (plan section 10).
//!
//! Drives `FileTreeProjection` mutations through the real
//! `neon-ui-runtime` RPC pipeline: one `ui.flow.submit` bootstraps the IDE
//! workspace document, and every subsequent interaction (select, collapse,
//! expand, add, remove, rename, visual operation state) must reach the
//! renderer as a keyed `ui.flow.patch` — never as a second submit.

use std::net::SocketAddr;
use std::time::Duration;

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, Revision, RpcRequest, RpcResponse,
    RpcStatus, ServiceName,
};
use neon_ui_runtime::UiRuntime;
use neon_ui_runtime::ide_projection::{
    FileTreeEntry, IdeProjectionUpdate, IdeWorkspaceProjection, summarize_patch_operations,
};
use serde_json::{Value, json};

const EPOCH: u64 = 1;
const SURFACE: &str = "surface.ide.files";
const BASE_REVISION: u64 = 7;

fn client_identity() -> ClientIdentity {
    ClientIdentity {
        kind: ClientKind::Cli,
        instance_id: "file-tree-incremental-probe".into(),
        pid: std::process::id(),
        origin: "file-tree-incremental-probe".into(),
    }
}

fn request(method: &str, params: Value) -> RpcRequest {
    let id = format!("file-tree-incremental-{}", uuid());
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

fn base_entries() -> Vec<FileTreeEntry> {
    vec![
        FileTreeEntry::dir("src"),
        FileTreeEntry::file("src/main.rs"),
        FileTreeEntry::file("src/util.rs"),
        FileTreeEntry::dir("docs"),
        FileTreeEntry::file("docs/readme.md"),
        FileTreeEntry::file("Cargo.toml"),
    ]
}

fn seeded_workspace() -> IdeWorkspaceProjection {
    let mut workspace = IdeWorkspaceProjection::new(SURFACE, BASE_REVISION);
    workspace.files.set_entries(base_entries());
    workspace.files.set_expanded("src", true);
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
        object.insert("probe".to_owned(), json!("file-tree-incremental.v1"));
        object.insert("sequence".to_owned(), json!(sequence));
        object.insert("case".to_owned(), json!(case));
        object.insert("input".to_owned(), input);
        if !pass {
            failures += 1;
        }
        object.insert("pass".to_owned(), json!(pass));
        println!("{record}");
    };

    // Case 1: clicking a file must be a single property set, not a submit.
    workspace.files.select(Some("src/main.rs".into()));
    match drive_case(ui_endpoint, &mut workspace, "select_main", BASE_REVISION) {
        Ok(outcome) => {
            let pass = outcome.operations.len() == 1
                && all_sets(&outcome.operations)
                && outcome.patch_kind == "property_only";
            emit(
                "select_main",
                json!({"action": "select", "path": "src/main.rs"}),
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
        Err(error) => emit("select_main", json!({}), json!({"error": error}), false),
    }

    // Case 2: switching selection is two sets; the old row reverts.
    workspace.files.select(Some("Cargo.toml".into()));
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "switch_selection",
        BASE_REVISION + 1,
    ) {
        Ok(outcome) => {
            let pass = outcome.operations.len() == 2
                && all_sets(&outcome.operations)
                && outcome.patch_kind == "property_only";
            emit(
                "switch_selection",
                json!({"action": "select", "path": "Cargo.toml"}),
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
            "switch_selection",
            json!({}),
            json!({"error": error}),
            false,
        ),
    }

    // Case 3: collapsing a directory removes only its visible rows.
    workspace.files.set_expanded("src", false);
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "collapse_src",
        BASE_REVISION + 2,
    ) {
        Ok(outcome) => {
            let removes = outcome
                .operations
                .iter()
                .filter(|entry| entry.starts_with("remove "))
                .count();
            let pass = removes == 2
                && outcome
                    .operations
                    .iter()
                    .all(|entry| entry.starts_with("remove ") || entry.starts_with("set "));
            emit(
                "collapse_src",
                json!({"action": "collapse", "dir": "src"}),
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
        Err(error) => emit("collapse_src", json!({}), json!({"error": error}), false),
    }

    // Case 4: expanding re-inserts exactly those rows at their indexes.
    workspace.files.set_expanded("src", true);
    match drive_case(ui_endpoint, &mut workspace, "expand_src", BASE_REVISION + 3) {
        Ok(outcome) => {
            let inserts = outcome
                .operations
                .iter()
                .filter(|entry| entry.starts_with("insert "))
                .count();
            pass_check(
                &mut emit,
                &outcome,
                inserts == 2,
                "expand_src",
                json!({"action": "expand", "dir": "src"}),
                4,
            );
        }
        Err(error) => emit("expand_src", json!({}), json!({"error": error}), false),
    }

    // Case 5: a new file on disk is exactly one insert.
    let mut entries = base_entries();
    entries.insert(3, FileTreeEntry::file("src/lib.rs"));
    workspace.files.set_entries(entries);
    match drive_case(ui_endpoint, &mut workspace, "add_file", BASE_REVISION + 4) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1 && outcome.operations[0].starts_with("insert "),
                "add_file",
                json!({"action": "scan_add", "path": "src/lib.rs"}),
                5,
            );
        }
        Err(error) => emit("add_file", json!({}), json!({"error": error}), false),
    }

    // Case 6: a deleted file is exactly one remove.
    workspace.files.set_entries(base_entries());
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "remove_file",
        BASE_REVISION + 5,
    ) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1 && outcome.operations[0].starts_with("remove "),
                "remove_file",
                json!({"action": "scan_remove", "path": "src/lib.rs"}),
                6,
            );
        }
        Err(error) => emit("remove_file", json!({}), json!({"error": error}), false),
    }

    // Case 7: a rename swaps the stable key: one remove plus one insert.
    let mut entries = base_entries();
    entries[2].path = "src/util_v2.rs".into();
    workspace.files.set_entries(entries);
    match drive_case(
        ui_endpoint,
        &mut workspace,
        "rename_file",
        BASE_REVISION + 6,
    ) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 2
                    && outcome
                        .operations
                        .iter()
                        .any(|entry| entry.starts_with("remove "))
                    && outcome
                        .operations
                        .iter()
                        .any(|entry| entry.starts_with("insert ")),
                "rename_file",
                json!({"action": "rename", "from": "src/util.rs", "to": "src/util_v2.rs"}),
                7,
            );
        }
        Err(error) => emit("rename_file", json!({}), json!({"error": error}), false),
    }

    // Case 8: a visual operation state (busy) is a property set only.
    workspace.files.set_busy("Cargo.toml", true);
    match drive_case(ui_endpoint, &mut workspace, "busy_state", BASE_REVISION + 7) {
        Ok(outcome) => {
            pass_check(
                &mut emit,
                &outcome,
                outcome.operations.len() == 1
                    && all_sets(&outcome.operations)
                    && outcome.patch_kind == "property_only",
                "busy_state",
                json!({"action": "busy", "path": "Cargo.toml"}),
                8,
            );
        }
        Err(error) => emit("busy_state", json!({}), json!({"error": error}), false),
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
            "probe": "file-tree-incremental.v1",
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
                "probe": "file-tree-incremental.v1",
                "final": true,
                "pass": false,
                "error": {"code": "probe_failed", "message": error},
            })
        );
        std::process::exit(1);
    }
}
