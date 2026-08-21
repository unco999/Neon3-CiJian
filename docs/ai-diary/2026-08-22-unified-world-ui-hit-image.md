---
date: 2026-08-22
topic: Unify ScreenUi and WorldUi pointer ID rendering
crates:
  - neon-wgpu-runtime
  - neon3-bevy-nui-host
---

## Findings

WorldUi pointer handling selected a separate renderer and split fragment set. The
external ID pass used raw combined fragments, while the pointer readback used the
WorldUi-only path. This made WorldUi bindings visible in diagnostics without
guaranteeing a hit at the projected screen position.

## Changes

The runtime now creates one projected interaction snapshot from the combined
fragment set. Both the external R32Uint ID pass and pointer readback use
`self.ui` with that snapshot. Screen and World UI are therefore painted into one
ID image; later instances overwrite earlier IDs and pointer release uses only the
captured binding, with no bubbling.

The Bevy host surface ring remains three-buffered as required by the headless
external server, and WorldUi uses a valid placement.

## Verification

- `cargo test -p neon-wgpu-runtime --lib`: 122 passed, 1 ignored.
- Focused GPU regression: overlapping panels resolve to `combined/upper`.
- `cargo check` in `D:\bevy-nui-host`: passed.
- Release host was started on DX12; flow and input frame were accepted and the
  runtime rendered without surface-open rejection. Manual pointer interaction was
  not automated in this run.

## Status

Unified ID path implemented. Full two-monster JSONL interactive acceptance probe
remains pending.

## Follow-up

The first WorldUi click produced a valid numeric ID and node path, but its
animation did not select the clicked monster because every generated monster
state machine listened to the same semantic intent. Monster intents are now
unique per stable monster key, and close intents use the active combined-flow
program and input revisions. The GPU acceptance test also verifies that a
projected WorldUi panel writes a non-clear ID and maps it to its stable node
path.

World projection motion is now separated from presentation motion. Projected
WorldUi x/y changes snap to the current camera/anchor sample while active
size/color transitions retain their original start time. A regression test
covers camera movement during an active WorldUi transition.

## Performance Follow-up

Camera frames and anchor batches now use latest-value in-flight gates and stable
signatures. Unchanged state is not sent again, and camera dragging cannot queue
obsolete FIFO requests. The external renderer skips unified-ID plan animation
work when no external ID target is registered. Release runtime verification
showed idle frames with `static_skip=60`, `dropped=0`, and render average near
`0.06-0.07ms` after startup; the initial asset-loading window remains separate.

Pointer tracing now emits only lifecycle-boundary records: Bevy send, WGPU
semantic click, Bevy semantic receive, and WorldUi transition begin/end. This
separates transport queue delay, GPU readback delay, and presentation-transition
time without per-frame or per-node logging.
