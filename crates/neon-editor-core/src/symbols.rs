//! Document symbol index: declared inputs, nodes, machines, states,
//! text styles, and emitted intents. Rebuilt incrementally-cheaply (one linear
//! scan) after edits; sized for the 10k-line budget in the design doc.

use crate::buffer::TextBuffer;
use crate::grammar::FlowGrammar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Input,
    Surface,
    Node,
    Machine,
    State,
    TextStyle,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SymbolIndex {
    /// Declared input keys, nodes, machines, dotted `machine.state` names,
    /// text style keys, and dotted intents seen after `event` / `emit` / `on`.
    inputs: Vec<String>,
    nodes: Vec<String>,
    machines: Vec<String>,
    states: Vec<String>,
    text_styles: Vec<String>,
    intents: Vec<String>,
}

impl SymbolIndex {
    pub fn build(buffer: &TextBuffer, grammar: &FlowGrammar) -> Self {
        let mut index = SymbolIndex::default();
        for line in buffer.lines() {
            let trimmed = strip_comment(line).trim();
            if trimmed.is_empty() {
                continue;
            }
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            match tokens.first().copied() {
                Some("input") if tokens.len() >= 2 => {
                    index.inputs.push(tokens[1].to_string());
                }
                Some("surface") if tokens.len() >= 2 => {}
                Some("machine") if tokens.len() >= 2 => {
                    index.machines.push(tokens[1].to_string());
                }
                Some("state") if tokens.len() >= 3 => {
                    index.states.push(format!("{}.{}", tokens[1], tokens[2]));
                }
                Some("text_style") if tokens.len() >= 2 => {
                    index.text_styles.push(tokens[1].to_string());
                }
                Some(first) if grammar.is_node_kind(first) && tokens.len() >= 2 => {
                    index.nodes.push(tokens[1].to_string());
                }
                _ => {}
            }
            for (position, token) in tokens.iter().enumerate() {
                let introduces_intent = position > 0
                    && (tokens[position - 1] == "event"
                        || tokens[position - 1] == "emit"
                        || tokens[position - 1] == "on");
                if introduces_intent && token.contains('.') {
                    let intent = token.trim_end_matches(|c: char| {
                        !c.is_ascii_alphanumeric() && c != '.' && c != '_'
                    });
                    if !intent.is_empty() {
                        index.intents.push(intent.to_string());
                    }
                }
            }
        }
        index.inputs.sort();
        index.inputs.dedup();
        index.nodes.sort();
        index.nodes.dedup();
        index.machines.sort();
        index.machines.dedup();
        index.states.sort();
        index.states.dedup();
        index.text_styles.sort();
        index.text_styles.dedup();
        index.intents.sort();
        index.intents.dedup();
        index
    }

    pub fn inputs(&self) -> &[String] {
        &self.inputs
    }

    pub fn nodes(&self) -> &[String] {
        &self.nodes
    }

    pub fn machines(&self) -> &[String] {
        &self.machines
    }

    pub fn states(&self) -> &[String] {
        &self.states
    }

    pub fn text_styles(&self) -> &[String] {
        &self.text_styles
    }

    pub fn intents(&self) -> &[String] {
        &self.intents
    }
}

/// Strips a trailing `#` comment with the same rules as the Flow lexer
/// (token-boundary `#`, color literals excluded, quotes respected).
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    let mut at_token_start = true;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            at_token_start = false;
            continue;
        }
        if quoted {
            if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
                at_token_start = false;
            }
            continue;
        }
        if character == '"' {
            quoted = true;
            at_token_start = false;
            continue;
        }
        if character.is_whitespace() {
            at_token_start = true;
            continue;
        }
        if character == '#' && at_token_start && !starts_color_literal(&line[index + 1..]) {
            return &line[..index];
        }
        at_token_start = false;
    }
    line
}

fn starts_color_literal(rest: &str) -> bool {
    let mut count = 0usize;
    for character in rest.chars() {
        if character.is_ascii_hexdigit() && count < 8 {
            count += 1;
        } else {
            break;
        }
    }
    (count == 6 || count == 8) && rest.chars().nth(count).is_none_or(|c| c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::nui_flow_default;

    #[test]
    fn collects_symbols_and_intents() {
        let source = "\
input can_publish bool default false
# a comment line
surface workbench column w 400 h 300
  text title value \"Review\"
  button publish value \"Publish\" enabled $can_publish event asset.review.publish
machine review initial loading
state review ready
text_style neon-title fill #6EF3C5
";
        let buffer = TextBuffer::from_str(source);
        let index = SymbolIndex::build(&buffer, &nui_flow_default());
        assert_eq!(index.inputs(), vec!["can_publish"]);
        assert_eq!(index.nodes(), vec!["publish", "title"]);
        assert_eq!(index.machines(), vec!["review"]);
        assert_eq!(index.states(), vec!["review.ready"]);
        assert_eq!(index.text_styles(), vec!["neon-title"]);
        assert_eq!(index.intents(), vec!["asset.review.publish"]);
    }
}
