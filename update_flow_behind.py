path = r'D:\Neon3案例\node\src\cases\music-player\flow.ts'
with open(path, 'r', encoding='utf-8') as f:
    content = f.read()

# 1. Register pulse-flow-light shader after pulse-neon-edge
old_shader_decl = 'shader pulse-neon-edge version 3 fallback standard_ui'
new_shader_decl = '''shader pulse-neon-edge version 3 fallback standard_ui
shader pulse-flow-light version 1 fallback standard_ui'''
if old_shader_decl in content:
    content = content.replace(old_shader_decl, new_shader_decl, 1)
    print("OK: registered flow-light shader")
else:
    print("MISS: shader declaration not found")

# 2. Add behind_glass flow-light panel as first child of player-shell
# It should be behind everything, full size, with the flow light material
old_shell_start = '''  panel player-shell column x 0 y 0 w 360 h 720 gap 4 pad 14 fill #00000000 radius 0 clip bounds
    geometry cut 36 36 36 36
    material pulse-glass parameter rim_strength 0.20
    

    panel status-row'''

new_shell_start = '''  panel player-shell column x 0 y 0 w 360 h 720 gap 4 pad 14 fill #00000000 radius 0 clip bounds
    geometry cut 36 36 36 36
    material pulse-glass parameter rim_strength 0.20
    panel flow-light-layer overlay x 0 y 0 w 360 h 720 fill #00000000 radius 0 composition_layer behind_glass
      material pulse-flow-light

    panel status-row'''

if old_shell_start in content:
    content = content.replace(old_shell_start, new_shell_start, 1)
    print("OK: added behind_glass flow-light panel")
else:
    print("MISS: shell start not found")
    # Try to find the exact text
    import re
    idx = content.find('panel player-shell')
    if idx >= 0:
        print("Found player-shell at:", idx)
        print(repr(content[idx:idx+300]))

with open(path, 'w', encoding='utf-8') as f:
    f.write(content)
print("flow.ts updated")
