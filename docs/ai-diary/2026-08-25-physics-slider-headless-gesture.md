---
date: 2026-08-25
topic: Fix headless physics slider gesture delivery
type: fix
---

## Involved files

- `D:/bevy-nui-host/src/lib.rs`
- `D:/bevy-nui-host/Cargo.toml`
- `D:/Neon3/crates/neon-wgpu-runtime/src/lib.rs`

## Finding

The physics flow declaration was valid and the real window pointer hit
`gravity-slider`. Runtime logs showed `phys.gravity.value` was emitted, but
`requested_value` was `null`. The headless `HeadlessExternalGpu::pointer_event`
path captured the binding on `Down`, ignored value gestures on `Move`, and never
called `finish_value_gesture` on `Up`.

## Change

The headless path now starts value gestures on `Down`, updates them on `Move`,
and prefers the finished gesture value on `Up`. The Bevy host is patched to use
the local Neon3 runtime source so the fix is actually used instead of the
unfixed crates.io package.

## Verification

Before the fix, a real drag at logical `[100,130]` produced:

```json
{"intent":"phys.gravity.value","requested_value":null}
```

After rebuilding the patched runtime, a real Win32 drag at logical
coordinates `[100,130] -> [280,130]` produced:

```json
{"intent":"phys.gravity.value","requested_value":{"kind":"f32","value":17.1977}}
```

The physics host accepted the intent and logged `physics_intent_received`.

The value gesture hit bounds were also widened from the renderer's previous
hardcoded right-side 34% to the authored slider node bounds. The Bevy host now
coalesces queued move events to the latest value before sending IPC, reducing
input lag when the render/RPC lane is busy.

## Follow-up finding

The live report showed the semantic value was valid (`gravity-slider`, f32
69.44), but the visible control still did not change. Headless external GPU
uses three renderers: one unified hit-test renderer plus separate `screen_ui`
and `world_ui` color renderers. The local value preview had been retained on
the hit-test renderer, not the renderer that draws the selected surface.

The generic headless pointer path now performs value gesture preview,
completion, and local presentation retention on the selected color renderer.
No physics-specific input-key writeback is used.

Focused runtime test:

```text
cargo test -p neon-wgpu-runtime --lib value_gesture
test ui_renderer::tests::value_gesture_uses_the_current_pointer_value_after_interaction_preparation ... ok
```

The release test command was also attempted, but the existing runtime test
configuration references debug-only capture helpers and fails to compile under
`--release`; this is unrelated to the slider fix.

## Status

Implemented; final rebuild and real-window verification in progress.
