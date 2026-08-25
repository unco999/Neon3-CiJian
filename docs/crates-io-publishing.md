# crates.io Publishing

The Neon3 workspace is prepared for separate crates.io publication. Internal
dependencies use both `version = "0.1.0"` and `path = ...`, so Cargo can rewrite
the local path to the registry dependency in the published package.

## Order

Publish in this order:

```text
neon-protocol
neon-world-bridge
neon-observability
neon-ipc
neon-ui-schema
neon-projectd
neon-eventd
neon-wgpu-ai
neon-ui-runtime
neon-ui
neon-cli
neon-dev
neon-wgpu-runtime
```

## Publish

Set a crates.io API token in the shell. The token is never written to the
repository:

```powershell
$env:CARGO_REGISTRY_TOKEN = "<crates.io-api-token>"
cd D:/Neon3
./scripts/publish-crates.ps1 -DryRun -AllowDirty
./scripts/publish-crates.ps1 -AllowDirty
```

Do not use `--allow-dirty` in CI after the repository is committed. The current
workspace must also have a healthy Git object database because Cargo inspects
VCS state while creating package archives.

The foundational protocol, bridge, observability, IPC, UI schema, UI runtime,
UI facade, and CLI packages are already present in the registry at `0.1.0`.
The remaining service/runtime packages were published on 2026-08-25 after the
local pack indexes were rebuilt:

```text
neon-projectd 0.1.0
neon-eventd 0.1.0
neon-wgpu-ai 0.1.0
neon-dev 0.1.0
neon-wgpu-runtime 0.1.0
```

The coherent current release is `0.2.0` for all Neon3 crates. The `0.1.0`
release remains available for compatibility, but applications should use the
matched `0.2.0` set.
