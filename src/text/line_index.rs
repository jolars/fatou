//! Byte-offset → (line, column) conversion.
//!
//! Two coordinate systems share the same line-start table:
//! - 1-indexed (line, column) in **code points** for CLI diagnostics.
//! - 0-indexed (line, character) in **UTF-16 units** for LSP positions.

use std::ops::Range;

use lsp_types::Position;
use ropey::{LineType, Rope};

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

/// The text of a buffer together with its line-start table.
///
/// The text and its line-start offsets both live in a [`Rope`], whose line
/// metrics answer "where does line `i` start" and "which line is byte `b` on"
/// in O(log n) rather than by binary-searching a `Vec<usize>`.
///
/// It is a value of its own, so a live buffer can [patch](Self::patch) it
/// across an edit rather than rescanning. Building it is linear in the buffer,
/// which on a large file costs several times the incremental reparse the edit
/// goes on to trigger — see `benches/line_index.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    rope: Rope,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self { rope: Rope::new() }
    }
}

impl LineIndex {
    /// Build an index from `text`, copying it into a [`Rope`] of its own.
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from(text),
        }
    }

    /// Patch the index for a replacement of `range` with `insert`, leaving it
    /// exactly as [`new`](Self::new) would have scanned the edited text.
    ///
    /// `range` is a byte range in the *pre-edit* text. The rope edits its own
    /// copy of the text, so the line metrics stay in step with what
    /// [`crate::text::TextBuffer`] holds — one rope edit, not a per-line shift.
    pub fn patch(&mut self, range: Range<usize>, insert: &str) {
        self.rope.remove(range.clone());
        self.rope.insert(range.start, insert);
    }

    /// 1-indexed (line, column-in-code-points). Suitable for CLI diagnostics.
    pub fn byte_to_lc(&self, offset: usize) -> LineCol {
        let clamped = offset.min(self.rope.len());
        let line_idx = self.line_index_for(clamped);
        let line_start = self.rope.line_to_byte_idx(line_idx, LineType::LF);
        let column = self.rope.slice(line_start..clamped).chars().count() + 1;
        LineCol {
            line: line_idx + 1,
            column,
        }
    }

    /// 0-indexed LSP `Position` with the `character` offset in `encoding`
    /// units.
    pub fn byte_to_position(&self, offset: usize, encoding: PositionEncoding) -> Position {
        let clamped = offset.min(self.rope.len());
        let line_idx = self.line_index_for(clamped);
        let line_start = self.rope.line_to_byte_idx(line_idx, LineType::LF);
        let prefix = self.rope.slice(line_start..clamped);
        let character = match encoding {
            PositionEncoding::Utf8 => prefix.len() as u32,
            PositionEncoding::Utf16 => prefix.len_utf16() as u32,
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
        if line >= self.line_count() {
            return self.rope.len();
        }
        let line_start = self.rope.line_to_byte_idx(line, LineType::LF);
        let line_end = self.rope.line_to_byte_idx(line + 1, LineType::LF);
        // The line slice runs to its `\n`/`\r\n` terminator; drop it so a
        // character past the end clamps to the content before it.
        let mut content_end = line_end;
        if content_end > line_start && self.rope.byte(content_end - 1) == b'\n' {
            content_end -= 1;
            if content_end > line_start && self.rope.byte(content_end - 1) == b'\r' {
                content_end -= 1;
            }
        }
        let content = self.rope.slice(line_start..content_end);
        let mut units = 0u32;
        for (byte_off, ch) in content.char_indices() {
            if units >= position.character {
                return line_start + byte_off;
            }
            units += encoding.units_of(ch);
        }
        line_start + content.len()
    }

    /// Total line count (1 even for empty text).
    pub fn line_count(&self) -> usize {
        self.rope.len_lines(LineType::LF)
    }

    /// Byte offset of the start of the 0-indexed `line`. A line past the end
    /// clamps to the buffer end, so `line_start(n)..line_start(n + 1)` is always
    /// a valid slice range covering line `n` *including* its newline. The pretty
    /// diagnostic renderer slices a snippet window with it.
    pub fn line_start(&self, line: usize) -> usize {
        self.line_starts
            .get(line)
            .copied()
            .unwrap_or(self.text.len())
    }

    fn line_index_for(&self, offset: usize) -> usize {
        self.rope.byte_to_line_idx(offset, LineType::LF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTF8: PositionEncoding = PositionEncoding::Utf8;
    const UTF16: PositionEncoding = PositionEncoding::Utf16;

    /// The whole point of [`LineIndex::patch`]: over every replacement of
    /// every char-boundary range of a handful of awkward texts, the patched
    /// table equals the one a rescan of the edited text would produce.
    #[test]
    fn patching_matches_a_rescan() {
        let texts = [
            "",
            "\n",
            "\n\n",
            "abc",
            "ab\ncd\nef\n",
            "a\r\nb\r\n",
            "\u{1F600}\nx\n",
        ];
        let inserts = ["", "z", "\n", "\n\n", "x\ny\n", "\r\n", "\u{1F600}"];
        for text in texts {
            for start in 0..=text.len() {
                for end in start..=text.len() {
                    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                        continue;
                    }
                    for insert in inserts {
                        let mut patched = LineIndex::new(text);
                        patched.patch(start..end, insert);
                        let mut edited = text.to_string();
                        edited.replace_range(start..end, insert);
                        assert_eq!(
                            patched,
                            LineIndex::new(&edited),
                            "{text:?} [{start}..{end}] -> {insert:?} gives {edited:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_maintained_table_indexes_the_same_as_a_scanned_one() {
        let text = "ab\ncd\u{1F600}\nef";
        let buffer = crate::text::TextBuffer::from(text);
        let maintained = buffer.line_index();
        let scanned = LineIndex::new(text);
        for offset in 0..=text.len() {
            if !text.is_char_boundary(offset) {
                continue;
            }
            assert_eq!(
                maintained.byte_to_position(offset, UTF16),
                scanned.byte_to_position(offset, UTF16),
            );
            assert_eq!(maintained.byte_to_lc(offset), scanned.byte_to_lc(offset));
        }
    }

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
