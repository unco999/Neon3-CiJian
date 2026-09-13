# Neon3 Animation Showcase

`animation.nui` is the first button-driven animation catalog for the rebuilt UI
animation path. It is declarative: buttons emit semantic intents, the
`NuiFlowStateMachineRuntime` selects a presentation state and motion, and the
WGPU runtime samples the resulting visual target.

## Catalog

| Section | Demonstrates | Buttons |
| --- | --- | --- |
| A | bounds resize, enter baseline, retarget | Toggle / Collapse |
| B | opacity fade with descendant opacity propagation | Toggle / Show |
| C | RGBA color, border width, corner radius | Normal / Warning / Error |
| D | position interpolation | Toggle / Left |
| E | parent and nested child tracks in one state transition | Toggle / Collapse |
| F | three-state cycle and state-aware same-intent dispatch | Next / Large |
| G | border and rounded-corner interpolation | Toggle / Quiet |
| H | delay, zero-opacity render retention, rapid retarget | Toggle / Retarget |

## Protocol-driven verification

From the workspace root:

```text
cargo run -p neon-ui-runtime --bin animation_showcase_interactive_probe
```

The probe starts the real `neon-wgpu-runtime` window server and the headless
`neon-ui-runtime`, submits this Flow document, activates each button through
`debug.window.input.activate_target`, and emits JSONL records containing the
input intent, UI state/revision, WGPU transition, frame pairing, and final
pass/fail result. It also writes a debug capture to:

```text
target/animation-showcase-interactive.png
```

The probe does not mutate styles or state in the client. Renderer-local hit IDs,
GPU handles, and pointer coordinates remain inside WGPU.
