// Neon3 conditioning kernels: sinusoidal time embedding and embedding-table gather.
// Time embedding matches torch's positional encoding: emb = [sin(half); cos(half)].
// Gather sums rows `indices[t]` from `tables` at row base `bases[t]`.
// Both entries share one bind group layout: uniform(0), tables/y(1), indices(2),
// bases(3), out(4). The timefreq entry ignores bindings 1-3 (dummy bindings).

struct Params {
    t: u32,
    dim: u32,
    row_len: u32,
    table_count: u32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> tables: array<f32>;
@group(0) @binding(2) var<storage, read> indices: array<u32>;
@group(0) @binding(3) var<storage, read> bases: array<u32>;
@group(0) @binding(4) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(256)
fn main_timefreq(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
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

@compute @workgroup_size(256)
fn main_gather(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= p.row_len) { return; }
    var acc = 0.0;
    for (var t: u32 = 0u; t < p.table_count; t++) {
        let idx = indices[t];
        let base = bases[t] + idx * p.row_len;
        acc += tables[base + i];
    }
    out[i] = acc;
}