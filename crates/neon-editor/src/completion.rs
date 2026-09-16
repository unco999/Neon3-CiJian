//! Context-aware completion for NUI Flow.

use crate::buffer::{Position, TextBuffer};
use crate::grammar::FlowGrammar;
use crate::highlight::{LineState, TokenClass, tokenize_line};
use crate::symbols::SymbolIndex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    Keyword,
    NodeKind,
    Attribute,
    Input,
    InputKind,
    /// Generic language value (LSP-sourced completions for TS/Rust/C++).
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionSource {
    Grammar,
    DocumentSymbol,
    Host,
    /// Resolved through the LSP client bridge (non-Flow languages).
    Lsp,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionItem {
    pub item_id: String,
    pub label: String,
    pub insert_text: String,
    pub replace_start: Position,
    pub replace_end: Position,
    pub kind: CompletionKind,
    /// One-line explanation rendered by the popup.
    pub detail: String,
    pub sort_text: String,
    pub source: CompletionSource,
    pub commit_characters: Vec<char>,
}

/// Computes completion candidates for the cursor at `position`. The cursor
/// may sit at end-of-line or inside a partial token; the partial token filters
/// candidates but is not treated as an already-used one.
pub fn completions(
    buffer: &TextBuffer,
    grammar: &FlowGrammar,
    symbols: &SymbolIndex,
    position: Position,
) -> Vec<CompletionItem> {
    let Some(line_text) = buffer.line(position.line) else {
        return Vec::new();
    };
    let column = position.column.min(line_text.chars().count() as u32) as usize;
    let prefix: String = line_text.chars().take(column).collect();

    // Inside a string or comment: no completion.
    let tokens = tokenize_line(&prefix, LineState::default(), grammar);
    if let Some(last_span) = tokens.spans.last()
        && matches!(
            last_span.class,
            TokenClass::StringLiteral | TokenClass::Comment
        )
    {
        return Vec::new();
    }

    let partial = current_partial(&prefix);
    let partial_start = Position::new(
        position.line,
        column
            .saturating_sub(partial.chars().count())
            .try_into()
            .unwrap_or(u32::MAX),
    );
    let partial_end = Position::new(position.line, column as u32);
    let line_tokens: Vec<&str> = prefix.split_whitespace().collect();
    let indented = prefix.starts_with(' ');
    let first_is_node = line_tokens
        .first()
        .is_some_and(|first| grammar.is_node_kind(first));
    let filter =
        |label: &str| partial.is_empty() || label.starts_with(&partial.to_ascii_lowercase());

    let mut candidates: Vec<CompletionItem> = Vec::new();

    if partial.starts_with('$') {
        let typed = partial[1..].to_ascii_lowercase();
        for input in symbols.inputs() {
            if typed.is_empty() || input.to_ascii_lowercase().contains(&typed) {
                candidates.push(CompletionItem {
                    item_id: format!("input:{input}"),
                    label: format!("${input}"),
                    insert_text: format!("${input}"),
                    replace_start: partial_start,
                    replace_end: partial_end,
                    kind: CompletionKind::Input,
                    detail: "declared input".into(),
                    sort_text: input.clone(),
                    source: CompletionSource::DocumentSymbol,
                    commit_characters: Vec::new(),
                });
            }
        }
        return candidates;
    }

    if first_is_node {
        // `  button publish value "..."`: node kind, key, then attributes.
        if line_tokens.len() >= 2 {
            let node_kind = line_tokens[0];
            // The still-being-typed trailing token is not a used attribute.
            let mut seen: Vec<&str> = line_tokens[2..].to_vec();
            if !partial.is_empty() && line_tokens.last() == Some(&partial.as_str()) {
                seen.pop();
            }
            seen.sort_unstable();
            seen.dedup();
            for attribute in grammar.attributes_for(node_kind) {
                if seen.contains(&attribute) {
                    continue;
                }
                if filter(attribute) {
                    candidates.push(CompletionItem {
                        item_id: format!("attribute:{node_kind}:{attribute}"),
                        label: (*attribute).to_string(),
                        insert_text: (*attribute).to_string(),
                        replace_start: partial_start,
                        replace_end: partial_end,
                        kind: CompletionKind::Attribute,
                        detail: format!("{node_kind} attribute"),
                        sort_text: (*attribute).to_string(),
                        source: CompletionSource::Grammar,
                        commit_characters: vec![' '],
                    });
                }
            }
        }
        // len == 1: the semantic key slot gets no candidates in V1.
        return candidates;
    }

    // A partially typed first token has not become a known node kind yet, but
    // it is still the node-kind completion context on an indented line.
    if indented && line_tokens.len() == 1 {
        for kind in &grammar.node_kinds {
            if filter(kind) {
                candidates.push(CompletionItem {
                    item_id: format!("node-kind:{kind}"),
                    label: (*kind).to_string(),
                    insert_text: (*kind).to_string(),
                    replace_start: partial_start,
                    replace_end: partial_end,
                    kind: CompletionKind::NodeKind,
                    detail: "node kind".into(),
                    sort_text: (*kind).to_string(),
                    source: CompletionSource::Grammar,
                    commit_characters: vec![' '],
                });
            }
        }
        return candidates;
    }

    match line_tokens.first().copied() {
        None => {
            if indented {
                for kind in &grammar.node_kinds {
                    if filter(kind) {
                        candidates.push(CompletionItem {
                            item_id: format!("node-kind:{kind}"),
                            label: (*kind).to_string(),
                            insert_text: (*kind).to_string(),
                            replace_start: partial_start,
                            replace_end: partial_end,
                            kind: CompletionKind::NodeKind,
                            detail: "node kind".into(),
                            sort_text: (*kind).to_string(),
                            source: CompletionSource::Grammar,
                            commit_characters: vec![' '],
                        });
                    }
                }
            } else {
                for keyword in &grammar.keywords {
                    if filter(keyword) {
                        candidates.push(CompletionItem {
                            item_id: format!("keyword:{keyword}"),
                            label: (*keyword).to_string(),
                            insert_text: (*keyword).to_string(),
                            replace_start: partial_start,
                            replace_end: partial_end,
                            kind: CompletionKind::Keyword,
                            detail: "top-level statement".into(),
                            sort_text: (*keyword).to_string(),
                            source: CompletionSource::Grammar,
                            commit_characters: vec![' '],
                        });
                    }
                }
            }
        }
        Some("input") => {
            // `input <key> <kind> ...`: the kind slot follows the key.
            let kind_slot = match line_tokens.len() {
                0 | 1 => false,
                2 => true,
                _ => line_tokens[2] == partial,
            };
            if kind_slot {
                for kind in &grammar.input_kinds {
                    if filter(kind) {
                        candidates.push(CompletionItem {
                            item_id: format!("input-kind:{kind}"),
                            label: (*kind).to_string(),
                            insert_text: (*kind).to_string(),
                            replace_start: partial_start,
                            replace_end: partial_end,
                            kind: CompletionKind::InputKind,
                            detail: "input kind".into(),
                            sort_text: (*kind).to_string(),
                            source: CompletionSource::Grammar,
                            commit_characters: vec![' '],
                        });
                    }
                }
            }
        }
        Some(_) if !indented && line_tokens.len() == 1 => {
            for keyword in &grammar.keywords {
                if filter(keyword) {
                    candidates.push(CompletionItem {
                        item_id: format!("keyword:{keyword}"),
                        label: (*keyword).to_string(),
                        insert_text: (*keyword).to_string(),
                        replace_start: partial_start,
                        replace_end: partial_end,
                        kind: CompletionKind::Keyword,
                        detail: "top-level statement".into(),
                        sort_text: (*keyword).to_string(),
                        source: CompletionSource::Grammar,
                        commit_characters: vec![' '],
                    });
                }
            }
        }
        Some(_) => {}
    }

    candidates.sort_by(|a, b| a.label.cmp(&b.label));
    candidates.dedup_by(|a, b| a.label == b.label);
    candidates
}

/// The trailing partial token after the last whitespace before the cursor.
fn current_partial(prefix: &str) -> String {
    prefix
        .rsplit_once(char::is_whitespace)
        .map_or(prefix, |(_, tail)| tail)
        .to_string()
}

/// Static keyword completions for non-Flow languages (TS / Rust / C++).
/// The cursor's trailing identifier is the filter prefix; LSP completions
/// supersede these at the runtime layer when a server is connected.
pub fn keyword_completions(
    buffer: &TextBuffer,
    kind: crate::languages::LanguageKind,
    position: Position,
) -> Vec<CompletionItem> {
    let Some(line_text) = buffer.line(position.line) else {
        return Vec::new();
    };
    let column = position.column.min(line_text.chars().count() as u32) as usize;
    let prefix: String = line_text.chars().take(column).collect();
    let partial = trailing_word(&prefix);
    let partial_start = Position::new(
        position.line,
        column
            .saturating_sub(partial.chars().count())
            .try_into()
            .unwrap_or(u32::MAX),
    );
    let partial_end = Position::new(position.line, column as u32);
    let typed = partial.to_ascii_lowercase();
    let mut candidates = Vec::new();
    for keyword in crate::languages::keywords::keywords_for(kind) {
        if typed.is_empty() || keyword.starts_with(&typed) {
            candidates.push(CompletionItem {
                item_id: format!("keyword:{keyword}"),
                label: (*keyword).to_string(),
                insert_text: (*keyword).to_string(),
                replace_start: partial_start,
                replace_end: partial_end,
                kind: CompletionKind::Keyword,
                detail: format!("{} keyword", kind.name()),
                sort_text: (*keyword).to_string(),
                source: CompletionSource::Grammar,
                commit_characters: vec![' '],
            });
        }
    }
    candidates
}

/// The trailing identifier-ish token of `prefix` (letters, digits, `_`).
fn trailing_word(prefix: &str) -> String {
    let mut word = String::new();
    for ch in prefix.chars().rev() {
        if ch.is_alphanumeric() || ch == '_' {
            word.insert(0, ch);
        } else {
            break;
        }
    }
    word
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::nui_flow_default;

    fn setup() -> (TextBuffer, FlowGrammar, SymbolIndex) {
        let source = "\
input can_publish bool default false
input amount f32 default 0.5
surface workbench column w 400 h 300
  button publish value \"Publish\" enabled $can_publish event asset.review.publish
";
        let buffer = TextBuffer::from_str(source);
        let grammar = nui_flow_default();
        let symbols = SymbolIndex::build(&buffer, &grammar);
        (buffer, grammar, symbols)
    }

    #[test]
    fn line_start_suggests_top_level_keywords() {
        let (buffer, grammar, symbols) = setup();
        let items = completions(&buffer, &grammar, &symbols, Position::new(4, 0));
        assert!(
            items
                .iter()
                .any(|item| item.label == "input" && item.kind == CompletionKind::Keyword)
        );
        assert!(
            !items
                .iter()
                .any(|item| item.kind == CompletionKind::NodeKind)
        );
    }

    #[test]
    fn indented_line_start_suggests_node_kinds() {
        let (mut buffer, grammar, symbols) = setup();
        buffer.set_text(&format!("{}\n  ", buffer.text()));
        let last = buffer.line_count() - 1;
        let column = buffer.line_char_len(last);
        let items = completions(&buffer, &grammar, &symbols, Position::new(last, column));
        assert!(
            items
                .iter()
                .any(|item| item.label == "slider" && item.kind == CompletionKind::NodeKind)
        );
    }

    #[test]
    fn node_line_suggests_attributes_and_skips_used_ones() {
        let (mut buffer, grammar, symbols) = setup();
        buffer.set_text(&format!(
            "{}\n  code_editor doc source $doc line_numbers true ",
            buffer.text()
        ));
        let last = buffer.line_count() - 1;
        let column = buffer.line_char_len(last);
        let items = completions(&buffer, &grammar, &symbols, Position::new(last, column));
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"wrap"));
        assert!(labels.contains(&"language"));
        assert!(!labels.contains(&"line_numbers"));
        assert!(!labels.contains(&"value"));
        assert!(
            items
                .iter()
                .all(|item| item.kind == CompletionKind::Attribute)
        );
    }

    #[test]
    fn mid_token_filters_attributes_by_prefix() {
        let (mut buffer, grammar, symbols) = setup();
        buffer.set_text(&format!("{}\n  code_editor doc tab", buffer.text()));
        let last = buffer.line_count() - 1;
        let column = buffer.line_char_len(last);
        let items = completions(&buffer, &grammar, &symbols, Position::new(last, column));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "tab_size");
    }

    #[test]
    fn typescript_keywords_filter_by_prefix() {
        let mut buffer = TextBuffer::default();
        buffer.insert(Position::new(0, 0), "const t = 1\n");
        let items = keyword_completions(
            &buffer,
            crate::languages::LanguageKind::Typescript,
            Position::new(0, 5),
        );
        assert!(items
            .iter()
            .any(|i| i.label == "const" && i.kind == CompletionKind::Keyword));
        assert!(!items
            .iter()
            .any(|i| i.label == "let" && i.kind == CompletionKind::Keyword));
    }

    #[test]
    fn dollar_prefix_suggests_declared_inputs() {
        let (buffer, grammar, symbols) = setup();
        // `$can_publish` starts at char column 41 on the button line.
        let items = completions(&buffer, &grammar, &symbols, Position::new(3, 45));
        assert!(
            items
                .iter()
                .any(|item| item.label == "$can_publish" && item.kind == CompletionKind::Input)
        );
        assert!(!items.iter().any(|item| item.label == "$amount"));
    }

    #[test]
    fn input_kind_position_suggests_kinds() {
        let (mut buffer, grammar, symbols) = setup();
        buffer.set_text(&format!("{}\ninput fresh ", buffer.text()));
        let last = buffer.line_count() - 1;
        let column = buffer.line_char_len(last);
        let items = completions(&buffer, &grammar, &symbols, Position::new(last, column));
        assert!(
            items
                .iter()
                .any(|item| item.label == "bool" && item.kind == CompletionKind::InputKind)
        );
    }

    #[test]
    fn no_completion_inside_string() {
        let (buffer, grammar, symbols) = setup();
        // Column 27 sits inside `"Publish"` on the button line.
        let items = completions(&buffer, &grammar, &symbols, Position::new(3, 27));
        assert!(items.is_empty());
    }
}
