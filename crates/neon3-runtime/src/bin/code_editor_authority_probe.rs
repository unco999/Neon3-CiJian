//! End-to-end authority probe for the Neon3 CodeEditor document frame.

use std::net::SocketAddr;
use std::thread::sleep;
use std::time::{Duration, Instant};

use neon_ipc::RpcClient;
use neon_protocol::{
    ClientIdentity, ClientKind, ProtocolVersion, RequestId, RpcRequest, RpcStatus, ServiceName,
};
use serde_json::{Value, json};

const FLOW: &str = include_str!("../../../../tests/fixtures/editor-authority.nui");
const DOCUMENT_ID: &str = "D:/Neon3/tests/fixtures/editor-authority.nui";

fn request(target: &str, method: &str, sequence: u64, params: Value) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: ProtocolVersion { major: 1, minor: 0 },
        request_id: RequestId(format!("code-editor-authority-{sequence}")),
        client: ClientIdentity {
            kind: ClientKind::Cli,
            instance_id: "code-editor-authority-probe".into(),
            pid: std::process::id(),
            origin: "code-editor-authority-probe".into(),
        },
        target: ServiceName(target.into()),
        method: method.into(),
        params,
        expected_revision: None,
        idempotency_key: Some(format!("code-editor-authority-{sequence}")),
    }
}

fn call(
    endpoint: SocketAddr,
    target: &str,
    method: &str,
    sequence: u64,
    params: Value,
) -> Result<Value, String> {
    let request = request(target, method, sequence, params);
    let response = RpcClient::connect(endpoint)
        .and_then(|client| client.with_timeout(Duration::from_secs(3)))
        .and_then(|mut client| client.call(&request))
        .map_err(|error| error.to_string())?;
    if response.status != RpcStatus::Accepted {
        return Err(format!(
            "{target}.{method} rejected: {}",
            response
                .error
                .map(|error| error.code)
                .unwrap_or_else(|| "unknown".into())
        ));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

fn emit(
    sequence: u64,
    method: &str,
    producer: Value,
    consumer: Value,
    pass: bool,
    error: Option<String>,
) {
    println!(
        "{}",
        json!({
            "probe": "code-editor-authority.v1",
            "sequence": sequence,
            "method": method,
            "producer": producer,
            "consumer": consumer,
            "frame_pairing": {"document_id": DOCUMENT_ID, "sequence": sequence},
            "error": error,
            "pass_result": pass,
        })
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: code_editor_authority_probe <ui> <wgpu> <editor>");
        std::process::exit(2);
    }
    let ui: SocketAddr = args[1].parse().expect("ui endpoint");
    let wgpu: SocketAddr = args[2].parse().expect("wgpu endpoint");
    let editor: SocketAddr = args[3].parse().expect("editor endpoint");
    let started = Instant::now();
    let mut failed = false;

    let health = call(editor, "editor-runtime", "service.health", 1, json!({}));
    emit(
        1,
        "editor.service.health",
        json!({}),
        health.clone().unwrap_or(Value::Null),
        health.is_ok(),
        health.clone().err(),
    );
    failed |= health.is_err();

    let submitted = call(
        ui,
        "ui-runtime",
        "ui.flow.submit",
        2,
        json!({"source": FLOW}),
    );
    emit(
        2,
        "ui.flow.submit",
        json!({"source_bytes": FLOW.len()}),
        submitted.clone().unwrap_or(Value::Null),
        submitted.is_ok(),
        submitted.clone().err(),
    );
    failed |= submitted.is_err();

    let mut observed = None;
    for attempt in 0..30_u64 {
        sleep(Duration::from_millis(100));
        let snapshot = call(
            wgpu,
            "wgpu-runtime",
            "wgpu.ui.editor.presentation.snapshot",
            10 + attempt,
            json!({}),
        );
        if let Ok(value) = snapshot {
            let presentations = value
                .get("presentations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|presentation| {
                    presentation.get("node_key").and_then(Value::as_str) == Some("source-view")
                        && presentation
                            .get("document")
                            .and_then(|document| document.get("document_id"))
                            .and_then(Value::as_str)
                            == Some(DOCUMENT_ID)
                        && presentation
                            .get("document")
                            .and_then(|document| document.get("revision"))
                            .and_then(Value::as_u64)
                            .is_some_and(|revision| revision >= 1)
                        && presentation
                            .get("document")
                            .and_then(|document| document.get("source_hash"))
                            .and_then(Value::as_str)
                            .is_some_and(|hash| !hash.is_empty())
                })
                .cloned();
            if presentations.is_some() {
                observed = Some(value);
                break;
            }
        }
    }
    let presentation_pass = observed.is_some();
    emit(
        40,
        "wgpu.ui.fragment.snapshot.document_frame",
        json!({"document_id": DOCUMENT_ID, "expected": "CodeEditorPresentation.document"}),
        observed.clone().unwrap_or(Value::Null),
        presentation_pass,
        (!presentation_pass)
            .then(|| "document presentation was not observed within 3 seconds".into()),
    );
    failed |= !presentation_pass;

    let editor_snapshot = call(
        editor,
        "editor-runtime",
        "editor.document.snapshot.get",
        41,
        json!({
            "document_id": DOCUMENT_ID,
            "session_id": "neon-ide",
            "epoch": 1,
        }),
    );
    let editor_pass = editor_snapshot.as_ref().is_ok_and(|value| {
        value
            .get("snapshot")
            .and_then(|snapshot| snapshot.get("document_id"))
            .and_then(Value::as_str)
            == Some(DOCUMENT_ID)
    });
    emit(
        41,
        "editor.document.snapshot.get",
        json!({"document_id": DOCUMENT_ID, "session_id": "neon-ide", "epoch": 1}),
        editor_snapshot.clone().unwrap_or(Value::Null),
        editor_pass,
        (!editor_pass).then(|| format!("editor snapshot unavailable: {editor_snapshot:?}")),
    );
    failed |= !editor_pass;

    println!(
        "{}",
        json!({
            "probe": "code-editor-authority.v1",
            "final": true,
            "elapsed_ms": started.elapsed().as_millis(),
            "pass_result": !failed,
        })
    );
    if failed {
        std::process::exit(1);
    }
}
