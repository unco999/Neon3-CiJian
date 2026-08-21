//! Errors from the GPU executor.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("scene has no scene named `{0}` (expected exactly one for execution)")]
    NoScene(String),
    #[error("input alias `{alias}` has no caller-provided buffer")]
    MissingInput { alias: String },
    #[error("unknown kernel `{0}` in codelet registry")]
    UnknownCodelet(String),
    #[error("codelet `{name}` got disallowed constant `{key}`")]
    DisallowedConst { name: String, key: String },
    #[error("codelet `{name}` expects {expected} value inputs, got {actual}")]
    ValueCount {
        name: String,
        expected: usize,
        actual: usize,
    },
    #[error("internal: value node {0} has no buffer yet (dependency order violation)")]
    MissingValueBuffer(usize),
    #[error("device poll failed: {0}")]
    Poll(String),
    #[error("readback failed: {0}")]
    Readback(String),
    #[error("empty buffer rejected")]
    EmptyBuffer,
}
