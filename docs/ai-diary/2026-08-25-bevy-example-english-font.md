---
date: 2026-08-25
topic: Bevy example English labels and published font strategy
type: implementation
---

## Change

The ordinary world-UI and physics world-UI examples now use English visible
labels and object names. This avoids requiring CJK glyph coverage from the
small crates.io fallback font. README now documents both examples and their
startup commands.

The published WGPU runtime keeps a small open-source Latin font to stay within
registry package limits. CJK support remains an application/project font asset
that must be preloaded through the existing resource path.

## Validation

- `cargo check --manifest-path D:/bevy-nui-host/Cargo.toml`: passed.
- `cargo run --manifest-path D:/bevy-nui-host/Cargo.toml --release -- --help`:
  passed; release build used registry Neon `0.2.0` dependencies.
- Existing warnings: unused `MONSTER_INTRO`, plugin service thread field, and
  DX12 consumer dead-code warnings. No build failures.
- A missing-UI regression was reproduced and fixed. English text replacement
  had introduced 7/9-space indentation in `status-flow.nui`; Neon rejected the
  generated Flow with `nui_flow_mixed_indentation`. The host now validates the
  generated Flow before startup and the corrected Flow produced renderer
  `fragment_count=1`.
- The world-UI path then exposed an independent Bevy composite validation error:
  the screen bind group used a 4x-MSAA depth view with a single-sample binding.
  Screen UI now binds a single-sample dummy depth view, while the world path
  uses the single-sample scene depth contract. Physics runtime validation then
  observed camera/anchor batches and rendered frames without the validation
  crash.
