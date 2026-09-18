//! `code_editor` UI component — editing semantics owned by the ui-runtime.
//!
//! Architecture (user-decided): the editor is a fully independent component
//! that depends on the ui-runtime, and the wgpu renderer only consumes
//! presentation data. The renderer maps pointer positions to (line, column)
//! with its font metrics and forwards `UiEditorInputEvent`s; this module owns
//! the `neon-editor` core (buffer / highlight / completion / undo / edits),
//! the view state (caret, selection, scroll, focus, preedit, transient fx,
//! font scale) and the editing semantics. Every change is projected into a
//! `UiCodeEditorPresentation` that rides a `UiEffect::CodeEditorPresentation`
//! inside the UiFragment the renderer draws.
//!
//! Nothing here touches GPU, atlas or fonts: the renderer supplies the few
//! metric numbers it needs (row height, gutter width, viewport) inside the
//! input events; text advance uses a monospaced approximation only for
//! scroll-into-view decisions (the renderer positions glyphs exactly).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use neon_editor::{
    CompletionItem, EditEvent, EditEventKind, EditorCore, Language, LanguageKind, Position,
};
use neon_ui_schema::{
    TextRef, UiCodeEditorDeclaration, UiCodeEditorPresentation, UiEditorCompletionItem,
    UiEditorCompletionSnapshot, UiEditorEditFx, UiEditorInputEvent, UiEffect, UiFragment,
    UiFragmentId, UiEditorKeyKind, UiEditorTokenSpan, UiIntent, UiNode, UiNodeKind,
};

/// Map a NUI code_editor declaration language to the kernel Language.
/// NuiFlow uses the built-in table-driven grammar; the rest use tree-sitter.
fn core_language_for(declaration: &UiCodeEditorDeclaration) -> Language {
    match declaration.language {
        neon_ui_schema::UiEditorLanguage::NuiFlow => Language::nui_flow(),
        neon_ui_schema::UiEditorLanguage::Typescript => Language {
            kind: LanguageKind::Typescript,
        },
        neon_ui_schema::UiEditorLanguage::Rust => Language {
            kind: LanguageKind::Rust,
        },
        neon_ui_schema::UiEditorLanguage::Cpp => Language {
            kind: LanguageKind::Cpp,
        },
    }
}

/// Transient-fx packages agreed with the renderer's shader registry
/// (see `nui_flow_code_editor_demo`). Type-in plays once on insert, the
/// delete fragment plays once on delete; both are removed when their
/// duration elapses.
const FX_TYPE_IN_PACKAGE: &str = "text-type-in";
const FX_DELETE_PACKAGE: &str = "text-delete-fragment";
const FX_TYPE_IN_DURATION_MS: u32 = 420;
const FX_DELETE_DURATION_MS: u32 = 620;

/// Monospaced advance factor used only for scroll-into-view / page navigation
/// metric decisions. The renderer positions glyphs with real font metrics.
const MONO_ADVANCE_FACTOR: f32 = 0.6;

/// Completion popup state (item list plus selected index).
pub struct EditorCompletionState {
    pub items: Vec<CompletionItem>,
    pub selected: usize,
}

/// Kind of a transient edit effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditFxKind {
    Insert,
    Delete,
}

/// Transient edit effect (type-in / delete fragment), projected into
/// `UiEditorEditFx` inside the presentation.
#[derive(Clone, Debug)]
pub struct EditorEditFx {
    pub kind: EditFxKind,
    pub package_id: String,
    pub row: u32,
    pub col: u32,
    pub text: String,
    pub started_seconds: f32,
    pub duration_ms: u32,
}

/// Document commit produced by the editor component (on explicit save or
/// blur), handed to the host like the renderer used to.
#[derive(Clone, Debug)]
pub struct EditorCommit {
    pub node_path: String,
    pub event_action: Option<String>,
    pub document: String,
}

/// The `code_editor` component: editing core + view state + semantics.
pub struct EditorComponent {
    pub declaration: UiCodeEditorDeclaration,
    pub core: EditorCore,
    pub event_action: Option<String>,
    pub adopted_source: String,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub caret: Position,
    pub selection_anchor: Option<Position>,
    pub focus: bool,
    pub completion: Option<EditorCompletionState>,
    pub preedit: String,
    pub last_edit_seconds: f32,
    pub pending_edits: bool,
    pub edit_fx: Vec<EditorEditFx>,
    pub font_scale: f32,
    pub revision: u64,
    pub clipboard: String,
}

impl EditorComponent {
    pub fn new(declaration: UiCodeEditorDeclaration, source: &str) -> Self {
        let core = EditorCore::from_language(source, core_language_for(&declaration));
        Self {
            declaration,
            core,
            event_action: None,
            adopted_source: source.to_string(),
            scroll_x: 0.0,
            scroll_y: 0.0,
            caret: Position::START,
            selection_anchor: None,
            focus: false,
            completion: None,
            preedit: String::new(),
            last_edit_seconds: 0.0,
            pending_edits: false,
            edit_fx: Vec::new(),
            font_scale: 1.0,
            revision: 0,
            clipboard: String::new(),
        }
    }

    /// Rebuilds the core when the declaration language or the external
    /// source changed while the editor was not focused.
    pub fn adopt_source(&mut self, source: &str) {
        if self.adopted_source == source && self.focus {
            return;
        }
        let rebuild = self.adopted_source != source || !self.focus;
        if rebuild {
            self.core = EditorCore::from_language(source, core_language_for(&self.declaration));
            self.adopted_source = source.to_string();
            self.caret = Position::START;
            self.selection_anchor = None;
            self.completion = None;
            self.scroll_x = 0.0;
            self.scroll_y = 0.0;
            self.preedit.clear();
            self.pending_edits = false;
            self.revision += 1;
        }
    }

    // ---------------------------------------------------------------- edit

    fn mark_edit(&mut self, now: f32) {
        self.pending_edits = true;
        self.last_edit_seconds = now;
    }

    fn take_edit_fx(&mut self, events: Vec<EditEvent>, now: f32) {
        for event in events {
            let (kind, package_id, duration_ms) = match event.kind {
                EditEventKind::Insert => (
                    EditFxKind::Insert,
                    FX_TYPE_IN_PACKAGE.to_string(),
                    FX_TYPE_IN_DURATION_MS,
                ),
                EditEventKind::Delete => (
                    EditFxKind::Delete,
                    FX_DELETE_PACKAGE.to_string(),
                    FX_DELETE_DURATION_MS,
                ),
            };
            self.edit_fx.push(EditorEditFx {
                kind,
                package_id,
                row: event.row,
                col: event.column,
                text: event.text,
                started_seconds: now,
                duration_ms,
            });
        }
    }

    fn insert_text(&mut self, value: &str, now: f32) {
        if let Some(anchor) = self.selection_anchor {
            let (start, end) = ordered_selection(anchor, self.caret);
            self.caret = self.core.delete(start, end);
            self.selection_anchor = None;
            self.caret = self.core.insert(self.caret, value);
        } else {
            self.caret = self.core.insert(self.caret, value);
        }
        self.completion = None;
        let events = self.core.take_edit_events();
        self.take_edit_fx(events, now);
        self.mark_edit(now);
        self.revision += 1;
    }

    /// Map an opening bracket to its closing partner, if it should be
    /// auto-closed on insertion.
    fn pair_for(ch: char) -> Option<char> {
        match ch {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            _ => None,
        }
    }

    /// Insert text from keyboard/IME. Single characters go through the
    /// bracket auto-pairing path; multi-char strings (IME commits,
    /// paste) go straight through.
    fn insert_character(&mut self, value: &str, now: f32) {
        let mut chars = value.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            self.insert_char_paired(ch, now);
        } else {
            self.insert_text(value, now);
        }
    }

    /// Insert `ch` with bracket auto-pairing. When `ch` is an opening
    /// bracket and no text is selected, the matching close bracket is
    /// inserted immediately after and the caret lands between them. If the
    /// character right after the caret is already the matching closer, we
    /// just advance the caret past it instead of duplicating it. A closing
    /// bracket that already exists right after the caret is likewise skipped.
    fn insert_char_paired(&mut self, ch: char, now: f32) {
        // With a selection, just wrap it (plain insert of the char).
        if self.selection_anchor.is_some() {
            self.insert_text(&ch.to_string(), now);
            return;
        }
        // Character immediately after the caret on the same line.
        let after = self
            .core
            .buffer()
            .line(self.caret.line)
            .and_then(|line| line.chars().nth(self.caret.column as usize));
        if let Some(close) = Self::pair_for(ch) {
            if after == Some(close) {
                // Already closed: just step over the closer.
                self.caret.column += 1;
                self.revision += 1;
                return;
            }
            // Insert "open+close", caret between them.
            let pair: String = format!("{ch}{close}");
            self.caret = self.core.insert(self.caret, &pair);
            self.caret.column -= 1;
            self.completion = None;
            let events = self.core.take_edit_events();
            self.take_edit_fx(events, now);
            self.mark_edit(now);
            self.revision += 1;
            return;
        }
        // Typing a closer that already exists right after the caret: skip it.
        if matches!(ch, ')' | ']' | '}') && after == Some(ch) {
            self.caret.column += 1;
            self.revision += 1;
            return;
        }
        self.insert_text(&ch.to_string(), now);
    }

    fn delete_backward(&mut self, now: f32) {
        if let Some(anchor) = self.selection_anchor {
            let (start, end) = ordered_selection(anchor, self.caret);
            self.caret = self.core.delete(start, end);
            self.selection_anchor = None;
            let events = self.core.take_edit_events();
            self.take_edit_fx(events, now);
            self.mark_edit(now);
            self.revision += 1;
            return;
        }
        if self.caret.column > 0 {
            let position = Position::new(self.caret.line, self.caret.column - 1);
            self.caret = self.core.delete(position, self.caret);
        } else if self.caret.line > 0 {
            let previous_len = self
                .core
                .buffer()
                .line(self.caret.line - 1)
                .map_or(0, |line| line.chars().count() as u32);
            let start = Position::new(self.caret.line - 1, previous_len);
            self.caret = self.core.delete(start, self.caret);
        } else {
            return;
        }
        self.completion = None;
        let events = self.core.take_edit_events();
        self.take_edit_fx(events, now);
        self.mark_edit(now);
        self.revision += 1;
    }

    fn delete_forward(&mut self, now: f32) {
        if let Some(anchor) = self.selection_anchor {
            let (start, end) = ordered_selection(anchor, self.caret);
            self.caret = self.core.delete(start, end);
            self.selection_anchor = None;
            let events = self.core.take_edit_events();
            self.take_edit_fx(events, now);
            self.mark_edit(now);
            self.revision += 1;
            return;
        }
        let line_len = self
            .core
            .buffer()
            .line(self.caret.line)
            .map_or(0, |line| line.chars().count() as u32);
        if self.caret.column < line_len {
            let end = Position::new(self.caret.line, self.caret.column + 1);
            self.caret = self.core.delete(self.caret, end);
        } else if self.caret.line + 1 < self.core.buffer().line_count() {
            let end = Position::new(self.caret.line + 1, 0);
            self.caret = self.core.delete(self.caret, end);
        } else {
            return;
        }
        self.completion = None;
        let events = self.core.take_edit_events();
        self.take_edit_fx(events, now);
        self.mark_edit(now);
        self.revision += 1;
    }

    fn move_caret(&mut self, to: Position, extend: bool) {
        let to = self.core.buffer().clamp_position(to);
        if extend {
            self.selection_anchor.get_or_insert(self.caret);
        } else {
            self.selection_anchor = None;
        }
        self.caret = to;
        self.revision += 1;
    }

    fn commit_pending(&mut self) -> Option<EditorCommit> {
        if !self.pending_edits {
            return None;
        }
        self.pending_edits = false;
        self.completion = None;
        self.preedit.clear();
        let document = self.core.buffer().text();
        self.core.commit();
        Some(EditorCommit {
            node_path: self.declaration.node_key.clone(),
            event_action: self.event_action.clone(),
            document,
        })
    }

    fn auto_complete(&mut self) {
        let line = self
            .core
            .buffer()
            .line(self.caret.line)
            .unwrap_or_default();
        let before = line
            .chars()
            .take(self.caret.column as usize)
            .collect::<String>();
        let ident_typed = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .count()
            > 0;
        let line_leading = !before.is_empty() && before.chars().all(|c| c.is_whitespace());
        if !ident_typed && !line_leading {
            self.completion = None;
            return;
        }
        let items = self.core.completions(self.caret);
        if items.is_empty() {
            self.completion = None;
            return;
        }
        let selected = self
            .completion
            .as_ref()
            .map_or(0, |c| c.selected.min(items.len().saturating_sub(1)));
        self.completion = Some(EditorCompletionState { items, selected });
        self.revision += 1;
    }

    fn open_completion(&mut self) {
        let items = self.core.completions(self.caret);
        self.completion = Some(EditorCompletionState { items, selected: 0 });
        self.revision += 1;
    }

    // ------------------------------------------------------------- input

    /// Routes a renderer-forwarded input event. Returns any document
    /// commits produced (explicit save / blur).
    pub fn handle_input(
        &mut self,
        event: &UiEditorInputEvent,
        now: f32,
    ) -> Vec<EditorCommit> {
        match event {
            UiEditorInputEvent::Key {
                kind,
                text,
                shift,
                ctrl,
                viewport_height,
                viewport_width,
                row_height,
                gutter_width,
                ..
            } => self.handle_key(
                kind,
                text.as_deref(),
                *shift,
                *ctrl,
                *viewport_height,
                *viewport_width,
                *row_height,
                *gutter_width,
                now,
            ),
            UiEditorInputEvent::PointerPress { line, column, .. } => {
                self.handle_pointer_press(*line, *column)
            }
            UiEditorInputEvent::PointerDrag { line, column, .. } => {
                self.handle_pointer_drag(*line, *column);
                Vec::new()
            }
            UiEditorInputEvent::PointerRelease => Vec::new(),
            UiEditorInputEvent::Scroll { delta, .. } => {
                self.handle_scroll(*delta);
                Vec::new()
            }
            UiEditorInputEvent::Zoom {
                factor,
                viewport_height,
                viewport_width,
                row_height,
                gutter_width,
                ..
            } => {
                self.handle_zoom(
                    *factor,
                    *viewport_height,
                    *viewport_width,
                    *row_height,
                    *gutter_width,
                );
                Vec::new()
            }
            UiEditorInputEvent::ImePreedit { value, .. } => {
                self.preedit = value.clone();
                self.revision += 1;
                Vec::new()
            }
            UiEditorInputEvent::ImeCommit {
                value,
                viewport_height,
                viewport_width,
                row_height,
                gutter_width,
                ..
            } => self.handle_ime_commit(
                value,
                *viewport_height,
                *viewport_width,
                *row_height,
                *gutter_width,
                now,
            ),
            UiEditorInputEvent::Reveal { .. } => Vec::new(),
        }
    }

    fn handle_pointer_press(&mut self, line: u32, column: u32) -> Vec<EditorCommit> {
        let mut commits = Vec::new();
        if !self.focus {
            commits.extend(self.commit_pending());
        }
        self.focus = true;
        self.completion = None;
        let line = line.min(self.core.buffer().line_count().saturating_sub(1));
        let line_text = self.core.buffer().line(line).unwrap_or_default();
        let column = column.min(line_text.chars().count() as u32);
        self.caret = Position::new(line, column);
        self.selection_anchor = Some(self.caret);
        self.revision += 1;
        commits
    }

    fn handle_pointer_drag(&mut self, line: u32, column: u32) {
        let line = line.min(self.core.buffer().line_count().saturating_sub(1));
        let line_text = self.core.buffer().line(line).unwrap_or_default();
        let column = column.min(line_text.chars().count() as u32);
        self.caret = Position::new(line, column);
        self.revision += 1;
    }

    fn handle_scroll(&mut self, delta: [f32; 2]) {
        self.scroll_x += delta[0];
        self.scroll_y += delta[1];
        self.scroll_x = self.scroll_x.max(0.0);
        self.scroll_y = self.scroll_y.max(0.0);
        self.revision += 1;
    }

    fn handle_zoom(
        &mut self,
        factor: f32,
        viewport_height: f32,
        viewport_width: f32,
        row_height: f32,
        gutter_width: f32,
    ) {
        let min_scale = 0.5_f32.max(12.0 / self.declaration.font_size.max(1.0));
        let max_scale = 3.0_f32.min(96.0 / self.declaration.font_size.max(1.0));
        let old = self.font_scale;
        self.font_scale = (self.font_scale * factor).clamp(min_scale, max_scale);
        if (self.font_scale - old).abs() < f32::EPSILON {
            return;
        }
        let old_row = row_height;
        let new_row = old_row * (self.font_scale / old.max(f32::EPSILON)).max(f32::EPSILON);
        if new_row > 0.0 {
            // Anchor on the top-left so zoom keeps the current line pinned.
            let anchor_y = self.scroll_y;
            self.scroll_y = (anchor_y / old_row.max(f32::EPSILON)) * new_row;
        }
        let viewport_w = (viewport_width - gutter_width).max(1.0);
        let old_advance = self.declaration.font_size * old * MONO_ADVANCE_FACTOR;
        let new_advance = self.declaration.font_size * self.font_scale * MONO_ADVANCE_FACTOR;
        if new_advance > 0.0 && old_advance > 0.0 {
            let anchor_x = self.scroll_x;
            self.scroll_x = (anchor_x / old_advance) * new_advance;
        }
        self.clamp_scroll_approx();
        self.revision += 1;
    }

    fn handle_ime_commit(
        &mut self,
        value: &str,
        viewport_height: f32,
        viewport_width: f32,
        row_height: f32,
        gutter_width: f32,
        now: f32,
    ) -> Vec<EditorCommit> {
        self.preedit.clear();
        if value.is_empty() {
            return Vec::new();
        }
        self.insert_text(value, now);
        self.auto_complete();
        self.scroll_caret_into_view(viewport_height, viewport_width, row_height, gutter_width);
        Vec::new()
    }

    fn handle_key(
        &mut self,
        kind: &UiEditorKeyKind,
        text: Option<&str>,
        shift: bool,
        ctrl: bool,
        viewport_height: f32,
        viewport_width: f32,
        row_height: f32,
        gutter_width: f32,
        now: f32,
    ) -> Vec<EditorCommit> {
        let mut commits = Vec::new();

        // Character input (including Ctrl shortcuts — winit reports Ctrl+A
        // as Character("a"), so shortcuts live here, not in the named
        // branch).
        if let UiEditorKeyKind::Character(value) = kind {
            if ctrl {
                let lower = value.to_ascii_lowercase();
                match lower.as_str() {
                    " " => self.open_completion(),
                    "a" => {
                        let last = self.core.buffer().line_count().saturating_sub(1);
                        let len = self
                            .core
                            .buffer()
                            .line(last)
                            .map_or(0, |line| line.chars().count() as u32);
                        self.selection_anchor = Some(Position::START);
                        self.caret = Position::new(last, len);
                        self.revision += 1;
                    }
                    "c" => {
                        if let Some(anchor) = self.selection_anchor {
                            let (start, end) = ordered_selection(anchor, self.caret);
                            self.clipboard = extract_range(self, start, end);
                        }
                    }
                    "x" => {
                        if let Some(anchor) = self.selection_anchor {
                            let (start, end) = ordered_selection(anchor, self.caret);
                            self.clipboard = extract_range(self, start, end);
                            self.caret = self.core.delete(start, end);
                            self.selection_anchor = None;
                            let events = self.core.take_edit_events();
            self.take_edit_fx(events, now);
                            self.mark_edit(now);
                            self.revision += 1;
                        }
                    }
                    "v" => {
                        if !self.clipboard.is_empty() {
                            let clip = self.clipboard.clone();
                            self.insert_text(&clip, now);
                            self.auto_complete();
                        }
                    }
                    "z" => {
                        if shift {
                            self.core.redo();
                        } else {
                            self.core.undo();
                        }
                        self.caret = self.core.buffer().clamp_position(self.caret);
                        self.selection_anchor = None;
                        self.completion = None;
                        let events = self.core.take_edit_events();
            self.take_edit_fx(events, now);
                        self.mark_edit(now);
                        self.revision += 1;
                    }
                    "y" => {
                        self.core.redo();
                        self.caret = self.core.buffer().clamp_position(self.caret);
                        self.selection_anchor = None;
                        self.completion = None;
                        let events = self.core.take_edit_events();
            self.take_edit_fx(events, now);
                        self.mark_edit(now);
                        self.revision += 1;
                    }
                    "s" => {
                        if let Some(commit) = self.commit_pending() {
                            commits.push(commit);
                        }
                    }
                    _ => return Vec::new(),
                }
            } else if let Some(inserted) = text {
                self.insert_character(inserted, now);
                self.auto_complete();
            } else {
                self.insert_character(value, now);
                self.auto_complete();
            }
            self.scroll_caret_into_view(viewport_height, viewport_width, row_height, gutter_width);
            return commits;
        }

        let UiEditorKeyKind::Named(name) = kind else {
            return Vec::new();
        };

        // Completion popup interaction takes precedence.
        if self.completion.is_some() {
            match name.as_str() {
                "Escape" => {
                    self.completion = None;
                    self.revision += 1;
                    return Vec::new();
                }
                "ArrowDown" => {
                    let count = self.completion.as_ref().map_or(0, |c| c.items.len());
                    if count > 0 {
                        let selected = self.completion.as_mut().unwrap().selected;
                        self.completion.as_mut().unwrap().selected = (selected + 1).min(count - 1);
                        self.revision += 1;
                    }
                    return Vec::new();
                }
                "ArrowUp" => {
                    let selected = self.completion.as_ref().map_or(0, |c| c.selected);
                    if selected > 0 {
                        self.completion.as_mut().unwrap().selected = selected - 1;
                        self.revision += 1;
                    }
                    return Vec::new();
                }
                "Enter" | "Tab" => {
                    let item = self
                        .completion
                        .as_ref()
                        .and_then(|c| c.items.get(c.selected).map(|item| item.clone()));
                    if let Some(item) = item {
                        self.caret = self.core.apply_completion(&item);
                        self.selection_anchor = None;
                        self.completion = None;
                        self.mark_edit(now);
                        self.revision += 1;
                        self.scroll_caret_into_view(
                            viewport_height,
                            viewport_width,
                            row_height,
                            gutter_width,
                        );
                        return Vec::new();
                    }
                    self.completion = None;
                }
                _ => {}
            }
        }

        match name.as_str() {
            "Escape" => {
                // Blur + commit; the host clears focus after this returns.
                if let Some(commit) = self.commit_pending() {
                    commits.push(commit);
                }
                self.focus = false;
                self.completion = None;
                self.preedit.clear();
                self.revision += 1;
                true
            }
            "Space" => {
                self.insert_text(" ", now);
                true
            }
            "Enter" => {
                let line_text = self
                    .core
                    .buffer()
                    .line(self.caret.line)
                    .unwrap_or_default();
                let indent: String = line_text
                    .chars()
                    .take_while(|ch| *ch == ' ' || *ch == '\t')
                    .collect();
                // Auto-indent one extra level after an opening bracket.
                let before_caret: String = line_text.chars().take(self.caret.column as usize).collect();
                let trimmed = before_caret.trim_end();
                let extra: String = if trimmed.ends_with('{') || trimmed.ends_with('(') || trimmed.ends_with('[') {
                    " ".repeat(self.declaration.tab_size as usize)
                } else {
                    String::new()
                };
                self.insert_text(&format!("\n{indent}{extra}"), now);
                true
            }
            "Tab" => {
                if shift {
                    // Shift+Tab: outdent the current line by one tab stop.
                    let line = self.caret.line;
                    let line_text = self.core.buffer().line(line).unwrap_or_default();
                    let leading = line_text
                        .chars()
                        .take_while(|ch| *ch == ' ' || *ch == '\t')
                        .count();
                    if leading > 0 {
                        let remove = leading.min(self.declaration.tab_size as usize);
                        let old_col = self.caret.column;
                        self.core
                            .delete(Position::new(line, 0), Position::new(line, remove as u32));
                        self.caret =
                            Position::new(line, old_col.saturating_sub(remove as u32));
                        self.mark_edit(now);
                        self.revision += 1;
                    }
                } else {
                    let spaces = " ".repeat(self.declaration.tab_size as usize);
                    self.insert_text(&spaces, now);
                }
                true
            }
            "Backspace" => {
                if ctrl {
                    let line = self
                        .core
                        .buffer()
                        .line(self.caret.line)
                        .unwrap_or_default();
                    let chars: Vec<char> = line.chars().collect();
                    let mut column = self.caret.column as usize;
                    while column > 0
                        && chars.get(column - 1).is_some_and(|c| c.is_whitespace())
                    {
                        column -= 1;
                    }
                    while column > 0
                        && chars.get(column - 1).is_some_and(|c| !c.is_whitespace())
                    {
                        column -= 1;
                    }
                    let start = Position::new(self.caret.line, column as u32);
                    self.caret = self.core.delete(start, self.caret);
                    self.completion = None;
                    let events = self.core.take_edit_events();
            self.take_edit_fx(events, now);
                    self.mark_edit(now);
                    self.revision += 1;
                } else {
                    self.delete_backward(now);
                }
                self.auto_complete();
                true
            }
            "Delete" => {
                if ctrl {
                    let line = self
                        .core
                        .buffer()
                        .line(self.caret.line)
                        .unwrap_or_default();
                    let chars: Vec<char> = line.chars().collect();
                    let mut column = self.caret.column as usize;
                    let len = chars.len();
                    while column < len
                        && chars.get(column).is_some_and(|c| !c.is_whitespace())
                    {
                        column += 1;
                    }
                    while column < len && chars.get(column).is_some_and(|c| c.is_whitespace()) {
                        column += 1;
                    }
                    let end = Position::new(self.caret.line, column as u32);
                    self.caret = self.core.delete(self.caret, end);
                    self.completion = None;
                    let events = self.core.take_edit_events();
            self.take_edit_fx(events, now);
                    self.mark_edit(now);
                    self.revision += 1;
                } else {
                    self.delete_forward(now);
                }
                self.auto_complete();
                true
            }
            "ArrowLeft" => {
                if ctrl {
                    let line = self
                        .core
                        .buffer()
                        .line(self.caret.line)
                        .unwrap_or_default();
                    let chars: Vec<char> = line.chars().collect();
                    let mut column = self.caret.column as usize;
                    while column > 0
                        && chars.get(column - 1).is_some_and(|c| c.is_whitespace())
                    {
                        column -= 1;
                    }
                    while column > 0
                        && chars.get(column - 1).is_some_and(|c| !c.is_whitespace())
                    {
                        column -= 1;
                    }
                    self.move_caret(Position::new(self.caret.line, column as u32), shift);
                } else if self.caret.column > 0 {
                    self.move_caret(
                        Position::new(self.caret.line, self.caret.column - 1),
                        shift,
                    );
                } else if self.caret.line > 0 {
                    let previous_len = self
                        .core
                        .buffer()
                        .line(self.caret.line - 1)
                        .map_or(0, |line| line.chars().count() as u32);
                    self.move_caret(
                        Position::new(self.caret.line - 1, previous_len),
                        shift,
                    );
                }
                true
            }
            "ArrowRight" => {
                let line_len = self
                    .core
                    .buffer()
                    .line(self.caret.line)
                    .map_or(0, |line| line.chars().count() as u32);
                if ctrl {
                    let line = self
                        .core
                        .buffer()
                        .line(self.caret.line)
                        .unwrap_or_default();
                    let chars: Vec<char> = line.chars().collect();
                    let mut column = self.caret.column as usize;
                    let len = chars.len();
                    while column < len
                        && chars.get(column).is_some_and(|c| !c.is_whitespace())
                    {
                        column += 1;
                    }
                    while column < len && chars.get(column).is_some_and(|c| c.is_whitespace()) {
                        column += 1;
                    }
                    self.move_caret(Position::new(self.caret.line, column as u32), shift);
                } else if self.caret.column < line_len {
                    self.move_caret(
                        Position::new(self.caret.line, self.caret.column + 1),
                        shift,
                    );
                } else if self.caret.line + 1 < self.core.buffer().line_count() {
                    self.move_caret(Position::new(self.caret.line + 1, 0), shift);
                }
                true
            }
            "ArrowUp" => {
                let previous_len = self
                    .core
                    .buffer()
                    .line(self.caret.line.saturating_sub(1))
                    .map_or(0, |line| line.chars().count() as u32);
                let line = self.caret.line.saturating_sub(1);
                self.move_caret(
                    Position::new(line, self.caret.column.min(previous_len)),
                    shift,
                );
                true
            }
            "ArrowDown" => {
                let next_len = self
                    .core
                    .buffer()
                    .line(self.caret.line.saturating_add(1))
                    .map_or(0, |line| line.chars().count() as u32);
                let line = self
                    .caret
                    .line
                    .saturating_add(1)
                    .min(self.core.buffer().line_count().saturating_sub(1));
                self.move_caret(
                    Position::new(line, self.caret.column.min(next_len)),
                    shift,
                );
                true
            }
            "Home" => {
                let to = if ctrl {
                    Position::START
                } else {
                    Position::new(self.caret.line, 0)
                };
                self.move_caret(to, shift);
                true
            }
            "End" => {
                let to = if ctrl {
                    let last = self.core.buffer().line_count().saturating_sub(1);
                    let len = self
                        .core
                        .buffer()
                        .line(last)
                        .map_or(0, |line| line.chars().count() as u32);
                    Position::new(last, len)
                } else {
                    let len = self
                        .core
                        .buffer()
                        .line(self.caret.line)
                        .map_or(0, |line| line.chars().count() as u32);
                    Position::new(self.caret.line, len)
                };
                self.move_caret(to, shift);
                true
            }
            "PageUp" => {
                let rows = (viewport_height / row_height.max(1.0)).floor() as u32;
                let line = self.caret.line.saturating_sub(rows.max(1));
                self.move_caret(Position::new(line, self.caret.column), shift);
                true
            }
            "PageDown" => {
                let rows = (viewport_height / row_height.max(1.0)).floor() as u32;
                let line = self
                    .caret
                    .line
                    .saturating_add(rows.max(1))
                    .min(self.core.buffer().line_count().saturating_sub(1));
                self.move_caret(Position::new(line, self.caret.column), shift);
                true
            }
            _ => return Vec::new(),
        };
        self.scroll_caret_into_view(viewport_height, viewport_width, row_height, gutter_width);
        commits
    }

    /// Keeps scroll offsets non-negative after zoom / wheel events. Precise
    /// clamping to content bounds happens inside `scroll_caret_into_view`.
    fn clamp_scroll_approx(&mut self) {
        self.scroll_x = self.scroll_x.max(0.0);
        self.scroll_y = self.scroll_y.max(0.0);
    }

    fn scroll_caret_into_view(
        &mut self,
        viewport_height: f32,
        viewport_width: f32,
        row_height: f32,
        gutter_width: f32,
    ) {
        let viewport_w = (viewport_width - gutter_width).max(1.0);
        let raster_px = self.declaration.font_size * self.font_scale;
        let advance = raster_px * MONO_ADVANCE_FACTOR;
        let line_text = self
            .core
            .buffer()
            .line(self.caret.line)
            .unwrap_or_default();
        let caret_x = self.caret.column as f32 * advance;
        if caret_x < self.scroll_x {
            self.scroll_x = caret_x;
        } else if caret_x > self.scroll_x + viewport_w - 8.0 {
            self.scroll_x = caret_x - viewport_w + 16.0;
        }
        let caret_y = self.caret.line as f32 * row_height;
        if caret_y < self.scroll_y {
            self.scroll_y = caret_y;
        } else if caret_y + row_height > self.scroll_y + viewport_height {
            self.scroll_y = caret_y + row_height - viewport_height;
        }
        let content_width = (line_text.chars().count() as f32 * advance).max(viewport_w);
        let content_height = self.core.buffer().line_count() as f32 * row_height;
        let max_x = (content_width - viewport_w).max(0.0);
        let max_y = (content_height - viewport_height).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
    }

    /// Apply a host-directed reveal using the editor's own caret, selection,
    /// and scroll calculations. This is the consumer-side state used by Agent
    /// visual operation acknowledgements.
    pub fn reveal(
        &mut self,
        line: u32,
        column: u32,
        end_line: Option<u32>,
        end_column: Option<u32>,
        viewport_height: f32,
        viewport_width: f32,
        row_height: f32,
        gutter_width: f32,
    ) {
        self.caret = self.core.buffer().clamp_position(Position::new(line, column));
        self.selection_anchor = end_line.zip(end_column).map(|(line, column)| {
            self.core.buffer().clamp_position(Position::new(line, column))
        });
        self.focus = true;
        self.scroll_caret_into_view(
            viewport_height,
            viewport_width,
            row_height,
            gutter_width,
        );
    }

    // ------------------------------------------------------- presentation

    /// Projects the current editing state into the renderer presentation.
    /// Expired transient fx are dropped here (the renderer also skips them
    /// defensively). `now` drives fx lifetime.
    pub fn to_presentation(&mut self, now: f32) -> UiCodeEditorPresentation {
        self.revision += 1;
        self.edit_fx
            .retain(|fx| now - fx.started_seconds < fx.duration_ms as f32 / 1000.0);
        let line_count = self.core.buffer().line_count();
        let mut token_rows: Vec<Vec<UiEditorTokenSpan>> = Vec::with_capacity(line_count as usize);
        for line in 0..line_count {
            let text = self.core.buffer().line(line).unwrap_or_default();
            let chars: Vec<char> = text.chars().collect();
            let mut spans: Vec<UiEditorTokenSpan> = Vec::new();
            if let Some(tokens) = self.core.line_spans(line) {
                for span in &tokens.spans {
                    let start = (span.start as usize).min(chars.len());
                    let len = (span.len as usize).min(chars.len() - start);
                    if len == 0 {
                        continue;
                    }
                    let span_text: String = chars[start..start + len].iter().collect();
                    spans.push(UiEditorTokenSpan {
                        start: span.start,
                        text: span_text,
                        class: span.class.name().to_string(),
                    });
                }
            }
            token_rows.push(spans);
        }
        let completion = self.completion.as_ref().map(|c| UiEditorCompletionSnapshot {
            items: c
                .items
                .iter()
                .map(|item| UiEditorCompletionItem {
                    label: item.label.clone(),
                    kind: format!("{:?}", item.kind),
                    detail: item.detail.clone(),
                })
                .collect(),
            selected: c.selected as u32,
        });
        UiCodeEditorPresentation {
            node_key: self.declaration.node_key.clone(),
            revision: self.revision,
            source: self.core.buffer().text(),
            token_rows,
            caret_line: self.caret.line,
            caret_column: self.caret.column,
            selection_anchor_line: self.selection_anchor.map(|p| p.line),
            selection_anchor_column: self.selection_anchor.map(|p| p.column),
            scroll_x: self.scroll_x,
            scroll_y: self.scroll_y,
            focus: self.focus,
            preedit: self.preedit.clone(),
            completion,
            edit_fx: self
                .edit_fx
                .iter()
                .map(|fx| UiEditorEditFx {
                    kind: match fx.kind {
                        EditFxKind::Insert => "insert",
                        EditFxKind::Delete => "delete",
                    }
                    .to_string(),
                    package_id: fx.package_id.clone(),
                    row: fx.row,
                    col: fx.col,
                    text: fx.text.clone(),
                    started_seconds: fx.started_seconds,
                    duration_ms: fx.duration_ms,
                })
                .collect(),
            font_scale: self.font_scale,
            last_edit_seconds: self.last_edit_seconds,
        }
    }
}

/// Registry of code-editor components keyed by renderer node path.
#[derive(Default)]
pub struct EditorComponentRegistry {
    pub editors: HashMap<String, EditorComponent>,
}

impl EditorComponentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates / updates / destroys components from the fragment's
    /// `CodeEditorDeclaration` effects.
    pub fn reconcile(
        &mut self,
        desired: &HashMap<String, (UiCodeEditorDeclaration, Option<String>, String)>,
    ) {
        self.editors
            .retain(|path, _| desired.contains_key(path));
        for (path, (declaration, event_action, source)) in desired {
            if let Some(state) = self.editors.get_mut(path) {
                let needs_rebuild = state.declaration.language != declaration.language
                    || (state.adopted_source != *source && !state.focus);
                state.declaration = declaration.clone();
                state.event_action = event_action.clone();
                if needs_rebuild {
                    state.core = EditorCore::from_language(source, core_language_for(declaration));
                    state.adopted_source = source.clone();
                    state.caret = Position::START;
                    state.selection_anchor = None;
                    state.completion = None;
                    state.scroll_x = 0.0;
                    state.scroll_y = 0.0;
                    state.preedit.clear();
                    state.revision += 1;
                }
            } else {
                let mut state = EditorComponent::new(declaration.clone(), source);
                state.event_action = event_action.clone();
                self.editors.insert(path.clone(), state);
            }
        }
    }

    /// Routes an input event to the owning component and returns commits.
    pub fn handle_input(
        &mut self,
        event: &UiEditorInputEvent,
        now: f32,
    ) -> Vec<EditorCommit> {
        let path = match event {
            UiEditorInputEvent::Key { path, .. }
            | UiEditorInputEvent::PointerPress { path, .. }
            | UiEditorInputEvent::PointerDrag { path, .. }
            | UiEditorInputEvent::Scroll { path, .. }
            | UiEditorInputEvent::Zoom { path, .. }
            | UiEditorInputEvent::ImePreedit { path, .. }
            | UiEditorInputEvent::ImeCommit { path, .. }
            | UiEditorInputEvent::Reveal { path, .. } => path.clone(),
            UiEditorInputEvent::PointerRelease => return Vec::new(),
        };
        let Some(state) = self.editors.get_mut(&path) else {
            return Vec::new();
        };
        state.handle_input(event, now)
    }

    pub fn reveal(
        &mut self,
        path: &str,
        line: u32,
        column: u32,
        end_line: Option<u32>,
        end_column: Option<u32>,
        viewport_height: f32,
        viewport_width: f32,
        row_height: f32,
        gutter_width: f32,
    ) -> bool {
        let Some(state) = self.editors.get_mut(path) else { return false };
        state.reveal(
            line,
            column,
            end_line,
            end_column,
            viewport_height,
            viewport_width,
            row_height,
            gutter_width,
        );
        true
    }

    pub fn focused(&self) -> Option<&str> {
        self.editors
            .iter()
            .find(|(_, s)| s.focus)
            .map(|(path, _)| path.as_str())
    }

    pub fn to_presentations(&mut self, now: f32) -> Vec<UiCodeEditorPresentation> {
        self.editors
            .iter_mut()
            .map(|(_, state)| state.to_presentation(now))
            .collect()
    }
}

/// Ordered selection endpoints from `anchor` + `caret`.
fn ordered_selection(anchor: Position, caret: Position) -> (Position, Position) {
    if anchor <= caret {
        (anchor, caret)
    } else {
        (caret, anchor)
    }
}

/// Extracts `start..end` text from the editor buffer (for clipboard ops).
fn extract_range(state: &EditorComponent, start: Position, end: Position) -> String {
    let start = state.core.buffer().clamp_position(start);
    let end = state.core.buffer().clamp_position(end);
    if end <= start {
        return String::new();
    }
    let mut text = String::new();
    for line in start.line..=end.line {
        let line_text = state.core.buffer().line(line).unwrap_or_default();
        let chars: Vec<char> = line_text.chars().collect();
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

/// Host-side editor bridge shared between the ui-runtime and the wgpu
/// renderer. The host creates one `Arc<EditorBridge>`, injects the input sink
/// and the presentations slot into the renderer, and registers a fragment
/// observer so every submitted fragment (declarations) keeps the
/// `EditorComponentRegistry` in sync.
///
/// This is the single place that turns "fragment + input events" into
/// "presentations the renderer can draw", so the renderer never needs to know
/// the editor core.
pub struct EditorBridge {
    /// The editor component registry (one `EditorComponent` per
    /// `CodeEditorDeclaration`).
    pub registry: Mutex<EditorComponentRegistry>,
    /// Fresh presentations after the last input handling / fragment sync.
    /// The renderer reads this slot each frame (Arc so the host can hand the
    /// same slot to the renderer without a copy).
    pub presentations: Arc<Mutex<Vec<UiCodeEditorPresentation>>>,
}

impl EditorBridge {
    pub fn new() -> Self {
        Self {
            registry: Mutex::new(EditorComponentRegistry::new()),
            presentations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Rescans submitted fragments for `code_editor` declarations, reconciles
    /// the registry and republishes presentations. Called by the host's
    /// fragment observer after every fragment submission/replacement.
    pub fn sync_fragments(&self, fragments: &HashMap<UiFragmentId, UiFragment>) {
        let mut desired: HashMap<String, (UiCodeEditorDeclaration, Option<String>, String)> =
            HashMap::new();
        for fragment in fragments.values() {
            let mut kinds = HashMap::new();
            collect_node_kinds(&fragment.root, &mut kinds);
            let mut sources = HashMap::new();
            collect_node_literal_text(&fragment.root, &mut sources);
            let mut events = HashMap::new();
            for effect in &fragment.effects {
                if let UiEffect::BoundSemanticIntent { node_id, intent } = effect
                    && let UiIntent::Invoke { action, .. } = intent
                {
                    events.insert(node_id.0.clone(), action.clone());
                }
            }
            for effect in &fragment.effects {
                let UiEffect::CodeEditorDeclaration { node_key, declaration } = effect else {
                    continue;
                };
                // Only accept declarations whose lowered node is still a
                // Panel in this fragment (the renderer needs a draw target).
                if kinds.get(node_key) != Some(&UiNodeKind::Panel) {
                    continue;
                }
                let path = format!("{}/{}", fragment.fragment_id.0, node_key);
                let source = sources.get(node_key).cloned().unwrap_or_default();
                desired.insert(
                    path,
                    (declaration.clone(), events.get(node_key).cloned(), source),
                );
            }
        }
        let mut registry = self.registry.lock().expect("editor bridge registry lock");
        registry.reconcile(&desired);
        *self.presentations.lock().expect("editor bridge presentations lock") =
            registry.to_presentations(0.0);
    }

    /// Routes one renderer input event into the component registry, returns
    /// the commits produced, and republishes the updated presentations.
    pub fn handle_input(
        &self,
        event: &UiEditorInputEvent,
        now: f32,
    ) -> Vec<EditorCommit> {
        let mut registry = self.registry.lock().expect("editor bridge registry lock");
        let commits = registry.handle_input(event, now);
        *self.presentations.lock().expect("editor bridge presentations lock") =
            registry.to_presentations(now);
        commits
    }

    /// Host-directed editor reveal. The returned presentation is the
    /// consumer-side acknowledgement for a visual operation.
    pub fn reveal(
        &self,
        path: &str,
        line: u32,
        column: u32,
        end_line: Option<u32>,
        end_column: Option<u32>,
        viewport_height: f32,
        viewport_width: f32,
        row_height: f32,
        gutter_width: f32,
        now: f32,
    ) -> Option<UiCodeEditorPresentation> {
        let mut registry = self.registry.lock().expect("editor bridge registry lock");
        if !registry.reveal(
            path,
            line,
            column,
            end_line,
            end_column,
            viewport_height,
            viewport_width,
            row_height,
            gutter_width,
        ) {
            return None;
        }
        let presentations = registry.to_presentations(now);
        *self.presentations.lock().expect("editor bridge presentations lock") = presentations.clone();
        presentations.into_iter().find(|presentation| presentation.node_key == path)
    }
}

impl Default for EditorBridge {
    fn default() -> Self {
        Self::new()
    }
}

/// Collects node kinds (node_id -> kind) for lower validation.
fn collect_node_kinds(node: &UiNode, out: &mut HashMap<String, UiNodeKind>) {
    out.insert(node.node_id.0.clone(), node.kind.clone());
    for child in &node.children {
        collect_node_kinds(child, out);
    }
}

/// Collects literal text nodes (node_id -> value) so a code editor's source
/// input key can be resolved without font / GPU access.
fn collect_node_literal_text(node: &UiNode, out: &mut HashMap<String, String>) {
    if let Some(TextRef::Literal { value }) = &node.text {
        out.insert(node.node_id.0.clone(), value.clone());
    }
    for child in &node.children {
        collect_node_literal_text(child, out);
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use neon_ui_schema::{UiCodeEditorDeclaration, UiEditorInputEvent, UiEditorKeyKind, UiEditorLanguage, UiEditorWrap};

    fn ts_declaration() -> UiCodeEditorDeclaration {
        UiCodeEditorDeclaration {
            node_key: "source-view".into(),
            source_input_key: "document".into(),
            language: UiEditorLanguage::Typescript,
            line_numbers: true,
            wrap: UiEditorWrap::None,
            font_size: 17.0,
            tab_size: 4,
            read_only_input_key: None,
            completion_input_key: None,
            source_file: None,
            gutter_diagnostics: true,
            token_materials: Default::default(),
            syntax_colors: Default::default(),
            ui_colors: Default::default(),
            selection_material: None,
        }
    }

    fn type_key(path: &str, ch: char) -> UiEditorInputEvent {
        UiEditorInputEvent::Key {
            path: path.into(),
            kind: UiEditorKeyKind::Character(ch.to_string()),
            text: Some(ch.to_string()),
            shift: false,
            ctrl: false,
            viewport_height: 480.0,
            viewport_width: 868.0,
            gutter_width: 56.0,
            row_height: 22.0,
        }
    }

    /// Register the built-in tree-sitter providers, releasing the global
    /// registry lock before constructing an EditorCore (creating a core while
    /// holding the lock would deadlock on the non-reentrant global mutex).
    fn register_providers() {
        let mut registry = neon_editor::default_registry();
        neon_languages::register_builtins(&mut registry);
    }

    fn classes(row: &[neon_ui_schema::UiEditorTokenSpan]) -> Vec<(String, String)> {
        row.iter().map(|t| (t.class.clone(), t.text.clone())).collect()
    }

    #[test]
    fn typescript_initial_presentation_has_keyword() {
        register_providers();
        let mut comp = EditorComponent::new(ts_declaration(), "interface Track\n");
        let pres0 = comp.to_presentation(0.0);
        assert_eq!(pres0.token_rows.len(), 2);
        assert!(
            pres0.token_rows[0]
                .iter()
                .any(|t| t.class == "Keyword" && t.text == "interface"),
            "initial line should carry Keyword 'interface', got {:?}",
            classes(&pres0.token_rows[0])
        );
    }

    #[test]
    fn typescript_presentation_carries_keyword_tokens_after_input() {
        register_providers();
        // User's exact real document: initial 4 lines + typed const lines.
        let mut comp = EditorComponent::new(
            ts_declaration(),
            "interface Track\nconst t makeTrack\nlet x\n// comment\n",
        );
        for ch in "const test = 1;\nconst test = 2;\nclass e{".chars() {
            comp.handle_input(&type_key("source-view", ch), 0.5);
        }
        let pres = comp.to_presentation(1.0);
        eprintln!("[ui] source={:?}", pres.source);
        eprintln!("[ui] source lines={}", pres.source.split('\n').count());
        eprintln!(
            "[ui] token_rows={}",
            pres
                .token_rows
                .iter()
                .enumerate()
                .map(|(i, r)| format!(
                    "r{i}:{}",
                    r.iter()
                        .map(|t| format!("{}@{}", t.class, t.text))
                        .collect::<Vec<_>>()
                        .join(",")
                ))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        assert!(pres.token_rows.len() >= 7, "rows = {}", pres.token_rows.len());
        // Every keyword must classify as Keyword even on syntactically
        // invalid lines (the lexer-level fallback guarantees this).
        // Input lands at the caret (START), so the final buffer is:
        //   const test = 1; / const test = 2; / class e{interface Track /
        //   const t makeTrack / let x / // comment
        // Every keyword must classify as Keyword (lexer fallback), including
        // keywords inside syntax-error regions.
        for (row, kw) in [(0, "const"), (1, "const"), (2, "class"), (3, "const"), (4, "let")] {
            let row1 = &pres.token_rows[row];
            assert!(
                row1.iter().any(|t| t.class == "Keyword" && t.text == kw),
                "row {row} should carry Keyword '{kw}', got {:?}",
                classes(row1)
            );
        }
        // The comment line stays a Comment.
        assert!(
            pres.token_rows[5]
                .iter()
                .any(|t| t.class == "Comment" && t.text == "// comment"),
            "row 5 should be Comment, got {:?}",
            classes(&pres.token_rows[5])
        );
    }

    #[test]
    fn host_reveal_updates_caret_selection_and_scroll() {
        register_providers();
        let mut comp = EditorComponent::new(
            ts_declaration(),
            &(0..300).map(|line| format!("const value_{line} = {line};\n")).collect::<String>(),
        );
        comp.reveal(220, 4, Some(222), Some(10), 200.0, 868.0, 20.0, 56.0);
        let presentation = comp.to_presentation(1.0);
        assert_eq!(presentation.caret_line, 220);
        assert_eq!(presentation.caret_column, 4);
        assert_eq!(presentation.selection_anchor_line, Some(222));
        assert_eq!(presentation.selection_anchor_column, Some(10));
        assert!(presentation.scroll_y > 0.0);
    }

    fn named_key(path: &str, name: &str, shift: bool) -> UiEditorInputEvent {
        UiEditorInputEvent::Key {
            path: path.into(),
            kind: UiEditorKeyKind::Named(name.into()),
            text: None,
            shift,
            ctrl: false,
            viewport_height: 480.0,
            viewport_width: 868.0,
            gutter_width: 56.0,
            row_height: 22.0,
        }
    }

    #[test]
    fn enter_after_opening_bracket_extra_indent() {
        register_providers();
        let mut comp = EditorComponent::new(ts_declaration(), "fn foo() {");
        comp.handle_input(&named_key("source-view", "End", false), 0.0);
        comp.handle_input(&named_key("source-view", "Enter", false), 0.0);
        assert_eq!(
            comp.core.buffer().text(),
            "fn foo() {\n    ",
            "Enter after {{ should add one extra indent level, got {:?}",
            comp.core.buffer().text()
        );
    }

    #[test]
    fn enter_plain_line_keeps_same_indent() {
        register_providers();
        let mut comp = EditorComponent::new(ts_declaration(), "    let x = 1;");
        comp.handle_input(&named_key("source-view", "End", false), 0.0);
        comp.handle_input(&named_key("source-view", "Enter", false), 0.0);
        assert_eq!(
            comp.core.buffer().text(),
            "    let x = 1;\n    ",
            "Enter on a plain line should preserve indent, got {:?}",
            comp.core.buffer().text()
        );
    }

    #[test]
    fn opening_bracket_auto_closes() {
        register_providers();
        let mut comp = EditorComponent::new(ts_declaration(), "");
        comp.handle_input(&type_key("source-view", '('), 0.0);
        assert_eq!(comp.core.buffer().text(), "()", "type ( -> (), got {:?}", comp.core.buffer().text());
        // Caret should be between the parens.
        assert_eq!(comp.caret.column, 1, "caret should be at col 1 between ()");
    }

    #[test]
    fn closer_skips_when_already_present() {
        register_providers();
        let mut comp = EditorComponent::new(ts_declaration(), "()");
        // Move caret between the parens (col 1) via ArrowRight.
        comp.handle_input(&named_key("source-view", "ArrowRight", false), 0.0);
        assert_eq!(comp.caret.column, 1);
        // Typing ( when the next char is ) should step over it.
        comp.handle_input(&type_key("source-view", '('), 0.0);
        assert_eq!(comp.core.buffer().text(), "()", "buffer unchanged, got {:?}", comp.core.buffer().text());
        assert_eq!(comp.caret.column, 2, "caret should be past the closer");
    }

    #[test]
    fn closer_typing_skips_existing_closer() {
        register_providers();
        let mut comp = EditorComponent::new(ts_declaration(), "()");
        // Move caret between parens.
        comp.handle_input(&named_key("source-view", "ArrowRight", false), 0.0);
        assert_eq!(comp.caret.column, 1);
        // Type ) — should skip over the existing ).
        comp.handle_input(&type_key("source-view", ')'), 0.0);
        assert_eq!(comp.core.buffer().text(), "()", "buffer unchanged, got {:?}", comp.core.buffer().text());
        assert_eq!(comp.caret.column, 2, "caret should skip past )");
    }

    #[test]
    fn tab_inserts_and_shift_tab_outdents() {
        register_providers();
        // Start with an already-indented line.
        let mut comp = EditorComponent::new(ts_declaration(), "    let x = 1;\n");
        // Caret starts at (0,0); move it to end of line 0.
        comp.handle_input(&named_key("source-view", "End", false), 0.0);
        assert_eq!(comp.core.buffer().text(), "    let x = 1;\n");
        // Tab inserts 4 spaces at caret.
        comp.handle_input(&named_key("source-view", "Tab", false), 0.0);
        assert_eq!(
            comp.core.buffer().text(),
            "    let x = 1;    \n",
            "Tab should insert 4 spaces at caret, got {:?}",
            comp.core.buffer().text()
        );
        // Shift+Tab removes one tab stop of leading whitespace.
        comp.handle_input(&named_key("source-view", "Tab", true), 0.0);
        assert_eq!(
            comp.core.buffer().text(),
            "let x = 1;    \n",
            "Shift+Tab should outdent leading 4 spaces, got {:?}",
            comp.core.buffer().text()
        );
    }
}
