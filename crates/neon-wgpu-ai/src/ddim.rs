//! DDIM sampler with optional classifier-free guidance, batch size 1.
//! Mirrors the inference loop in `assets/ai/terrain_run1/sample.py`.

use crate::format::TerrainCond;
use crate::gpu::{Buf, GpuCtx};
use crate::ops;
use crate::unet::{Model, UnetExecutor};
use crate::AiError;

/// Two-forward CFG. The unconditional pass is skipped when guidance is zero.
pub struct DdimSampler<'a> {
    ctx: &'a mut GpuCtx,
    model: &'a Model,
}

impl<'a> DdimSampler<'a> {
    pub fn new(ctx: &'a mut GpuCtx, model: &'a Model) -> Self {
        Self { ctx, model }
    }

    fn forward(
        &mut self,
        x: &wgpu::Buffer,
        t: u32,
        indices: [u32; 5],
        size: u32,
    ) -> Result<Buf, AiError> {
        let mut executor = UnetExecutor::new(self.ctx, self.model);
        executor.forward(x, t, indices, size)
    }

    /// Run `steps` DDIM iterations starting from `x` (owned by the caller).
    /// Returns the final latent Buf and optional previews captured every
    /// `preview_every` steps.
    pub fn sample(
        &mut self,
        x: &wgpu::Buffer,
        cond: TerrainCond,
        guidance: f32,
        steps: u32,
        size: u32,
        preview_every: u32,
    ) -> Result<(Buf, Vec<Vec<f32>>), AiError> {
        let len = size as u64 * size as u64;
        let idx = cond.indices();
        let null_idx = TerrainCond::default().indices();
        let use_cfg = guidance > 0.0;
        let batch_steps = std::env::var("NEON_AI_DIAG").is_err();
        let mut previews: Vec<Vec<f32>> = Vec::new();
        let mut carried: Option<Buf> = None;
        let mut current = x;
        for i in 0..steps {
            if batch_steps {
                self.ctx.begin_batch();
            }
            let step_result = (|| -> Result<Buf, AiError> {
                let (t, t0) = self.model.schedule.ddim_times(steps, i);
                let ec = self.forward(current, t, idx, size)?;
                let e = if use_cfg {
                    let eu = self.forward(current, t, null_idx, size)?;
                    ops::cfg_combine(self.ctx, &ec.buffer, &eu.buffer, guidance, len)
                } else {
                    ec
                };
                let (sab_t, s1ab_t) = (
                    self.model.schedule.sab[t as usize],
                    self.model.schedule.s1ab[t as usize],
                );
                let (sab_t0, s1ab_t0) = (
                    self.model.schedule.sab[t0 as usize],
                    self.model.schedule.s1ab[t0 as usize],
                );
                Ok(ops::ddim_step(
                    self.ctx,
                    current,
                    &e.buffer,
                    sab_t,
                    s1ab_t,
                    sab_t0,
                    s1ab_t0,
                    len,
                ))
            })();
            if batch_steps {
                if step_result.is_ok() {
                    self.ctx.submit_batch();
                } else {
                    self.ctx.discard_batch();
                }
            }
            let next = step_result?;
            if preview_every > 0 && (i + 1) % preview_every == 0 {
                previews.push(self.ctx.readback_f32(&next.buffer, len as usize)?);
            }
            carried = Some(next);
            current = &carried.as_ref().expect("carried").buffer;
        }
        Ok((carried.expect("steps must be >= 1"), previews))
    }
}
