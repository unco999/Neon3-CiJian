# Launch Neon3 three services headless (no window), then idle.
# Ports: eventd=39101, ui-runtime=39102, wgpu-runtime=39103
$ErrorActionPreference = "Stop"
$root = "d:/Neon3"

$ev = Start-Process -FilePath "$root/target/debug/neon-eventd.exe" -ArgumentList @("--server", "127.0.0.1:39101", "1") -WindowStyle Hidden -PassThru
$wg = Start-Process -FilePath "$root/target/debug/neon-wgpu-runtime.exe" -ArgumentList @("--headless-server", "127.0.0.1:39103") -WindowStyle Hidden -PassThru
$ui = Start-Process -FilePath "$root/target/debug/neon-ui-runtime.exe" -ArgumentList @("--forward-server", "127.0.0.1:39102", "127.0.0.1:39103", "127.0.0.1:39104", "--eventd", "127.0.0.1:39101") -WindowStyle Hidden -PassThru

Write-Host "standby (eventd 39101 / wgpu 39103 / ui 39102, press Enter to stop)"
Read-Host | Out-Null

Stop-Process -Id $ev.Id, $wg.Id, $ui.Id -Force -ErrorAction SilentlyContinue
Write-Host "stopped"
