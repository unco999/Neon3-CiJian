#!/usr/bin/env python3
import sys

def generate_stress_test_file():
    print("=== Generating imgui-component-gallery-stress.nui ===")
    
    with open("tests/fixtures/ui/imgui-component-gallery.nui", "r", encoding="utf-8") as f:
        content = f.read()
    
    # 1. Update the budget declaration to allocate sufficient memory buffers for the stress test
    # Old: budget nodes=200 bindings=132 instances=144 text=200 glyphs=16384 events=76 clips=84
    # New: budget nodes=900 bindings=600 instances=600 text=900 glyphs=65536 events=300 clips=300
    old_budget = "budget nodes=200 bindings=132 instances=144 text=200 glyphs=16384 events=76 clips=84"
    new_budget = "budget nodes=900 bindings=600 instances=600 text=900 glyphs=65536 events=300 clips=300"
    content = content.replace(old_budget, new_budget)
    
    # 2. Design the high-density IMGUI stress panel
    imgui_panel = """
    scroll imgui-perf-stress column minw 360 maxw 430 grow 1 shrink 0 gap 6 pad 10 align stretch fill #EAF6FA line #6FA8C9
      panel imgui-header row h 32 pad 6 gap 6 align center fill #6FA8C9 line #4E8FAE
        text imgui-title value "性能压力测试 (IMGUI UI)"
"""
    
    # We add 60 rows of deep component structures simulating an active IMGUI inspector window
    for i in range(1, 61):
        imgui_panel += f"""      panel imgui-row-{i} row h 24 pad 4 gap 6 align center
        text imgui-label-{i} w 120 h 16 value "IMGUI 仪表盘 #{i}"
        checkbox imgui-cb-{i} w 60 h 20 checked $radio_selected enabled $controls_enabled value "开"
        slider imgui-slider-{i} w 120 h 20 numeric $slider_value enabled $controls_enabled value "调节"
"""
        
    # Find the top-level container (panel gallery-layout row...) and append our imgui stress panel right after the scroll gallery-controls
    # gallery-controls ends before "data_grid asset-grid minw 720..."
    target_pos = content.find("data_grid asset-grid minw 720")
    if target_pos == -1:
        print("Error: Could not find insertion point in nui file!")
        sys.exit(1)
        
    content_with_imgui = content[:target_pos] + imgui_panel + content[target_pos:]
    
    with open("tests/fixtures/ui/imgui-component-gallery-stress.nui", "w", encoding="utf-8") as f:
        f.write(content_with_imgui)
        
    # Also write a copy to the other fixtures path just in case
    with open("crates/neon-ui-runtime/tests/fixtures/ui/imgui-component-gallery-stress.nui", "w", encoding="utf-8") as f:
        f.write(content_with_imgui)
        
    print("imgui-component-gallery-stress.nui generated successfully.")

if __name__ == "__main__":
    generate_stress_test_file()
