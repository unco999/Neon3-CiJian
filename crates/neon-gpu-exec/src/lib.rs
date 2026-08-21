//! # neon-gpu-exec
//!
//! The GPU executor of the neon-gpu-script pipeline: turns a compiled
//! scene (IR + waves, see `neon-gpu-script`) into wgpu compute dispatches
//! and reads the exported world values back to the CPU.
//!
//! Ownership matches `AGENTS.md`: this crate is a library. The caller
//! (`neon-wgpu-runtime`) owns the `wgpu::Device` and `wgpu::Queue`; the
//! executor only borrows them to create resources and submit work.
//!
//! ## Execution model (docs/neon3-gpu-script.md §4.6)
//!
//! The executor walks the wave plan in order and submits every wave to the
//! queue with no readback in between: a script runs GPU-side to completion
//! once submitted. Only the exported outputs are copied back to the CPU at
//! the end (the frame boundary).
//!
//! ## Memory model
//!
//! - Scene inputs (world resources) are caller-provided storage buffers,
//!   keyed by the scene's input alias.
//! - Every kernel node writes to a freshly allocated storage buffer (simple
//!   allocation for this milestone; liveness/scratch reuse is the next
//!   phase).
//! - Constant args are baked into the WGSL at pipeline-compile time, so each
//!   distinct `(kernel, consts, value_count)` combination gets its own
//!   cached pipeline instance.

pub mod codelet;
pub mod error;
pub mod executor;

pub use codelet::{Codelet, ConstArg, FieldTy};
pub use error::ExecError;
pub use executor::{Executor, InputField};
