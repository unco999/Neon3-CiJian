$path = "D:\Neon3\crates\neon-wgpu-runtime\src\ui_renderer.rs"
$content = Get-Content $path -Raw
$old = "                    neon_ui_schema::UiEffect::ControlSkin { skin } => {"
$new = @"
                    neon_ui_schema::UiEffect::ContextMenuBinding { node_id, context_menu_id } => {
                        self.context_menu_bindings.insert(
                            format!("{}/{}", fragment.fragment_id.0, node_id.0),
                            context_menu_id.clone(),
                        );
                    }
                    neon_ui_schema::UiEffect::ControlSkin { skin } => {
"@
if ($content.Contains($old)) {
    $content = $content.Replace($old, $new)
    [System.IO.File]::WriteAllText($path, $content, [System.Text.UTF8Encoding]::new($false))
    Write-Output "OK"
} else {
    Write-Output "NOT FOUND"
}
