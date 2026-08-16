# AI Debug Workflow

AI, scripts, and CI diagnose Neon3 through public, machine-readable RPC
contracts. They do not inspect pixels, use pointer coordinates, inject clicks,
or depend on renderer hit IDs.

## Start A Session

```text
neon-dev case component-gallery
```

Startup prints a JSON session manifest containing the public WGPU, UI runtime,
and host endpoints. Keep the manifest with the test artifact or issue report.

## Read Current State

```text
neon-cli debug snapshot <wgpu-endpoint>
neon-cli debug snapshot <ui-endpoint>
```

Snapshots contain revisioned state and service diagnostics. They do not expose
render IDs, coordinates, GPU handles, or internal pointer state as business
identity.

## Trace An Interaction

Real OS interactions receive a stable `interaction_id`. WGPU records the
renderer lifecycle; UI runtime records host validation and publication stages.

```text
neon-cli debug interaction get <wgpu-endpoint> <interaction-id>
neon-cli debug interaction get <ui-endpoint> <interaction-id>
neon-cli debug interaction query <wgpu-endpoint> '{"limit":32}'
neon-dev debug-interaction <wgpu-endpoint> <interaction-id>
```

Trace stages include preparation, capture resolution, semantic forwarding,
host response, publication application, and composition revision application.
Records carry semantic node identity, declared intent, request IDs, fragment
and composition revisions, terminal outcome, and stable error code. They never
contain pointer coordinates, hit IDs, renderer-local paths, or GPU handles.

## Diagnose Failures

Use this order:

1. Query the WGPU interaction record and identify its terminal stage.
2. Query the same ID on UI runtime for adapter and host publication stages.
3. Query `debug.command.get` using linked request IDs.
4. Query `debug.trace.query` or `debug.journal.query` for service context.
5. Compare fragment, scalar-input, grid-input, and composition revisions.

An accepted semantic delivery without a later composition stage indicates a
host publication or renderer submission failure. A rejected record exposes its
stable error code and no downstream mutation is expected.

## Testing Boundary

`debug.window.input.probe` and `debug.window.input.activate` are test-only
window diagnostics. They are not AI workflow commands and must not be used by
an AI to operate a user interface. Automated acceptance uses declared scenarios
and interaction traces, such as:

```text
neon-dev scenario component-gallery-window-input
```

The scenario proves a prepared semantic target, host delivery, and composition
revision transition without making screen pixels the source of truth.
