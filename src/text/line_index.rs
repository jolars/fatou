//! Byte-offset → (line, column) conversion.
//!
//! Two coordinate systems share the same line-start table:
//! - 1-indexed (line, column) in **code points** for CLI diagnostics.
//! - 0-indexed (line, character) in **UTF-16 units** for LSP positions.

use lsp_types::Position;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    /// 1-indexed line number.
    pub line: usize,
    /// 1-indexed column in code points (not bytes, not UTF-16 units).
    pub column: usize,
}

/// Precomputed line-start byte offsets for a text buffer.
///
/// `line_starts[i]` is the byte offset of the first character of line `i`
/// (0-indexed). `line_starts` always starts with `0`.
#[derive(Debug, Clone)]
pub struct LineIndex<'a> {
    text: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut line_starts = Vec::with_capacity(text.len() / 40 + 1);
        line_starts.push(0);
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self { text, line_starts }
    }

    /// 1-indexed (line, column-in-code-points). Suitable for CLI diagnostics.
    pub fn byte_to_lc(&self, offset: usize) -> LineCol {
        let clamped = offset.min(self.text.len());
        let line_idx = self.line_index_for(clamped);
        let line_start = self.line_starts[line_idx];
        let column = self.text[line_start..clamped].chars().count() + 1;
        LineCol {
            line: line_idx + 1,
            column,
        }
    }

    /// 0-indexed LSP `Position` (UTF-16 character offsets).
    pub fn byte_to_position(&self, offset: usize) -> Position {
        let clamped = offset.min(self.text.len());
        let line_idx = self.line_index_for(clamped);
        let line_start = self.line_starts[line_idx];
        let character = self.text[line_start..clamped].encode_utf16().count() as u32;
        Position::new(line_idx as u32, character)
    }

    /// Inverse of [`byte_to_position`](Self::byte_to_position): a 0-indexed LSP
    /// `Position` (UTF-16 character offset) back to a byte offset. A line past
    /// the end clamps to the end of the buffer; a character past the end of
    /// the line clamps to the line's content, before its terminator.
    pub fn position_to_byte(&self, position: Position) -> usize {
        let line = position.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return self.text.len();
        };
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());
        let line_text = self.text[line_start..line_end]
            .trim_end_matches('\n')
            .trim_end_matches('\r');
        let mut utf16 = 0u32;
        for (byte_off, ch) in line_text.char_indices() {
            if utf16 >= position.character {
                return line_start + byte_off;
            }
            utf16 += ch.len_utf16() as u32;
        }
        line_start + line_text.len()
    }

    /// Total line count (1 even for empty text).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    fn line_index_for(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string() {
        let idx = LineIndex::new("");
        assert_eq!(idx.byte_to_lc(0), LineCol { line: 1, column: 1 });
        assert_eq!(idx.byte_to_position(0), Position::new(0, 0));
    }

    #[test]
    fn multi_line() {
        let idx = LineIndex::new("ab\ncd\nef");
        assert_eq!(idx.byte_to_lc(0), LineCol { line: 1, column: 1 });
        assert_eq!(idx.byte_to_lc(3), LineCol { line: 2, column: 1 });
        assert_eq!(idx.byte_to_position(6), Position::new(2, 0));
    }

    #[test]
    fn utf16_surrogate_pair() {
        // U+1F600 (emoji) is 4 bytes in UTF-8, 2 UTF-16 units (surrogate pair).
        let idx = LineIndex::new("\u{1F600}x");
        assert_eq!(idx.byte_to_lc(4), LineCol { line: 1, column: 2 });
        assert_eq!(idx.byte_to_position(4), Position::new(0, 2));
    }

    #[test]
    fn position_to_byte_clamps_before_line_terminator() {
        let idx = LineIndex::new("ab\ncd");
        assert_eq!(idx.position_to_byte(Position::new(0, 9)), 2);
        assert_eq!(idx.position_to_byte(Position::new(9, 0)), 5);
        let idx = LineIndex::new("ab\r\ncd");
        assert_eq!(idx.position_to_byte(Position::new(0, 9)), 2);
    }

    #[test]
    fn position_to_byte_round_trips() {
        let text = "ab\ncde\nf";
        let idx = LineIndex::new(text);
        for offset in 0..=text.len() {
            if !text.is_char_boundary(offset) {
                continue;
            }
            let pos = idx.byte_to_position(offset);
            assert_eq!(idx.position_to_byte(pos), offset, "offset {offset}");
        }
    }
}
