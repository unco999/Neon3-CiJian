//! Headless code editor kernel for Neon3.
//!
//! Pure Rust, no I/O, no window, no GPU. Links into both `neon-ui-runtime`
//! (frame validation) and `neon-wgpu-runtime` (frame-rate local interaction)
//! plus CLI tools, per `docs/nui-flow-code-editor.md`. The kernel is
//! grammar-agnostic: a [`grammar::FlowGrammar`] is plain data supplied by the
//! embedder, so NUI Flow grammar stays owned by `neon-ui-schema` (slice 3)
//! without a dependency edge from this crate.

pub mod buffer;
pub mod completion;
pub mod edits;
pub mod grammar;
pub mod highlight;
pub mod symbols;

pub use buffer::{Position, TextBuffer};
pub use completion::{CompletionItem, CompletionKind, CompletionSource};
pub use edits::{ChangeSet, EditOp, EditSession};
pub use grammar::FlowGrammar;
pub use highlight::{HighlightCache, LineTokens, Span, TokenClass};
pub use symbols::{SymbolIndex, SymbolKind};

/// Facade tying buffer, highlight cache, symbol index, and the edit session
/// together. This is the type embedders hold; individual modules stay usable
/// on their own for tests and tooling.
pub struct EditorCore {
    buffer: TextBuffer,
    grammar: FlowGrammar,
    highlight: HighlightCache,
    symbols: SymbolIndex,
    session: EditSession,
    /// Text as last handed to (or adopted from) the host. When undo/redo or
    /// adoption diverges the buffer from this mirror, the next ChangeSet is a
    /// full resync instead of incremental ops.
    host_text: String,
}

impl EditorCore {
    pub fn new(source: &str, grammar: FlowGrammar) -> Self {
        let buffer = TextBuffer::from_str(source);
        let highlight = HighlightCache::rebuild(&buffer, &grammar);
        let symbols = SymbolIndex::build(&buffer, &grammar);
        Self {
            buffer,
            grammar,
            highlight,
            symbols,
            session: EditSession::default(),
            host_text: source.to_string(),
        }
    }

    pub fn buffer(&self) -> &TextBuffer {
        &self.buffer
    }

    pub fn highlight(&self) -> &HighlightCache {
        &self.highlight
    }

    pub fn symbols(&self) -> &SymbolIndex {
        &self.symbols
    }

    pub fn session(&self) -> &EditSession {
        &self.session
    }

    pub fn grammar(&self) -> &FlowGrammar {
        &self.grammar
    }

    /// Host accepted a document frame: adopt the full text and reset the
    /// session baseline (local preview state is discarded, matching the
    /// accept/reject rules in the design doc).
    pub fn adopt_document(&mut self, source: &str, revision: u64) {
        self.buffer.set_text(source);
        self.highlight = HighlightCache::rebuild(&self.buffer, &self.grammar);
        self.symbols = SymbolIndex::build(&self.buffer, &self.grammar);
        self.session.set_revision(revision);
        self.host_text = source.to_string();
    }

    pub fn insert(&mut self, position: Position, text: &str) -> Position {
        let at = self.buffer.clamp_position(position);
        let past = self.buffer.insert(at, text);
        self.after_edit(at.line);
        self.session.record_insert(at, past, text);
        past
    }

    pub fn delete(&mut self, start: Position, end: Position) -> Position {
        let start = self.buffer.clamp_position(start);
        let end = self.buffer.clamp_position(end);
        if end <= start {
            return start;
        }
        let deleted = self.extract_range(start, end);
        let removed = self.buffer.delete(start, end);
        self.after_edit(removed.line);
        self.session.record_delete(removed, end, &deleted);
        removed
    }

    pub fn undo(&mut self) -> Option<u32> {
        let first = self.session.undo(&mut self.buffer)?;
        self.after_edit(first);
        Some(first)
    }

    pub fn redo(&mut self) -> Option<u32> {
        let first = self.session.redo(&mut self.buffer)?;
        self.after_edit(first);
        Some(first)
    }

    /// Host accepted the pending ChangeSet: the baseline moves forward.
    pub fn commit(&mut self) {
        self.session.commit();
    }

    /// Builds the next ChangeSet for the host. Incremental pending ops are
    /// the fast path; after undo/redo diverged the buffer from what the host
    /// already applied, a full delete-all + insert resync is emitted instead
    /// so the host never silently diverges.
    pub fn take_change_set(&mut self) -> Option<ChangeSet> {
        if !self.session.diverged() {
            let change_set = self.session.take_change_set()?;
            self.host_text = self.buffer.text();
            return Some(change_set);
        }
        let host_lines: Vec<&str> = self.host_text.split('\n').collect();
        let host_end_line = host_lines.len() as u32 - 1;
        let host_end_column = host_lines
            .last()
            .map_or(0, |line| line.chars().count() as u32);
        self.session.take_change_set();
        self.host_text = self.buffer.text();
        let end_line = self.buffer.line_count().saturating_sub(1);
        Some(ChangeSet {
            base_revision: self.session.revision(),
            ops: vec![
                EditOp::Delete {
                    start: Position::START,
                    end: Position::new(host_end_line, host_end_column),
                    text: String::new(),
                },
                EditOp::Insert {
                    line: 0,
                    column: 0,
                    end: Position::new(end_line, self.buffer.line_char_len(end_line)),
                    text: self.buffer.text(),
                },
            ],
        })
    }

    pub fn completions(&self, position: Position) -> Vec<CompletionItem> {
        completion::completions(&self.buffer, &self.grammar, &self.symbols, position)
    }

    /// Replaces the candidate's exact range and records the change as one
    /// local edit sequence. The host still receives the resulting ChangeSet;
    /// applying a completion never bypasses revision or idempotency handling.
    pub fn apply_completion(&mut self, item: &CompletionItem) -> Position {
        let start = self.buffer.clamp_position(item.replace_start);
        let end = self.buffer.clamp_position(item.replace_end);
        let position = self.delete(start, end);
        self.insert(position, &item.insert_text)
    }

    /// Token spans of one line for the renderer.
    pub fn line_spans(&self, line: u32) -> Option<&LineTokens> {
        self.highlight.line(line)
    }

    fn extract_range(&self, start: Position, end: Position) -> String {
        let start = self.buffer.clamp_position(start);
        let end = self.buffer.clamp_position(end);
        if end <= start {
            return String::new();
        }
        let mut text = String::new();
        for line in start.line..=end.line {
            let line_text = self.buffer.line(line).unwrap_or("");
            let chars = line_text.chars().collect::<Vec<char>>();
            let from = if line == start.line {
                start.column as usize
            } else {
                0
            };
            let to = if line == end.line {
                end.column as usize
            } else {
                chars.len()
            };
            if line > start.line {
                text.push('\n');
            }
            text.extend(chars[from.min(chars.len())..to.min(chars.len())].iter());
        }
        text
    }

    fn after_edit(&mut self, first_line: u32) {
        self.highlight
            .update(&self.buffer, &self.grammar, first_line);
        self.symbols = SymbolIndex::build(&self.buffer, &self.grammar);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::nui_flow_default;

    const DOC: &str = "\
input can_publish bool default false
surface workbench column w 400 h 300
  button publish value \"Publish\" enabled $can_publish event asset.review.publish
";

    #[test]
    fn editing_updates_highlight_and_symbols() {
        let mut editor = EditorCore::new(DOC, nui_flow_default());
        let line = editor.buffer().line_count() - 1;
        let column = editor.buffer().line_char_len(line);
        editor.insert(Position::new(line, column), "\n  text note value \"hello\"");
        assert_eq!(editor.symbols().nodes(), vec!["note", "publish"]);
        let new_line = line + 1;
        let tokens = editor.line_spans(new_line).expect("highlighted line");
        assert!(
            tokens
                .spans
                .iter()
                .any(|span| span.class == TokenClass::NodeKind)
        );
    }

    #[test]
    fn completion_contains_a_replacement_range_and_can_be_applied() {
        let mut editor = EditorCore::new(
            "version 1\nsurface root w 100 h 100\n  sli",
            nui_flow_default(),
        );
        let position = Position::new(2, 5);
        let item = editor
            .completions(position)
            .into_iter()
            .find(|item| item.label == "slider")
            .expect("slider completion");
        assert_eq!(item.replace_start, Position::new(2, 2));
        assert_eq!(item.replace_end, position);
        let end = editor.apply_completion(&item);
        assert_eq!(editor.buffer().line(2), Some("  slider"));
        assert_eq!(end, Position::new(2, 8));
    }

    #[test]
    fn change_set_reflects_pending_ops() {
        let mut editor = EditorCore::new(DOC, nui_flow_default());
        editor.insert(Position::new(0, 0), "# draft\n");
        let change_set = editor.take_change_set().expect("pending");
        assert_eq!(change_set.base_revision, 0);
        assert_eq!(change_set.ops.len(), 1);
        assert!(matches!(change_set.ops[0], EditOp::Insert { .. }));
        editor.commit();
        assert!(editor.take_change_set().is_none());
    }

    #[test]
    fn undo_restores_previous_state() {
        let mut editor = EditorCore::new(DOC, nui_flow_default());
        let before = editor.buffer().text();
        editor.insert(Position::new(0, 0), "x");
        assert_ne!(editor.buffer().text(), before);
        editor.undo();
        assert_eq!(editor.buffer().text(), before);
    }

    #[test]
    fn undo_after_flush_emits_full_resync() {
        let mut editor = EditorCore::new(DOC, nui_flow_default());
        editor.insert(Position::new(0, 0), "x");
        let first = editor.take_change_set().expect("incremental ops");
        assert_eq!(first.ops.len(), 1);
        editor.commit();
        editor.undo();
        let second = editor.take_change_set().expect("resync after undo");
        assert_eq!(second.base_revision, 1);
        assert_eq!(second.ops.len(), 2);
        assert!(matches!(second.ops[0], EditOp::Delete { .. }));
        assert!(matches!(second.ops[1], EditOp::Insert { .. }));
    }

    #[test]
    fn adopt_document_resets_revision_baseline() {
        let mut editor = EditorCore::new(DOC, nui_flow_default());
        editor.insert(Position::new(0, 0), "x");
        editor.take_change_set();
        editor.adopt_document("version 1\nsurface fresh revision 1\n", 9);
        assert_eq!(
            editor.buffer().text(),
            "version 1\nsurface fresh revision 1\n"
        );
        assert!(editor.take_change_set().is_none());
        editor.insert(Position::new(0, 0), "# note\n");
        let change_set = editor.take_change_set().expect("ops after adopt");
        assert_eq!(change_set.base_revision, 9);
    }

    #[test]
    fn delete_across_lines_reports_deleted_text() {
        let mut editor = EditorCore::new("ab\ncd", nui_flow_default());
        let deleted = editor.delete(Position::new(0, 1), Position::new(1, 1));
        assert_eq!(deleted, Position::new(0, 1));
        assert_eq!(editor.buffer().text(), "ad");
        editor.undo();
        assert_eq!(editor.buffer().text(), "ab\ncd");
    }
}
