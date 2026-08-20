//! Error types for the neon-gpu-script frontend.

use thiserror::Error;

/// A source position in the script text (byte offset).
pub type Pos = usize;

/// Compile-time errors produced by the script frontend.
#[derive(Debug, Error, PartialEq)]
pub enum ScriptError {
    #[error("lex error at {pos}: {msg}")]
    Lex { pos: Pos, msg: String },

    #[error("parse error at {pos}: {msg}")]
    Parse { pos: Pos, msg: String },

    #[error("unknown world resource `{domain}.{name}`")]
    UnknownWorld { domain: String, name: String },

    #[error("world resource `{domain}.{name}` is read-only, cannot be an output")]
    ReadOnlyOutput { domain: String, name: String },

    #[error("output `{domain}.{name}` has multiple writers")]
    WriterConflict { domain: String, name: String },

    #[error("export target `{domain}.{name}` is not declared in scene outputs")]
    UndeclaredOutput { domain: String, name: String },

    #[error("unknown kernel `{name}`")]
    UnknownKernel { name: String },

    #[error("kernel `{name}` expects {expected} value args, got {actual}")]
    KernelArgCount { name: String, expected: usize, actual: usize },

    #[error("kernel `{name}` has no parameter `{param}`")]
    UnknownParam { name: String, param: String },

    #[error("SSA violation: value `{name}` is assigned twice")]
    SsaViolation { name: String },

    #[error("undefined value `{name}`")]
    UndefinedValue { name: String },
}
