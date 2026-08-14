// Neon3 generic 2D convolution kernel. Batch is fixed to 1.
// Input  layout: (1, in_c, in_h, in_w)  row-major, f32
// Weight layout: (out_c, in_c, kh, kw)  row-major, f32
// Output layout: (1, out_c, out_h, out_w)
// Each 8x8 workgroup computes one output tile for 4 output channels. Input
// patches and weights are loaded once per 8-channel block into workgroup memory.

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

const TILE: u32 = 8u;
const IC_BLOCK: u32 = 8u;
const OC_BLOCK: u32 = 4u;
var<workgroup> input_tile: array<f32, 800>;
var<workgroup> weights: array<f32, 288>;

@compute @workgroup_size(8, 8, 1)
fn main(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wgid: vec3<u32>,
) {
    let ox = wgid.x * TILE + lid.x;
    let oy = wgid.y * TILE + lid.y;
    let oc0 = wgid.z * OC_BLOCK;
    let thread = lid.y * TILE + lid.x;
    let wch = p.kh * p.kw;
    let in_chw = p.in_h * p.in_w;
    let out_chw = p.out_h * p.out_w;
    let patch_h = TILE + p.kh - 1u;
    let patch_w = TILE + p.kw - 1u;
    let patch_hw = patch_h * patch_w;
    var acc = vec4<f32>();

    for (var ic0: u32 = 0u; ic0 < p.in_c; ic0 += IC_BLOCK) {
        let patch_values = IC_BLOCK * patch_hw;
        for (var index = thread; index < patch_values; index += TILE * TILE) {
            let block_ic = index / patch_hw;
            let spatial = index % patch_hw;
            let py = spatial / patch_w;
            let px = spatial % patch_w;
            let ic = ic0 + block_ic;
            let iy = i32(wgid.y * TILE + py) - i32(p.pad);
            let ix = i32(wgid.x * TILE + px) - i32(p.pad);
            var value = 0.0;
            if (ic < p.in_c && iy >= 0 && iy < i32(p.in_h) && ix >= 0 && ix < i32(p.in_w)) {
                value = x[ic * in_chw + u32(iy) * p.in_w + u32(ix)];
            }
            input_tile[index] = value;
        }

        let weight_values = IC_BLOCK * OC_BLOCK * wch;
        for (var index = thread; index < weight_values; index += TILE * TILE) {
            let kernel_index = index % wch;
            let lane = (index / wch) % OC_BLOCK;
            let block_ic = index / (OC_BLOCK * wch);
            let ic = ic0 + block_ic;
            let oc = oc0 + lane;
            var value = 0.0;
            if (ic < p.in_c && oc < p.out_c) {
                value = w[oc * p.in_c * wch + ic * wch + kernel_index];
            }
            weights[index] = value;
        }
        workgroupBarrier();

        if (ox < p.out_w && oy < p.out_h) {
            for (var block_ic: u32 = 0u; block_ic < IC_BLOCK; block_ic++) {
                if (ic0 + block_ic >= p.in_c) { break; }
                for (var ky: u32 = 0u; ky < p.kh; ky++) {
                    for (var kx: u32 = 0u; kx < p.kw; kx++) {
                        let kernel_index = ky * p.kw + kx;
                        let xv = input_tile[block_ic * patch_hw + (lid.y + ky) * patch_w + lid.x + kx];
                        for (var lane: u32 = 0u; lane < OC_BLOCK; lane++) {
                            acc[lane] += weights[(block_ic * OC_BLOCK + lane) * wch + kernel_index] * xv;
                        }
                    }
                }
            }
        }
        workgroupBarrier();
    }

    if (ox < p.out_w && oy < p.out_h) {
        for (var lane: u32 = 0u; lane < OC_BLOCK; lane++) {
            let oc = oc0 + lane;
            if (oc < p.out_c) {
                y[oc * out_chw + oy * p.out_w + ox] = acc[lane] + b[oc];
            }
        }
    }
}
