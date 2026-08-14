// Neon3 sinusoidal time embedding. Matches torch's positional encoding:
// emb = [sin(half); cos(half)], h = t * exp(-log(10000) * k / (dim/2)).
// Single storage binding (1, read_write); binding 0 is the uniform.

struct Params {
    t: u32,
    dim: u32,
    row_len: u32,
    table_count: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(256)
fn main_timefreq(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x + gid.y * 16776960u;
    if (i >= p.dim) { return; }
    let half = p.dim / 2u;
    let k = f32(i % half);
    let h = f32(p.t) * exp(-log(10000.0) * k / f32(half));
    if (i < half) {
        out[i] = sin(h);
    } else {
        out[i] = cos(h);
    }
}
