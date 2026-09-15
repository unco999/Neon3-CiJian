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
| I | numeric presentation interpolation with linear easing and enter syntax | Fill / Empty |
| J | damped spring overshoot and reverse retarget | Spring / Reset |
| K | GPU translate, scale, rotate entry/retarget track with depth pairing | Transform / Reset |
| L | exit tombstone retention, final removal, and re-mount generation protection | Unmount / Mount |
| M | bounded keyframes, sequence playback, and repeat cycles | Play M / Reset |

## Test Case Method

Run the complete protocol-driven case from the workspace root. The first command
builds both the windowed renderer and the probe; the second command starts the
real WGPU window server and the headless UI runtime:

```powershell
cargo build --quiet -p neon-wgpu-runtime --bin neon-wgpu-runtime -p neon-ui-runtime --bin animation_showcase_interactive_probe
& .\target\debug\animation_showcase_interactive_probe.exe | Tee-Object target\animation-showcase-interactive.jsonl
if ($LASTEXITCODE -ne 0) { throw "animation showcase probe failed" }
```

The probe submits this Flow document, activates every declared button through
`debug.window.input.activate_target`, and emits JSONL records containing the
input intent, UI state/revision, WGPU transition, numeric producer/consumer
values, frame pairing, and final `pass_result`. It exits non-zero on any failed
assertion and writes a debug capture to:

```text
target/animation-showcase-interactive.png
```

The probe does not mutate styles or state in the client. Renderer-local hit IDs,
GPU handles, and pointer coordinates remain inside WGPU.

Run the contract and renderer-focused checks separately when changing the
animation implementation:

```powershell
cargo test --quiet -p neon-ui-schema
cargo test --quiet -p neon-ui-runtime
cargo test --quiet -p neon-wgpu-runtime --lib timeline_segments_advance_repeat_and_finish_on_the_exact_target -- --test-threads=1
cargo test --quiet -p neon-wgpu-runtime --lib animation_controls_pause_resume_seek_and_cancel_are_renderer_owned -- --test-threads=1
cargo test --quiet -p neon-wgpu-runtime --lib exit_transition_retains_removed_node_until_its_deadline_and_resurrects -- --test-threads=1
```

The long-running retarget case is a separate real-window probe:

```powershell
cargo build --quiet -p neon-wgpu-runtime --bin neon-wgpu-runtime --bin animation_retarget_probe
& .\target\debug\animation_retarget_probe.exe | Tee-Object target\animation-retarget.jsonl
if ($LASTEXITCODE -ne 0) { throw "animation retarget probe failed" }
```

Each probe record includes producer-side target values, consumer-side sampled
values, transition identity/generation, frame pairing, and the final result.
The JSONL files and PNG capture are disposable acceptance artifacts and are not
part of the project protocol or application state.

## Current boundary

The showcase covers every property in `UiAnimationProperty` and every V1
easing: bounds position/size, opacity, background and border colors, border
width, corner radius, numeric control value, linear, ease-in, ease-out,
ease-in-out, spring, delay, nested propagation, retarget, and final settling.

The transform slice supports canonical target-side state styles plus `from
transform` entry/retarget values. Static target transforms survive transition
completion and fragment refresh. Origins are normalized per node, and panel,
text, image, material, depth, and hit-test paths share the same transform track.

The timeline slice supports `keyframe` declarations with bounded offsets,
per-segment easing, `sequence`/`parallel` metadata, fixed stagger, and bounded
repeat counts including `infinite`. The renderer advances segments at control
boundaries and keeps steady-state interpolation in WGSL. V1 curves include
linear, quadratic ease variants, spring, bounce, and fixed CSS-compatible
`cubic_bezier`. Public renderer control methods expose
pause/resume/seek/cancel receipts through RPC.

Still outside the animation contract: per-property easing, image/texture
cross-fades, scroll-offset animation, animated clip geometry, and shader
parameter keyframe tracks. Spinner chrome time animation remains a separate
renderer feature rather than a state-machine motion.
