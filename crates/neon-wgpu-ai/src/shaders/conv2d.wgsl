// Neon3 generic 2D convolution kernel. Batch is fixed to 1.
// Input  layout: (1, in_c, in_h, in_w)  row-major, f32
// Weight layout: (out_c, in_c, kh, kw)  row-major, f32
// Output layout: (1, out_c, out_h, out_w)
// Each invocation computes 4 output channels for one output pixel.

struct Params {
    in_c: u32,
    out_c: u32,
    in_h: u32,
    in_w: u32,
    kh: u32,
    kw: u32,
    stride: u32,
    pad: u32,
    out_h: u32,
    out_w: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> x: array<f32>;
@group(0) @binding(2) var<storage, read> w: array<f32>;
@group(0) @binding(3) var<storage, read> b: array<f32>;
@group(0) @binding(4) var<storage, read_write> y: array<f32>;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ox = gid.x;
    let oy = gid.y;
    let oc0 = gid.z * 4u;
    if (ox >= p.out_w || oy >= p.out_h) { return; }

    let wch = p.kh * p.kw;
    let in_chw = p.in_h * p.in_w;
    let out_chw = p.out_h * p.out_w;

    var acc = vec4<f32>();
    for (var ic: u32 = 0u; ic < p.in_c; ic++) {
        for (var ky: u32 = 0u; ky < p.kh; ky++) {
            let iy = i32(oy) * i32(p.stride) - i32(p.pad) + i32(ky);
            if (iy < 0 || iy >= i32(p.in_h)) { continue; }
            for (var kx: u32 = 0u; kx < p.kw; kx++) {
                let ix = i32(ox) * i32(p.stride) - i32(p.pad) + i32(kx);
                if (ix < 0 || ix >= i32(p.in_w)) { continue; }
                let xv = x[ic * in_chw + u32(iy) * p.in_w + u32(ix)];
                for (var l: u32 = 0u; l < 4u; l++) {
                    let oc = oc0 + l;
                    if (oc < p.out_c) {
                        acc[l] += w[oc * p.in_c * wch + ic * wch + ky * p.kw + kx] * xv;
                    }
                }
            }
        }
    }

    for (var l: u32 = 0u; l < 4u; l++) {
        let oc = oc0 + l;
        if (oc < p.out_c) {
            y[oc * out_chw + oy * p.out_w + ox] = acc[l] + b[oc];
        }
    }
}