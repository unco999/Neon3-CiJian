$path = "D:\Neon3\crates\neon-wgpu-runtime\src\ui_renderer.rs"
$content = Get-Content $path -Raw
$old = "    /// Show all context menus in the current plan (built-in right-click)."
$new = @"
    /// Find the context menu bound to the node (or its ancestors) under the pointer.
    /// Returns the context menu node key (without fragment prefix) if a binding is found.
    pub(crate) fn context_menu_at_pointer(&self) -> Option<String> {
        let pointer = self.pointer_position?;
        // Find the topmost node under the pointer.
        let hit_index = self.plan.iter().enumerate().rev().find_map(|(index, node)| {
            let b = node.target.bounds;
            if pointer[0] >= b.x && pointer[0] <= b.x + b.width
                && pointer[1] >= b.y && pointer[1] <= b.y + b.height
            {
                Some(index)
            } else {
                None
            }
        })?;
        // Walk up the ancestor chain looking for a context_menu binding.
        let mut current = Some(hit_index);
        while let Some(idx) = current {
            let node = &self.plan[idx];
            if let Some(menu_id) = self.context_menu_bindings.get(&node.id) {
                return Some(menu_id.clone());
            }
            current = node.parent_id.as_deref()
                .and_then(|pid| self.plan.iter().position(|n| n.id == pid));
        }
        None
    }

    /// Show all context menus in the current plan (built-in right-click).
"@
if ($content.Contains($old)) {
    $content = $content.Replace($old, $new)
    [System.IO.File]::WriteAllText($path, $content, [System.Text.UTF8Encoding]::new($false))
    Write-Output "OK"
} else {
    Write-Output "NOT FOUND"
}
