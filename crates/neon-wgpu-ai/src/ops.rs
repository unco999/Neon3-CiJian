//! Kernel dispatch wrappers. Each op acquires its output from the arena and
//! returns a pooled `Buf`; the caller keeps outputs alive as long as needed.

use crate::gpu::{Buf, GpuCtx};
use crate::AiError;

const GROUPNORM_WGS: u32 = 32;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ElemParams {
    len: u32,
    c: u32,
    a: f32,
    b: f32,
    d: f32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ConvParams {
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
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NormParams {
    groups: u32,
    c: u32,
    h: u32,
    w: u32,
    eps: f32,
    num_wgs: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResizeParams {
    c: u32,
    h: u32,
    w: u32,
    c2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MatmulParams {
    m: u32,
    n: u32,
    k: u32,
    a_off: u32,
    b_off: u32,
    c_off: u32,
    trans_a: u32,
    trans_b: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SoftmaxParams {
    rows: u32,
    cols: u32,
    off: u32,
    scale: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RandnParams {
    len: u32,
    seed: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CondParams {
    t: u32,
    dim: u32,
    row_len: u32,
    table_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransposeParams {
    len: u32,
    k: u32,
    n: u32,
    in_off: u32,
    out_off: u32,
}

fn wgs(len: u64) -> u32 {
    len.div_ceil(256).max(1) as u32
}

/// y = silu(x)
pub fn silu(ctx: &mut GpuCtx, x: &wgpu::Buffer, len: u64) -> Buf {
    let y = ctx.scratch(len * 4);
    let uniform = ctx.uniform(&ElemParams {
        len: len as u32,
        c: 0,
        a: 0.0,
        b: 0.0,
        d: 0.0,
        pad: 0,
    });
    ctx.run("silu", &uniform, &[x, x, x, &y.buffer], [wgs(len), 1, 1]);
    y
}

/// y = a + b
pub fn add(ctx: &mut GpuCtx, a: &wgpu::Buffer, b: &wgpu::Buffer, len: u64) -> Buf {
    let y = ctx.scratch(len * 4);
    let uniform = ctx.uniform(&ElemParams {
        len: len as u32,
        c: 0,
        a: 0.0,
        b: 0.0,
        d: 0.0,
        pad: 0,
    });
    ctx.run("add", &uniform, &[a, b, a, &y.buffer], [wgs(len), 1, 1]);
    y
}

/// y = a * b
pub fn mul(ctx: &mut GpuCtx, a: &wgpu::Buffer, b: &wgpu::Buffer, len: u64) -> Buf {
    let y = ctx.scratch(len * 4);
    let uniform = ctx.uniform(&ElemParams {
        len: len as u32,
        c: 0,
        a: 0.0,
        b: 0.0,
        d: 0.0,
        pad: 0,
    });
    ctx.run("mul", &uniform, &[a, b, a, &y.buffer], [wgs(len), 1, 1]);
    y
}

/// CFG: y = eu + guidance * (ec - eu)
pub fn cfg_combine(
    ctx: &mut GpuCtx,
    ec: &wgpu::Buffer,
    eu: &wgpu::Buffer,
    guidance: f32,
    len: u64,
) -> Buf {
    let y = ctx.scratch(len * 4);
    let uniform = ctx.uniform(&ElemParams {
        len: len as u32,
        c: 0,
        a: guidance,
        b: 0.0,
        d: 0.0,
        pad: 0,
    });
    ctx.run("cfg", &uniform, &[eu, ec, eu, &y.buffer], [wgs(len), 1, 1]);
    y
}

/// Fused DDIM step: y = sab_t0 * clamp((x - s1ab_t * e) / sab_t, -3, 3) + s1ab_t0 * e
pub fn ddim_step(
    ctx: &mut GpuCtx,
    x: &wgpu::Buffer,
    e: &wgpu::Buffer,
    sab_t: f32,
    s1ab_t: f32,
    sab_t0: f32,
    s1ab_t0: f32,
    len: u64,
) -> Buf {
    let y = ctx.scratch(len * 4);
    let uniform = ctx.uniform(&ElemParams {
        len: len as u32,
        c: sab_t.to_bits(),
        a: sab_t0,
        b: s1ab_t,
        d: s1ab_t0,
        pad: 0,
    });
    ctx.run("ddim", &uniform, &[x, e, x, &y.buffer], [wgs(len), 1, 1]);
    y
}

/// FiLM: y = h * (1 + s) + b where film holds 2c parameters (scale half, then bias half).
pub fn film(ctx: &mut GpuCtx, h: &wgpu::Buffer, film_buf: &wgpu::Buffer, c: u32, len: u64) -> Buf {
    let y = ctx.scratch(len * 4);
    let uniform = ctx.uniform(&ElemParams {
        len: len as u32,
        c,
        a: 0.0,
        b: 0.0,
        d: 0.0,
        pad: 0,
    });
    ctx.run("film", &uniform, &[h, film_buf, film_buf, &y.buffer], [wgs(len), 1, 1]);
    y
}

/// 2D convolution with bias, batch fixed at 1. Input (1, in_c, in_h, in_w),
/// weight (out_c, in_c, kh, kw), output (1, out_c, out_h, out_w).
pub fn conv2d(
    ctx: &mut GpuCtx,
    x: &wgpu::Buffer,
    weight: &wgpu::Buffer,
    bias: &wgpu::Buffer,
    in_c: u32,
    out_c: u32,
    in_h: u32,
    in_w: u32,
    kh: u32,
    kw: u32,
    stride: u32,
    pad: u32,
) -> Buf {
    let out_h = (in_h + 2 * pad - kh) / stride + 1;
    let out_w = (in_w + 2 * pad - kw) / stride + 1;
    let y = ctx.scratch(out_c as u64 * out_h as u64 * out_w as u64 * 4);
    let uniform = ctx.uniform(&ConvParams {
        in_c,
        out_c,
        in_h,
        in_w,
        kh,
        kw,
        stride,
        pad,
        out_h,
        out_w,
    });
    ctx.run(
        "conv2d",
        &uniform,
        &[x, weight, bias, &y.buffer],
        [out_w.div_ceil(8), out_h.div_ceil(8), out_c.div_ceil(4)],
    );
    y
}

/// GroupNorm with per-channel affine, 3 passes.
pub fn group_norm(
    ctx: &mut GpuCtx,
    x: &wgpu::Buffer,
    w: &wgpu::Buffer,
    b: &wgpu::Buffer,
    groups: u32,
    c: u32,
    h: u32,
    w_: u32,
    eps: f32,
    tag: &str,
) -> Result<Buf, AiError> {
    if c % groups != 0 {
        return Err(AiError::Shape(format!(
            "group norm channels {c} must be divisible by {groups}"
        )));
    }
    let len = c as u64 * h as u64 * w_ as u64;
    let partial = ctx.scratch(groups as u64 * GROUPNORM_WGS as u64 * 2 * 4);
    let stats = ctx.scratch(groups as u64 * 2 * 4);
    let y = ctx.scratch(len * 4);
    let uniform = ctx.uniform(&NormParams {
        groups,
        c,
        h,
        w: w_,
        eps,
        num_wgs: GROUPNORM_WGS,
    });
    ctx.run(
        "gn_reduce",
        &uniform,
        &[x, w, b, &partial.buffer, &stats.buffer, &y.buffer],
        [1, GROUPNORM_WGS, groups],
    );
    ctx.run(
        "gn_finalize",
        &uniform,
        &[x, w, b, &partial.buffer, &stats.buffer, &y.buffer],
        [groups, 1, 1],
    );
    if std::env::var("NEON_AI_DIAG").is_ok() {
        let sv = ctx.readback_f32(&stats.buffer, groups as usize * 2);
        if let Ok(v) = sv {
            let (mut mlo, mut mhi) = (f32::INFINITY, f32::NEG_INFINITY);
            let (mut ilo, mut ihi) = (f32::INFINITY, f32::NEG_INFINITY);
            for g in 0..groups as usize {
                mlo = mlo.min(v[g * 2]);
                mhi = mhi.max(v[g * 2]);
                ilo = ilo.min(v[g * 2 + 1]);
                ihi = ihi.max(v[g * 2 + 1]);
            }
            println!("diag {tag} stats: mean [{mlo}, {mhi}] inv [{ilo}, {ihi}]");
        }
    }
    ctx.run(
        "gn_apply",
        &uniform,
        &[x, w, b, &partial.buffer, &stats.buffer, &y.buffer],
        [wgs(len), 1, 1],
    );
    Ok(y)
}

/// LayerNorm over a flat vector (groups = 1).
pub fn layer_norm(
    ctx: &mut GpuCtx,
    x: &wgpu::Buffer,
    w: &wgpu::Buffer,
    b: &wgpu::Buffer,
    len: u32,
    eps: f32,
) -> Result<Buf, AiError> {
    group_norm(ctx, x, w, b, 1, len, 1, 1, eps, "ln")
}

/// 2x2 average pool with stride 2.
pub fn avg_pool2d(ctx: &mut GpuCtx, x: &wgpu::Buffer, c: u32, in_h: u32, in_w: u32) -> Buf {
    let out_h = in_h / 2;
    let out_w = in_w / 2;
    let y = ctx.scratch(c as u64 * out_h as u64 * out_w as u64 * 4);
    let uniform = ctx.uniform(&ResizeParams { c, h: in_h, w: in_w, c2: 0 });
    ctx.run(
        "avgpool",
        &uniform,
        &[x, x, &y.buffer],
        [wgs(c as u64 * out_h as u64 * out_w as u64), 1, 1],
    );
    y
}

/// Nearest-neighbor 2x upsample.
pub fn upsample2x(ctx: &mut GpuCtx, x: &wgpu::Buffer, c: u32, in_h: u32, in_w: u32) -> Buf {
    let y = ctx.scratch(c as u64 * in_h as u64 * in_w as u64 * 4 * 4);
    let uniform = ctx.uniform(&ResizeParams { c, h: in_h, w: in_w, c2: 0 });
    ctx.run(
        "upsample",
        &uniform,
        &[x, x, &y.buffer],
        [wgs(c as u64 * in_h as u64 * in_w as u64 * 4), 1, 1],
    );
    y
}

/// Channel concat of (1, c1, h, w) and (1, c2, h, w); hw = h * w.
pub fn concat_c(
    ctx: &mut GpuCtx,
    a: &wgpu::Buffer,
    c1: u32,
    b: &wgpu::Buffer,
    c2: u32,
    hw: u64,
) -> Buf {
    let y = ctx.scratch((c1 as u64 + c2 as u64) * hw * 4);
    let uniform = ctx.uniform(&ResizeParams { c: c1, h: 1, w: hw as u32, c2 });
    ctx.run(
        "concat",
        &uniform,
        &[a, b, &y.buffer],
        [wgs((c1 as u64 + c2 as u64) * hw), 1, 1],
    );
    y
}

/// Transpose a (K, N) row-major slice of `x` into a (N, K) slice of `y`.
/// Element offsets are in f32 elements.
pub fn transpose_into(
    ctx: &mut GpuCtx,
    x: &wgpu::Buffer,
    y: &wgpu::Buffer,
    k: u32,
    n: u32,
    in_off: u32,
    out_off: u32,
) {
    let len = k as u64 * n as u64;
    let uniform = ctx.uniform(&TransposeParams {
        len: len as u32,
        k,
        n,
        in_off,
        out_off,
    });
    ctx.run("transpose", &uniform, &[x, y], [wgs(len), 1, 1]);
}

/// Fresh-output transpose of a whole (K, N) buffer.
pub fn transpose(ctx: &mut GpuCtx, x: &wgpu::Buffer, k: u32, n: u32) -> Buf {
    let y = ctx.scratch(k as u64 * n as u64 * 4);
    transpose_into(ctx, x, &y.buffer, k, n, 0, 0);
    y
}

/// C[m,n] = sum_k A[m,k] * B[k,n] into a preallocated `c` at `c_off`.
/// All buffers row-major. `trans_a`: A stored as (K, M); `trans_b`: B stored as (N, K).
pub fn matmul_into(
    ctx: &mut GpuCtx,
    a: &wgpu::Buffer,
    a_off: u32,
    b: &wgpu::Buffer,
    b_off: u32,
    m: u32,
    n: u32,
    k: u32,
    trans_a: bool,
    trans_b: bool,
    c: &wgpu::Buffer,
    c_off: u32,
) {
    let uniform = ctx.uniform(&MatmulParams {
        m,
        n,
        k,
        a_off,
        b_off,
        c_off,
        trans_a: trans_a as u32,
        trans_b: trans_b as u32,
    });
    ctx.run(
        "matmul",
        &uniform,
        &[a, b, c],
        [n.div_ceil(16), m.div_ceil(16), 1],
    );
}

/// Fresh-output matmul with explicit transpose flags.
pub fn matmul_t(
    ctx: &mut GpuCtx,
    a: &wgpu::Buffer,
    b: &wgpu::Buffer,
    m: u32,
    n: u32,
    k: u32,
    trans_a: bool,
    trans_b: bool,
) -> Buf {
    let c = ctx.scratch(m as u64 * n as u64 * 4);
    matmul_into(ctx, a, 0, b, 0, m, n, k, trans_a, trans_b, &c.buffer, 0);
    c
}

/// Plain matmul: A (m, k) row-major, B (k, n) row-major.
pub fn matmul(ctx: &mut GpuCtx, a: &wgpu::Buffer, b: &wgpu::Buffer, m: u32, n: u32, k: u32) -> Buf {
    matmul_t(ctx, a, b, m, n, k, false, false)
}

/// Linear-layer matmul: A (1, in), weight stored as PyTorch `(out, in)`
/// (trans_b), producing (1, out).
pub fn lin(ctx: &mut GpuCtx, a: &wgpu::Buffer, weight: &wgpu::Buffer, out: u32, ins: u32) -> Buf {
    matmul_t(ctx, a, weight, 1, out, ins, false, true)
}

/// In-place row-wise softmax over `rows` rows of `cols` elements starting at
/// `off` (used on the dense attention scores buffer). Every entry is scaled
/// by `scale` before the softmax (attention uses 1 / sqrt(head_dim)).
pub fn softmax_rows(
    ctx: &mut GpuCtx,
    x: &wgpu::Buffer,
    rows: u32,
    cols: u32,
    off: u32,
    scale: f32,
) {
    let uniform = ctx.uniform(&SoftmaxParams { rows, cols, off, scale });
    ctx.run("softmax", &uniform, &[x], [rows, 1, 1]);
}

/// Deterministic standard normal noise.
pub fn randn(ctx: &mut GpuCtx, len: u64, seed: u64) -> Buf {
    let y = ctx.scratch(len * 4);
    let uniform = ctx.uniform(&RandnParams {
        len: len as u32,
        seed: (seed ^ (seed >> 32)) as u32,
    });
    ctx.run("randn", &uniform, &[&y.buffer], [wgs(len), 1, 1]);
    y
}

/// Sinusoidal time embedding (256 dims), matching PyTorch.
pub fn timefreq(ctx: &mut GpuCtx, t: u32, dim: u32) -> Buf {
    let y = ctx.scratch(dim as u64 * 4);
    let uniform = ctx.uniform(&CondParams {
        t,
        dim,
        row_len: 0,
        table_count: 0,
    });
    let yb = &y.buffer;
    ctx.run("timefreq", &uniform, &[yb], [wgs(dim as u64), 1, 1]);
    y
}

/// Sum embedding-table rows selected by `indices` (row offsets from `bases`).
pub fn gather_sum(
    ctx: &mut GpuCtx,
    tables: &wgpu::Buffer,
    indices: &[u32],
    bases: &[u32],
    row_len: u32,
) -> Buf {
    debug_assert_eq!(indices.len(), bases.len());
    let table_count = indices.len() as u32;
    let indices_buf = ctx.upload(bytemuck::cast_slice(indices), "neon-ai-cond-indices");
    let bases_buf = ctx.upload(bytemuck::cast_slice(bases), "neon-ai-cond-bases");
    let y = ctx.scratch(row_len as u64 * 4);
    let uniform = ctx.uniform(&CondParams {
        t: 0,
        dim: 0,
        row_len,
        table_count,
    });
    ctx.run(
        "gather",
        &uniform,
        &[tables, &indices_buf, &bases_buf, &y.buffer],
        [wgs(row_len as u64), 1, 1],
    );
    y
}