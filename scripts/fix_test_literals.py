"""Repair test-code struct literals after paint_group_id/world_depth fields were added.
Fixes: UiVisual/PlannedNode missing paint_group_id, UiNode/UiVisual missing world_depth,
duplicate world_depth lines. Uses rustc-reported line/col to locate literal boundaries.
"""
import re
import sys

# (file, line, col, kind) — line/col 1-based, kind in {uv_pg, uv_pg_wd, pn_pg, un_wd}
FIXES = [
    # ui_renderer.rs
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 7642, 22, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 7687, 13, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 7694, 13, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 7701, 13, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 7871, 26, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 7895, 28, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 7961, 21, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 7958, 28, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8045, 21, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8042, 28, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8209, 21, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8206, 28, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8286, 22, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8313, 28, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8385, 22, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8410, 28, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8456, 22, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8482, 13, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8489, 13, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8531, 22, "uv_pg_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8556, 13, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8563, 13, "pn_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8703, 34, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 8868, 24, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 10126, 9, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 10710, 32, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12348, 20, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12405, 21, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12433, 20, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12517, 13, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12537, 13, "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12574, 22, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12615, 24, "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12640, 22, "uv_pg_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12685, 22, "uv_pg_wd"),
    # lib.rs
    ("crates/neon-wgpu-runtime/src/lib.rs", 9452, 20, "un_wd"),
    ("crates/neon-wgpu-runtime/src/lib.rs", 9581, 13, "un_wd"),
    ("crates/neon-wgpu-runtime/src/lib.rs", 9677, 23, "un_wd"),
    ("crates/neon-wgpu-runtime/src/lib.rs", 9700, 20, "un_wd"),
]

# line -> (file, kind) for wd_dup removal (delete one of two identical consecutive lines)
WD_DUP = [
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 10663),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 10894),
    ("crates/neon-wgpu-runtime/src/lib.rs", 9136),
]

def find_block_end(lines, line_no, col):
    """Find 0-based index of the line containing the matching closing brace of the
    struct literal starting at (line_no 1-based, col 1-based)."""
    idx = line_no - 1
    pos = lines[idx].find("{", col - 1)
    if pos == -1:
        return None
    depth = 0
    for i in range(idx, len(lines)):
        line = lines[i]
        start = pos if i == idx else 0
        for j in range(start, len(line)):
            ch = line[j]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return i
        pos = 0
    return None


def process_file(path, fixes, wd_dup_lines):
    with open(path, "r", encoding="utf-8", newline="") as f:
        lines = f.readlines()
    changed = []
    # process wd_dup first (line numbers of following inserts shift)
    for (line_no,) in wd_dup_lines:
        # delete this line (one of the duplicate pair) and its newline
        if lines[line_no - 1].strip() == "world_depth: None," and line_no - 2 >= 0 and lines[line_no - 2].strip() == "world_depth: None,":
            del lines[line_no - 1]
            changed.append((line_no, "wd_dup: removed duplicate line"))
    # process inserts (line numbers refer to the ORIGINAL file; process in reverse
    # order so earlier inserts don't shift later targets)
    for f, line_no, col, kind in sorted(fixes, key=lambda x: x[1], reverse=True):
        end = find_block_end(lines, line_no, col)
        if end is None:
            changed.append((line_no, f"FAIL: no block end found for {kind}"))
            continue
        indent = re.match(r"[ \t]*", lines[end]).group(0)
        field_indent = indent + "    "
        if kind == "pn_pg":
            insert = [field_indent + "paint_group_id: 0,\n"]
        elif kind == "uv_pg":
            insert = [field_indent + "paint_group_id: 0,\n"]
        elif kind == "uv_pg_wd":
            insert = [field_indent + "world_depth: None,\n", field_indent + "paint_group_id: 0,\n"]
        elif kind == "un_wd":
            insert = [field_indent + "world_depth: None,\n"]
        else:
            continue
        lines[end:end] = insert
        changed.append((line_no, f"{kind}: inserted at end line {end + 1}"))
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.writelines(lines)
    return changed


for path in {"crates/neon-wgpu-runtime/src/ui_renderer.rs", "crates/neon-wgpu-runtime/src/lib.rs"}:
    fixes = [(f, l, c, k) for f, l, c, k in FIXES if f == path]
    wd = [(l,) for f, l in WD_DUP if f == path]
    changes = process_file(path, fixes, wd)
    print(f"== {path}: {len(changes)} changes ==")
    for line_no, desc in changes:
        print(f"  line {line_no}: {desc}")
