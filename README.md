# Neon3

> A multi-process, declarative **UI runtime** for Rust — UI declaration and GPU rendering live in
> separate processes, connected by a versioned, AI-friendly RPC protocol.

![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)
![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)
![Status](https://img.shields.io/badge/status-early--development-red.svg)

Neon3 is a from-scratch Rust workspace for building tool/editor-style user interfaces as a set of
independent, restartable processes. A single `neon-wgpu-runtime` process owns the **only** window and
the **only** GPU device; every other process is windowless and describes UI declaratively over a public
control-plane protocol.

## ⚠️ Status

Neon3 is **early-stage**. The multi-process skeleton, the declarative UI schema, and the public RPC
protocol are stabilizing, but APIs change between commits and documentation is incomplete. Use it if you
want to explore or contribute to the architecture — not yet for production.

## Why Neon3?

Most UI stacks couple layout, state, and rendering inside one process. Neon3 deliberately separates them:

- **UI is data, not pixels.** `neon-ui-runtime` emits `UiFragment` trees (`UiNode` graphs with
  `UiNodeKind`, layout, style, transitions, and NUI-Flow state machines). It never creates a window or a
  GPU resource.
- **One GPU owner.** `neon-wgpu-runtime` is the sole owner of the `winit` window, the `wgpu` device/queue,
  and the final composition. It receives fragments and draws the final image.
- **AI and CLI are first-class clients.** Every behavior is reachable through the same typed, revisioned
  RPC the UI itself uses — no screen scraping, no simulated clicks, no private memory access.

## Design Goals

- **Process isolation:** UI, domain (terrain/resource/project), and rendering are separate processes with
  clear authority boundaries.
- **Declarative UI:** a renderer-independent schema (`neon-ui-schema`) describes panels, controls, layout,
  style, and presentation state machines.
- **Public, versioned protocol:** length-prefixed JSON RPC over loopback TCP (`neon-protocol` + `neon-ipc`).
  Transport-independent; named pipes / Unix domain sockets can be added later.
- **Observability by default:** structured trace, command journal, and debug snapshots for every service.
- **Deterministic acceptance:** features are verified through declarative scenarios and machine-readable
  results, not manual clicking.

## Architecture

Today the workspace centers on the UI / runtime / protocol core. The domain runtimes and `sessiond`
described below are part of the target architecture (see `AGENTS.md`).

```text
neon-ui-runtime       windowless; declarative UI + NUI Flow compiler + input state
neon-projectd         windowless; project & asset authority (sole writer)
neon-eventd           event journal & subscription
neon-wgpu-runtime     THE window + THE wgpu device + final composition
neon-cli / AI         public protocol clients (same contract as the UI)
                      ── planned / target ──
neon-terrain-runtime  windowless; terrain domain state machine
neon-resource-runtime windowless; asset selection / import domain
neon-sessiond         optional service discovery, supervision, capability registry
```

All UI is submitted to `neon-wgpu-runtime` as a `UiFragment`. All domain mutations go through typed,
revisioned commands. The renderer never decides business rules; business runtimes never create GPU
resources.

## Features

- **Declarative UI schema** with a rich control set: panel, label, button, image, text input, checkbox,
  radio, slider, drag-value, combo, dropdown, tabs, tooltip, modal/dialog, selectable, list box,
  scrollbar, progress bar, and a virtualized `DataGrid`.
- **Flex-style layout** — row / column / absolute / overlay, with padding, margin, gap, alignment,
  clipping, and scrolling.
- **Presentation state machines** via **NUI Flow**, a declarative authoring notation compiled to a
  versioned JSON IR (`UiIrDocument`). State transitions drive renderer-owned motion/tweening.
- **Camera-anchored world panels:** project a screen-space panel onto a world-space anchor with
  depth-aware occlusion and distance scaling.
- **Bounded `DataGrid` inputs** supplied as whole-window frames rather than per-cell runtime topology.
- **Semantic interaction:** pointer / value / selection / focus events carry *intent*, never renderer
  hit-IDs or coordinates.
- **Optional high-frequency input sharing** via negotiated SPSC ring buffers (not hardcoded globals).

## Getting Started

## Runtime Release

The current Windows x86_64 runtime bundle is published as
[Neon3 v0.2.0](https://github.com/unco999/Neon3-CiJian/releases/tag/v0.2.0).
It contains the three cross-process services required by SDK clients:
`neon-eventd`, `neon-ui-runtime`, and `neon-wgpu-runtime`, plus runtime assets.

The [Neon3 SDK](https://github.com/unco999/Neon3Sdk) language clients download
and cache this bundle automatically when a local `NEON_ROOT` is not supplied.
The bundle is separate from the SDK packages so Python/npm installation stays
small while the WGPU/window ownership boundary remains explicit.

Requirements:

- Rust **1.85+** (edition 2024), pinned via `rust-version` in `Cargo.toml`.
- Primary target: **Windows** (wgpu / D3D12). The protocol and architecture are transport/platform
  agnostic; other GPU backends are part of the design, not the current focus.

Build:

```bash
cargo build
```

The legacy React case launcher requires `packages/neon-ui-react-client`. That
package is not included in this checkout, so the following commands are not a
standalone smoke test until the client package is restored:

```text
scripts\start-ui-case.cmd workbench
scripts\start-ui-case.cmd terrain
scripts\start-ui-case.cmd terrain-generation --projectd
scripts\start-ui-case.cmd ui-platform
```

Available legacy cases: `terrain`, `terrain-generation`, `workbench`, `workbench-interactive`, `animation`,
`nested-animation`, `ui-platform`.

Or launch the headless service trio directly:

```bash
cargo run -p neon-wgpu-runtime -- --headless-server 127.0.0.1:39103
cargo run -p neon-ui-runtime    -- --forward-server 127.0.0.1:39102 127.0.0.1:39103 127.0.0.1:39104 --eventd 127.0.0.1:39101
cargo run -p neon-eventd        -- --server 127.0.0.1:39101
```

See `scripts/run-neon-services.ps1` for a ready-made headless launcher.

### The `component-gallery` case

This case exercises the full declarative UI control set (buttons, sliders, combos,
dropdowns, data grids, dialogs, drag-and-drop, camera-anchored world panels, …) and is
the canonical UI smoke test. Build every binary the local session starts, then run it:

```bash
cargo build -p neon-projectd -p neon-eventd -p neon-ui-runtime -p neon-wgpu-runtime -p neon-dev --bins
cargo run -p neon-dev -- case component-gallery --show-logs
```

![component-gallery walkthrough](docs/media/component-gallery/component-gallery.gif)

## Usage Example

Drive any service through the **same public protocol** the UI uses. From the CLI:

```bash
neon service describe --target terrain-runtime
neon project assets list
neon terrain tool select --terrain 12 --tool water_inject
neon terrain resource bind --session 92 --asset 81 --revision 5
```

Or send a typed control-plane request directly — the envelope is transport-independent JSON:

```json
{
  "protocol": "neon3.rpc",
  "version": 1,
  "request_id": "uuid",
  "client": { "kind": "ui_runtime", "instance_id": "uuid", "pid": 1234 },
  "target": "terrain-runtime",
  "method": "terrain.tool.select",
  "params": { "tool": "water_inject" },
  "expected_revision": 42,
  "idempotency_key": "uuid"
}
```

A UI surface is a declarative `UiFragment` (serialized form shown; types come from `neon-ui-schema`):

```json
{
  "fragment_id": "toolbox",
  "revision": 1,
  "root": {
    "node_id": "root",
    "kind": "panel",
    "bounds": { "x": 0, "y": 0, "width": 240, "height": 480 },
    "visible": true,
    "enabled": true,
    "style": { "background_color": [0.12, 0.14, 0.18, 1.0] },
    "children": [
      { "node_id": "btn", "kind": "button", "bounds": { "x": 8, "y": 8, "width": 224, "height": 32 }, "visible": true, "enabled": true, "children": [] }
    ]
  },
  "effects": []
}
```

In Rust you build a `UiFragment` and submit it to the single renderer via
`wgpu.ui.submit_fragment`. No drawing code lives outside `neon-wgpu-runtime`.

## Workspace Crates

| Crate | Responsibility |
|-------|----------------|
| `neon-protocol` | Versioned control-plane protocol types |
| `neon-ipc` | Length-prefixed JSON RPC / event transport over loopback TCP |
| `neon-observability` | Command journal, receipts, and debug snapshots |
| `neon-eventd` | Event journal and subscription service |
| `neon-ui-schema` | Declarative UI schema, input frames, and fragment types |
| `neon-ui` | Unified windowless UI layer: schema, flow compiler, runtime entry |
| `neon-ui-runtime` | Headless UI declaration runtime: NUI Flow compiler, host adapter, input state |
| `neon-wgpu-runtime` | The only window + wgpu device + final composition |
| `neon-wgpu-ai` | GPU inference kernels (compute only; sole caller is `neon-wgpu-runtime`) |
| `neon-world-bridge` | Renderer-neutral world/camera synchronization contract |
| `neon-projectd` | Project and asset authority service (sole writer) |
| `neon-cli` | Public command-line protocol client |
| `neon-dev` | Local development and scenario tooling |

## Observability & Acceptance

Every service exposes a structured, subscribable diagnostic stream and a bounded command journal.
Verification uses declarative scenarios that emit machine-readable JSON (status, steps, trace request
IDs, artifacts) instead of manual clicking.

See **`AGENTS.md`** for the full architecture contract, process boundaries, and acceptance levels.

## Contributing

Contributions are welcome. Please read `AGENTS.md` first — it defines the process boundaries, authority
ownership, and the AI/debug discipline this project follows. Open an issue or PR for changes; use GitHub
Discussions for larger architecture decisions.

## License

Neon3 is dual-licensed under either of:

- [MIT License](https://opensource.org/licenses/MIT)
- [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0)

at your option.

> Note: `Cargo.toml` declares `MIT OR Apache-2.0`; the `LICENSE-MIT` / `LICENSE-APACHE` files should be
> added to the repository before publishing.
