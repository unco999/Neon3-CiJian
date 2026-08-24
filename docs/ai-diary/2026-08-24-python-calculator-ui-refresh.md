---
date: 2026-08-24
topic: Python calculator domain UI refresh and layout correction
type: implementation
---

## Files

- `crates/neon-ui-runtime/src/lib.rs`
- `D:/Neon3Sdk/src/neon3_sdk/calculator.py`
- `D:/Neon3Sdk/src/neon3_sdk/cli.py`

## Problem

The Python calculator published accepted revisioned input frames, but the
window showed a static `Result` label. The sample also used content-sized
buttons, producing an unusable keypad layout.

The renderer consumed numeric presentation state, but the refreshed fragment
did not materialize a readable numeric value for a progress control. A stale
Python calculator listener on port 39104 initially caused the launcher to read
a different process's in-memory state; the launcher now rejects an occupied
calculator endpoint before it starts.

## Change

- `refresh_fragment_from_program` now materializes `ProgressBar` numeric state
  as a literal fragment label while retaining renderer-owned numeric
  presentation.
- The calculator NUI declares fixed 112x52 button cells, fixed row height, and
  a larger surface/display region.
- The calculator JSONL scenario queries `wgpu.ui.fragment.snapshot` and records
  producer domain/input revisions plus renderer fragment revision/sequence and
  the expected final text.

## Status

Completed.

## Validation

- `cargo test -p neon-ui-runtime --lib`: 107 passed, 0 failed.
- `cargo build -p neon-ui-runtime`: passed.
- `python -m neon3_sdk calculator --neon-root D:/Neon3 --once --timeout-seconds 20`:
  passed. Domain revision 4 and UI input revision 4 resulted in renderer
  fragment revision/sequence 5/5; fragment snapshot contained final text `3`.
- Default interactive calculator startup no longer executes the test sequence;
  it starts with display `0`. The deterministic sequence is restricted to
  `--once` or `--headless` execution.
- Calculator domain state machine now tracks `awaiting_operand` so an operator
  pressed after `=` selects the next operation without re-applying the previous
  display. Real service validation executed `1 + 1 = + 1 =` and produced 3;
  domain/input revisions were 7/7 and renderer fragment revision/sequence 8/8.
