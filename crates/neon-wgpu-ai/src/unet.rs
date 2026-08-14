//! Conditional terrain UNet executor. Mirrors `TerrainUNet.forward` from
//! `assets/ai/terrain_run1/train.py` with batch size 1, one t and one cond.
//! All weights stay resident on the GPU; per-timestep conditioning vectors
//! are recomputed for each forward (cheap; `temb` is 2 small matmuls).

use std::collections::HashMap;

use crate::format::{PackMeta, TerrainUnetSpec, WeightPack};
use crate::gpu::{Buf, GpuCtx};
use crate::ops;
use crate::schedule::Schedule;
use crate::AiError;

const GN_EPS: f32 = 1e-5;
const LN_EPS: f32 = 1e-5;

/// Loaded model: all weights resident as GPU storage buffers.
pub struct Model {
    pub meta: PackMeta,
    pub spec: TerrainUnetSpec,
    pub schedule: Schedule,
    tensors: HashMap<String, wgpu::Buffer>,
    /// Concatenated embedding tables (sub, parent, relief, texture, water),
    /// one row per class plus the null class.
    emb_tables: wgpu::Buffer,
    /// Element row offset of each table inside `emb_tables`.
    emb_bases: Vec<u32>,
    pub resident_bytes: u64,
}

impl Model {
    /// Parse and validate a pack, then upload every weight to the GPU.
    pub fn load(ctx: &mut GpuCtx, pack: &WeightPack<'_>) -> Result<Self, AiError> {
        let spec = TerrainUnetSpec::default_v1();
        spec.validate_terrain_pack(pack)?;
        let mut tensors = HashMap::with_capacity(pack.tensors.len());
        for tensor in pack.tensors.values() {
            let buffer = ctx.upload(tensor.bytes, &format!("neon-ai-w:{}", tensor.name));
            tensors.insert(tensor.name.clone(), buffer);
        }
        // Concatenate the five embedding tables for gather_sum.
        let mut emb_rows: Vec<f32> = Vec::with_capacity(48 * 256);
        let mut emb_bases = Vec::new();
        for name in ["cemb.sub", "cemb.parent", "cemb.relief", "cemb.texture", "cemb.water"] {
            let table = pack.tensor(&format!("{name}.weight"))?;
            emb_bases.push((emb_rows.len() / 256) as u32);
            emb_rows.extend_from_slice(&table.to_f32()?);
        }
        let emb_tables = ctx.upload(bytemuck::cast_slice(&emb_rows), "neon-ai-embedding-tables");
        let resident_bytes = ctx.resident_bytes;
        Ok(Self {
            meta: pack.meta.clone(),
            spec,
            schedule: Schedule::cosine(pack.meta.T),
            tensors,
            emb_tables,
            emb_bases,
            resident_bytes,
        })
    }

    fn tensor(&self, name: &str) -> Result<&wgpu::Buffer, AiError> {
        self.tensors
            .get(name)
            .ok_or_else(|| AiError::Model(format!("weight '{}' is not resident", name)))
    }

    fn tensor_pair(&self, name: &str) -> Result<(&wgpu::Buffer, &wgpu::Buffer), AiError> {
        Ok((
            self.tensor(&format!("{name}.weight"))?,
            self.tensor(&format!("{name}.bias"))?,
        ))
    }
}

/// Executor producing epsilon predictions for one (x, t, cond) triple.
pub struct UnetExecutor<'a> {
    ctx: &'a mut GpuCtx,
    model: &'a Model,
}

/// Temporary NaN checkpoint (diagnostic only; gated by NEON_AI_DIAG=1).
fn diag_check(ctx: &mut GpuCtx, buf: &wgpu::Buffer, n: usize, tag: &str) {
    if std::env::var("NEON_AI_DIAG").is_err() {
        return;
    }
    let data = ctx.readback_f32(buf, n).expect("diag readback");
    let (mut nan, mut inf, mut lo, mut hi) = (0usize, 0usize, f32::INFINITY, f32::NEG_INFINITY);
    for v in data {
        if v.is_nan() {
            nan += 1;
        } else if v.is_infinite() {
            inf += 1;
        } else {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    eprintln!("DIAG {tag}: nan={nan} inf={inf} min={lo} max={hi}");
}

impl<'a> UnetExecutor<'a> {
    pub fn new(ctx: &'a mut GpuCtx, model: &'a Model) -> Self {
        Self { ctx, model }
    }

    fn cond_vector(&mut self, t: u32, indices: [u32; 5]) -> Result<Buf, AiError> {
        // te = temb(t)
        let (tw1, tb1) = self.model.tensor_pair("temb.mlp.0")?;
        let te0 = ops::timefreq(self.ctx, t, 256);
        let te1 = ops::lin(self.ctx, &te0.buffer, tw1, 1024, 256);
        let te2 = ops::add(self.ctx, &te1.buffer, tb1, 1024);
        let te3 = ops::silu(self.ctx, &te2.buffer, 1024);
        let (tw2, tb2) = self.model.tensor_pair("temb.mlp.2")?;
        let te4 = ops::lin(self.ctx, &te3.buffer, tw2, 256, 1024);
        let te = ops::add(self.ctx, &te4.buffer, tb2, 256);

        // ce = cond_proj(cond_norm(sum of embeddings))
        let ce0 = ops::gather_sum(
            self.ctx,
            &self.model.emb_tables,
            &indices,
            &self.model.emb_bases,
            256,
        );
        let (lnw, lnb) = self.model.tensor_pair("cond_norm")?;
        let ce1 = ops::layer_norm(self.ctx, &ce0.buffer, lnw, lnb, 256, LN_EPS)?;
        let (pw, pb) = self.model.tensor_pair("cond_proj.0")?;
        let ce2 = ops::lin(self.ctx, &ce1.buffer, pw, 256, 256);
        let ce3 = ops::add(self.ctx, &ce2.buffer, pb, 256);
        let ce4 = ops::silu(self.ctx, &ce3.buffer, 256);
        let (qw, qb) = self.model.tensor_pair("cond_proj.2")?;
        let ce5 = ops::lin(self.ctx, &ce4.buffer, qw, 256, 256);
        let ce = ops::add(self.ctx, &ce5.buffer, qb, 256);

        // FiLM conditioning vector for this timestep
        Ok(ops::add(self.ctx, &te.buffer, &ce.buffer, 256))
    }

    fn res_block(
        &mut self,
        x: &wgpu::Buffer,
        name: &str,
        cin: u32,
        cout: u32,
        res: u32,
        c: &wgpu::Buffer,
    ) -> Result<Buf, AiError> {
        let (n1w, n1b) = self.model.tensor_pair(&format!("{name}.n1"))?;
        let (c1w, c1b) = self.model.tensor_pair(&format!("{name}.c1"))?;
        let (n2w, n2b) = self.model.tensor_pair(&format!("{name}.n2"))?;
        let (c2w, c2b) = self.model.tensor_pair(&format!("{name}.c2"))?;
        let (fw, fb) = self.model.tensor_pair(&format!("{name}.film"))?;

        let g1 = ops::group_norm(self.ctx, x, n1w, n1b, 8, cin, res, res, GN_EPS)?;
        let s1 = ops::silu(
            self.ctx,
            &g1.buffer,
            cin as u64 * res as u64 * res as u64,
        );
        diag_check(self.ctx, &s1.buffer, (cin * res * res) as usize, &format!("{name}.s1"));
        let h1 = ops::conv2d(self.ctx, &s1.buffer, c1w, c1b, cin, cout, res, res, 3, 3, 1, 1);
        diag_check(self.ctx, &h1.buffer, (cout * res * res) as usize, &format!("{name}.h1"));
        let g2 = ops::group_norm(self.ctx, &h1.buffer, n2w, n2b, 8, cout, res, res, GN_EPS)?;
        let s2 = ops::silu(
            self.ctx,
            &g2.buffer,
            cout as u64 * res as u64 * res as u64,
        );
        let h2 = ops::conv2d(self.ctx, &s2.buffer, c2w, c2b, cout, cout, res, res, 3, 3, 1, 1);
        diag_check(self.ctx, &h2.buffer, (cout * res * res) as usize, &format!("{name}.h2"));

        // film params = c @ film.weight^T + film.bias  (2*cout)
        let film0 = ops::lin(self.ctx, c, fw, cout * 2, 256);
        let film = ops::add(self.ctx, &film0.buffer, fb, cout as u64 * 2);
        diag_check(self.ctx, &film.buffer, (cout * 2) as usize, &format!("{name}.film"));
        let h3 = ops::film(
            self.ctx,
            &h2.buffer,
            &film.buffer,
            cout,
            cout as u64 * res as u64 * res as u64,
        );

        if cin != cout {
            let (sw, sb) = self.model.tensor_pair(&format!("{name}.skip"))?;
            let skip = ops::conv2d(self.ctx, x, sw, sb, cin, cout, res, res, 1, 1, 1, 0);
            Ok(ops::add(
                self.ctx,
                &h3.buffer,
                &skip.buffer,
                cout as u64 * res as u64 * res as u64,
            ))
        } else {
            Ok(ops::add(
                self.ctx,
                &h3.buffer,
                x,
                cout as u64 * res as u64 * res as u64,
            ))
        }
    }

    fn attn_block(
        &mut self,
        x: &wgpu::Buffer,
        name: &str,
        c: u32,
        res: u32,
    ) -> Result<Buf, AiError> {
        let (qw, qb) = self.model.tensor_pair(&format!("{name}.q"))?;
        let (kw_, kb) = self.model.tensor_pair(&format!("{name}.k"))?;
        let (vw, vb) = self.model.tensor_pair(&format!("{name}.v"))?;
        let (ow, ob) = self.model.tensor_pair(&format!("{name}.o"))?;
        let (nw, nb) = self.model.tensor_pair(&format!("{name}.n"))?;

        let q = ops::conv2d(self.ctx, x, qw, qb, c, c, res, res, 1, 1, 1, 0);
        let k = ops::conv2d(self.ctx, x, kw_, kb, c, c, res, res, 1, 1, 1, 0);
        let v = ops::conv2d(self.ctx, x, vw, vb, c, c, res, res, 1, 1, 1, 0);

        let heads = self.model.spec.heads;
        if c % heads != 0 {
            return Err(AiError::Shape(format!(
                "attention channels {c} must be divisible by {heads} heads"
            )));
        }
        let hd = c / heads;
        let hw = res * res;
        let scale = 1.0 / (hd as f32).sqrt();

        // Per-head sequential attention: one (hw x hw) scores buffer and one
        // (hw x hd) v^T buffer, reused across heads to bound scratch memory.
        let scores = self.ctx.scratch(hw as u64 * hw as u64 * 4);
        let v_t = self.ctx.scratch(hw as u64 * hd as u64 * 4);
        let attn = self.ctx.scratch(c as u64 * hw as u64 * 4);
        for head in 0..heads {
            let off = head * hd * hw;
            ops::transpose_into(self.ctx, &v.buffer, &v_t.buffer, hd, hw, off, 0);
            ops::matmul_into(
                self.ctx,
                &q.buffer,
                off,
                &k.buffer,
                off,
                hw,
                hw,
                hd,
                true,
                false,
                &scores.buffer,
                0,
            );
            ops::softmax_rows(self.ctx, &scores.buffer, hw, hw, 0, scale);
            ops::matmul_into(
                self.ctx,
                &scores.buffer,
                0,
                &v_t.buffer,
                0,
                hw,
                hd,
                hw,
                false,
                false,
                &attn.buffer,
                off,
            );
        }

        let gn = ops::group_norm(self.ctx, &attn.buffer, nw, nb, 8, c, res, res, GN_EPS)?;
        let o = ops::conv2d(self.ctx, &gn.buffer, ow, ob, c, c, res, res, 1, 1, 1, 0);
        Ok(ops::add(self.ctx, x, &o.buffer, c as u64 * hw as u64))
    }

    /// Full UNet forward; returns epsilon prediction (1, 1, size, size).
    pub fn forward(
        &mut self,
        x: &wgpu::Buffer,
        t: u32,
        indices: [u32; 5],
        size: u32,
    ) -> Result<Buf, AiError> {
        let c = self.cond_vector(t, indices)?;
        let cbuf = &c.buffer;
        diag_check(self.ctx, cbuf, 256, "cond");
        let chs = self.model.spec.channels();
        let [c96, _, _, c768] = chs;

        let (iw, ib) = self.model.tensor_pair("input")?;
        let mut h = ops::conv2d(self.ctx, x, iw, ib, 1, c96, size, size, 3, 3, 1, 1);
        diag_check(self.ctx, &h.buffer, (c96 * size * size) as usize, "input_conv");

        let mut skips: Vec<Buf> = Vec::new();
        let mut res = size;
        for i in 0..4 {
            let cout = chs[i];
            let cin = if i == 0 { cout } else { chs[i - 1] };
            h = self.res_block(&h.buffer, &format!("downs.{i}.b1"), cin, cout, res, cbuf)?;
            h = self.res_block(&h.buffer, &format!("downs.{i}.b2"), cout, cout, res, cbuf)?;
            diag_check(self.ctx, &h.buffer, (cout * res * res) as usize, &format!("downs.{i}.b2"));
            if i >= self.model.spec.attn_from_level as usize {
                h = self.attn_block(&h.buffer, &format!("downs.{i}.attn"), cout, res)?;
            }
            if i < 3 {
                skips.push(h);
                h = ops::avg_pool2d(self.ctx, &skips.last().unwrap().buffer, cout, res, res);
                res /= 2;
            }
        }

        h = self.res_block(&h.buffer, "mid.0.b1", c768, c768, res, cbuf)?;
        h = self.attn_block(&h.buffer, "mid.1.attn", c768, res)?;
        h = self.res_block(&h.buffer, "mid.2.b1", c768, c768, res, cbuf)?;
        diag_check(self.ctx, &h.buffer, (c768 * res * res) as usize, "mid_end");

        for i in 0..3 {
            let cout = chs[2 - i];
            h = ops::upsample2x(self.ctx, &h.buffer, cout * 2, res, res);
            let skip = skips.pop().expect("skip stack must match down path");
            h = ops::concat_c(self.ctx, &h.buffer, cout * 2, &skip.buffer, cout, (res * 2) as u64 * (res * 2) as u64);
            h = self.res_block(&h.buffer, &format!("ups.{i}.b1"), cout * 3, cout, res * 2, cbuf)?;
            h = self.res_block(&h.buffer, &format!("ups.{i}.b2"), cout, cout, res * 2, cbuf)?;
            if i == 0 {
                h = self.attn_block(&h.buffer, &format!("ups.{i}.attn"), cout, res * 2)?;
            }
            res *= 2;
        }
        diag_check(self.ctx, &h.buffer, (c96 * res * res) as usize, "ups_end");
        h = self.res_block(&h.buffer, "ups.3.b1", c96, c96, res, cbuf)?;
        h = self.res_block(&h.buffer, "ups.3.b2", c96, c96, res, cbuf)?;

        let (ow, ob) = self.model.tensor_pair("out.0")?;
        h = ops::group_norm(self.ctx, &h.buffer, ow, ob, 8, c96, res, res, GN_EPS)?;
        h = ops::silu(self.ctx, &h.buffer, c96 as u64 * res as u64 * res as u64);
        diag_check(self.ctx, &h.buffer, (c96 * res * res) as usize, "out_silu");
        let (cw, cb) = self.model.tensor_pair("out.2")?;
        Ok(ops::conv2d(self.ctx, &h.buffer, cw, cb, c96, 1, res, res, 3, 3, 1, 1))
    }
}