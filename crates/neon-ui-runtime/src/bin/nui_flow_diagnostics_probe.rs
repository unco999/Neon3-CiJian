//! Loopback probe for the public NUI Flow compile/submit diagnostics contract.
//!
//! This exercises the same `neon3.rpc` boundary used by an SDK. It deliberately
//! checks structured error details instead of parsing log text.

use std::net::SocketAddr;
use std::time::Duration;

use neon_ipc::{RpcClient, RpcServer};
use neon_protocol::{
    ClientIdentity, ClientKind, PROTOCOL_VERSION, RequestId, RpcRequest, RpcResponse, RpcStatus,
    ServiceName,
};
use neon_ui_runtime::UiRuntime;
use serde_json::{Value, json};

const INVALID_FLOW: &str =
    "version 1\nsurface surface.probe revision 1\nsurface root\n  text title value invalid text\n";
const VALID_FLOW: &str =
    "version 1\nsurface surface.probe revision 1\nsurface root\n  text title value \"ok\"\n";

fn request(request_id: &str, method: &str, source: &str) -> RpcRequest {
    RpcRequest {
        protocol: "neon3.rpc".into(),
        version: PROTOCOL_VERSION,
        request_id: RequestId(request_id.into()),
        client: ClientIdentity {
            kind: ClientKind::ExternalHost,
            instance_id: "nui-flow-diagnostics-probe".into(),
            pid: std::process::id(),
            origin: "nui-flow-diagnostics-probe".into(),
        },
        target: ServiceName("ui-runtime".into()),
        method: method.into(),
        params: json!({"source": source}),
        expected_revision: None,
        idempotency_key: Some(request_id.into()),
    }
}

fn call(endpoint: SocketAddr, request: &RpcRequest) -> Result<RpcResponse, String> {
    RpcClient::connect(endpoint)
        .map_err(|error| error.to_string())?
        .with_timeout(Duration::from_secs(2))
        .map_err(|error| error.to_string())?
        .call(request)
        .map_err(|error| error.to_string())
}

fn diagnostic_response_check(response: &RpcResponse) -> Result<Value, String> {
    if response.status != RpcStatus::Rejected {
        return Err(format!(
            "expected rejected response, got {:?}",
            response.status
        ));
    }
    let error = response
        .error
        .as_ref()
        .ok_or_else(|| "rejected response has no error".to_owned())?;
    if error.code != "nui_flow_parse" {
        return Err(format!("unexpected error code: {}", error.code));
    }
    let details = error
        .details
        .as_ref()
        .ok_or_else(|| "NUI Flow rejection has no structured details".to_owned())?;
    let diagnostics = details
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or_else(|| "diagnostic details has no diagnostics array".to_owned())?;
    let first = diagnostics
        .first()
        .ok_or_else(|| "diagnostics array is empty".to_owned())?;
    if details["schema_version"] != 1
        || details["status"] != "invalid"
        || first["stage"] != "parse"
        || first["code"] != "nui_flow_unquoted_text"
        || first["span"]["line"] != 4
    {
        return Err(format!("unexpected diagnostic details: {details}"));
    }
    Ok(json!({
        "request_id": response.request_id,
        "status": response.status,
        "error_code": error.code,
        "diagnostics": diagnostics,
        "revision": response.revision,
    }))
}

fn valid_response_check(response: &RpcResponse) -> Result<Value, String> {
    if response.status != RpcStatus::Accepted {
        return Err(format!(
            "expected accepted response, got {:?}",
            response.status
        ));
    }
    let result = response
        .result
        .as_ref()
        .ok_or_else(|| "valid compile response has no result".to_owned())?;
    if result["schema_version"] != 1
        || result["status"] != "valid"
        || result["surface_id"] != "surface.probe"
        || result["diagnostics"] != json!([])
    {
        return Err(format!("unexpected valid compile result: {result}"));
    }
    Ok(json!({
        "request_id": response.request_id,
        "status": response.status,
        "compile": result,
        "revision": response.revision,
    }))
}

fn run() -> Result<Vec<Value>, String> {
    let reservation = RpcServer::bind(
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let endpoint = reservation
        .local_addr()
        .map_err(|error| error.to_string())?;
    drop(reservation);

    let server = std::thread::spawn(move || {
        UiRuntime::serve_forwarder(
            endpoint,
            "127.0.0.1:9".parse().expect("discarded WGPU endpoint"),
            "127.0.0.1:9".parse().expect("discarded domain endpoint"),
            None,
            7,
        )
        .map_err(|error| error.to_string())
    });

    let compile_invalid = call(
        endpoint,
        &request("nui-flow-compile-invalid", "ui.flow.compile", INVALID_FLOW),
    )?;
    let submit_invalid = call(
        endpoint,
        &request("nui-flow-submit-invalid", "ui.flow.submit", INVALID_FLOW),
    )?;
    let compile_valid = call(
        endpoint,
        &request("nui-flow-compile-valid", "ui.flow.compile", VALID_FLOW),
    )?;
    let shutdown = call(
        endpoint,
        &RpcRequest {
            protocol: "neon3.rpc".into(),
            version: PROTOCOL_VERSION,
            request_id: RequestId("nui-flow-diagnostics-shutdown".into()),
            client: ClientIdentity {
                kind: ClientKind::ExternalHost,
                instance_id: "nui-flow-diagnostics-probe".into(),
                pid: std::process::id(),
                origin: "nui-flow-diagnostics-probe".into(),
            },
            target: ServiceName("ui-runtime".into()),
            method: "service.shutdown".into(),
            params: json!({}),
            expected_revision: None,
            idempotency_key: None,
        },
    )?;
    if shutdown.status != RpcStatus::Accepted {
        return Err(format!("shutdown failed: {shutdown:?}"));
    }
    server
        .join()
        .map_err(|_| "forwarder thread panicked".to_owned())??;

    Ok(vec![
        json!({
            "probe": "nui_flow_diagnostics",
            "sequence": 1,
            "method": "ui.flow.compile",
            "input": {"source_bytes": INVALID_FLOW.len()},
            "producer": {"request_id": "nui-flow-compile-invalid"},
            "consumer": diagnostic_response_check(&compile_invalid)?,
            "pass_result": true,
        }),
        json!({
            "probe": "nui_flow_diagnostics",
            "sequence": 2,
            "method": "ui.flow.submit",
            "input": {"source_bytes": INVALID_FLOW.len()},
            "producer": {"request_id": "nui-flow-submit-invalid"},
            "consumer": diagnostic_response_check(&submit_invalid)?,
            "pass_result": true,
        }),
        json!({
            "probe": "nui_flow_diagnostics",
            "sequence": 3,
            "method": "ui.flow.compile",
            "input": {"source_bytes": VALID_FLOW.len()},
            "producer": {"request_id": "nui-flow-compile-valid"},
            "consumer": valid_response_check(&compile_valid)?,
            "pass_result": true,
        }),
    ])
}

fn main() {
    match run() {
        Ok(records) => {
            for record in records {
                println!("{record}");
            }
        }
        Err(error) => {
            println!(
                "{}",
                json!({
                    "probe": "nui_flow_diagnostics",
                    "status": "failed",
                    "error": {"code": "probe_failed", "message": error},
                    "pass_result": false,
                })
            );
            std::process::exit(1);
        }
    }
}
