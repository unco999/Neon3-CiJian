// Neon3 elementwise inference kernels.
// Layout: uniform(0), in0(1), in1(2), in2(3), out(4).
// Used for: silu, add, mul, CFG combine, fused DDIM step, FiLM scale+bias.

struct Params {
    len: u32,
    c: u32,
    a: f32,
    b: f32,
    d: f32,
    pad: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x0: array<f32>;
@group(0) @binding(2) var<storage, read> x1: array<f32>;
@group(0) @binding(3) var<storage, read> x2: array<f32>;
@group(0) @binding(4) var<storage, read_write> y: array<f32>;

@compute @workgroup_size(256)
fn main_silu(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.len) { return; }
    let v = x0[i];
    y[i] = v / (1.0 + exp(-v));
}

@compute @workgroup_size(256)
fn main_add(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.len) { return; }
    y[i] = x0[i] + x1[i];
}

@compute @workgroup_size(256)
fn main_mul(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.len) { return; }
    y[i] = x0[i] * x1[i];
}

// CFG combine: y = x0 + a * (x1 - x0), guidance in p.a.
@compute @workgroup_size(256)
fn main_cfg(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.len) { return; }
    y[i] = x0[i] + p.a * (x1[i] - x0[i]);
}

// Fused DDIM step: y = p.d * clamp((x0 - p.b * x1) / p.c, -3.0, 3.0) + p.a * x1
// where p.a = sqrt(alpha_bar[t0]), p.b = sqrt(1 - alpha_bar[t]), p.c = sqrt(alpha_bar[t]),
// p.d = sqrt(1 - alpha_bar[t0]).
@compute @workgroup_size(256)
fn main_ddim(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.len) { return; }
    let x = x0[i];
    let e = x1[i];
    let x0h = clamp((x - p.b * e) / p.c, -3.0, 3.0);
    y[i] = p.d * x0h + p.a * e;
}

// FiLM: y = x0 * (1 + x1[i % c]) + x2[i % c]
@compute @workgroup_size(256)
fn main_film(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.len) { return; }
    let s = x1[i % p.c];
    let b = x2[i % p.c];
    y[i] = x0[i] * (1.0 + s) + b;
}
