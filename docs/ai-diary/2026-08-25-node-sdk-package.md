---
date: 2026-08-25
topic: Node TypeScript SDK package
type: implementation
---

## Files

- `D:/Neon3Sdk/packages/node-sdk/package.json`
- `D:/Neon3Sdk/packages/node-sdk/src/client.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/runtime.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/ui.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/render.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/input.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/examples/api-probe.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/test/wire.test.ts`

## Change

Created the independent `@neon3/sdk` TypeScript package. It exposes the same
canonical RPC framing and typed UI, runtime, camera, pointer, and external
surface APIs as the Python SDK. The repository root now separates `packages/
node-sdk`, Python `src/`, probes, and tests; the obsolete root Cargo manifest
was removed.

## Validation

- `npm test`: TypeScript build and Node framing test passed.
- `npm run probe`: real headless Neon3 describe/diagnostics/UI snapshot passed.
- `NEON_EXTERNAL=1 npm run probe`: real windowed DX12 surface open, brokered
  handle acquire, frame sequence, and UI snapshot passed.
- Python unit tests: 7 passed after the directory/API changes.
