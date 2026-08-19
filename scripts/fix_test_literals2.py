"""Second pass: repair remaining struct literals. Locates the literal by scanning
upward for 'StructName {' from the reported error line, then brace-matches."""
import re

FIXES = [
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 10735, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12373, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12430, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12458, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12542, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12562, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12599, "UiVisual", "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12641, "UiVisual", "uv_pg"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12667, "UiVisual", "uv_pg_wd"),
    ("crates/neon-wgpu-runtime/src/ui_renderer.rs", 12714, "UiVisual", "uv_pg_wd"),
    ("crates/neon-wgpu-runtime/src/lib.rs", 9451, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/lib.rs", 9580, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/lib.rs", 9676, "UiNode", "un_wd"),
    ("crates/neon-wgpu-runtime/src/lib.rs", 9699, "UiNode", "un_wd"),
]

WD_DUP = [("crates/neon-wgpu-runtime/src/ui_renderer.rs", 10919)]

def find_literal(lines, line_no, struct_name):
    """Scan upward from line_no (1-based) for a line matching
    'StructName {' (possibly with other code before it); return the 0-based
    line index of that line and the column of the '{'."""
    for i in range(line_no - 1, -1, -1):
        line = lines[i]
        m = re.search(re.escape(struct_name) + r"\s*\{", line)
        if m:
            return i, m.start() + len(struct_name) + 1  # col of '{' (0-based)
    return None

def block_end(lines, start_line, brace_col):
    depth = 0
    for i in range(start_line, len(lines)):
        line = lines[i]
        start = brace_col if i == start_line else 0
        for j in range(start, len(line)):
            ch = line[j]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return i
        brace_col = 0
    return None

def process(path, fixes, wd_dup):
    with open(path, "r", encoding="utf-8", newline="") as f:
        lines = f.readlines()
    report = []
    for (line_no,) in wd_dup:
        if lines[line_no - 1].strip() == "world_depth: None," and lines[line_no - 2].strip() == "world_depth: None,":
            del lines[line_no - 1]
            report.append(f"line {line_no}: removed duplicate world_depth")
    for f, line_no, struct_name, kind in sorted(fixes, key=lambda x: x[1], reverse=True):
        loc = find_literal(lines, line_no, struct_name)
        if loc is None:
            report.append(f"line {line_no}: FAIL no literal for {kind}")
            continue
        start_line, brace_col = loc
        end = block_end(lines, start_line, brace_col)
        if end is None:
            report.append(f"line {line_no}: FAIL no block end for {kind}")
            continue
        indent = re.match(r"[ \t]*", lines[end]).group(0)
        fi = indent + "    "
        if kind == "un_wd":
            insert = [fi + "world_depth: None,\n"]
        elif kind == "uv_pg":
            insert = [fi + "paint_group_id: 0,\n"]
        else:  # uv_pg_wd
            insert = [fi + "world_depth: None,\n", fi + "paint_group_id: 0,\n"]
        lines[end:end] = insert
        report.append(f"line {line_no}: {kind} inserted before line {end + 1}")
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.writelines(lines)
    return report

for path in {"crates/neon-wgpu-runtime/src/ui_renderer.rs", "crates/neon-wgpu-runtime/src/lib.rs"}:
    fixes = [(f, l, s, k) for f, l, s, k in FIXES if f == path]
    wd = [(l,) for f, l in WD_DUP if f == path]
    print(f"== {path} ==")
    for r in process(path, fixes, wd):
        print("  " + r)
