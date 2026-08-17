//! neon-wgpu-ai: GPU inference kernels and model executors for Neon3.
//!
//! This crate is compute-only. It never creates a `wgpu::Instance`, adapter or
//! device, never opens a window, and never speaks the network protocol. The
//! sole caller is `neon-wgpu-runtime`, the single GPU owner, which hands its
//! device/queue clone into [`engine::AiEngine`]. External clients reach this
//! functionality exclusively through the `ai.*` RPC methods of
//! `neon-wgpu-runtime`.

// `T` (diffusion timesteps) mirrors the PyTorch source spelling on purpose.
#![allow(non_snake_case)]

pub mod ddim;
pub mod engine;
pub mod format;
pub mod gpu;
pub mod ops;
pub mod schedule;
pub mod unet;

pub use engine::{AiEngine, GenerateRequest, Generation, GpuGeneration, ModelInfo};
pub use format::{TerrainCond, WeightPack};
pub use gpu::GpuCtx;

use std::fmt;

/// Stable error codes used by the engine and surfaced over `ai.*` responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    InvalidPack(String),
    Model(String),
    Shape(String),
    InvalidRequest(String),
    Gpu(String),
}

impl AiError {
    /// Stable, machine-readable code for the protocol layer.
    pub fn code(&self) -> &'static str {
        match self {
            AiError::InvalidPack(_) => "ai_invalid_pack",
            AiError::Model(_) => "ai_model_error",
            AiError::Shape(_) => "ai_shape_error",
            AiError::InvalidRequest(_) => "ai_invalid_request",
            AiError::Gpu(_) => "ai_gpu_error",
        }
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiError::InvalidPack(message)
            | AiError::Model(message)
            | AiError::Shape(message)
            | AiError::InvalidRequest(message)
            | AiError::Gpu(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_validation_rules() {
        let ok = engine::GenerateRequest {
            cond: TerrainCond::default(),
            guidance: 7.0,
            steps: 50,
            seed: 1,
            size: 256,
            preview_every: 0,
        };
        assert!(engine::validate_request(&ok).is_ok());
        assert!(
            engine::validate_request(&GenerateRequest {
                size: 64,
                ..ok.clone()
            })
            .is_ok()
        );
        assert!(
            engine::validate_request(&GenerateRequest {
                size: 100,
                ..ok.clone()
            })
            .is_err()
        );
        assert!(
            engine::validate_request(&GenerateRequest {
                size: 31,
                ..ok.clone()
            })
            .is_err()
        );
        assert!(
            engine::validate_request(&GenerateRequest {
                steps: 0,
                ..ok.clone()
            })
            .is_err()
        );
        assert!(
            engine::validate_request(&GenerateRequest {
                guidance: 20.0,
                ..ok.clone()
            })
            .is_err()
        );
        let bad_cond = GenerateRequest {
            cond: TerrainCond {
                sub: Some(23),
                ..TerrainCond::default()
            },
            ..ok
        };
        assert!(engine::validate_request(&bad_cond).is_err());
    }
}
