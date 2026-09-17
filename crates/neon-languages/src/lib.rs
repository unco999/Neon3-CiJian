//! tree-sitter bridge: parse a document with a compiled grammar and convert
//! the concrete syntax tree into the kernel's [`neon_editor::LineTokens`]
//! representation, so the existing highlight/rendering pipeline is reused
//! unchanged.

use std::sync::Arc;

use neon_editor::buffer::TextBuffer;
use neon_editor::highlight::{LineTokens, Span, TokenClass};
use neon_editor::{LanguageKind, LanguageRegistry, LspServerConfig, SyntaxProvider};
use tree_sitter::{Node, Parser, Point};

/// Byte offset of the start of every line in the document.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn build(text: &str) -> Self {
        let mut starts = vec![0usize];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset + 1);
            }
        }
        LineIndex { starts }
    }

    /// Map a byte offset to (line, char column).
    fn point(&self, text: &str, byte_offset: usize) -> Point {
        let line = match self.starts.binary_search(&byte_offset) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        };
        let line_start = self.starts[line];
        let col_bytes = &text[line_start..byte_offset.min(text.len())];
        let column = col_bytes.chars().count();
        Point {
            row: line as usize,
            column,
        }
    }
}

/// Parse `text` with `grammar` and return per-line token vectors.
///
/// `kind` drives a lexer-level keyword fallback: tree-sitter classifies
/// keywords that appear inside syntax-error regions as plain identifiers
/// (e.g. `const` in `const t makeTrack`), which makes highlighting unstable
/// while typing. The fallback rescans every identifier span and upgrades it
/// to `Keyword` when its text is in the language's keyword table, matching
/// the editor convention that keywords always highlight.
pub fn tokenize(
    buffer: &TextBuffer,
    grammar: &tree_sitter::Language,
    kind: LanguageKind,
) -> Vec<LineTokens> {
    let text = buffer.text();
    let mut parser = Parser::new();
    let _ = parser.set_language(grammar);
    let tree = parser.parse(&text, None);

    let line_count = buffer.line_count() as usize;
    let mut per_line: Vec<Vec<Span>> = vec![Vec::new(); line_count];
    let index = LineIndex::build(&text);

    if let Some(tree) = tree {
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            collect_node(&mut per_line, &text, &index, node);
            // Push children in reverse so the walk stays depth-first.
            let mut child_cursor = node.walk();
            let mut children = Vec::new();
            if child_cursor.goto_first_child() {
                loop {
                    children.push(child_cursor.node());
                    if !child_cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
    }

    apply_keyword_fallback(&mut per_line, &text, kind);
    per_line
        .into_iter()
        .map(|spans| LineTokens {
            spans,
            state: Default::default(),
        })
        .collect()
}

/// Lexer-level keyword fallback: upgrade identifier spans whose text is a
/// language keyword to `Keyword` (see [`tokenize`]).
fn apply_keyword_fallback(per_line: &mut [Vec<Span>], text: &str, kind: LanguageKind) {
    let keywords = neon_editor::languages::keywords::keywords_for(kind);
    if keywords.is_empty() {
        return;
    }
    let mut offset = 0usize;
    for spans in per_line.iter_mut() {
        let line_len = text[offset..]
            .find('\n')
            .map_or(text.len() - offset, |n| n);
        let line_text = &text[offset..offset + line_len];
        for span in spans.iter_mut() {
            if span.class != TokenClass::Ident {
                continue;
            }
            let start = (span.start as usize).min(line_text.len());
            let end = (start + span.len as usize).min(line_text.len());
            let word: String = line_text[start..end].chars().collect();
            if keywords.contains(&word.as_str()) {
                span.class = TokenClass::Keyword;
            }
        }
        offset += line_len + 1;
    }
}

/// Classify an identifier-like node by its own kind and parent context.
///
/// tree-sitter Rust/TS/C++ use the same `identifier` leaf for variables,
/// function calls, macro names, and type references; the parent node tells
/// us which visual bucket it belongs to.
fn classify_identifier(node: Node<'_>) -> Option<TokenClass> {
    let kind = node.kind();
    let parent = node.parent();
    let parent_kind = parent.as_ref().map(|p| p.kind()).unwrap_or("");

    match kind {
        // Type identifiers: struct/enum/trait/type names and function def names.
        "type_identifier" => match parent_kind {
            "function_item" | "function_signature_item" => Some(TokenClass::Function),
            _ => Some(TokenClass::Type),
        },
        // Field / property names.
        "field_identifier" | "shorthand_field_identifier" | "property_identifier"
        | "shorthand_property_identifier_pattern" => Some(TokenClass::Property),
        // Plain identifiers: look at the parent to decide Function/Type/Macro/Ident.
        "identifier" => match parent_kind {
            // `foo(...)` — function call.
            "call_expression" | "function_call_expression" => Some(TokenClass::Function),
            // `println!(...)` — macro.
            "macro_invocation" => Some(TokenClass::Macro),
            // `fn foo(...)` — function definition name.
            "function_item" | "function_signature_item" => Some(TokenClass::Function),
            // `String::from` — path segment (type or associated fn).
            "scoped_identifier" => Some(TokenClass::Type),
            // `Some(v) => ...` / `None => ...` in match arms.
            "match_arm" | "match_pattern" | "tuple_struct_pattern" => Some(TokenClass::Type),
            _ => Some(TokenClass::Ident),
        },
        _ => Some(TokenClass::Ident),
    }
}

/// Map a named CST node onto a [`TokenClass`] if the node is a leaf-ish
/// token (keyword / comment / string / number / identifier). Parent nodes are
/// skipped so spans never overlap.
fn collect_node(
    per_line: &mut [Vec<Span>],
    text: &str,
    index: &LineIndex,
    node: Node<'_>,
) {
    let kind = node.kind();
    let class = match kind {
        "comment" | "line_comment" | "block_comment" | "comment_block" => {
            // Check if it's a doc comment (/// or //!) — override to Documentation.
            let text = &text[node.start_byte()..node.end_byte()];
            let trimmed = text.trim_start();
            if trimmed.starts_with("///") || trimmed.starts_with("//!") {
                Some(TokenClass::Documentation)
            } else {
                Some(TokenClass::Comment)
            }
        }
        "string" | "string_literal" | "raw_string_literal" | "char_literal"
        | "template_string" | "concatenated_string" | "interpreted_string_literal"
        | "regex_pattern" | "regex" => Some(TokenClass::StringLiteral),
        "integer_literal" | "float_literal" | "number" | "number_literal"
        | "numeric_literal" | "decimal_integer_literal" | "hex_integer_literal"
        | "octal_integer_literal" | "binary_integer_literal" | "decimal_float_literal"
        | "boolean" => Some(TokenClass::NumericLiteral),
        "identifier" | "field_identifier" | "type_identifier" | "function_name"
        | "variable_name" | "property_identifier" | "shorthand_property_identifier"
        | "shorthand_property_identifier_pattern" | "constant" | "parameter"
        | "assignment_identifier" => classify_identifier(node),
        "primitive_type" => Some(TokenClass::Type),
        "lifetime" => Some(TokenClass::Lifetime),
        // Escape sequences inside strings: \n, \t, \\, \", \x41, \u{...}
        "escape_sequence" => Some(TokenClass::Escape),
        // Rust attributes: #[derive(Debug)], #[cfg(test)]
        "attribute_item" | "inner_attribute_item" | "attribute" => Some(TokenClass::Attribute),
        _ => {
            // Keywords are anonymous nodes: their `kind()` is the literal
            // text ("fn", "let", "if", ...). Named nodes that do not match
            // the tables above are skipped so parent nodes never produce
            // overlapping spans.
            if !node.is_named() {
                let kind = node.kind();
                let is_word =
                    !kind.is_empty() && kind.chars().all(|c| c.is_alphabetic() || c == '_');
                if is_word {
                    Some(TokenClass::Keyword)
                } else {
                    // Operator / punctuation nodes: + - * / = == != < > & | ! etc.
                    Some(TokenClass::Operator)
                }
            } else {
                None
            }
        }
    };
    let Some(class) = class else { return };

    let start = index.point(text, node.start_byte());
    let end = index.point(text, node.end_byte());
    if start.row == end.row {
        push_span(per_line, start.row, start.column, end.column - start.column, class);
    } else {
        // Multi-line node: emit only the first line's visible slice to keep
        // the span contract (one line per entry) intact.
        let line_len = text
            .lines()
            .nth(start.row)
            .map_or(0, |line| line.chars().count());
        if start.column < line_len {
            push_span(per_line, start.row, start.column, line_len - start.column, class);
        }
    }
}

fn push_span(per_line: &mut [Vec<Span>], line: usize, start: usize, len: usize, class: TokenClass) {
    if len == 0 {
        return;
    }
    if let Some(spans) = per_line.get_mut(line) {
        // Merge adjacent spans of the same class.
        if let Some(last) = spans.last_mut() {
            if last.class == class && (last.start as usize) + (last.len as usize) == start {
                last.len += len as u32;
                return;
            }
        }
        spans.push(Span {
            start: start as u32,
            len: len as u32,
            class,
        });
    }
}

// ---------------------------------------------------------------------------
// Syntax providers
// ---------------------------------------------------------------------------

pub struct RustSyntax;

impl SyntaxProvider for RustSyntax {
    fn name(&self) -> &'static str {
        "tree-sitter/rust"
    }

    fn tokenize(&self, buffer: &TextBuffer) -> Vec<LineTokens> {
        tokenize(buffer, &tree_sitter_rust::LANGUAGE.into(), LanguageKind::Rust)
    }
}

pub struct TypescriptSyntax;

impl SyntaxProvider for TypescriptSyntax {
    fn name(&self) -> &'static str {
        "tree-sitter/typescript"
    }

    fn tokenize(&self, buffer: &TextBuffer) -> Vec<LineTokens> {
        tokenize(buffer, &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), LanguageKind::Typescript)
    }
}

pub struct CppSyntax;

impl SyntaxProvider for CppSyntax {
    fn name(&self) -> &'static str {
        "tree-sitter/cpp"
    }

    fn tokenize(&self, buffer: &TextBuffer) -> Vec<LineTokens> {
        tokenize(buffer, &tree_sitter_cpp::LANGUAGE.into(), LanguageKind::Cpp)
    }
}

/// Register the built-in tree-sitter syntax providers on `registry`.
pub fn register_builtins(registry: &mut LanguageRegistry) {

    registry.register_syntax(LanguageKind::Rust, Arc::new(RustSyntax));
    registry.register_syntax(LanguageKind::Typescript, Arc::new(TypescriptSyntax));
    registry.register_syntax(LanguageKind::Cpp, Arc::new(CppSyntax));
}

/// Standard language-server launch configs for the built-in languages.
///
/// Hosts can override these per language through
/// `neon_editor::register_default_lsp` or the `editor.lsp.configure` RPC.
pub fn default_lsp_configs() -> Vec<(LanguageKind, LspServerConfig)> {
    vec![
        (
            LanguageKind::Rust,
            LspServerConfig::stdio("rust-analyzer", Vec::new()),
        ),
        (
            LanguageKind::Typescript,
            LspServerConfig::stdio(
                "typescript-language-server",
                vec!["--stdio".into()],
            ),
        ),
        (
            LanguageKind::Cpp,
            LspServerConfig::stdio("clangd", Vec::new()),
        ),
    ]
}

/// Register both syntax providers and default LSP configs on `registry`.
pub fn register_builtin_languages(registry: &mut LanguageRegistry) {
    register_builtins(registry);
    for (kind, config) in default_lsp_configs() {
        registry.register_lsp(kind, config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::*;
    use neon_editor::buffer::Position;
    use neon_editor::registry::default_registry;

    fn spans_at(lines: &[LineTokens], row: usize) -> Vec<(String, String)> {
        lines[row]
            .spans
            .iter()
            .map(|s| (s.class.name().to_string(), format!("{}+{}", s.start, s.len)))
            .collect()
    }

    /// The exact text the user typed in the TS editor probe: const/let/class/
    /// interface must classify as Keyword, variables as Ident, comments as
    /// Comment — the "const and variable look the same" complaint.
    #[test]
    fn typescript_real_user_text_tokenizes() {
        let mut registry = default_registry();
        register_builtins(&mut registry);
        let source = "interface Track\nconst t makeTrack\nlet x\nconst test = 1;\nconst test = 2;\nclass e{\n// comment\n";
        let mut buffer = TextBuffer::default();
        buffer.insert(Position::new(0, 0), source);
        let provider = registry
            .syntax(LanguageKind::Typescript)
            .expect("typescript provider registered");
        let lines = provider.tokenize(&buffer);

        for (i, l) in lines.iter().enumerate() {
            eprintln!(
                "[ts] row {i}: {}",
                l.spans
                    .iter()
                    .map(|s| format!("{}@{}+{}", s.class.name(), s.start, s.len))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        let row0 = spans_at(&lines, 0); // interface Track
        assert!(
            row0.iter().any(|(c, r)| c == "Keyword" && r == "0+9"),
            "interface should be Keyword 0+9, got {row0:?}"
        );
        assert!(
            row0.iter().any(|(c, r)| c == "Ident" && r == "10+5"),
            "Track should be Ident 10+5, got {row0:?}"
        );

        let row3 = spans_at(&lines, 3); // const test = 1;
        assert!(
            row3.iter().any(|(c, r)| c == "Keyword" && r == "0+5"),
            "const should be Keyword 0+5, got {row3:?}"
        );
        assert!(
            row3.iter().any(|(c, r)| c == "Ident" && r == "6+4"),
            "test should be Ident 6+4, got {row3:?}"
        );

        let row5 = spans_at(&lines, 5); // class e{
        assert!(
            row5.iter().any(|(c, r)| c == "Keyword" && r == "0+5"),
            "class should be Keyword 0+5, got {row5:?}"
        );

        let row6 = spans_at(&lines, 6); // // comment
        assert!(
            row6.iter().any(|(c, _)| c == "Comment"),
            "comment line should have Comment, got {row6:?}"
        );

        let row2 = spans_at(&lines, 2); // let x
        assert!(
            row2.iter().any(|(c, r)| c == "Keyword" && r == "0+3"),
            "let should be Keyword 0+3, got {row2:?}"
        );
    }

    #[test]
    fn rust_keywords_strings_comments() {
        let buffer = TextBuffer::from_str(
            "// header\nfn main() {\n  let x = 42;\n  println!(\"ok\");\n}\n",
        );
        let tokens = RustSyntax.tokenize(&buffer);
        assert_eq!(tokens.len(), 6); // trailing newline -> final empty line
        assert!(tokens[0].spans.iter().any(|s| s.class == TokenClass::Comment));
        let line1 = &tokens[1].spans;
        assert!(line1.iter().any(|s| s.class == TokenClass::Keyword)); // fn
        let line2 = &tokens[2].spans;
        assert!(line2.iter().any(|s| s.class == TokenClass::Keyword)); // let
        assert!(line2.iter().any(|s| s.class == TokenClass::NumericLiteral)); // 42
        let line3 = &tokens[3].spans;
        assert!(line3.iter().any(|s| s.class == TokenClass::StringLiteral)); // "ok"
        assert!(line3.iter().any(|s| s.class == TokenClass::Macro)); // println
    }

    #[test]
    fn typescript_tokenizes() {
        let buffer = TextBuffer::from_str(
            "interface Foo { bar: number }\nconst x: Foo = { bar: 1 };\n",
        );
        let tokens = TypescriptSyntax.tokenize(&buffer);
        assert_eq!(tokens.len(), 3); // trailing newline -> final empty line
        assert!(tokens[0].spans.iter().any(|s| s.class == TokenClass::Keyword));
        assert!(tokens[0].spans.iter().any(|s| s.class == TokenClass::Type
            || s.class == TokenClass::Ident)); // Foo type name
        assert!(tokens[1].spans.iter().any(|s| s.class == TokenClass::Keyword));
    }

    #[test]
    fn cpp_tokenizes() {
        let buffer = TextBuffer::from_str(
            "#include <vector>\nint main() { return 0; }\n",
        );
        let tokens = CppSyntax.tokenize(&buffer);
        assert_eq!(tokens.len(), 3); // trailing newline -> final empty line
        assert!(tokens[1].spans.iter().any(|s| s.class == TokenClass::Keyword));
        assert!(tokens[1].spans.iter().any(|s| s.class == TokenClass::NumericLiteral));
    }

    #[test]
    fn builtins_registered_by_kind() {
        let mut registry = LanguageRegistry::new();
        register_builtin_languages(&mut registry);
        assert!(registry.syntax(LanguageKind::Typescript).is_some());
        assert!(registry.syntax(LanguageKind::Rust).is_some());
        assert!(registry.syntax(LanguageKind::Cpp).is_some());
        assert!(registry.syntax(LanguageKind::NuiFlow).is_none()); // built into kernel
        assert!(registry.lsp(LanguageKind::Typescript).is_some());
        assert_eq!(
            registry
                .lsp(LanguageKind::Rust)
                .map(|c| c.command.as_str()),
            Some("rust-analyzer")
        );
    }
}
