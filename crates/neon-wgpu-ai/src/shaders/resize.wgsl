// Neon3 spatial resize and channel concat kernels.
// avgpool: 2x2 stride-2. upsample: nearest 2x. concat: along channels.

struct Params {
    c: u32,
    h: u32,
    w: u32,
    c2: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> y: array<f32>;

// Dispatch over output elements (c * oh * ow). Input is (c, h, w), output (c, h/2, w/2).
@compute @workgroup_size(256)
fn main_avgpool(@builtin(global_invocation_id) gid: vec3<u32>) {
    let oh = p.h / 2u;
    let ow = p.w / 2u;
    let i = gid.x;
    let total = p.c * oh * ow;
    if (i >= total) { return; }
    let c = i / (oh * ow);
    let r = i % (oh * ow);
    let oy = r / ow;
    let ox = r % ow;
    let base = c * p.h * p.w + oy * 2u * p.w + ox * 2u;
    y[i] = (a[base] + a[base + 1u] + a[base + p.w] + a[base + p.w + 1u]) * 0.25;
}

// Dispatch over output elements (c * 2h * 2w). Input is (c, h, w).
@compute @workgroup_size(256)
fn main_upsample(@builtin(global_invocation_id) gid: vec3<u32>) {
    let hw = p.h * p.w;
    let i = gid.x;
    if (i >= p.c * hw * 4u) { return; }
    let c = i / (hw * 4u);
    let r = i % (hw * 4u);
    let oy = r / (p.w * 2u);
    let ox = r % (p.w * 2u);
    y[i] = a[c * hw + (oy / 2u) * p.w + (ox / 2u)];
}

// Dispatch over output elements ((c + c2) * hw). Channels of a first, then b.
// Height and width of both inputs are identical; hw = h * w.
@compute @workgroup_size(256)
fn main_concat(@builtin(global_invocation_id) gid: vec3<u32>) {
    let hw = p.h * p.w;
    let i = gid.x;
    if (i >= (p.c + p.c2) * hw) { return; }
    let c = i / hw;
    let r = i % hw;
    if (c < p.c) {
        y[i] = a[c * hw + r];
    } else {
        y[i] = b[(c - p.c) * hw + r];
    }
}