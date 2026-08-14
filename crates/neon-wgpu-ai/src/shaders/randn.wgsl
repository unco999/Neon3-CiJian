// Neon3 deterministic Gaussian noise generation (PCG32 + Box-Muller).
// value(i) is a pure function of (seed, i), so results are reproducible.

struct Params {
    len: u32,
    seed: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> y: array<f32>;

fn pcg_step(state: u32) -> u32 {
    let old = state;
    let s2 = old * 747796405u + 2891336453u;
    let word = ((old >> ((old >> 28u) + 4u)) ^ old) * 277803737u;
    return (word >> 22u) ^ word;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.len) { return; }
    let s0 = (p.seed ^ (i * 2654435761u)) | 1u;
    let u1 = (f32(pcg_step(s0)) + 0.5) / 4294967296.0;
    let u2 = (f32(pcg_step(pcg_step(s0))) + 0.5) / 4294967296.0;
    let r = sqrt(-2.0 * log(max(u1, 1e-12))) * cos(6.28318530718 * u2);
    y[i] = r;
}