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
