// Neon3 tiled matrix multiply kernel.
// C[m,n] = sum_k A[m,k] * B[k,n], all row-major f32 buffers.
// trans_a=1: A is stored as K x M (A[m,k] = a[k*m + m_index... stored[k * M + m]).
// trans_b=1: B is stored as N x K (B[k,n] = b_stored[n * K + k]).
// This covers attention qk^T and (scores @ v^T) by using base offsets per head.

struct Params {
    m: u32,
    n: u32,
    k: u32,
    a_off: u32,
    b_off: u32,
    c_off: u32,
    trans_a: u32,
    trans_b: u32,
};

const TILE: u32 = 16u;

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> c: array<f32>;

var<workgroup> atile: array<f32, 256>;
var<workgroup> btile: array<f32, 256>;

@compute @workgroup_size(TILE, TILE)
fn main(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(workgroup_id) wgid: vec3<u32>,
) {
    let m = p.m;
    let n = p.n;
    let k = p.k;
    let m0 = wgid.y * TILE;
    let n0 = wgid.x * TILE;
    let li = lid.y * TILE + lid.x;
    let ktiles = (k + TILE - 1u) / TILE;
    var acc = 0.0;
    for (var kt: u32 = 0u; kt < ktiles; kt++) {
        let k0 = kt * TILE;
        let am = m0 + lid.y;
        let ak = k0 + lid.x;
        if (am < m && ak < k) {
            if (p.trans_a == 0u) {
                atile[li] = a[p.a_off + am * k + ak];
            } else {
                atile[li] = a[p.a_off + ak * m + am];
            }
        } else {
            atile[li] = 0.0;
        }
        let bn = n0 + lid.x;
        let bk = k0 + lid.y;
        if (bn < n && bk < k) {
            if (p.trans_b == 0u) {
                btile[li] = b[p.b_off + bk * n + bn];
            } else {
                btile[li] = b[p.b_off + bn * k + bk];
            }
        } else {
            btile[li] = 0.0;
        }
        workgroupBarrier();
        for (var t: u32 = 0u; t < TILE; t++) {
            acc += atile[lid.y * TILE + t] * btile[t * TILE + lid.x];
        }
        workgroupBarrier();
    }
    if (m0 + lid.y < m && n0 + lid.x < n) {
        c[p.c_off + (m0 + lid.y) * n + n0 + lid.x] = acc;
    }
}
