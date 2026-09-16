//! Runtime language registry: syntax providers + LSP server configs keyed by
//! [`LanguageKind`].
//!
//! Hosts register built-ins (e.g. the `neon-languages` crate) or their own
//! providers once at startup; the kernel and the editor service read through
//! the same registry, so nothing language-specific is compiled into the
//! kernel itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::languages::LanguageKind;
use crate::provider::{LspServerConfig, SyntaxProvider};

/// All registered language capabilities for a process.
#[derive(Default)]
pub struct LanguageRegistry {
    syntax: HashMap<LanguageKind, Arc<dyn SyntaxProvider>>,
    lsp: HashMap<LanguageKind, LspServerConfig>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) the syntax provider for a language.
    pub fn register_syntax(&mut self, kind: LanguageKind, provider: Arc<dyn SyntaxProvider>) {
        self.syntax.insert(kind, provider);
    }

    /// Register (or replace) the LSP server config for a language.
    pub fn register_lsp(&mut self, kind: LanguageKind, config: LspServerConfig) {
        self.lsp.insert(kind, config);
    }

    /// The registered syntax provider for `kind`, if any.
    pub fn syntax(&self, kind: LanguageKind) -> Option<Arc<dyn SyntaxProvider>> {
        self.syntax.get(&kind).cloned()
    }

    /// The registered LSP server config for `kind`, if any.
    pub fn lsp(&self, kind: LanguageKind) -> Option<&LspServerConfig> {
        self.lsp.get(&kind)
    }

    pub fn lsp_mut(&mut self, kind: LanguageKind) -> Option<&mut LspServerConfig> {
        self.lsp.get_mut(&kind)
    }

    /// All kinds that have either a syntax provider or an LSP config.
    pub fn languages(&self) -> impl Iterator<Item = LanguageKind> + '_ {
        self.syntax.keys().copied().chain(self.lsp.keys().copied())
    }
}

// ---------------------------------------------------------------------------
// Process-wide default registry
// ---------------------------------------------------------------------------

static DEFAULT_REGISTRY: OnceLock<Mutex<LanguageRegistry>> = OnceLock::new();

/// The process-wide registry consulted by [`Language::tokenize`] and the
/// editor-runtime LSP spawn path when no explicit provider is given.
///
/// Hosts call [`register_default_syntax`] / [`register_default_lsp`] once at
/// startup (the `neon3-runtime` binary registers `neon-languages` built-ins).
pub fn default_registry() -> MutexGuard<'static, LanguageRegistry> {
    let mutex = DEFAULT_REGISTRY.get_or_init(|| Mutex::new(LanguageRegistry::new()));
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Register a syntax provider on the process-wide default registry.
pub fn register_default_syntax(kind: LanguageKind, provider: Arc<dyn SyntaxProvider>) {
    default_registry().register_syntax(kind, provider);
}

/// Register an LSP server config on the process-wide default registry.
pub fn register_default_lsp(kind: LanguageKind, config: LspServerConfig) {
    default_registry().register_lsp(kind, config);
}
