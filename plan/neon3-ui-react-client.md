# Neon3 React Client Architecture

## Boundary

`neon-ui-react-client` is a protocol client and declaration compiler. React
reconciliation happens in the client process, but React does not own a window,
DOM, canvas, WGPU object, project state, or terrain state.

```text
React JSX
  -> neon-ui-react-client declaration compiler
  -> ui.fragment.submit -> neon-ui-runtime
  -> validated UiFragment -> neon-wgpu-runtime
  -> final WGPU composition
```

The client must not submit directly to `neon-wgpu-runtime`. The UI runtime is
the authority for declaration cache, semantic intent binding, and recovery from
its own restart. WGPU remains the sole renderer and resolves hit testing only
inside its process.

## Identity model

1. React `key` is local reconciliation metadata and is never serialized.
2. `surfaceId` is the stable public UI surface/fragment identity.
3. `nodeKey` is a semantic, kebab-case, sibling-unique declaration key.
4. The client derives an opaque `UiNodeId` from `surfaceId` and declaration
   path using a deterministic hash. This value is fragment-local only.
5. Domain IDs are carried only in typed intent parameters or `AssetRef`.
6. Numeric element IDs, DOM IDs, render hit IDs, coordinates, callbacks, and
   React fiber handles are forbidden from the declaration and RPC payload.

This removes the Neon2 pattern of `ROOT + n`, numeric `id` props, and business
object IDs reused as render identity. Reordering siblings requires stable
`nodeKey` values; changing a node's semantic key intentionally creates a new
declaration identity.

## Interaction

Buttons declare a `UiIntent`; they do not receive `onClick`. WGPU performs
local hit testing and emits a semantic event. UI runtime checks the current
fragment revision and bound intent, then forwards the typed command to the
owning domain runtime with request ID, expected revision, and idempotency key.

## Current implementation level

The package currently compiles Panel, Label, Button, and Image JSX into the
version 1 `UiFragment` schema, validates identity rules, and has a Node
length-prefixed loopback transport. The Rust UI runtime accepts
`ui.fragment.submit` and maintains the declaration cache. The persistent UI
Runtime forwards validated declarations to WGPU using the same request ID and
an idempotency key. It publishes its cache only after WGPU accepts the exact
fragment, so renderer rejection, transport failure, duplicate submission, and
stale revisions cannot advance recovery state or replace a newer composition.

## Dirty Frames

The window renderer does not render every event-loop iteration. It requests a
redraw only for a newly accepted composition revision, resize, local input
feedback, pending hit readback, or a live transition/press animation. The
window applies only a strictly newer composition revision received from its
control-plane server; stale queued snapshots are ignored. Static UI consumes no
continuous redraws after its final presentation.
