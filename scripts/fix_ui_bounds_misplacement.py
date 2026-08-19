"""Remove paint_group_id/world_depth lines mistakenly inserted inside UiBounds
literals, then re-insert missing fields at the correct UiVisual/UiNode/PlannedNode
level. Works by brace analysis, not line numbers."""
import re

FILES = [
    "crates/neon-wgpu-runtime/src/ui_renderer.rs",
    "crates/neon-wgpu-runtime/src/lib.rs",
]

def brace_span(lines, start_line, brace_col):
    """Return (end_line, end_col) of the brace block starting at (start_line, brace_col)."""
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
                    return i, j
        brace_col = 0
    return None

def analyze(path):
    with open(path, "r", encoding="utf-8", newline="") as f:
        lines = f.readlines()

    # 1) locate every UiBounds literal start (name 'UiBounds' followed by '{')
    bounds_spans = []
    for i, line in enumerate(lines):
        for m in re.finditer(r"UiBounds\s*\{", line):
            brace_col = m.end() - 1  # position of '{'
            span = brace_span(lines, i, brace_col)
            if span:
                bounds_spans.append((i, span[0]))  # inclusive start/end line
    # merge/normalize: keep list of (start, end) line ranges

    removed = []
    out = []
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped in ("paint_group_id: 0,", "world_depth: None,"):
            inside = any(s <= i <= e for s, e in bounds_spans)
            if inside:
                removed.append((i + 1, stripped))
                continue  # drop the line
        out.append(line)

    with open(path, "w", encoding="utf-8", newline="") as f:
        f.writelines(out)
    return removed

for path in FILES:
    removed = analyze(path)
    print(f"== {path}: removed {len(removed)} misplaced lines ==")
    for ln, txt in removed:
        print(f"  line {ln}: {txt}")
