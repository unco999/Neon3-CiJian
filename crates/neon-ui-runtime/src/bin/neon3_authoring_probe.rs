//! Machine-readable NUI Flow authoring probe for external AI tooling.
//!
//! The probe deliberately reuses the production parser and compiler. It accepts
//! JSONL on stdin and emits one JSONL result per input line, making it useful to
//! MCP servers, CI, and editor integrations without inventing a second grammar.

use std::io::{self, BufRead, Write};

use neon_protocol::Revision;
use neon_ui_runtime::{compile_nui_flow_program, parse_nui_flow};
use neon_ui_schema::{
    UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME, UI_PROGRAM_CAPABILITY_NAME,
    UI_PROGRAM_SCHEMA_VERSION, UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
    UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME, UiProgramCapability, UiProgramCapabilityOwner,
    UiProgramCapabilityStatus, UiProgramRevision,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
struct ProbeRequest {
    #[serde(default)]
    request_id: String,
    #[serde(default = "default_operation")]
    operation: String,
    source: String,
    #[serde(default)]
    sequence: u64,
}

fn default_operation() -> String {
    "validate".into()
}

fn main() {
    let stdin = io::stdin();
    let mut output = io::BufWriter::new(io::stdout().lock());
    let mut failed = false;

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) if !line.trim().is_empty() => line,
            Ok(_) => continue,
            Err(error) => {
                failed = true;
                let _ = writeln!(
                    output,
                    "{}",
                    json!({
                        "status": "failed",
                        "error": {"code": "probe_io_error", "message": error.to_string()}
                    })
                );
                break;
            }
        };
        let result = match serde_json::from_str::<ProbeRequest>(&line) {
            Ok(request) => handle(request),
            Err(error) => {
                failed = true;
                json!({
                    "status": "failed",
                    "error": {"code": "invalid_probe_request", "message": error.to_string()}
                })
            }
        };
        if result.get("status").and_then(Value::as_str) != Some("passed") {
            failed = true;
        }
        let _ = writeln!(output, "{result}");
        let _ = output.flush();
    }

    if failed {
        std::process::exit(1);
    }
}

fn handle(request: ProbeRequest) -> Value {
    let input = json!({
        "request_id": request.request_id,
        "operation": request.operation,
        "sequence": request.sequence,
        "source_bytes": request.source.len(),
    });

    let document = match parse_nui_flow(&request.source) {
        Ok(document) => document,
        Err(error) => {
            return json!({
                "status": "failed",
                "input": input,
                "stage": "parse",
                "diagnostics": error.diagnostics,
            });
        }
    };

    let revision = UiProgramRevision {
        program_id: document.ir.surface_id.0.clone(),
        revision: Revision(document.ir.revision.0.max(1)),
        schema_version: UI_PROGRAM_SCHEMA_VERSION,
        capabilities: capabilities(),
    };
    let program = match compile_nui_flow_program(&document, revision) {
        Ok(program) => program,
        Err(error) => {
            return json!({
                "status": "failed",
                "input": input,
                "stage": "compile",
                "diagnostics": [{
                    "code": "nui_flow_compile_failed",
                    "message": format!("{error:?}")
                }],
            });
        }
    };

    let node_outline = program
        .nodes
        .iter()
        .map(|node| {
            json!({
                "key": node.key,
                "parent_key": node.parent_key,
                "kind": node.kind,
            })
        })
        .collect::<Vec<_>>();
    let result = json!({
        "surface_id": document.ir.surface_id,
        "flow_version": document.version,
        "input_schema": document.input_schema,
        "state_machines": document.state_machines,
        "motions": document.motions,
        "drags": document.drags,
        "drops": document.drops,
        "world_panels": document.world_panels,
        "ir": document.ir,
        "program": {
            "revision": program.revision,
            "node_count": program.nodes.len(),
            "binding_count": program.binding_records.len(),
            "event_count": program.event_records.len(),
            "resource_budget": program.resource_budget,
            "layout_hash": program.layout_hash,
            "nodes": node_outline,
        }
    });
    json!({
        "status": "passed",
        "input": input,
        "stage": "compiled",
        "result": result,
    })
}

fn capabilities() -> Vec<UiProgramCapability> {
    [
        UI_PROGRAM_CAPABILITY_NAME,
        UI_PROGRAM_TEXT_REGISTRY_CAPABILITY_NAME,
        UI_PROGRAM_BOUNDED_STRUCTURE_CAPABILITY_NAME,
        UI_PROGRAM_SEMANTIC_EVENT_CAPABILITY_NAME,
    ]
    .into_iter()
    .map(|name| UiProgramCapability {
        name: name.into(),
        version: 1,
        owner: UiProgramCapabilityOwner::SharedContract,
        status: UiProgramCapabilityStatus::Supported,
    })
    .collect()
}
