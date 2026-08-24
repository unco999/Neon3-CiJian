---
date: 2026-08-25
topic: Modular Node calculator example
type: implementation
---

## Files

- `D:/Neon3Sdk/packages/node-sdk/src/examples/calculator/domain.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/examples/calculator/rpc-service.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/examples/calculator/flow.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/examples/calculator/calculator.nui`
- `D:/Neon3Sdk/packages/node-sdk/src/examples/calculator/app.ts`
- `D:/Neon3Sdk/packages/node-sdk/src/test/calculator.test.ts`

## Change

Added a modular Node calculator example. The reusable SDK remains in the
package root; domain rules, Python-equivalent RPC service, NUI asset loading,
and application lifecycle are separate example modules.

## Validation

- `npm test`: TypeScript build and 2 Node tests passed.
- `npm run calculator:once`: real Neon3 service chain passed the scenario
  `1 + 1 = + 1 =`; Node domain revision 7, renderer fragment revision 8, final
  display 3.
- SDK repository layout was normalized so Python now lives under
  `D:/Neon3Sdk/packages/python-sdk`; Python editable installation with the
  setuptools backend and package tests passed.
