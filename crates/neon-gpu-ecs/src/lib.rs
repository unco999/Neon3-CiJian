//! neon-gpu-ecs: IR-driven GPU ECS runtime for Neon3.
//!
//! This crate is compute-only. It never creates a `wgpu::Instance`, adapter or
//! device, never opens a window, and never speaks the network protocol. The
//! sole caller is `neon-wgpu-runtime`, the single GPU owner, which hands its
//! device/queue clone into the runtime context.
//!
//! The pipeline follows the design notes: a serde-serializable, language
//! agnostic [`ir::EcsIr`] is validated, translated into one WGSL module with
//! N `@compute fn system_*` entry points plus a `system_preparation` sorting
//! kernel, and executed stage by stage with `dispatch_workgroups_indirect`.

pub mod generator;
pub mod ir;
pub mod runtime;
pub mod tests_support;

pub use ir::EcsIr;
pub use runtime::GpuEcsCtx;

use std::fmt;

/// Stable error codes used across the crate and, once integrated, surfaced
/// over the `neon-wgpu-runtime` protocol surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EcsError {
    /// The IR failed structural validation.
    IrInvalid(String),
    /// Systems in the same schedule stage write the same component.
    ScheduleConflict(String),
    /// Generated WGSL is malformed or failed compilation.
    WgslInvalid(String),
    /// The injected device lacks the limits required by the compiled world.
    Limits(String),
    /// GPU-side failure: mapping, dispatch or readback.
    Gpu(String),
}

impl EcsError {
    /// Stable, machine-readable code for the protocol layer.
    pub fn code(&self) -> &'static str {
        match self {
            EcsError::IrInvalid(_) => "ecs_ir_invalid",
            EcsError::ScheduleConflict(_) => "ecs_schedule_conflict",
            EcsError::WgslInvalid(_) => "ecs_wgsl_invalid",
            EcsError::Limits(_) => "ecs_limits_insufficient",
            EcsError::Gpu(_) => "ecs_gpu_error",
        }
    }
}

impl fmt::Display for EcsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EcsError::IrInvalid(message)
            | EcsError::ScheduleConflict(message)
            | EcsError::WgslInvalid(message)
            | EcsError::Limits(message)
            | EcsError::Gpu(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for EcsError {}
