//! Line-based text buffer with char-column positions.
//!
//! Positions are 0-based; `column` counts Unicode scalar values (chars), not
//! bytes, so CJK text and IME composition stay stable. Lines never contain
//! `\n`; a buffer's full text is lines joined with `\n`.

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

impl Position {
    pub const START: Position = Position { line: 0, column: 0 };

    pub fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextBuffer {
    lines: Vec<String>,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
        }
    }
}

impl TextBuffer {
    pub fn from_str(text: &str) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        Self {
            lines: text.split('\n').map(str::to_string).collect(),
        }
    }

    pub fn line_count(&self) -> u32 {
        self.lines.len() as u32
    }

    pub fn line(&self, line: u32) -> Option<&str> {
        self.lines.get(line as usize).map(String::as_str)
    }

    pub fn line_char_len(&self, line: u32) -> u32 {
        self.line(line)
            .map_or(0, |text| text.chars().count() as u32)
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    pub fn is_valid_position(&self, position: Position) -> bool {
        if position.line >= self.line_count() {
            return false;
        }
        position.column <= self.line_char_len(position.line)
    }

    /// Clamps a position into the buffer (end of line / end of buffer).
    pub fn clamp_position(&self, position: Position) -> Position {
        let line = position.line.min(self.line_count().saturating_sub(1));
        Position::new(line, position.column.min(self.line_char_len(line)))
    }

    /// Full text with a trailing newline when the buffer ends with an empty
    /// line is NOT added; the text is exactly the joined lines.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Inserts `text` (which may contain `\n`) at `position`. Returns the
    /// position just past the inserted text.
    pub fn insert(&mut self, position: Position, text: &str) -> Position {
        let position = self.clamp_position(position);
        let line_index = position.line as usize;
        let byte_column = char_to_byte(&self.lines[line_index], position.column);
        let head = self.lines[line_index][..byte_column].to_string();
        let tail = self.lines[line_index][byte_column..].to_string();
        let parts: Vec<&str> = text.split('\n').collect();

        if parts.len() == 1 {
            self.lines[line_index] = format!("{head}{text}{tail}");
            return Position::new(position.line, position.column + text.chars().count() as u32);
        }

        let mut replacement = Vec::with_capacity(parts.len());
        replacement.push(format!("{head}{}", parts[0]));
        for middle in &parts[1..parts.len() - 1] {
            replacement.push((*middle).to_string());
        }
        let last = parts[parts.len() - 1];
        replacement.push(format!("{last}{tail}"));
        let new_line = position.line + (parts.len() - 1) as u32;
        let new_column = last.chars().count() as u32;
        let mut rebuilt = Vec::with_capacity(self.lines.len() + parts.len() - 1);
        rebuilt.extend_from_slice(&self.lines[..line_index]);
        rebuilt.extend(replacement);
        rebuilt.extend_from_slice(&self.lines[line_index + 1..]);
        self.lines = rebuilt;
        Position::new(new_line, new_column)
    }

    /// Deletes the half-open range `start..end` (positions clamped). Returns
    /// the position where the deleted text used to start.
    pub fn delete(&mut self, start: Position, end: Position) -> Position {
        let start = self.clamp_position(start);
        let end = self.clamp_position(end);
        if end <= start {
            return start;
        }
        let start_index = start.line as usize;
        let end_index = end.line as usize;
        let start_byte = char_to_byte(&self.lines[start_index], start.column);
        let end_byte = char_to_byte(&self.lines[end_index], end.column);
        let head = self.lines[start_index][..start_byte].to_string();
        let tail = self.lines[end_index][end_byte..].to_string();
        self.lines[start_index] = format!("{head}{tail}");
        self.lines.drain(start_index + 1..=end_index);
        start
    }

    /// Removes every line (for tests and full reattachments).
    pub fn set_text(&mut self, text: &str) {
        *self = Self::from_str(text);
    }
}

fn char_to_byte(line: &str, column: u32) -> usize {
    line.char_indices()
        .nth(column as usize)
        .map_or(line.len(), |(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_delete_round_trip() {
        let mut buffer = TextBuffer::from_str("ab\ncd");
        let past = buffer.insert(Position::new(1, 2), "ef\ng");
        assert_eq!(past, Position::new(2, 1));
        assert_eq!(buffer.text(), "ab\ncdef\ng");
        // (2,2) clamps to end of "g": everything from "b" on is removed.
        let deleted = buffer.delete(Position::new(0, 1), Position::new(2, 2));
        assert_eq!(deleted, Position::new(0, 1));
        assert_eq!(buffer.text(), "a");
    }

    #[test]
    fn delete_keeps_end_line_tail() {
        let mut buffer = TextBuffer::from_str("ab\ncdef\ng");
        let deleted = buffer.delete(Position::new(0, 1), Position::new(2, 0));
        assert_eq!(deleted, Position::new(0, 1));
        assert_eq!(buffer.text(), "ag");
    }

    #[test]
    fn insert_clamps_out_of_range_positions() {
        let mut buffer = TextBuffer::from_str("hi");
        let past = buffer.insert(Position::new(9, 9), "!");
        assert_eq!(past, Position::new(0, 3));
        assert_eq!(buffer.text(), "hi!");
    }

    #[test]
    fn columns_count_chars_not_bytes() {
        let mut buffer = TextBuffer::from_str("中文");
        let past = buffer.insert(Position::new(0, 2), "x");
        assert_eq!(past, Position::new(0, 3));
        assert_eq!(buffer.line_char_len(0), 3);
        buffer.delete(Position::new(0, 1), Position::new(0, 3));
        assert_eq!(buffer.text(), "中");
    }

    #[test]
    fn empty_text_round_trip() {
        let mut buffer = TextBuffer::from_str("");
        assert_eq!(buffer.line_count(), 1);
        buffer.insert(Position::START, "a\nb");
        assert_eq!(buffer.text(), "a\nb");
    }
}
