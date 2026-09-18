# Window Shell And Event Forwarding

This change makes the windowed `neon3-runtime` host use a full-viewport shell
without allowing a NUI Flow root to resize or restore the native window.

## Window defaults

When `neon3-runtime --window` starts, it supplies defaults only when the caller
has not already set them:

```text
NEON_WINDOW_BACKDROP=acrylic
NEON_WINDOW_MAXIMIZED=1
NEON_WINDOW_CHROME=borderless
```

Explicit environment values still win. `neon-wgpu-runtime` also accepts:

```text
NEON_WINDOW_FULLSCREEN=borderless|1|true
NEON_WINDOW_MAXIMIZED=0|false|off
```

Borderless fullscreen uses winit's native `Fullscreen::Borderless` state. A
maximized or fullscreen shell does not apply Flow-driven scripted inner-size
requests and does not call `drag_window`, because either operation would
restore the window and expose the old small fixture size.

## Click ownership

`neon-wgpu-runtime` owns pointer hit testing and immediate interaction state. It
does not publish the same semantic click to eventd. The UI Runtime host-forward
path is the single eventd publisher, preventing duplicate SDK events.

The forwarded payload contains the semantic action, parameters, event id, node
path, and the optional committed text/control value:

```json
{
  "intent": "button.save",
  "params": {},
  "event_id": "...",
  "node_path": "root.toolbar.save",
  "committed_text": null,
  "control_value": null
}
```

`[wgpu-click]` diagnostics identify the hit node path while debugging the
producer side. SDK consumers should use the eventd contract and request/event
ids rather than log text.

## Verification

Compile the runtime and the real window probe with:

```text
cargo check -p neon-wgpu-runtime -p neon3-runtime
cargo test -p neon-wgpu-runtime --lib
cargo run -p neon-wgpu-runtime --bin window_backdrop_probe
```

The probe emits JSONL containing backdrop state, premultiplied alpha mode,
producer/consumer geometry, frame pairing, and pass/fail status. It requires a
Windows desktop session with a working WGPU adapter.
