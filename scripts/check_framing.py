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

def make_request(method, params, sequence):
    return {
        "protocol": "neon3.rpc",
        "version": {"major": 1, "minor": 0},
        "request_id": f"benchmark-{sequence}",
        "client": {
            "kind": "cli",
            "instance_id": "benchmark-runner",
            "pid": 1234,
            "origin": "benchmark-runner"
        },
        "target": "wgpu-runtime",
        "method": method,
        "params": params,
        "expected_revision": None,
        "idempotency_key": f"benchmark-{method}-{sequence}"
    }

def send_rpc(port, method, params, sequence):
    req_data = (json.dumps(make_request(method, params, sequence)) + "\n").encode('utf-8')
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2.0)
        s.connect(('127.0.0.1', port))
        
        # Write length prefix (or plain newline-terminated depending on neon3 protocol)
        # As per crates/neon-ipc/src/lib.rs, our TCP server uses newline-terminated or length-prefixed JSON.
        # Let's write the length prefix if length-prefixed JSON, but wait, length-prefixed is common.
        # Let's check how the protocol framing is implemented.
        # Let's inspect crates/neon-ipc/src/lib.rs if needed, or simply send:
        # In length prefixed transport: 4 bytes length, then data.
        # Let's write both or check. Actually, since neon_ipc has framed transport, let's write 4 bytes big-endian length.
        length = len(req_data) - 1 # without the newline if it was appended
        # Let's check neon-ipc framing first to be 100% correct.
    except Exception as e:
        print(f"RPC connection failed: {e}")
        return None

if __name__ == "__main__":
    print("Pre-checking neon-ipc transport framing...")
