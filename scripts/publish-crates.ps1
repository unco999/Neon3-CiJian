param(
    [switch]$DryRun,
    [switch]$AllowDirty
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path Env:CARGO_REGISTRY_TOKEN)) {
    throw "CARGO_REGISTRY_TOKEN is not set. Create a crates.io API token and set it in this shell before publishing."
}

$crates = @(
    "neon-protocol",
    "neon-world-bridge",
    "neon-observability",
    "neon-ipc",
    "neon-ui-schema",
    "neon-projectd",
    "neon-eventd",
    "neon-wgpu-ai",
    "neon-ui-runtime",
    "neon-ui",
    "neon-cli",
    "neon-dev",
    "neon-wgpu-runtime"
)

$dirty = if ($AllowDirty) { "--allow-dirty" } else { $null }
$mode = if ($DryRun) { "--dry-run" } else { $null }

foreach ($crate in $crates) {
    Write-Host "Publishing $crate"
    $arguments = @("publish", "-p", $crate, "--locked")
    if ($dirty) { $arguments += $dirty }
    if ($mode) { $arguments += $mode }
    $output = & cargo @arguments 2>&1
    $exitCode = $LASTEXITCODE
    $output | ForEach-Object { Write-Host $_ }
    if ($output -match "already exists on crates\.io index") {
        Write-Host "$crate already published; skipping"
        continue
    }
    if ($exitCode -ne 0) { throw "Publishing failed for $crate (exit code $exitCode)" }
}
