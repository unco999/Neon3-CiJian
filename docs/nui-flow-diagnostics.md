# NUI Flow Diagnostics Contract

NUI Flow source has a public compile gate before SDK integration:

```text
SDK -> neon3.rpc -> ui-runtime.ui.flow.compile
```

The method parses and compiles the source without activating it or contacting
`neon-wgpu-runtime`. A valid request returns `accepted` with a
`NuiFlowCompileReport` in `result`:

```json
{
  "schema_version": 1,
  "status": "valid",
  "source_id": "nui-flow",
  "surface_id": "surface.probe",
  "program_revision": 1,
  "node_count": 2,
  "binding_count": 0,
  "event_count": 0,
  "layout_hash": "fnv1a64-...",
  "diagnostics": []
}
```

An invalid source returns `rejected`. The stable top-level `error.code` is
`nui_flow_parse` or `nui_flow_compile`; the complete typed report is in
`error.details`:

```json
{
  "code": "nui_flow_parse",
  "message": "literal text must be a single quoted token",
  "current_revision": 0,
  "object_id": null,
  "details": {
    "schema_version": 1,
    "status": "invalid",
    "source_id": "nui-flow",
    "diagnostics": [
      {
        "stage": "parse",
        "code": "nui_flow_unquoted_text",
        "severity": "error",
        "message": "literal text must be a single quoted token",
        "span": {
          "line": 4,
          "column": 1,
          "end_line": 4,
          "end_column": 1
        }
      }
    ]
  }
}
```

`ui.flow.submit` uses the same compile gate and returns the same `details`
shape on parse, compile, or activation failure. Renderer submission failures
remain ordinary downstream RPC errors and are not misreported as parser errors.

The Rust API is also public through `neon-ui-runtime` and `neon-ui`:

```rust
compile_nui_flow_source(source, program_revision)
    -> Result<NuiFlowCompiledProgram, NuiFlowCompileError>
```

`NuiFlowCompileError.report` is the same structured report used by the RPC
adapter. SDKs must branch on stable `code`/`stage`, not on human-readable log
text. The executable boundary probe is:

```text
cargo run -p neon-ui-runtime --bin nui_flow_diagnostics_probe
```
