//! Deterministic loopback acceptance probe for neon-editor-runtime.

use std::net::SocketAddr;
use std::thread;
use std::time::Duration;

use neon_editor_core::{ChangeSet, EditOp, Position};
use neon_editor_runtime::{
    EDITOR_DOCUMENT_CAPABILITY, EditorChangeApply, EditorChangeKind, EditorCompletionRequest,
    EditorDocumentOpen, EditorDocumentRef, EditorRuntime, SERVICE_NAME,
};
use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, Revision, RpcRequest, RpcStatus,
    ServiceName,
};
use serde_json::{Value, json};

const SOURCE: &str = "version 1\nsurface root w 400 h 200\n  text title value \"Hello\"";

fn request(
    method: &str,
    id: &str,
    params: Value,
    revision: Option<Revision>,
    key: &str,
) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(id.into()),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "editor-protocol-probe".into(),
            pid: std::process::id(),
            origin: "editor-protocol-probe".into(),
        },
        target: ServiceName(SERVICE_NAME.into()),
        method: method.into(),
        params,
        expected_revision: revision,
        idempotency_key: Some(key.into()),
    }
}

fn call(endpoint: SocketAddr, request: RpcRequest) -> Result<neon_protocol::RpcResponse, String> {
    RpcClient::connect(endpoint)
        .and_then(|client| client.with_timeout(Duration::from_secs(2)))
        .and_then(|mut client| client.call(&request))
        .map_err(|error| error.to_string())
}

fn emit(
    sequence: u64,
    method: &str,
    response: &Result<neon_protocol::RpcResponse, String>,
    pass: bool,
    data: Value,
) {
    println!(
        "{}",
        json!({
            "probe": "editor-protocol.v1",
            "sequence": sequence,
            "request": {"method": method},
            "response": response.as_ref().ok().map(|value| json!({"status": value.status, "revision": value.revision, "result": value.result, "error": value.error})),
            "error": response.as_ref().err(),
            "data": data,
            "result": if pass { "passed" } else { "failed" },
            "pass_result": pass,
        })
    );
}

fn main() {
    let server = RpcServer::bind("127.0.0.1:0".parse().expect("loopback"))
        .expect("bind editor probe server");
    let endpoint = server.local_addr().expect("editor endpoint");
    let thread = thread::spawn(move || {
        let mut runtime = EditorRuntime::new(11);
        server
            .serve_until(|request| runtime.handle(request))
            .map_err(|error| error.to_string())
    });
    let mut failed = false;
    let health = call(
        endpoint,
        request("service.health", "health", json!({}), None, "health"),
    );
    let health_pass = matches!(&health, Ok(response) if response.status == RpcStatus::Accepted);
    emit(
        1,
        "service.health",
        &health,
        health_pass,
        json!({"epoch": 11}),
    );
    failed |= !health_pass;

    let describe = call(
        endpoint,
        request("service.describe", "describe", json!({}), None, "describe"),
    );
    let describe_pass = matches!(&describe, Ok(response) if response.status == RpcStatus::Accepted && response.result.as_ref().is_some_and(|result| result.to_string().contains(EDITOR_DOCUMENT_CAPABILITY)));
    emit(
        2,
        "service.describe",
        &describe,
        describe_pass,
        json!({"capability": EDITOR_DOCUMENT_CAPABILITY}),
    );
    failed |= !describe_pass;

    let open = call(
        endpoint,
        request(
            "editor.document.open",
            "open",
            json!(EditorDocumentOpen {
                document_id: "doc.probe".into(),
                session_id: "session.probe".into(),
                language: "nui_flow".into(),
                source: SOURCE.into(),
            }),
            None,
            "open",
        ),
    );
    let open_pass = matches!(&open, Ok(response) if response.status == RpcStatus::Accepted && response.revision == Some(Revision(1)));
    emit(
        3,
        "editor.document.open",
        &open,
        open_pass,
        json!({"producer": {"source_bytes": SOURCE.len()}, "consumer": {"revision": 1}}),
    );
    failed |= !open_pass;

    let change = EditorChangeApply {
        document_id: "doc.probe".into(),
        session_id: "session.probe".into(),
        epoch: 11,
        change_set: ChangeSet {
            base_revision: 1,
            ops: vec![EditOp::Insert {
                line: 2,
                column: 2,
                end: Position::new(2, 3),
                text: "#".into(),
            }],
        },
        kind: EditorChangeKind::Draft,
        cursor: Some(Position::new(2, 3)),
        selection: None,
    };
    let applied = call(
        endpoint,
        request(
            "editor.document.change.apply",
            "apply",
            json!(change.clone()),
            None,
            "apply",
        ),
    );
    let applied_pass = matches!(&applied, Ok(response) if response.status == RpcStatus::Accepted && response.revision == Some(Revision(2)));
    emit(
        4,
        "editor.document.change.apply",
        &applied,
        applied_pass,
        json!({"producer": {"base_revision": 1, "op_count": 1}, "consumer": {"accepted_revision": 2, "state": "draft"}, "frame_pairing": {"document_revision": 2}}),
    );
    failed |= !applied_pass;

    let duplicate = call(
        endpoint,
        request(
            "editor.document.change.apply",
            "apply-duplicate",
            json!(change),
            None,
            "apply",
        ),
    );
    let duplicate_pass = match (&applied, &duplicate) {
        (Ok(first), Ok(second)) => {
            first.status == second.status
                && first.revision == second.revision
                && first.result == second.result
                && second.request_id == RequestId("apply-duplicate".into())
        }
        _ => false,
    };
    emit(
        5,
        "editor.document.change.apply.duplicate",
        &duplicate,
        duplicate_pass,
        json!({"idempotency": "same response"}),
    );
    failed |= !duplicate_pass;

    let stale = EditorChangeApply {
        document_id: "doc.probe".into(),
        session_id: "session.probe".into(),
        epoch: 11,
        change_set: ChangeSet {
            base_revision: 1,
            ops: vec![EditOp::Insert {
                line: 0,
                column: 0,
                end: Position::new(0, 1),
                text: "!".into(),
            }],
        },
        kind: EditorChangeKind::Draft,
        cursor: None,
        selection: None,
    };
    let conflict = call(
        endpoint,
        request(
            "editor.document.change.apply",
            "conflict",
            json!(stale),
            None,
            "conflict",
        ),
    );
    let conflict_pass = matches!(&conflict, Ok(response) if response.status == RpcStatus::Rejected && response.error.as_ref().is_some_and(|error| error.code == "editor_revision_conflict"));
    emit(
        6,
        "editor.document.change.apply.stale",
        &conflict,
        conflict_pass,
        json!({"expected_code": "editor_revision_conflict", "consumer_revision": 2}),
    );
    failed |= !conflict_pass;

    let committed = call(
        endpoint,
        request(
            "editor.document.change.commit",
            "commit",
            json!(EditorDocumentRef {
                document_id: "doc.probe".into(),
                session_id: "session.probe".into(),
                epoch: 11
            }),
            Some(Revision(2)),
            "commit",
        ),
    );
    let committed_pass = matches!(&committed, Ok(response) if response.status == RpcStatus::Accepted && response.revision == Some(Revision(2)));
    emit(
        7,
        "editor.document.change.commit",
        &committed,
        committed_pass,
        json!({"editor_baseline": "committed", "external_mutation": false}),
    );
    failed |= !committed_pass;

    let snapshot = call(
        endpoint,
        request(
            "editor.document.snapshot.get",
            "snapshot",
            json!(EditorDocumentRef {
                document_id: "doc.probe".into(),
                session_id: "session.probe".into(),
                epoch: 11
            }),
            None,
            "snapshot",
        ),
    );
    let snapshot_pass = matches!(&snapshot, Ok(response) if response.status == RpcStatus::Accepted && response.result.as_ref().is_some_and(|result| result.to_string().contains("Hello")));
    emit(
        8,
        "editor.document.snapshot.get",
        &snapshot,
        snapshot_pass,
        json!({"producer": {"requested_document": "doc.probe"}, "consumer": {"revision": 2}}),
    );
    failed |= !snapshot_pass;

    let completion_request = EditorCompletionRequest {
        document_id: "doc.probe".into(),
        session_id: "session.probe".into(),
        epoch: 11,
        document_revision: Revision(2),
        position: Position::new(0, 0),
        trigger_kind: "invoked".into(),
    };
    let completion = call(
        endpoint,
        request(
            "editor.completion.request",
            "completion",
            json!(completion_request),
            None,
            "completion",
        ),
    );
    let completion_pass = matches!(&completion, Ok(response) if response.status == RpcStatus::Accepted && response.result.as_ref().is_some_and(|result| result.to_string().contains("surface")));
    emit(
        9,
        "editor.completion.request",
        &completion,
        completion_pass,
        json!({"producer": {"document_revision": 2, "position": {"line": 0, "column": 0}}, "consumer": {"candidate": "surface", "revision": 2}}),
    );
    failed |= !completion_pass;

    let stale_completion = EditorCompletionRequest {
        document_id: "doc.probe".into(),
        session_id: "session.probe".into(),
        epoch: 11,
        document_revision: Revision(1),
        position: Position::new(0, 0),
        trigger_kind: "automatic".into(),
    };
    let stale = call(
        endpoint,
        request(
            "editor.completion.request",
            "completion-stale",
            json!(stale_completion),
            None,
            "completion-stale",
        ),
    );
    let stale_pass = matches!(&stale, Ok(response) if response.status == RpcStatus::Rejected && response.error.as_ref().is_some_and(|error| error.code == "editor_completion_stale"));
    emit(
        10,
        "editor.completion.request.stale",
        &stale,
        stale_pass,
        json!({"expected_code": "editor_completion_stale", "consumer_revision": 2}),
    );
    failed |= !stale_pass;

    let shutdown = call(
        endpoint,
        request("service.shutdown", "shutdown", json!({}), None, "shutdown"),
    );
    let shutdown_pass = matches!(&shutdown, Ok(response) if response.status == RpcStatus::Accepted);
    emit(11, "service.shutdown", &shutdown, shutdown_pass, json!({}));
    failed |= !shutdown_pass;
    let server_result = thread
        .join()
        .unwrap_or_else(|_| Err("server thread panicked".into()));
    failed |= server_result.is_err();
    println!(
        "{}",
        json!({"probe": "editor-protocol.v1", "final": true, "pass_result": !failed, "server": server_result})
    );
    if failed {
        std::process::exit(1);
    }
}
