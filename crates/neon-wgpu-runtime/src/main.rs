//! The sole Neon3 process permitted to own windows and GPU objects.

fn main() {
    let _runtime = neon_wgpu_runtime::WgpuRuntime::headless(1);
}
