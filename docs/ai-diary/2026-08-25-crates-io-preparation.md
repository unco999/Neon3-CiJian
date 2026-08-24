---
date: 2026-08-25
topic: Neon3 crates.io publication preparation
type: implementation
---

## Files

- `Cargo.toml`
- `crates/*/Cargo.toml`
- `crates/{neon-projectd,neon-eventd,neon-wgpu-runtime,neon-wgpu-ai,neon-cli,neon-dev}/README.md`
- `scripts/publish-crates.ps1`
- `docs/crates-io-publishing.md`

## Change

Added workspace repository metadata, package descriptions/readmes, and
versioned path dependencies required for registry publication. Added a
dependency-ordered publish script covering protocol, bridge, IPC, UI, tooling,
and WGPU crates.

## Validation and blockers

- `cargo metadata --no-deps --format-version 1`: passed.
- `cargo check --workspace`: passed; existing WGPU dead-code/unused warnings
  remain warnings.
- `cargo package -p neon-protocol --allow-dirty --no-verify`: blocked because
  the pre-existing `D:/Neon3/.git` object database is corrupt and Cargo cannot
  retrieve Git status (`missing object c106a59...`).
- `CARGO_REGISTRY_TOKEN`: not set.
- `cargo search neon-protocol`: blocked by unavailable crates.io network through
  the configured proxy.

No crate was uploaded.
