// Neon3 GroupNorm / LayerNorm kernels (3 passes).
// Pass 1: per (group, workgroup) partial sum / sum-of-squares.
// Pass 2: reduce partials to (mean, inv_std) per group.
// Pass 3: normalize + per-channel affine.
// Layout of x/y: (1, c, h, w) row-major; affine weights are per-channel.

struct Params {
    groups: u32,
    c: u32,
    h: u32,
    w: u32,
    eps: f32,
    num_wgs: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x: array<f32>;
@group(0) @binding(2) var<storage, read> gnw: array<f32>;
@group(0) @binding(3) var<storage, read> gnb: array<f32>;
@group(0) @binding(4) var<storage, read_write> partial: array<f32>;
@group(0) @binding(5) var<storage, read_write> stats: array<f32>;
@group(0) @binding(6) var<storage, read_write> y: array<f32>;

var<workgroup> red_s: array<f32, 256>;
var<workgroup> red_ss: array<f32, 256>;

// Dispatch: (1, num_wgs, groups)
@compute @workgroup_size(256)
fn main_reduce(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let group = gid.z;
    let wg = gid.y;
    let hw = p.h * p.w;
    let cpg = p.c / p.groups;
    let total = cpg * hw;
    let span = (total + p.num_wgs - 1u) / p.num_wgs;
    let base = group * cpg * hw;
    var s = 0.0;
    var ss = 0.0;
    var i = min(wg * span, total) + lid.x;
    let end = min(wg * span + span, total);
    while (i < end) {
        let v = x[base + i];
        s += v;
        ss += v * v;
        i += 256u;
    }
    red_s[lid.x] = s;
    red_ss[lid.x] = ss;
    workgroupBarrier();
    var off = 128u;
    while (off > 0u) {
        if (lid.x < off) {
            red_s[lid.x] += red_s[lid.x + off];
            red_ss[lid.x] += red_ss[lid.x + off];
        }
        workgroupBarrier();
        off >>= 1u;
    }
    if (lid.x == 0u) {
        let o = (group * p.num_wgs + wg) * 2u;
        partial[o] = red_s[0];
        partial[o + 1u] = red_ss[0];
    }
}

// Dispatch: (groups, 1, 1)
@compute @workgroup_size(256)
fn main_finalize(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    if (lid.x != 0u) { return; }
    let group = gid.x;
    let hw = p.h * p.w;
    let cpg = p.c / p.groups;
    let total = cpg * hw;
    var s = 0.0;
    var ss = 0.0;
    for (var wg: u32 = 0u; wg < p.num_wgs; wg++) {
        let o = (group * p.num_wgs + wg) * 2u;
        s += partial[o];
        ss += partial[o + 1u];
    }
    let mean = s / f32(total);
    let variance = ss / f32(total) - mean * mean;
    let inv = 1.0 / sqrt(variance + p.eps);
    stats[group * 2u] = mean;
    stats[group * 2u + 1u] = inv;
}

// Dispatch: (c*h*w, 1, 1)
@compute @workgroup_size(256)
fn main_apply(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.c * p.h * p.w) { return; }
    let hw = p.h * p.w;
    let c = i / hw;
    let group = c / (p.c / p.groups);
    let mean = stats[group * 2u];
    let inv = stats[group * 2u + 1u];
    y[i] = (x[i] - mean) * inv * gnw[c] + gnb[c];
}