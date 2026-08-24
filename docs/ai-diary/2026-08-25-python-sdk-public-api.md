---
date: 2026-08-25
topic: Python SDK public multi-mode API
type: implementation
---

## Files

- `D:/Neon3Sdk/src/neon3_sdk/render.py`
- `D:/Neon3Sdk/src/neon3_sdk/ui.py`
- `D:/Neon3Sdk/src/neon3_sdk/input.py`
- `D:/Neon3Sdk/src/neon3_sdk/runtime.py`
- `D:/Neon3Sdk/src/neon3_sdk/fixtures/calculator.nui`
- `D:/Neon3Sdk/src/neon3_sdk/bin/api_contract_probe.py`

## Change

Added public typed APIs for windowed/headless/external-surface runtime modes,
NUI flow submission and input publication, render diagnostics/capture, world
configuration, 3D camera frames, pointer input, DX12 shared surface open/
acquire/frame, and backend negotiation. Moved the calculator NUI source to a
package fixture file.

Keyboard API is capability-gated. The current Neon3 runtime does not advertise
`wgpu.ui.keyboard.v1`, so Python raises a stable capability error instead of
pretending that an unsupported keyboard RPC succeeded.

## Validation

- Python unit tests: 7 passed.
- Python compileall: passed.
- Headless JSONL API probe: passed for world configure, 3D camera submit,
  diagnostics, and render graph.
- Windowed DX12 external-surface JSONL probe: passed for surface open,
  brokered handle acquire, frame sequence, world configure, and camera submit.
