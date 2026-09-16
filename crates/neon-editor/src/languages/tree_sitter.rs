//! tree-sitter bridge: parse a document with a compiled grammar and convert
//! the concrete syntax tree into the kernel's [`LineTokens`] representation,
//! so the existing highlight/rendering pipeline is reused unchanged.

use tree_sitter::{Node, Parser, Point};

use crate::buffer::TextBuffer;
use crate::highlight::{LineTokens, Span, TokenClass};

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
pub fn tokenize(buffer: &TextBuffer, grammar: &tree_sitter::Language) -> Vec<LineTokens> {
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

    per_line
        .into_iter()
        .map(|spans| LineTokens {
            spans,
            state: Default::default(),
        })
        .collect()
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
            Some(TokenClass::Comment)
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
        | "assignment_identifier" => Some(TokenClass::Ident),
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
                    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keywords_strings_comments() {
        let buffer = TextBuffer::from_str(
            "// header\nfn main() {\n  let x = 42;\n  println!(\"ok\");\n}\n",
        );
        let tokens = tokenize(&buffer, &tree_sitter_rust::LANGUAGE.into());
        assert_eq!(tokens.len(), 6); // trailing newline -> final empty line
        assert!(tokens[0].spans.iter().any(|s| s.class == TokenClass::Comment));
        let line1 = &tokens[1].spans;
        assert!(line1.iter().any(|s| s.class == TokenClass::Keyword)); // fn
        let line2 = &tokens[2].spans;
        assert!(line2.iter().any(|s| s.class == TokenClass::Keyword)); // let
        assert!(line2.iter().any(|s| s.class == TokenClass::NumericLiteral)); // 42
        let line3 = &tokens[3].spans;
        assert!(line3.iter().any(|s| s.class == TokenClass::StringLiteral)); // "ok"
        assert!(line3.iter().any(|s| s.class == TokenClass::Ident)); // println
    }

    #[test]
    fn typescript_tokenizes() {
        let buffer = TextBuffer::from_str(
            "interface Foo { bar: number }\nconst x: Foo = { bar: 1 };\n",
        );
        let tokens = tokenize(
            &buffer,
            &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        );
        assert_eq!(tokens.len(), 3); // trailing newline -> final empty line
        assert!(tokens[0].spans.iter().any(|s| s.class == TokenClass::Keyword));
        assert!(tokens[0].spans.iter().any(|s| s.class == TokenClass::Ident));
        assert!(tokens[1].spans.iter().any(|s| s.class == TokenClass::Keyword));
    }

    #[test]
    fn cpp_tokenizes() {
        let buffer = TextBuffer::from_str(
            "#include <vector>\nint main() { return 0; }\n",
        );
        let tokens = tokenize(&buffer, &tree_sitter_cpp::LANGUAGE.into());
        assert_eq!(tokens.len(), 3); // trailing newline -> final empty line
        assert!(tokens[1].spans.iter().any(|s| s.class == TokenClass::Keyword));
        assert!(tokens[1].spans.iter().any(|s| s.class == TokenClass::NumericLiteral));
    }
}
