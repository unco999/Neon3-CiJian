#!/usr/bin/env python3
import sys
import subprocess
import time
import socket
import json

def get_free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(('127.0.0.1', 0))
    port = s.getsockname()[1]
    s.close()
    return port

def run_performance_test():
    print("=== Neon3 UI IMGUI Component Performance Benchmark ===")
    
    wgpu_port = get_free_port()
    ui_port = get_free_port()
    domain_port = get_free_port()
    print(f"Allocated ports: wgpu_port={wgpu_port}, ui_port={ui_port}, domain_port={domain_port}")
    
    # 1. Start the wgpu-runtime in window server mode
    # Window server mode launches the actual wgpu window loop and spawns an RPC listener
    print("Launching neon-wgpu-runtime in --window-server mode...")
    wgpu_proc = subprocess.Popen(
        ["cargo", "run", "--bin", "neon-wgpu-runtime", "--", "--window-server", f"127.0.0.1:{wgpu_port}"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )
    
    time.sleep(3.0)
    if wgpu_proc.poll() is not None:
        print("Failed to start neon-wgpu-runtime:")
        print(wgpu_proc.stderr.read())
        sys.exit(1)
        
    print("neon-wgpu-runtime is running.")
    
    # 2. Start the domain controller for imgui-component-gallery
    # This serves as the backend logic provider for UI inputs, statecharts, and grid elements.
    print("Launching component gallery domain controller...")
    gallery_image_ref = json.dumps({
        "project_id": "00000000-0000-0000-0000-000000000000",
        "asset_id": 81,
        "revision": 1,
        "kind": "gallery-image"
    })
    domain_proc = subprocess.Popen(
        ["cargo", "run", "--bin", "component_gallery_domain_controller", "--", f"127.0.0.1:{domain_port}", gallery_image_ref],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )
    
    time.sleep(2.0)
    if domain_proc.poll() is not None:
        print("Failed to start domain controller:")
        print(domain_proc.stderr.read())
        wgpu_proc.terminate()
        sys.exit(1)
        
    print("component-gallery domain controller is running.")
    
    # 3. Launch the neon-ui-runtime in forwarder mode
    # Forwarder bridges the wgpu renderer and the domain controller
    # Args format: --forward-server <listener-endpoint> <wgpu-endpoint> <domain-endpoint> [eventd-endpoint]
    print("Launching neon-ui-runtime in --forward-server mode...")
    ui_proc = subprocess.Popen(
        ["cargo", "run", "--bin", "neon-ui-runtime", "--", "--forward-server", f"127.0.0.1:{ui_port}", f"127.0.0.1:{wgpu_port}", f"127.0.0.1:{domain_port}"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )
    
    time.sleep(2.0)
    if ui_proc.poll() is not None:
        print("Failed to start neon-ui-runtime:")
        print(ui_proc.stderr.read())
        domain_proc.terminate()
        wgpu_proc.terminate()
        sys.exit(1)
        
    print("neon-ui-runtime forwarder is running.")
    
    # 4. Submit the component-gallery flow schema to the active forwarder to build initial UI topology on WGPU Renderer
    print("Running nui-flow-demo to compile and submit the Component Gallery Fragment...")
    submit_proc = subprocess.run(
        ["cargo", "run", "--bin", "nui-flow-demo", "--", "component-gallery", f"127.0.0.1:{ui_port}", gallery_image_ref],
        capture_output=True,
        text=True
    )
    print("--- Submission Output ---")
    print(submit_proc.stdout)
    if submit_proc.returncode != 0:
        print(f"Error submitting fragment: {submit_proc.stderr}")
    
    # 5. Measure latency and performance metrics under full layout load
    print("Starting interaction latency measurement probe...")
    latency_proc = subprocess.run(
        ["cargo", "run", "--bin", "interaction_latency_probe", "--", f"127.0.0.1:{wgpu_port}"],
        capture_output=True,
        text=True
    )
    print("--- Latency Metrics (JSONL) ---")
    print(latency_proc.stdout)
    
    # Give some running time to accumulate frames & perform diagnostics
    time.sleep(3.0)
    
    # Clean up all spawned subprocesses cleanly
    print("Cleaning up services...")
    ui_proc.terminate()
    domain_proc.terminate()
    wgpu_proc.terminate()
    
    for p in [ui_proc, domain_proc, wgpu_proc]:
        try:
            p.wait(timeout=2)
        except subprocess.TimeoutExpired:
            p.kill()
            
    print("=== IMGUI Component Performance Benchmark Completed ===")

if __name__ == "__main__":
    run_performance_test()
