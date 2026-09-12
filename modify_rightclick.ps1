$path = "D:\Neon3\crates\neon-wgpu-runtime\src\lib.rs"
$content = Get-Content $path -Raw
$old = @"
                // Built-in context menu: show all ContextMenu components on right-click
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.ui.show_context_menus();
                    self.redraw_pending = true;
                }
"@
$new = @"
                // Built-in context menu: show only if the node under the pointer
                // (or an ancestor) has a context_menu binding.
                if let Some(gpu) = self.gpu.as_mut() {
                    if gpu.ui.context_menu_at_pointer().is_some() {
                        gpu.ui.show_context_menus();
                        self.redraw_pending = true;
                    }
                    { use std::io::Write; if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(r"D:\Neon3\debug.log") { let _ = writeln!(f, "[CTX] right-click, binding={:?}", gpu.ui.context_menu_at_pointer()); } }
                }
"@
if ($content.Contains($old)) {
    $content = $content.Replace($old, $new)
    [System.IO.File]::WriteAllText($path, $content, [System.Text.UTF8Encoding]::new($false))
    Write-Output "OK"
} else {
    Write-Output "NOT FOUND"
}
