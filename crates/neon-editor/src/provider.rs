//! Provider contracts: pluggable syntax highlighters and language-server
//! configurations.
//!
//! The kernel itself is language-agnostic. Concrete grammars (tree-sitter,
//! custom tokenizers) and language-server launch configs are *registered at
//! runtime* through [`crate::registry::LanguageRegistry`]; SDKs and hosts
//! decide which languages a deployment supports instead of baking them into
//! the kernel.

use std::collections::HashMap;

use crate::buffer::TextBuffer;
use crate::highlight::LineTokens;

/// A pluggable syntax highlighter.
///
/// Implementations tokenize a whole buffer into per-line [`LineTokens`];
/// the kernel's highlight / render pipeline consumes the same
/// representation for every language, so a provider is a pure
/// "text -> tokens" mapping.
pub trait SyntaxProvider: Send + Sync {
    /// Stable provider name, e.g. `"tree-sitter/typescript"` or
    /// `"nui_flow/table"`.
    fn name(&self) -> &'static str;

    /// Tokenize the whole buffer into per-line token vectors.
    fn tokenize(&self, buffer: &TextBuffer) -> Vec<LineTokens>;
}

/// How to launch (or dial) the language server for one language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspServerConfig {
    /// Executable (resolved on PATH) or absolute path.
    pub command: String,
    pub args: Vec<String>,
    /// Extra environment variables for the server process.
    pub env: HashMap<String, String>,
}

impl LspServerConfig {
    /// A stdio language server spawned from a command.
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            command: command.into(),
            args,
            env: HashMap::new(),
        }
    }
}
