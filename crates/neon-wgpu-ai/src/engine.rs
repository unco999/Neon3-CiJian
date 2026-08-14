//! AI inference engine: owns the compute context and the resident model.
//! Runs on the worker thread of `neon-wgpu-runtime`; the device is handed in
//! by the runtime (never created here).

use crate::ddim::DdimSampler;
use crate::format::TerrainCond;
use crate::gpu::GpuCtx;
use crate::ops;
use crate::unet::Model;
use crate::{AiError, WeightPack};

pub const DEFAULT_SIZE: u32 = 256;
pub const DEFAULT_STEPS: u32 = 50;
pub const DEFAULT_GUIDANCE: f32 = 7.0;

/// Model summary surfaced over `ai.model.status`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ModelInfo {
    pub model_kind: String,
    pub T: u32,
    pub base: u32,
    pub schedule: String,
    pub source_ckpt: String,
    pub param_count: u64,
    pub sha256: String,
    pub created_at: String,
    pub resident_bytes: u64,
}

/// One generation request, validated before any GPU work starts.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GenerateRequest {
    pub cond: TerrainCond,
    #[serde(default = "default_guidance")]
    pub guidance: f32,
    #[serde(default = "default_steps")]
    pub steps: u32,
    pub seed: u64,
    #[serde(default = "default_size")]
    pub size: u32,
    /// Read back the intermediate latent every N steps (0 = no previews).
    #[serde(default)]
    pub preview_every: u32,
}

fn default_guidance() -> f32 {
    DEFAULT_GUIDANCE
}
fn default_steps() -> u32 {
    DEFAULT_STEPS
}
fn default_size() -> u32 {
    DEFAULT_SIZE
}

/// One finished generation, ready for `ai.heightmap.export`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Generation {
    pub size: u32,
    pub steps: u32,
    pub seed: u64,
    pub guidance: f32,
    /// Final heightmap, row-major f32 (the latent x after the last DDIM step).
    pub heightmap: Vec<f32>,
    /// Optional intermediate latents (row-major f32, one per captured step).
    pub previews: Vec<Vec<f32>>,
    pub elapsed_ms: f64,
}

pub struct AiEngine {
    ctx: GpuCtx,
    model: Option<Model>,
}

impl AiEngine {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            ctx: GpuCtx::new(device, queue),
            model: None,
        }
    }

    /// Diagnostic access for GPU acceptance tests.
    pub fn ctx(&mut self) -> &mut GpuCtx {
        &mut self.ctx
    }

    /// Read-only access to the loaded model (diagnostic path).
    pub fn model(&self) -> &Model {
        self.model.as_ref().expect("no model loaded")
    }

    /// Diagnostic access to the model plus the compute context.
    pub fn model_and_ctx(&mut self) -> (&Model, &mut GpuCtx) {
        (self.model.as_ref().expect("no model loaded"), &mut self.ctx)
    }

    pub fn has_model(&self) -> bool {
        self.model.is_some()
    }

    pub fn model_info(&self) -> Option<ModelInfo> {
        self.model.as_ref().map(|m| ModelInfo {
            model_kind: m.meta.model_kind.clone(),
            T: m.meta.T,
            base: m.meta.base,
            schedule: m.meta.schedule.clone(),
            source_ckpt: m.meta.source_ckpt.clone(),
            param_count: m.meta.param_count,
            sha256: m.meta.sha256.clone(),
            created_at: m.meta.created_at.clone(),
            resident_bytes: m.resident_bytes,
        })
    }

    /// Validate and upload a `NEONWAI1` pack.
    pub fn load_model(&mut self, bytes: &[u8]) -> Result<ModelInfo, AiError> {
        let pack = WeightPack::parse(bytes)?;
        let model = Model::load(&mut self.ctx, &pack)?;
        let info = ModelInfo {
            model_kind: model.meta.model_kind.clone(),
            T: model.meta.T,
            base: model.meta.base,
            schedule: model.meta.schedule.clone(),
            source_ckpt: model.meta.source_ckpt.clone(),
            param_count: model.meta.param_count,
            sha256: model.meta.sha256.clone(),
            created_at: model.meta.created_at.clone(),
            resident_bytes: model.resident_bytes,
        };
        self.model = Some(model);
        Ok(info)
    }

    pub fn generate(&mut self, request: GenerateRequest) -> Result<Generation, AiError> {
        validate_request(&request)?;
        let model = self
            .model
            .as_ref()
            .ok_or_else(|| AiError::InvalidRequest("no model loaded".into()))?;
        let len = request.size as u64 * request.size as u64;
        let started = std::time::Instant::now();

        let noise = ops::randn(&mut self.ctx, len, request.seed);
        let (final_buf, previews) = {
            let mut sampler = DdimSampler::new(&mut self.ctx, model);
            sampler.sample(
                &noise.buffer,
                request.cond,
                request.guidance,
                request.steps,
                request.size,
                request.preview_every,
            )?
        };
        let heightmap = self.ctx.readback_f32(&final_buf.buffer, len as usize)?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        Ok(Generation {
            size: request.size,
            steps: request.steps,
            seed: request.seed,
            guidance: request.guidance,
            heightmap,
            previews,
            elapsed_ms,
        })
    }
}

pub(crate) fn validate_request(request: &GenerateRequest) -> Result<(), AiError> {
    request.cond.validate()?;
    if !(0.0..=16.0).contains(&request.guidance) {
        return Err(AiError::InvalidRequest("guidance must be in 0..=16".into()));
    }
    if !(1..=200).contains(&request.steps) {
        return Err(AiError::InvalidRequest("steps must be in 1..=200".into()));
    }
    if request.size % 32 != 0 || !((request.size / 32) & ((request.size / 32) - 1) == 0) {
        return Err(AiError::InvalidRequest(format!(
            "size must be 32 * 2^k, got {}",
            request.size
        )));
    }
    if request.size < 32 {
        return Err(AiError::InvalidRequest(format!(
            "size must be at least 32, got {}",
            request.size
        )));
    }
    Ok(())
}