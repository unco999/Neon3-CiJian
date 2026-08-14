// Neon3 row-wise softmax over a dense scores buffer (in place).
// Rows and columns share the same resolution in attention (HW x HW).
// `scale` pre-scales every entry (attention: 1 / sqrt(head_dim)).

struct Params {
    rows: u32,
    cols: u32,
    off: u32,
    scale: f32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> x: array<f32>;

var<workgroup> smax: array<f32, 256>;
var<workgroup> ssum: array<f32, 256>;

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wgid: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let row = wgid.x + wgid.y * 65535u;
    if (row >= p.rows) { return; }
    let base = p.off + row * p.cols;
    let per = (p.cols + 255u) / 256u;
    var m = -1e30;
    for (var j: u32 = 0u; j < per; j++) {
        let c = j * 256u + lid.x;
        if (c < p.cols) {
            m = max(m, x[base + c] * p.scale);
        }
    }
    smax[lid.x] = m;
    workgroupBarrier();
    var off = 128u;
    while (off > 0u) {
        if (lid.x < off) {
            smax[lid.x] = max(smax[lid.x], smax[lid.x + off]);
        }
        workgroupBarrier();
        off >>= 1u;
    }
    let rmax = smax[0];
    var s = 0.0;
    for (var j: u32 = 0u; j < per; j++) {
        let c = j * 256u + lid.x;
        if (c < p.cols) {
            s += exp(x[base + c] * p.scale - rmax);
        }
    }
    ssum[lid.x] = s;
    workgroupBarrier();
    off = 128u;
    while (off > 0u) {
        if (lid.x < off) {
            ssum[lid.x] += ssum[lid.x + off];
        }
        workgroupBarrier();
        off >>= 1u;
    }
    let rsum = ssum[0];
    for (var j: u32 = 0u; j < per; j++) {
        let c = j * 256u + lid.x;
        if (c < p.cols) {
            x[base + c] = exp(x[base + c] * p.scale - rmax) / rsum;
        }
    }
}
