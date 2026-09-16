//! Language abstraction for the neon-editor kernel.
//!
//! The kernel itself is language-agnostic: a [`Language`] selects a
//! highlighter source (NUI Flow table-driven rules or a tree-sitter grammar)
//! and, at the runtime layer, an LSP server for completions/diagnostics.
//! This crate stays free of any Neon3 runtime dependency.

pub mod tree_sitter;

use crate::buffer::TextBuffer;
use crate::highlight::{LineTokens, TokenClass};

/// Stable language identifiers understood by the kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LanguageKind {
    /// Neon3's own declarative UI language (table-driven, built-in).
    NuiFlow,
    Typescript,
    Rust,
    Cpp,
}

impl LanguageKind {
    pub fn name(self) -> &'static str {
        match self {
            LanguageKind::NuiFlow => "nui_flow",
            LanguageKind::Typescript => "typescript",
            LanguageKind::Rust => "rust",
            LanguageKind::Cpp => "cpp",
        }
    }

    /// Extensions this language owns (without the leading dot).
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            LanguageKind::NuiFlow => &["nui"],
            LanguageKind::Typescript => &["ts", "tsx", "mts", "cts"],
            LanguageKind::Rust => &["rs"],
            LanguageKind::Cpp => &["cpp", "cc", "cxx", "h", "hpp", "hxx"],
        }
    }
}

/// A concrete language selection for an editor session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Language {
    pub kind: LanguageKind,
}

impl Language {
    pub fn nui_flow() -> Self {
        Language {
            kind: LanguageKind::NuiFlow,
        }
    }

    /// Resolve a language from a file extension ("ts", "rs", "cpp", "nui", ...).
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        for kind in [
            LanguageKind::NuiFlow,
            LanguageKind::Typescript,
            LanguageKind::Rust,
            LanguageKind::Cpp,
        ] {
            if kind.extensions().contains(&extension.as_str()) {
                return Some(Language { kind });
            }
        }
        None
    }

    pub fn name(&self) -> &'static str {
        self.kind.name()
    }

    /// Tree-sitter grammar for this language, when one is compiled in.
    pub fn tree_sitter_language(&self) -> Option<::tree_sitter::Language> {
        match self.kind {
            LanguageKind::NuiFlow => None,
            LanguageKind::Typescript => Some(
                ::tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            ),
            LanguageKind::Rust => Some(::tree_sitter_rust::LANGUAGE.into()),
            LanguageKind::Cpp => Some(::tree_sitter_cpp::LANGUAGE.into()),
        }
    }

    /// Tokenize the whole document into per-line token vectors.
    ///
    /// NUI Flow reuses the built-in table-driven tokenizer (see
    /// [`crate::highlight`]); tree-sitter languages parse the document once
    /// and map named CST nodes onto the kernel's [`TokenClass`] set.
    pub fn tokenize(&self, buffer: &TextBuffer) -> Vec<LineTokens> {
        match self.kind {
            LanguageKind::NuiFlow => {
                let grammar = crate::grammar::nui_flow_default();
                let mut tokens = Vec::with_capacity(buffer.line_count() as usize);
                let mut state = Default::default();
                for line in buffer.lines() {
                    let line_tokens =
                        crate::highlight::tokenize_line(line, state, &grammar);
                    state = line_tokens.state;
                    tokens.push(line_tokens);
                }
                tokens
            }
            _ => match self.tree_sitter_language() {
                Some(grammar) => tree_sitter::tokenize(buffer, &grammar),
                None => {
                    let mut tokens = Vec::with_capacity(buffer.line_count() as usize);
                    for line in buffer.lines() {
                        let mut spans = Vec::new();
                        let chars: Vec<char> = line.chars().collect();
                        if !chars.is_empty() {
                            spans.push(crate::highlight::Span {
                                start: 0,
                                len: chars.len() as u32,
                                class: TokenClass::Ident,
                            });
                        }
                        tokens.push(crate::highlight::LineTokens {
                            spans,
                            state: Default::default(),
                        });
                    }
                    tokens
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_extensions() {
        assert_eq!(
            Language::from_extension("ts").map(|l| l.kind),
            Some(LanguageKind::Typescript)
        );
        assert_eq!(
            Language::from_extension(".rs").map(|l| l.kind),
            Some(LanguageKind::Rust)
        );
        assert_eq!(
            Language::from_extension("cpp").map(|l| l.kind),
            Some(LanguageKind::Cpp)
        );
        assert_eq!(
            Language::from_extension("nui").map(|l| l.kind),
            Some(LanguageKind::NuiFlow)
        );
        assert_eq!(Language::from_extension("py"), None);
    }

    #[test]
    fn nui_flow_tokenize_still_classifies() {
        let buffer = TextBuffer::from_str(
            "version 1\nsurface root w 400 h 300\n  text title value \"hi\"\n",
        );
        let tokens = Language::nui_flow().tokenize(&buffer);
        assert_eq!(tokens.len(), 4); // trailing newline -> final empty line
        assert!(tokens[1]
            .spans
            .iter()
            .any(|s| s.class == TokenClass::Keyword)); // surface
        assert!(tokens[2]
            .spans
            .iter()
            .any(|s| s.class == TokenClass::NodeKind)); // text
    }

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn tree_sitter_tokenize_rust() {
        let buffer = TextBuffer::from_str(
            "// hi\nfn main() { let x = 1; println!(\"ok\"); }\n",
        );
        let tokens = Language {
            kind: LanguageKind::Rust,
        }
        .tokenize(&buffer);
        assert_eq!(tokens.len(), 3); // trailing newline -> final empty line
        assert!(tokens[0].spans.iter().any(|s| s.class == TokenClass::Comment));
        assert!(tokens[1].spans.iter().any(|s| s.class == TokenClass::Keyword));
        assert!(tokens[1]
            .spans
            .iter()
            .any(|s| s.class == TokenClass::StringLiteral));
        assert!(tokens[1]
            .spans
            .iter()
            .any(|s| s.class == TokenClass::NumericLiteral));
    }
}
