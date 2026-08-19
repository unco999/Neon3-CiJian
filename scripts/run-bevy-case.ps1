# Launch the Neon3 service chain for the Bevy NUI host case, then run Bevy.
# Ports must match Neon3BevyConfig::default (wgpu=39103, ui=39102).
$ErrorActionPreference = "Stop"
$root = "d:/Neon3"

Start-Process -FilePath "$root/target/debug/neon-eventd.exe" -ArgumentList @("--server", "127.0.0.1:39101", "1")
Start-Process -FilePath "$root/target/debug/neon-wgpu-runtime.exe" -ArgumentList @("--window-server", "127.0.0.1:39103", "127.0.0.1:39102")
Start-Process -FilePath "$root/target/debug/neon-ui-runtime.exe" -ArgumentList @("--forward-server", "127.0.0.1:39102", "127.0.0.1:39103", "127.0.0.1:39104", "--eventd", "127.0.0.1:39101")

Start-Sleep -Seconds 5
Set-Location $root
cargo run --manifest-path cases/bevy-nui-host/Cargo.toml
