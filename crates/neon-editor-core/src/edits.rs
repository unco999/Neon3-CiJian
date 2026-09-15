//! Edit operations, undo/redo, and ChangeSet accumulation.
//!
//! All operations are deterministic: undo grouping is adjacency-based (typed
//! characters merge into one entry until a newline or a different position
//! breaks the run), never time-based, so replay and tests stay reproducible.
//!
//! Pending ops are stored in undo-granularity groups; `take_change_set`
//! flattens them for the host and moves the groups onto the undo stack, so
//! undo keeps working across a debounce flush. Undo/redo are local buffer
//! state; the embedder reconciles with the host through ChangeSets (see
//! `EditorCore::take_change_set`, which emits a full resync after any
//! undo/redo divergence).

use crate::buffer::{Position, TextBuffer};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditOp {
    Insert {
        line: u32,
        column: u32,
        /// Position just past the inserted text, recorded when the insert ran
        /// (a newline insert ends on the next line, so this cannot be derived
        /// from `column + text.len()` at undo time).
        end: Position,
        text: String,
    },
    Delete {
        start: Position,
        end: Position,
        /// Deleted text, kept for undo.
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSet {
    /// Document revision the ops apply on top of.
    pub base_revision: u64,
    pub ops: Vec<EditOp>,
}

#[derive(Default)]
pub struct EditSession {
    revision: u64,
    undo: Vec<Vec<EditOp>>,
    redo: Vec<Vec<EditOp>>,
    /// Pending ops since the last ChangeSet, grouped by undo granularity.
    pending: Vec<Vec<EditOp>>,
    /// Set when undo/redo reverted ops already handed to the host; the next
    /// ChangeSet must be a full resync (see `EditorCore::take_change_set`).
    diverged: bool,
}

impl EditSession {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Adopts a host-provided baseline: local preview state is discarded.
    pub fn set_revision(&mut self, revision: u64) {
        self.revision = revision;
        self.undo.clear();
        self.redo.clear();
        self.pending.clear();
        self.diverged = false;
    }

    /// Whether uncommitted ops exist.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether undo/redo reverted ops the host already applied; the embedder
    /// must emit a full resync ChangeSet instead of incremental ops.
    pub fn diverged(&self) -> bool {
        self.diverged
    }

    /// Records an already-applied insert. `past` is the position returned by
    /// `TextBuffer::insert`. Merges with the previous op when it is a pure
    /// continuation of typed text (same line, adjacent column, no newline
    /// inside either text).
    pub fn record_insert(&mut self, at: Position, past: Position, text: &str) {
        self.redo.clear();
        let mergeable = matches!(
            self.pending.last().and_then(|group| group.last()),
            Some(EditOp::Insert {
                line: previous_line,
                column: previous_column,
                text: previous_text,
                ..
            }) if *previous_line == at.line
                && *previous_column + previous_text.chars().count() as u32 == at.column
                && !previous_text.contains('\n')
                && !text.contains('\n')
        );
        if mergeable
            && let Some(EditOp::Insert {
                text: previous_text,
                end,
                ..
            }) = self.pending.last_mut().and_then(|group| group.last_mut())
        {
            previous_text.push_str(text);
            *end = past;
            return;
        }
        self.pending.push(vec![EditOp::Insert {
            line: at.line,
            column: at.column,
            end: past,
            text: text.to_string(),
        }]);
    }

    /// Records an already-applied delete; merges with the previous op when it
    /// removes adjacent characters on the same line (backspace/delete keys).
    pub fn record_delete(&mut self, start: Position, end: Position, text: &str) {
        self.redo.clear();
        if let Some(EditOp::Delete {
            start: previous_start,
            end: previous_end,
            text: previous_text,
        }) = self.pending.last_mut().and_then(|group| group.last_mut())
        {
            let adjacent_backspace = previous_start.line == start.line
                && previous_start.column == end.column
                && !text.contains('\n');
            if adjacent_backspace {
                previous_start.column = start.column;
                previous_text.insert_str(0, text);
                return;
            }
            let adjacent_delete = previous_start.line == start.line
                && previous_end.column == start.column
                && !text.contains('\n');
            if adjacent_delete {
                previous_end.column = end.column;
                previous_text.push_str(text);
                return;
            }
        }
        self.pending.push(vec![EditOp::Delete {
            start,
            end,
            text: text.to_string(),
        }]);
    }

    /// Takes the accumulated ops as a ChangeSet for the host. The groups move
    /// onto the undo stack so undo still revokes them after a flush.
    pub fn take_change_set(&mut self) -> Option<ChangeSet> {
        self.diverged = false;
        if self.pending.is_empty() {
            return None;
        }
        let groups = std::mem::take(&mut self.pending);
        let ops: Vec<EditOp> = groups.iter().flatten().cloned().collect();
        self.undo.extend(groups);
        Some(ChangeSet {
            base_revision: self.revision,
            ops,
        })
    }

    /// A host accepted a ChangeSet: the baseline revision moves forward.
    pub fn commit(&mut self) {
        self.revision += 1;
    }

    /// Undoes the newest group (pending first, then the undo stack) and puts
    /// it on the redo stack. Undoing groups already flushed to the host sets
    /// the divergence flag. Returns the first affected line, or `None` when
    /// there was nothing to undo.
    pub fn undo(&mut self, buffer: &mut TextBuffer) -> Option<u32> {
        let group = match self.pending.pop() {
            Some(group) => group,
            None => {
                self.diverged = true;
                self.undo.pop()?
            }
        };
        apply_group_backward(buffer, &group);
        self.redo.push(group.clone());
        group_first_line(&group)
    }

    /// Redoes the newest redo group. Redo replays ops the host already
    /// received (or their reversal), so it always sets the divergence flag.
    /// Returns the first affected line.
    pub fn redo(&mut self, buffer: &mut TextBuffer) -> Option<u32> {
        let group = self.redo.pop()?;
        self.diverged = true;
        apply_group_forward(buffer, &group);
        let first = group_first_line(&group);
        self.undo.push(group);
        first
    }
}

fn group_first_line(group: &[EditOp]) -> Option<u32> {
    group
        .iter()
        .map(|op| match op {
            EditOp::Insert { line, .. } => *line,
            EditOp::Delete { start, .. } => start.line,
        })
        .min()
}

fn apply_group_forward(buffer: &mut TextBuffer, group: &[EditOp]) {
    for op in group {
        match op {
            EditOp::Insert {
                line, column, text, ..
            } => {
                buffer.insert(Position::new(*line, *column), text);
            }
            EditOp::Delete { start, end, .. } => {
                buffer.delete(*start, *end);
            }
        }
    }
}

fn apply_group_backward(buffer: &mut TextBuffer, group: &[EditOp]) {
    for op in group.iter().rev() {
        match op {
            EditOp::Insert {
                line, column, end, ..
            } => {
                let start = Position::new(*line, *column);
                buffer.delete(start, *end);
            }
            EditOp::Delete { start, text, .. } => {
                buffer.insert(*start, text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_characters_merge_into_one_undo_entry() {
        let mut buffer = TextBuffer::from_str("");
        let mut session = EditSession::default();
        for index in 0..3u32 {
            let character = ['a', 'b', 'c'][index as usize].to_string();
            let at = Position::new(0, index);
            let past = buffer.insert(at, &character);
            session.record_insert(at, past, &character);
        }
        assert_eq!(buffer.text(), "abc");
        session.undo(&mut buffer);
        assert_eq!(buffer.text(), "");
        session.redo(&mut buffer);
        assert_eq!(buffer.text(), "abc");
    }

    #[test]
    fn newline_breaks_typing_group() {
        let mut buffer = TextBuffer::from_str("");
        let mut session = EditSession::default();
        let (at, past) = (Position::new(0, 0), buffer.insert(Position::new(0, 0), "a"));
        session.record_insert(at, past, "a");
        let (at, past) = (
            Position::new(0, 1),
            buffer.insert(Position::new(0, 1), "\n"),
        );
        session.record_insert(at, past, "\n");
        let (at, past) = (Position::new(1, 0), buffer.insert(Position::new(1, 0), "b"));
        session.record_insert(at, past, "b");
        session.undo(&mut buffer);
        assert_eq!(buffer.text(), "a\n");
        session.undo(&mut buffer);
        assert_eq!(buffer.text(), "a");
        session.undo(&mut buffer);
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn backspace_merges_deletes() {
        let mut buffer = TextBuffer::from_str("abc");
        let mut session = EditSession::default();
        session.record_delete(Position::new(0, 2), Position::new(0, 3), "c");
        buffer.delete(Position::new(0, 2), Position::new(0, 3));
        session.record_delete(Position::new(0, 1), Position::new(0, 2), "b");
        buffer.delete(Position::new(0, 1), Position::new(0, 2));
        assert_eq!(buffer.text(), "a");
        session.undo(&mut buffer);
        assert_eq!(buffer.text(), "abc");
    }

    #[test]
    fn undo_works_across_change_set_flush() {
        let mut buffer = TextBuffer::from_str("");
        let mut session = EditSession::default();
        let (at, past) = (
            Position::new(0, 0),
            buffer.insert(Position::new(0, 0), "hello"),
        );
        session.record_insert(at, past, "hello");
        let change_set = session.take_change_set().expect("pending ops");
        assert_eq!(change_set.ops.len(), 1);
        session.commit();
        session.undo(&mut buffer);
        assert_eq!(buffer.text(), "");
        assert!(session.diverged());
        session.redo(&mut buffer);
        assert_eq!(buffer.text(), "hello");
    }

    #[test]
    fn change_set_accumulates_and_resets() {
        let mut session = EditSession::default();
        session.record_insert(Position::new(0, 0), Position::new(0, 1), "x");
        session.record_delete(Position::new(0, 0), Position::new(0, 1), "x");
        session.record_insert(Position::new(0, 0), Position::new(0, 1), "y");
        let change_set = session.take_change_set().expect("pending ops");
        assert_eq!(change_set.base_revision, 0);
        assert_eq!(change_set.ops.len(), 3);
        assert!(!session.has_pending());
        session.commit();
        assert_eq!(session.revision(), 1);
        assert!(session.take_change_set().is_none());
    }

    #[test]
    fn set_revision_discards_local_state() {
        let mut buffer = TextBuffer::from_str("old");
        let mut session = EditSession::default();
        let (at, past) = (Position::new(0, 0), buffer.insert(Position::new(0, 0), "x"));
        session.record_insert(at, past, "x");
        session.set_revision(7);
        assert_eq!(session.revision(), 7);
        assert!(!session.has_pending());
        assert_eq!(session.undo(&mut buffer), None);
    }
}
