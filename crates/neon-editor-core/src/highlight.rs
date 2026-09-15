//! NUI Flow line tokenizer and incremental highlight cache.
//!
//! Token rules mirror `neon-ui-runtime`'s lexer: whitespace-separated tokens,
//! quoted strings with backslash escapes, `#` line comments at token
//! boundaries, and `#RRGGBB` / `#RRGGBBAA` color literals. The cache keeps one
//! token vector per line plus the lexical state at end-of-line; an edit
//! re-tokenizes the affected line and cascades forward only while the
//! end-of-line state changes, so typing in one line stays O(line).

use crate::buffer::TextBuffer;
use crate::grammar::FlowGrammar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenClass {
    Keyword,
    NodeKind,
    NodeKey,
    Attribute,
    InputRef,
    ColorLiteral,
    NumericLiteral,
    StringLiteral,
    Intent,
    Comment,
    Ident,
}

impl TokenClass {
    /// Stable string key used by renderer-side `token_shader` lookups. The
    /// spelling is part of the NUI Flow declaration contract (`token_shader
    /// Keyword ...`), so it must not change casually.
    pub fn name(self) -> &'static str {
        match self {
            TokenClass::Keyword => "Keyword",
            TokenClass::NodeKind => "NodeKind",
            TokenClass::NodeKey => "NodeKey",
            TokenClass::Attribute => "Attribute",
            TokenClass::InputRef => "InputRef",
            TokenClass::ColorLiteral => "ColorLiteral",
            TokenClass::NumericLiteral => "NumericLiteral",
            TokenClass::StringLiteral => "StringLiteral",
            TokenClass::Intent => "Intent",
            TokenClass::Comment => "Comment",
            TokenClass::Ident => "Ident",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Char offset of the first character of the span within its line.
    pub start: u32,
    /// Char length of the span.
    pub len: u32,
    pub class: TokenClass,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineState {
    /// The line ends inside a quoted string (kept for pathological input;
    /// valid Flow strings terminate on their line).
    pub in_string: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineTokens {
    pub spans: Vec<Span>,
    pub state: LineState,
}

impl LineTokens {
    pub fn class_at(&self, column: u32) -> Option<TokenClass> {
        self.spans
            .iter()
            .find(|span| column >= span.start && column < span.start + span.len)
            .map(|span| span.class)
    }
}

/// Tokenizes one line. `previous_class`/`current_node_kind` drive attribute
/// and node-key classification from the tokens already seen on this line.
pub fn tokenize_line(line: &str, mut state: LineState, grammar: &FlowGrammar) -> LineTokens {
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut index = 0usize;
    let mut previous_class: Option<TokenClass> = None;
    let mut node_kind_in_line: Option<String> = None;

    while index < chars.len() {
        let character = chars[index];
        if character.is_whitespace() {
            index += 1;
            continue;
        }

        // Comment: `#` at token boundary that is not a color literal.
        if character == '#'
            && (index == 0 || chars[index - 1].is_whitespace())
            && !starts_color_literal(&chars[index + 1..])
        {
            spans.push(Span {
                start: index as u32,
                len: (chars.len() - index) as u32,
                class: TokenClass::Comment,
            });
            state.in_string = false;
            return LineTokens { spans, state };
        }

        // Quoted string (escapes handled like the parser lexer).
        if character == '"' {
            let start = index;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' {
                    index += 2;
                    continue;
                }
                if chars[index] == '"' {
                    index += 1;
                    break;
                }
                index += 1;
            }
            spans.push(Span {
                start: start as u32,
                len: (index.min(chars.len()) - start) as u32,
                class: TokenClass::StringLiteral,
            });
            state.in_string = index >= chars.len() && chars.last().copied() != Some('"');
            previous_class = Some(TokenClass::StringLiteral);
            continue;
        }
        if state.in_string {
            // Continuation of a string opened on an earlier line.
            let start = index;
            while index < chars.len() && chars[index] != '"' {
                index += 1;
            }
            let closed = index < chars.len();
            if closed {
                index += 1;
            }
            spans.push(Span {
                start: start as u32,
                len: (index - start) as u32,
                class: TokenClass::StringLiteral,
            });
            state.in_string = !closed;
            continue;
        }

        let start = index;
        while index < chars.len() && !chars[index].is_whitespace() {
            index += 1;
        }
        let token: String = chars[start..index].iter().collect();
        let class = classify(&token, previous_class, &node_kind_in_line, grammar);
        if class == TokenClass::NodeKind && node_kind_in_line.is_none() {
            node_kind_in_line = Some(token);
        }
        previous_class = Some(class);
        spans.push(Span {
            start: start as u32,
            len: (index - start) as u32,
            class,
        });
    }

    LineTokens { spans, state }
}

/// A single classification rule for the rule-table driven highlighter.
/// Rules are tried in order; the first one that matches wins. Rules that
/// inspect grammar tables (`Keyword`, `NodeKind`, `AttributeOfNodeKind`) stay
/// grammar-agnostic, so a different language only supplies a different table
/// plus its own rule list -- the classify() kernel never changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassifyRule {
    /// Token starts with a literal prefix.
    StartsWith(&'static str, TokenClass),
    /// Plain numeric literal or typed range (`i32:0..24`).
    Numeric,
    /// Dotted lowercase intent (`asset.review.publish`).
    Intent,
    /// `#` followed by 6/8 hex digits at a token boundary.
    HexColor,
    /// Present in the grammar keyword table.
    Keyword,
    /// Present in the grammar node-kind table.
    NodeKind,
    /// Token directly follows a NodeKind token on the same line.
    NodeKeyAfterNodeKind,
    /// Listed as an attribute of the line's current node kind.
    AttributeOfNodeKind,
    /// Terminal fallback.
    Fallback(TokenClass),
}

fn starts_color_literal(rest: &[char]) -> bool {
    let mut count = 0usize;
    for character in rest {
        if character.is_ascii_hexdigit() && count < 8 {
            count += 1;
        } else {
            break;
        }
    }
    if count != 6 && count != 8 {
        return false;
    }
    rest.get(count)
        .is_none_or(|character| character.is_whitespace())
}

/// Classifies one whitespace-separated token by walking the grammar's rule
/// table in order. The old hand-written if-else chain is now data: NUI Flow
/// keeps the same precedence by ordering its rules (prefix `$`/`#` first,
/// color before generic `#` ident, context rules before the fallback).
pub fn classify(
    token: &str,
    previous_class: Option<TokenClass>,
    node_kind_in_line: &Option<String>,
    grammar: &FlowGrammar,
) -> TokenClass {
    for rule in &grammar.classify_rules {
        let class = match rule {
            ClassifyRule::StartsWith(prefix, class) => {
                token.starts_with(prefix).then_some(*class)
            }
            ClassifyRule::Numeric => {
                is_numeric_token(token).then_some(TokenClass::NumericLiteral)
            }
            ClassifyRule::Intent => is_intent_token(token).then_some(TokenClass::Intent),
            ClassifyRule::HexColor => {
                let rest = token.chars().skip(1).collect::<Vec<char>>();
                (token.starts_with('#') && starts_color_literal(&rest))
                    .then_some(TokenClass::ColorLiteral)
            }
            ClassifyRule::Keyword => {
                grammar.is_keyword(token).then_some(TokenClass::Keyword)
            }
            ClassifyRule::NodeKind => {
                grammar.is_node_kind(token).then_some(TokenClass::NodeKind)
            }
            // The token directly after a node kind is the node's semantic key.
            ClassifyRule::NodeKeyAfterNodeKind => {
                (previous_class == Some(TokenClass::NodeKind)).then_some(TokenClass::NodeKey)
            }
            ClassifyRule::AttributeOfNodeKind => node_kind_in_line.as_ref().and_then(|kind| {
                grammar
                    .is_attribute_of(kind, token)
                    .then_some(TokenClass::Attribute)
            }),
            ClassifyRule::Fallback(class) => Some(*class),
        };
        if let Some(class) = class {
            return class;
        }
    }
    TokenClass::Ident
}

fn is_numeric_token(token: &str) -> bool {
    if token.parse::<f64>().is_ok() {
        return true;
    }
    // Typed range form: `i32:0..24`, `f32:0..1`, `u32:0..100`.
    if let Some((kind, range)) = token.split_once(':') {
        if !matches!(kind, "i32" | "u32" | "f32") {
            return false;
        }
        if let Some((minimum, maximum)) = range.split_once("..") {
            return !minimum.is_empty()
                && !maximum.is_empty()
                && minimum.parse::<f64>().is_ok()
                && maximum.parse::<f64>().is_ok();
        }
    }
    false
}

fn is_intent_token(token: &str) -> bool {
    if !token.contains('.') || token.ends_with('.') {
        return false;
    }
    token.split('.').all(|segment| {
        !segment.is_empty()
            && segment.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
            })
    })
}

/// Per-line highlight cache with cascade retokenization.
#[derive(Clone, Debug, Default)]
pub struct HighlightCache {
    lines: Vec<LineTokens>,
}

impl HighlightCache {
    pub fn rebuild(buffer: &TextBuffer, grammar: &FlowGrammar) -> Self {
        let mut state = LineState::default();
        let mut lines = Vec::with_capacity(buffer.line_count() as usize);
        for line in buffer.lines() {
            let tokens = tokenize_line(line, state, grammar);
            state = tokens.state;
            lines.push(tokens);
        }
        Self { lines }
    }

    pub fn line(&self, line: u32) -> Option<&LineTokens> {
        self.lines.get(line as usize)
    }

    pub fn line_count(&self) -> u32 {
        self.lines.len() as u32
    }

    /// Re-tokenizes starting at `first_changed` (line whose text changed or
    /// where lines were inserted/removed) and cascades forward while the
    /// end-of-line state differs from the cached state. When the line count
    /// changed (inserted/removed lines), every following line's index moved,
    /// so the cascade runs to the end of the buffer instead of stopping at
    /// the first state match.
    pub fn update(&mut self, buffer: &TextBuffer, grammar: &FlowGrammar, first_changed: u32) {
        let structural = self.lines.len() as u64 != buffer.line_count() as u64;
        let mut state = if first_changed == 0 {
            LineState::default()
        } else {
            self.lines
                .get(first_changed as usize - 1)
                .map_or(LineState::default(), |tokens| tokens.state)
        };
        let mut line = first_changed as usize;
        while line < buffer.line_count() as usize {
            let line_u32 = line as u32;
            let tokens = tokenize_line(buffer.line(line_u32).unwrap_or(""), state, grammar);
            state = tokens.state;
            if line < self.lines.len() {
                let stop_early = !structural && self.lines[line].state == state;
                self.lines[line] = tokens;
                if stop_early {
                    break;
                }
            } else {
                self.lines.push(tokens);
            }
            line += 1;
        }
        self.lines.truncate(buffer.line_count() as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Position;
    use crate::grammar::nui_flow_default;

    fn spans_text<'a>(line: &'a str, tokens: &'a LineTokens, class: TokenClass) -> Vec<&'a str> {
        tokens
            .spans
            .iter()
            .filter(|span| span.class == class)
            .map(|span| {
                let start = span.start as usize;
                let end = start + span.len as usize;
                &line[start..end]
            })
            .collect()
    }

    #[test]
    fn classifies_nui_flow_line() {
        let grammar = nui_flow_default();
        let line = "panel toolbar row h 44 fill #203040 # right side";
        let tokens = tokenize_line(line, LineState::default(), &grammar);
        let text = |class| spans_text(line, &tokens, class);
        assert_eq!(text(TokenClass::NodeKind), vec!["panel"]);
        assert_eq!(text(TokenClass::NodeKey), vec!["toolbar"]);
        assert_eq!(text(TokenClass::Attribute), vec!["h", "fill"]);
        assert_eq!(text(TokenClass::NumericLiteral), vec!["44"]);
        assert_eq!(text(TokenClass::ColorLiteral), vec!["#203040"]);
        assert_eq!(text(TokenClass::Comment), vec!["# right side"]);
    }

    #[test]
    fn strings_input_refs_and_intents() {
        let grammar = nui_flow_default();
        let line = "button publish value \"Publish it\" enabled $can event asset.review.publish";
        let tokens = tokenize_line(line, LineState::default(), &grammar);
        let text = |class| spans_text(line, &tokens, class);
        assert_eq!(text(TokenClass::StringLiteral), vec!["\"Publish it\""]);
        assert_eq!(text(TokenClass::InputRef), vec!["$can"]);
        assert_eq!(text(TokenClass::Intent), vec!["asset.review.publish"]);
    }

    #[test]
    fn comment_hash_inside_string_wins() {
        let grammar = nui_flow_default();
        let tokens = tokenize_line("text t value \"a # b\"", LineState::default(), &grammar);
        assert!(
            tokens
                .spans
                .iter()
                .all(|span| span.class != TokenClass::Comment)
        );
    }

    #[test]
    fn typed_range_is_numeric() {
        let grammar = nui_flow_default();
        let tokens = tokenize_line(
            "slider amount numeric $a i32:0..24",
            LineState::default(),
            &grammar,
        );
        let text = |class| spans_text("slider amount numeric $a i32:0..24", &tokens, class);
        assert_eq!(text(TokenClass::NumericLiteral), vec!["i32:0..24"]);
    }

    #[test]
    fn update_retokens_only_changed_line_unless_state_changes() {
        let grammar = nui_flow_default();
        let mut buffer = TextBuffer::from_str("surface root w 10 h 10\n  text a value \"x\"\n");
        let mut cache = HighlightCache::rebuild(&buffer, &grammar);
        buffer.insert(Position::new(1, 17), "y");
        cache.update(&buffer, &grammar, 1);
        assert_eq!(cache.line_count(), buffer.line_count());
        let tokens = cache.line(1).unwrap();
        let line = buffer.line(1).unwrap();
        assert_eq!(
            spans_text(line, tokens, TokenClass::StringLiteral),
            vec!["\"xy\""]
        );
    }
}
