# Neon3 AI Authoring Integration

Neon3 exposes a production parser/compiler probe for AI tools:

```text
cargo run -p neon-ui-runtime --bin neon3_authoring_probe
```

The executable accepts JSONL on stdin and emits one JSONL record per request.
It uses the same `parse_nui_flow` and `compile_nui_flow_program` functions used
by `neon-ui-runtime`; external tools must not implement a second NUI grammar.

Input:

```json
{"request_id":"validate-1","operation":"validate","sequence":1,"source":"version 1\nsurface demo\n  text title value \"Hello\""}
```

Successful output contains the input schema, canonical IR, program revision,
stable node outline, resource budget, and layout hash. Failure output contains
the parser source spans or compiler diagnostic. A failed record causes a nonzero
exit code after the input stream ends.

The MCP integration should use this probe for local authoring validation and
use the public `neon3.rpc` methods for live operations. The probe is not a
replacement for runtime submission.
