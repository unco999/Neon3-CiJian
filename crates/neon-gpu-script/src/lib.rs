//! # neon-gpu-script
//!
//! The GPU script frontend of Neon3: lexer, parser, SSA validation,
//! dataflow graph (IR) and topological wave planning.
//!
//! This crate is a pure-CPU leaf: it performs no GPU work and owns no
//! resources. It turns a script text into a [`CompiledScript`] whose scenes
//! are dataflow graphs with wave plans, ready for the executor layer
//! (`neon-wgpu-runtime`) to bind against resident handles and issue
//! dispatches.
//!
//! ## Design notes
//!
//! - Values are immutable (SSA): each name is assigned exactly once, so the
//!   graph is acyclic and topological layering is unconditional (see
//!   `docs/neon3-gpu-script.md` §4.1).
//! - The language is pure dataflow in v1: no control-flow statements;
//!   branches are `select(a, b, mask)` kernels, fixed loops are unrolled at
//!   compile time (not yet parsed), and convergence loops live inside
//!   kernels (see §4.6).
//! - Namespace is three-layered: scene-private SSA symbols, world resource
//!   qualified names (`domain.name`, resolved against a
//!   [`WorldRegistry`] authoritative table), and runtime handles which the
//!   script never touches (see §4.5).
//! - Compilation is a single pass: tokens -> AST -> IR (SSA + DAG) -> waves.

pub mod ast;
pub mod error;
pub mod ir;
pub mod lexer;
pub mod parser;
pub mod plan;
pub mod registry;

pub use error::{Pos, ScriptError};
pub use ir::{CompiledScene, CompiledScript, ConstValue, IrArg, IrNode, IrScene, NodeId, NodeKind};
pub use registry::{KernelRegistry, ResourceSpec, WorldRegistry};

/// Compiles a script text into a validated, layered script.
///
/// `world` must already contain every `domain.name` referenced by the
/// script's inputs and outputs; `kernels` must contain every kernel id.
/// All SSA, naming, arg-count and writer-conflict checks run here.
pub fn compile(
    source: &str,
    world: &WorldRegistry,
    kernels: &KernelRegistry,
) -> Result<CompiledScript, ScriptError> {
    let tokens = lexer::tokenize(source)?;
    let script = parser::parse(&tokens)?;
    ir::build(&script, world, kernels)
}
