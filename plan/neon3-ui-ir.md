# Neon3 AI UI IR Decision

## Decision

Neon3 will use a versioned, declarative UI intermediate representation (UI IR).
It will not add a general-purpose UI script language.

The IR is data, not executable code. An AI may generate or revise an IR document,
but the runtime accepts only a validated document, lowers it deterministically to
`UiFragment`, and applies normal fragment revision rules.

## Why Not A Script Language

A new script language would add an interpreter, sandbox, permissions, async
behavior, state lifetime, debugging, hot reload, and compatibility obligations
before it can describe a panel. It would also give an AI a path to express loops,
network access, file access, or unbounded rendering work.

Complex UI needs structure and constraints, not arbitrary execution. Conditional
domain decisions remain in the owning runtime snapshot. The IR selects layout,
component composition, visual tokens, stable semantic intent bindings, and
presentation variants from that snapshot.

## Ownership

```text
AI / CLI
  -> UiIrDocument proposal
  -> ui-runtime validation and lowering
  -> UiFragment revision
  -> wgpu-runtime final layout, measurement, hit-test, composition
```

- `neon-ui-runtime` owns IR validation, lowering, allowed intent policy, and
  revisioned declaration state.
- `neon-wgpu-runtime` owns final text measurement, flex arrangement, input
  prediction, hit testing, GPU resources, and pixels.
- React remains an alternative declaration authoring client. It is not required
  to interpret AI IR and it does not become the IR authority.
- Terrain, resource, and project runtimes remain the owners of business state.

## UiIrDocument V1

```json
{
  "schema_version": 1,
  "surface_id": "surface.editor.terrain-workbench",
  "revision": 12,
  "root": {
    "key": "workspace",
    "component": "panel",
    "layout": {
      "mode": "row",
      "gap": 8,
      "align_items": "stretch"
    },
    "children": []
  }
}
```

V1 components are finite and closed:

```text
panel
label
button
text_input
image
render_surface
```

Each node has:

```text
key                 semantic, sibling-local stable key
component           one allowed component kind
layout              fixed bounds or flex constraints
style_token         token reference, never raw arbitrary shader code
text                literal or resolved text reference
props               component-specific declarative properties
intent              optional typed semantic action from an allowlist
children            bounded node list
```

The lowering process hashes `surface_id + declaration path` into the existing
fragment-local `UiNodeId`. AI IR keys never cross to terrain/resource/project
protocols and never become render hit IDs.

## Capability Profiles

AI must target a renderer profile returned by `service.describe`:

```text
wgpu.ui.fragment.v1
wgpu.ui.layout.flex.v2
wgpu.ui.text.measure.v2
wgpu.ui.text_input.v2
wgpu.ui.render_surface.v1
```

The lowering rejects any component, layout property, text feature, token, or
intent unavailable in the accepted profile. AI must not assume future features.

## Validation And Budgets

Before lowering, ui-runtime validates:

- schema version and unique semantic keys;
- maximum depth, node count, text length, image count, and render surface count;
- finite layout values and valid flex factors;
- token existence in the selected theme revision;
- allowed component/property combinations;
- allowed intent actions and JSON parameter schema;
- no file paths, URLs, JavaScript, shader source, GPU handles, render IDs,
  pointer coordinates, or React callbacks;
- no domain IDs in render identity fields.

Suggested V1 limits:

```text
maximum nodes: 512
maximum depth: 32
maximum text scalars per node: 4,096
maximum images per surface: 128
maximum render surfaces per fragment: 16
```

Validation returns stable errors such as:

```text
ui_ir_unsupported_schema
ui_ir_unknown_component
ui_ir_duplicate_key
ui_ir_budget_exceeded
ui_ir_unknown_token
ui_ir_unsupported_capability
ui_ir_intent_rejected
ui_ir_invalid_layout
```

## Lowering

Lowering is deterministic and pure:

```text
UiIrDocument + capability profile + theme revision
  -> UiFragment
  -> schema validation
  -> revisioned submit to wgpu-runtime
```

It does not perform I/O, query the project, execute code, create GPU resources,
or infer domain mode. External text, tool states, assets, and selection values
must arrive in a typed snapshot before IR generation.

## AI Workflow

```text
1. Query service.describe and the current domain/UI snapshot.
2. Produce a UiIrDocument proposal.
3. Validate locally through ui-runtime dry-run.
4. Inspect layout diagnostics and lowering errors.
5. Submit one revisioned IR update.
6. Read accepted/rejected receipt and final render diagnostics.
```

The AI can request a machine-readable layout diagnostic containing node path,
measured text size, allocated flex bounds, overflow state, and clipped state.
It may adjust constraints in a later revision; it must not inspect pixels as its
only acceptance signal.

## Construction Order

1. Add `UiIrDocument`, node, component, token reference, and capability profile
   types in `neon-ui-schema` with serde and budget validation tests.
2. Add ui-runtime `ui.ir.validate` and `ui.ir.submit` methods with dry-run
   lowering diagnostics and revision/idempotency checks.
3. Implement deterministic lowering into existing `UiFragment` nodes and reuse
   the React compiler's stable node-id hashing contract in a Rust-owned helper.
4. Add a checked-in terrain-workbench IR fixture and headless scenario.
5. Add a layout diagnostic RPC from WGPU Runtime for measured/flex bounds.
6. Add an AI/CLI client adapter that emits JSON only; it never writes source
   files or invokes scripts.

## Explicit Non-Goals

- No JavaScript, Lua, DSL evaluation, macros, loops, or arbitrary expressions.
- No AI-owned mutable local UI state.
- No dynamic shader generation from UI IR.
- No direct project write, resource load, terrain command, or GPU call.
- No replacement for React authoring where a developer chooses React.
