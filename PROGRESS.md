# Progress: editor-runtime reveal acknowledgement contract

Date: 2026-09-18

## Current status

The Neon3 kernel-side formal `editor.visual.reveal` acknowledgement contract
is implemented. No SDK or IDE files were changed. The pre-existing user change
in `crates/neon-wgpu-runtime/src/ui_renderer.rs` was preserved.

## Root cause

The WGPU reveal handler previously returned only renderer-facing caret,
selection, scroll, and presentation fields. It dropped the visual operation
correlation id and document identity, accepted incomplete range input, and did
not prove that the returned presentation was bound to the requested document
or a positive authoritative revision. The IDE had already changed to reject
such an acknowledgement, so the cross-process reveal queue could not close.

## Implemented files

- `crates/neon-protocol/src/lib.rs`: added the canonical `EditorRevealRange`
  and `EditorVisualRevealAck` wire types, including normalized-range validation.
- `crates/neon-wgpu-runtime/src/lib.rs`: validates required operation/path/
  document/range fields and scalar-range consistency; checks returned document
  binding, optional document revision, and positive document/presentation
  revisions; emits the formal ack fields plus existing visual state.
- `CHANGELOG.md`: added the unreleased contract change and verification record.
- `PROGRESS.md`: this progress and handoff record.

## Cross-module impact

The IDE/SDK contract remains unchanged from the consumer perspective: generic
RPC responses carry the new result fields, so neither repository was modified.
`neon3-runtime` continues to inject the UI editor bridge; the WGPU kernel now
rejects a bridge presentation whose authoritative document does not match the
request. The response includes `visual_operation_id`, `document_id`,
normalized `range`, `document_revision`, and `presentation_revision`.

## Verification

- `cargo test -p neon-wgpu-runtime editor_reveal --lib --quiet`: passed, 3/3.
- `cargo test -p neon-protocol --lib --quiet`: passed, 20/20.
- `cargo fmt --all -- --check`: not clean because of pre-existing formatting
  drift across unrelated files, including the preserved user-modified
  `ui_renderer.rs`; no full-format rewrite was performed.
- `cargo build`: passed for the full workspace (warnings only).

## Next steps

1. Run the IDE reveal probe against a live runtime as the cross-repository
   end-to-end acceptance check; the IDE remains intentionally unmodified.
2. Keep the generic SDK/IDE response parsing aligned with this public result
   schema when those repositories next receive their planned updates.

## Final verification update

The full workspace `cargo build` passed on 2026-09-18. The targeted reveal and
protocol tests listed above also passed. `git diff --check` passed. Commit
`ff6ba88` (`Complete editor reveal acknowledgement contract`) was created and
pushed to `origin/master` on 2026-09-18. The remaining next step is the live
cross-repository IDE probe; it requires the runtime services to be running and
does not justify changing the IDE or SDK in this kernel task.
