//! Headless code editor kernel, split out of Neon3 as a language-agnostic
//! library.
//!
//! Pure Rust, no I/O, no window, no GPU. The kernel is grammar-agnostic: NUI
//! Flow uses the built-in table-driven [`grammar::FlowGrammar`]; other
//! languages (TypeScript, Rust, C++) plug in a tree-sitter grammar through
//! [`languages::Language`] and produce the same [`highlight::LineTokens`]
//! representation, so the highlight / render pipeline is shared. Completion
//! and diagnostics for non-Flow languages are delegated to an LSP server via
//! the [`lsp`] client bridge.

pub mod buffer;
pub mod completion;
pub mod edits;
pub mod grammar;
pub mod highlight;
pub mod languages;
pub mod lsp;
pub mod symbols;

pub use buffer::{Position, TextBuffer};
pub use completion::{CompletionItem, CompletionKind, CompletionSource};
pub use edits::{ChangeSet, EditOp, EditSession};
pub use grammar::FlowGrammar;
use grammar::nui_flow_default;
pub use highlight::{HighlightCache, LineTokens, Span, TokenClass};
pub use languages::{Language, LanguageKind};
pub use lsp::{
    LspClient, LspDiagnostic, LspEndpoint, LspError, LspLocation, LspPosition, LspRange, LspSymbol,
};
pub use symbols::{SymbolIndex, SymbolKind};

/// Facade tying buffer, highlight cache, symbol index, and the edit session
/// together. This is the type embedders hold; individual modules stay usable
/// on their own for tests and tooling.
/// One character-level edit the renderer can turn into a transient shader
/// (delete -> fragment burst, insert -> type-in). Recorded at the moment the
/// edit lands so the renderer knows *what* changed; it resolves screen
/// positions from the current layout, so no editor-core dependency on fonts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditEvent {
    pub kind: EditEventKind,
    /// Line of the edit (0-based).
    pub row: u32,
    /// Column of the edit (0-based, char units).
    pub column: u32,
    /// Inserted text (Insert) or the exact deleted text (Delete).
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditEventKind {
    Insert,
    Delete,
}

pub struct EditorCore {
    buffer: TextBuffer,
    grammar: FlowGrammar,
    language: Language,
    highlight: HighlightCache,
    symbols: SymbolIndex,
    session: EditSession,
    /// Text as last handed to (or adopted from) the host. When undo/redo or
    /// adoption diverges the buffer from this mirror, the next ChangeSet is a
    /// full resync instead of incremental ops.
    host_text: String,
    /// Character-level edits since the renderer last took them, front to
    /// back. Bounded FIFO: the renderer drains it every frame, so it stays
    /// tiny; the cap only guards against a stalled renderer.
    edit_events: std::collections::VecDeque<EditEvent>,
}

impl EditorCore {
    pub fn new(source: &str, grammar: FlowGrammar) -> Self {
        Self::new_with_language(source, grammar, Language::nui_flow())
    }

    /// Creates an editor for an arbitrary language (NUI Flow or a
    /// tree-sitter grammar). The grammar argument is the NUI Flow table
    /// (ignored for non-Flow languages; kept for API symmetry).
    pub fn from_language(source: &str, language: Language) -> Self {
        Self::new_with_language(source, nui_flow_default(), language)
    }

    fn new_with_language(source: &str, grammar: FlowGrammar, language: Language) -> Self {
        let buffer = TextBuffer::from_str(source);
        let highlight = match language.kind {
            LanguageKind::NuiFlow => HighlightCache::rebuild(&buffer, &grammar),
            _ => HighlightCache::rebuild_tokens(language.tokenize(&buffer)),
        };
        let symbols = match language.kind {
            LanguageKind::NuiFlow => SymbolIndex::build(&buffer, &grammar),
            _ => SymbolIndex::default(),
        };
        Self {
            buffer,
            grammar,
            language,
            highlight,
            symbols,
            session: EditSession::default(),
            host_text: source.to_string(),
            edit_events: std::collections::VecDeque::new(),
        }
    }

    /// The underlying editable text buffer.
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
        self.rebuild_derived();
        self.session.set_revision(revision);
        self.host_text = source.to_string();
    }

    fn rebuild_derived(&mut self) {
        match self.language.kind {
            LanguageKind::NuiFlow => {
                self.highlight = HighlightCache::rebuild(&self.buffer, &self.grammar);
                self.symbols = SymbolIndex::build(&self.buffer, &self.grammar);
            }
            _ => {
                self.highlight =
                    HighlightCache::rebuild_tokens(self.language.tokenize(&self.buffer));
                self.symbols = SymbolIndex::default();
            }
        }
    }

    /// Clamp any position into the buffer's valid range (SDK convenience;
    /// the render layer uses this after undo/redo and pointer moves).
    pub fn clamp_position(&self, position: Position) -> Position {
        self.buffer.clamp_position(position)
    }

    pub fn insert(&mut self, position: Position, text: &str) -> Position {
        let at = self.buffer.clamp_position(position);
        let past = self.buffer.insert(at, text);
        self.after_edit(at.line);
        self.session.record_insert(at, past, text);
        if !text.is_empty() {
            self.edit_events.push_back(EditEvent {
                kind: EditEventKind::Insert,
                row: at.line,
                column: at.column,
                text: text.to_string(),
            });
            Self::trim_edit_events(&mut self.edit_events);
        }
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
        if !deleted.is_empty() {
            self.edit_events.push_back(EditEvent {
                kind: EditEventKind::Delete,
                row: start.line,
                column: start.column,
                text: deleted,
            });
            Self::trim_edit_events(&mut self.edit_events);
        }
        removed
    }

    /// Undo the last session step, returns the new caret position.
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

    /// Drains the pending character-level edits. Called by the renderer once
    /// per frame before layout; events not consumed here are lost, so the
    /// renderer must call this exactly once per redraw.
    pub fn take_edit_events(&mut self) -> Vec<EditEvent> {
        self.edit_events.drain(..).collect()
    }

    fn trim_edit_events(events: &mut std::collections::VecDeque<EditEvent>) {
        const MAX_EDIT_EVENTS: usize = 256;
        while events.len() > MAX_EDIT_EVENTS {
            events.pop_front();
        }
    }

    pub fn completions(&self, position: Position) -> Vec<CompletionItem> {
        match self.language.kind {
            LanguageKind::NuiFlow => completion::completions(
                &self.buffer,
                &self.grammar,
                &self.symbols,
                position,
            ),
            // Non-Flow languages resolve completions through the LSP client
            // bridge at the runtime layer, not inside the kernel.
            _ => Vec::new(),
        }
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
        match self.language.kind {
            LanguageKind::NuiFlow => {
                self.highlight
                    .update(&self.buffer, &self.grammar, first_line);
                self.symbols = SymbolIndex::build(&self.buffer, &self.grammar);
            }
            _ => {
                // tree-sitter path: full re-parse per edit for now; a
                // fine-grained incremental re-parse is a later optimization.
                self.highlight =
                    HighlightCache::rebuild_tokens(self.language.tokenize(&self.buffer));
                self.symbols = SymbolIndex::default();
            }
        }
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
