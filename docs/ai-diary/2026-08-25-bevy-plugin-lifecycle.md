---
date: 2026-08-25
topic: Bevy host plugin lifecycle and parameterized example
type: implementation
---

## Files

- `D:/bevy-nui-host/Cargo.toml`
- `D:/bevy-nui-host/src/lib.rs`
- `D:/bevy-nui-host/src/main.rs`
- `D:/bevy-nui-host/README.md`
- `D:/bevy-nui-host/docs/plugin-architecture.md`

## Change

Moved Neon eventd, WGPU, and UI service startup into `Neon3BevyPlugin` behind
`Neon3ServiceMode`: `AutoHeadless`, `AutoWindowed`, and `External`. The example
binary now only parses mode, endpoint, asset, session, and world-UI parameters.
The crate is publishable and has package metadata.

## Validation

- `cargo check`: passed with existing Neon/WGPU warnings.
- `cargo test --lib`: 10 passed, 0 failed.
- `cargo run -- --help`: passed and prints parameterized startup usage.
- Real Bevy launch `--mode headless --no-world-ui`: Bevy window title
  `Neon3 Combined Screen + World UI` opened; plugin submitted the generated NUI
  flow and external image traffic was observed. Existing glTF `TEXCOORD_2/3`
  warnings remain warnings, not failures.
