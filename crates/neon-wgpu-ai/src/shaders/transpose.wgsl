// Neon3 transpose: x is (K, N) row-major, y is (N, K) row-major, both with a
// flat element offset (per-head attention slices).
// y[out_off + n * K + k] = x[in_off + k * N + n]

struct Params {
    len: u32,
    k: u32,
    n: u32,
    in_off: u32,
    out_off: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x: array<f32>;
@group(0) @binding(2) var<storage, read_write> y: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.len) { return; }
    let n = i / p.k;
    let k = i % p.k;
    y[p.out_off + i] = x[p.in_off + k * p.n + n];
}