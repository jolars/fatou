//! Byte-offset → (line, column) conversion.
//!
//! Two coordinate systems share the same line-start table:
//! - 1-indexed (line, column) in **code points** for CLI diagnostics.
//! - 0-indexed (line, character) in **UTF-16 units** for LSP positions.

use lsp_types::Position;

/// The character-offset encoding negotiated for LSP positions.
///
/// UTF-16 is the LSP default every client must support; UTF-8 (plain byte
/// offsets) is used when the client offers it during initialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionEncoding {
    /// `character` counts bytes from the line start.
    Utf8,
    /// `character` counts UTF-16 code units from the line start.
    #[default]
    Utf16,
}

impl PositionEncoding {
    fn units_of(self, ch: char) -> u32 {
        match self {
            PositionEncoding::Utf8 => ch.len_utf8() as u32,
            PositionEncoding::Utf16 => ch.len_utf16() as u32,
        }
    }
}

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

    /// 0-indexed LSP `Position` with the `character` offset in `encoding`
    /// units.
    pub fn byte_to_position(&self, offset: usize, encoding: PositionEncoding) -> Position {
        let clamped = offset.min(self.text.len());
        let line_idx = self.line_index_for(clamped);
        let line_start = self.line_starts[line_idx];
        let prefix = &self.text[line_start..clamped];
        let character = match encoding {
            PositionEncoding::Utf8 => prefix.len() as u32,
            PositionEncoding::Utf16 => prefix.encode_utf16().count() as u32,
        };
        Position::new(line_idx as u32, character)
    }

    /// Inverse of [`byte_to_position`](Self::byte_to_position): a 0-indexed LSP
    /// `Position` (`character` in `encoding` units) back to a byte offset. A
    /// line past the end clamps to the end of the buffer; a character past the
    /// end of the line clamps to the line's content, before its terminator; a
    /// character inside a code point rounds up to its end.
    pub fn position_to_byte(&self, position: Position, encoding: PositionEncoding) -> usize {
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
        let mut units = 0u32;
        for (byte_off, ch) in line_text.char_indices() {
            if units >= position.character {
                return line_start + byte_off;
            }
            units += encoding.units_of(ch);
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

    const UTF8: PositionEncoding = PositionEncoding::Utf8;
    const UTF16: PositionEncoding = PositionEncoding::Utf16;

    #[test]
    fn empty_string() {
        let idx = LineIndex::new("");
        assert_eq!(idx.byte_to_lc(0), LineCol { line: 1, column: 1 });
        assert_eq!(idx.byte_to_position(0, UTF16), Position::new(0, 0));
        assert_eq!(idx.byte_to_position(0, UTF8), Position::new(0, 0));
    }

    #[test]
    fn multi_line() {
        let idx = LineIndex::new("ab\ncd\nef");
        assert_eq!(idx.byte_to_lc(0), LineCol { line: 1, column: 1 });
        assert_eq!(idx.byte_to_lc(3), LineCol { line: 2, column: 1 });
        assert_eq!(idx.byte_to_position(6, UTF16), Position::new(2, 0));
        assert_eq!(idx.byte_to_position(6, UTF8), Position::new(2, 0));
    }

    #[test]
    fn encodings_diverge_after_a_surrogate_pair() {
        // U+1F600 (emoji) is 4 bytes in UTF-8, 2 UTF-16 units (surrogate pair).
        let idx = LineIndex::new("\u{1F600}x");
        assert_eq!(idx.byte_to_lc(4), LineCol { line: 1, column: 2 });
        assert_eq!(idx.byte_to_position(4, UTF16), Position::new(0, 2));
        assert_eq!(idx.byte_to_position(4, UTF8), Position::new(0, 4));
        assert_eq!(idx.position_to_byte(Position::new(0, 2), UTF16), 4);
        assert_eq!(idx.position_to_byte(Position::new(0, 4), UTF8), 4);
    }

    #[test]
    fn position_to_byte_clamps_before_line_terminator() {
        let idx = LineIndex::new("ab\ncd");
        assert_eq!(idx.position_to_byte(Position::new(0, 9), UTF16), 2);
        assert_eq!(idx.position_to_byte(Position::new(9, 0), UTF16), 5);
        assert_eq!(idx.position_to_byte(Position::new(0, 9), UTF8), 2);
        let idx = LineIndex::new("ab\r\ncd");
        assert_eq!(idx.position_to_byte(Position::new(0, 9), UTF16), 2);
        assert_eq!(idx.position_to_byte(Position::new(0, 9), UTF8), 2);
    }

    #[test]
    fn position_inside_a_code_point_rounds_up() {
        // é is 2 bytes; a UTF-8 character offset of 1 splits it.
        let idx = LineIndex::new("\u{00E9}x");
        assert_eq!(idx.position_to_byte(Position::new(0, 1), UTF8), 2);
        // The emoji is 2 UTF-16 units; an offset of 1 splits the surrogate pair.
        let idx = LineIndex::new("\u{1F600}x");
        assert_eq!(idx.position_to_byte(Position::new(0, 1), UTF16), 4);
    }

    #[test]
    fn position_to_byte_round_trips() {
        let text = "ab\ncd\u{00E9}\u{1F600}\nf";
        let idx = LineIndex::new(text);
        for encoding in [UTF8, UTF16] {
            for offset in 0..=text.len() {
                if !text.is_char_boundary(offset) {
                    continue;
                }
                let pos = idx.byte_to_position(offset, encoding);
                assert_eq!(
                    idx.position_to_byte(pos, encoding),
                    offset,
                    "offset {offset} ({encoding:?})"
                );
            }
        }
    }
}
