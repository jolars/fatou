//! A text buffer that is also its own line index.
//!
//! The language server resolves LSP positions against a document many times per
//! keystroke: once to splice the `didChange` in, then again in every read
//! handler that answers against the buffer. Storing the text as a [`ropey::Rope`]
//! makes text and its line-start table one thing — the rope's line metrics answer
//! "where does line `i` start" and "which line is byte `b` on" in O(log n) — and
//! lets a live buffer [patch](TextBuffer::replace_range) the rope across each
//! edit rather than rescanning.
//!
//! [`TextBuffer`] therefore owns the text outright. Getting it back as a
//! contiguous `&str` is a flatten ([`TextBuffer::text`]), linear in the buffer;
//! the parser and salsa still take `&str`, so the analysis write-phase flattens
//! once per keystroke (see `benches/line_index.rs`).

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

/// Text stored as a rope, together with the line-start table the rope's line
/// metrics already encode.
///
/// This is the text and its index in one: a live document edits the rope in
/// place ([`replace_range`](Self::replace_range)) and every reader resolves
/// positions against the same rope
/// ([`byte_to_position`](Self::byte_to_position)). [`new`](Self::new) builds a
/// rope over borrowed text — the one-off index over db-resolved text — and is
/// the only place text is copied into a rope.
///
/// A buffer does not deref to `str`: ropey has no zero-copy `&str` view of a
/// multi-chunk rope. Callers that need contiguous text call
/// [`text`](Self::text) (a flatten); callers that need a position or a length
/// use the rope methods directly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextBuffer {
    rope: Rope,
}

impl TextBuffer {
    /// Build a buffer over `text`, copying it into a [`Rope`].
    pub fn new<S: AsRef<str>>(text: S) -> Self {
        Self {
            rope: Rope::from(text.as_ref()),
        }
    }

    /// The text as a contiguous [`String`], flattening the rope. Linear in the
    /// buffer; reach for the rope methods where a position or a length suffices.
    pub fn text(&self) -> String {
        String::from(&self.rope)
    }

    /// The text, consuming the buffer. Linear in the buffer.
    pub fn into_string(self) -> String {
        String::from(self.rope)
    }

    /// Byte length of the text.
    pub fn len(&self) -> usize {
        self.rope.len()
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.rope.len() == 0
    }

    /// The line index over this buffer — `self`, since the buffer *is* the
    /// index. Kept so call sites reading `text.line_index().position_to_byte(..)`
    /// stay meaningful; returns a shared reference, not a copy.
    pub fn line_index(&self) -> &Self {
        self
    }

    /// Replace the bytes in `range` with `insert`, editing the rope in place.
    ///
    /// Panics on a range that is out of bounds or not on a char boundary, as
    /// [`String::replace_range`] does.
    pub fn replace_range(&mut self, range: Range<usize>, insert: &str) {
        self.rope.remove(range.clone());
        self.rope.insert(range.start, insert);
    }

    /// Replace the whole buffer. This is the `didChange`-without-a-range case
    /// and the `didOpen` case; there is no edit to patch with.
    pub fn set_text(&mut self, text: String) {
        self.rope = Rope::from(text);
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

    fn line_index_for(&self, offset: usize) -> usize {
        self.rope.byte_to_line_idx(offset, LineType::LF)
    }
}

/// Compare against plain text, so the language server's "does the db's tracked
/// input still match the live buffer" check reads exactly as it did when a
/// document was a `String`. Content-comparison against the rope — chunk-wise,
/// no allocation.
impl PartialEq<str> for TextBuffer {
    fn eq(&self, other: &str) -> bool {
        self.rope == *other
    }
}

impl PartialEq<TextBuffer> for str {
    fn eq(&self, other: &TextBuffer) -> bool {
        *self == other.rope
    }
}

impl From<String> for TextBuffer {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for TextBuffer {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTF8: PositionEncoding = PositionEncoding::Utf8;
    const UTF16: PositionEncoding = PositionEncoding::Utf16;

    #[test]
    fn an_empty_buffer_has_one_line() {
        let buffer = TextBuffer::default();
        assert_eq!(buffer.line_count(), 1);
        assert_eq!(buffer.text(), "");
    }

    #[test]
    fn edits_keep_the_buffer_coherent() {
        let mut buffer = TextBuffer::from("ab\ncd\nef");
        buffer.replace_range(2..2, "\nxy");
        assert_eq!(buffer.text(), "ab\nxy\ncd\nef");
        assert_eq!(buffer.line_count(), 4);
        // A deletion spanning the newlines it introduced.
        buffer.replace_range(2..9, "");
        assert_eq!(buffer.text(), "abef");
        assert_eq!(buffer.line_count(), 1);
    }

    #[test]
    fn set_text_replaces_wholesale() {
        let mut buffer = TextBuffer::from("one line");
        buffer.set_text("two\nlines".to_string());
        assert_eq!(buffer.line_count(), 2);
        assert_eq!(buffer.text(), "two\nlines");
    }

    /// The whole point of [`TextBuffer::replace_range`]: over every replacement
    /// of every char-boundary range of a handful of awkward texts, the edited
    /// buffer equals one built fresh from the edited text.
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
                        let mut patched = TextBuffer::new(text);
                        patched.replace_range(start..end, insert);
                        let mut edited = text.to_string();
                        edited.replace_range(start..end, insert);
                        assert_eq!(
                            patched,
                            TextBuffer::new(&edited),
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
        let buffer = TextBuffer::from(text);
        let maintained = buffer.line_index();
        let scanned = TextBuffer::new(text);
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
        let idx = TextBuffer::new("");
        assert_eq!(idx.byte_to_lc(0), LineCol { line: 1, column: 1 });
        assert_eq!(idx.byte_to_position(0, UTF16), Position::new(0, 0));
        assert_eq!(idx.byte_to_position(0, UTF8), Position::new(0, 0));
    }

    #[test]
    fn multi_line() {
        let idx = TextBuffer::new("ab\ncd\nef");
        assert_eq!(idx.byte_to_lc(0), LineCol { line: 1, column: 1 });
        assert_eq!(idx.byte_to_lc(3), LineCol { line: 2, column: 1 });
        assert_eq!(idx.byte_to_position(6, UTF16), Position::new(2, 0));
        assert_eq!(idx.byte_to_position(6, UTF8), Position::new(2, 0));
    }

    #[test]
    fn encodings_diverge_after_a_surrogate_pair() {
        // U+1F600 (emoji) is 4 bytes in UTF-8, 2 UTF-16 units (surrogate pair).
        let idx = TextBuffer::new("\u{1F600}x");
        assert_eq!(idx.byte_to_lc(4), LineCol { line: 1, column: 2 });
        assert_eq!(idx.byte_to_position(4, UTF16), Position::new(0, 2));
        assert_eq!(idx.byte_to_position(4, UTF8), Position::new(0, 4));
        assert_eq!(idx.position_to_byte(Position::new(0, 2), UTF16), 4);
        assert_eq!(idx.position_to_byte(Position::new(0, 4), UTF8), 4);
    }

    #[test]
    fn position_to_byte_clamps_before_line_terminator() {
        let idx = TextBuffer::new("ab\ncd");
        assert_eq!(idx.position_to_byte(Position::new(0, 9), UTF16), 2);
        assert_eq!(idx.position_to_byte(Position::new(9, 0), UTF16), 5);
        assert_eq!(idx.position_to_byte(Position::new(0, 9), UTF8), 2);
        let idx = TextBuffer::new("ab\r\ncd");
        assert_eq!(idx.position_to_byte(Position::new(0, 9), UTF16), 2);
        assert_eq!(idx.position_to_byte(Position::new(0, 9), UTF8), 2);
    }

    #[test]
    fn position_inside_a_code_point_rounds_up() {
        // é is 2 bytes; a UTF-8 character offset of 1 splits it.
        let idx = TextBuffer::new("\u{00E9}x");
        assert_eq!(idx.position_to_byte(Position::new(0, 1), UTF8), 2);
        // The emoji is 2 UTF-16 units; an offset of 1 splits the surrogate pair.
        let idx = TextBuffer::new("\u{1F600}x");
        assert_eq!(idx.position_to_byte(Position::new(0, 1), UTF16), 4);
    }

    #[test]
    fn position_to_byte_round_trips() {
        let text = "ab\ncd\u{00E9}\u{1F600}\nf";
        let idx = TextBuffer::new(text);
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
