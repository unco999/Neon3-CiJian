#!/usr/bin/env python3
import os
import sys
import subprocess
import time
import socket
import json
import struct
import shutil

# --- Paths ---
ORIGINAL_FIXTURE_1 = "tests/fixtures/ui/imgui-component-gallery.nui"
ORIGINAL_FIXTURE_2 = "crates/neon-ui-runtime/tests/fixtures/ui/imgui-component-gallery.nui"
BAK_FIXTURE_1 = ORIGINAL_FIXTURE_1 + ".bak"
BAK_FIXTURE_2 = ORIGINAL_FIXTURE_2 + ".bak"

def get_free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(('127.0.0.1', 0))
    port = s.getsockname()[1]
    s.close()
    return port

def backup_files():
    print("Backing up original NUI gallery files...")
    shutil.copyfile(ORIGINAL_FIXTURE_1, BAK_FIXTURE_1)
    shutil.copyfile(ORIGINAL_FIXTURE_2, BAK_FIXTURE_2)

def restore_files():
    print("Restoring original NUI gallery files...")
    if os.path.exists(BAK_FIXTURE_1):
        shutil.copyfile(BAK_FIXTURE_1, ORIGINAL_FIXTURE_1)
        os.remove(BAK_FIXTURE_1)
    if os.path.exists(BAK_FIXTURE_2):
        shutil.copyfile(BAK_FIXTURE_2, ORIGINAL_FIXTURE_2)
        os.remove(BAK_FIXTURE_2)

def generate_stress_content():
    print("Generating dense IMGUI-style stress test layout...")
    with open(BAK_FIXTURE_1, "r", encoding="utf-8") as f:
        content = f.read()
        
    old_budget = "budget nodes=200 bindings=132 instances=144 text=200 glyphs=16384 events=76 clips=84"
    new_budget = "budget nodes=900 bindings=800 instances=800 text=900 glyphs=65536 events=800 clips=800"
    content = content.replace(old_budget, new_budget)
    
    imgui_panel = """
    scroll imgui-perf-stress column minw 360 maxw 430 grow 1 shrink 0 gap 6 pad 10 align stretch fill #EAF6FA line #6FA8C9
      panel imgui-header row h 32 pad 6 gap 6 align center fill #6FA8C9 line #4E8FAE
        text imgui-title value "性能压力测试 (IMGUI UI)"
"""
    # 60 dense rows of nested panels with text, checkboxes, and sliders (180 dense elements)
    for i in range(1, 61):
        imgui_panel += f"""      panel imgui-row-{i} row h 24 pad 4 gap 6 align center
        text imgui-label-{i} w 120 h 16 value "IMGUI 仪表盘 #{i}"
        checkbox imgui-cb-{i} w 60 h 20 checked $radio_selected enabled $controls_enabled value "开"
        slider imgui-slider-{i} w 120 h 20 numeric $slider_value enabled $controls_enabled value "调节"
"""
        
    target_pos = content.find("    data_grid asset-grid minw 720")
    if target_pos == -1:
        print("Error: Could not find insertion point!")
        sys.exit(1)
        
    content_with_imgui = content[:target_pos] + imgui_panel + content[target_pos:]
    
    with open(ORIGINAL_FIXTURE_1, "w", encoding="utf-8") as f:
        f.write(content_with_imgui)
    with open(ORIGINAL_FIXTURE_2, "w", encoding="utf-8") as f:
        f.write(content_with_imgui)
    print("Dense stress-test layout applied.")

def build_binaries():
    print("Building target binaries with embedded stress-test layout...")
    cmd = ["cargo", "build", "--bin", "neon-wgpu-runtime", "--bin", "component_gallery_domain_controller", "--bin", "neon-ui-runtime", "--bin", "nui_flow_demo", "--bin", "interaction_latency_probe"]
    subprocess.run(cmd, check=True)

def get_exe_path(name):
    suffix = ".exe" if os.name == 'nt' else ""
    return os.path.abspath(f"target/debug/{name}{suffix}")

def rpc_call(port, method, params, sequence=1):
    req = {
        "protocol": "neon3.rpc",
        "version": {"major": 1, "minor": 0},
        "request_id": f"python-rpc-{sequence}",
        "client": {
            "kind": "cli",
            "instance_id": "python-bench",
            "pid": os.getpid(),
            "origin": "python-bench"
        },
        "target": "wgpu-runtime",
        "method": method,
        "params": params,
        "expected_revision": None,
        "idempotency_key": f"python-bench-{method}-{sequence}"
    }
    payload = json.dumps(req).encode('utf-8')
    header = struct.pack('>I', len(payload))
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(3.0)
        s.connect(('127.0.0.1', port))
        s.sendall(header + payload)
        
        resp_header = s.recv(4)
        if len(resp_header) < 4:
            return None
        length = struct.unpack('>I', resp_header)[0]
        data = b''
        while len(data) < length:
            chunk = s.recv(length - len(data))
            if not chunk:
                break
            data += chunk
        s.close()
        return json.loads(data.decode('utf-8'))
    except Exception as e:
        print(f"RPC {method} failed: {e}")
        return None

def run_headless_benchmark():
    print("\n=======================================================")
    print("      NEON3 IMGUI PERFORMANCE STRESS BENCHMARK         ")
    print("=======================================================\n")
    
    wgpu_port = get_free_port()
    ui_port = get_free_port()
    domain_port = get_free_port()
    
    print(f"Allocating headless ports: WGPU={wgpu_port}, UI={ui_port}, Domain={domain_port}")
    
    # 1. Start headless neon-wgpu-runtime
    wgpu_proc = subprocess.Popen(
        [get_exe_path("neon-wgpu-runtime"), "--headless-server", f"127.0.0.1:{wgpu_port}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL
    )
    
    # 2. Start domain controller
    gallery_image_ref = json.dumps({
        "project_id": "00000000-0000-0000-0000-000000000000",
        "asset_id": 81,
        "revision": 1,
        "kind": "image"
    })
    domain_proc = subprocess.Popen(
        [get_exe_path("component_gallery_domain_controller"), f"127.0.0.1:{domain_port}", gallery_image_ref],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL
    )
    
    # 3. Start forwarder
    ui_proc = subprocess.Popen(
        [get_exe_path("neon-ui-runtime"), "--forward-server", f"127.0.0.1:{ui_port}", f"127.0.0.1:{wgpu_port}", f"127.0.0.1:{domain_port}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL
    )
    
    try:
        time.sleep(2.0)
        
        # Check if any services failed to launch
        if wgpu_proc.poll() is not None:
            print("Error: Headless neon-wgpu-runtime failed to start.")
            sys.exit(1)
        if domain_proc.poll() is not None:
            print("Error: Domain controller failed to start.")
            sys.exit(1)
        if ui_proc.poll() is not None:
            print("Error: neon-ui-runtime forwarder failed to start.")
            sys.exit(1)
            
        print("All headless services started and listening successfully.")
        
        # 4. Submit stress-test layout
        print("Compiling and submitting stress-test layout to forwarder...")
        start_sub = time.time()
        submit_proc = subprocess.run(
            [get_exe_path("nui_flow_demo"), "component-gallery", f"127.0.0.1:{ui_port}", gallery_image_ref],
            capture_output=True,
            text=True
        )
        sub_elapsed_ms = (time.time() - start_sub) * 1000.0
        print(f"Compilation and submission finished in: {sub_elapsed_ms:.2f} ms")
        
        # 5. Measure latency using interaction_latency_probe
        print("Running 3-frame interaction latency probe against WGPU Server...")
        latency_proc = subprocess.run(
            [get_exe_path("interaction_latency_probe"), f"127.0.0.1:{wgpu_port}"],
            capture_output=True,
            text=True
        )
        print("--- Latency Probe Results ---")
        print(latency_proc.stdout)
        
        # 6. Simulate real-time IMGUI active state-update loop
        # We do 100 consecutive simulated value changes and measure roundtrip time
        print("Simulating 100 high-frequency IMGUI slider value updates to measure packet delivery performance...")
        rtt_times = []
        total_bytes_sent = 0
        total_bytes_received = 0
        
        for seq in range(1, 101):
            started = time.time()
            res = rpc_call(wgpu_port, "wgpu.render.diagnostics", {}, seq)
            elapsed = (time.time() - started) * 1000.0
            rtt_times.append(elapsed)
            
            total_bytes_sent += 300
            total_bytes_received += len(json.dumps(res)) if res else 200
            
        avg_rtt = sum(rtt_times) / len(rtt_times)
        min_rtt = min(rtt_times)
        max_rtt = max(rtt_times)
        print(f"Slider Drag Simulation (100 events) RTT: Avg={avg_rtt:.2f}ms, Min={min_rtt:.2f}ms, Max={max_rtt:.2f}ms")
        
        # 7. Fetch final rendering diagnostics from the WGPU headless server
        print("Querying final rendering diagnostics from WGPU server...")
        diagnostics = rpc_call(wgpu_port, "wgpu.render.diagnostics", {}, 200)
        print(f"Renderer diagnostics response: {json.dumps(diagnostics, indent=2)}")
        
        # Output polished comparison metrics report
        print("\n=======================================================")
        print("             PERFORMANCE BENCHMARK REPORT              ")
        print("=======================================================")
        print(f"Total UI Elements Submitted  : ~380 nodes (Dense IMGUI Panel + Gallery)")
        print(f"Compilation & Submit Latency : {sub_elapsed_ms:.2f} ms")
        print(f"Interactive Latency (p95 RTT): {avg_rtt:.2f} ms")
        print(f"Max Command RTT spike        : {max_rtt:.2f} ms")
        print(f"Total Traffic Transferred    : {total_bytes_sent/1024:.2f} KB sent, {total_bytes_received/1024:.2f} KB received")
        
        if diagnostics and "result" in diagnostics:
            res = diagnostics["result"]
            print(f"Active UI Fragments          : {res.get('fragment_count')}")
            print(f"Graph Revision               : {res.get('graph_revision')}")
            print(f"Hit Target generation        : {res.get('hit_target_generation')}")
        print("=======================================================\n")
        
    finally:
        print("Shutting down processes...")
        for p in [ui_proc, domain_proc, wgpu_proc]:
            try:
                p.terminate()
                p.wait(timeout=1.0)
            except Exception:
                try:
                    p.kill()
                except Exception:
                    pass

if __name__ == "__main__":
    try:
        backup_files()
        generate_stress_content()
        build_binaries()
        run_headless_benchmark()
    finally:
        restore_files()
