//! Byte-offset → (line, column) conversion.
//!
//! Two coordinate systems share the same line-start table:
//! - 1-indexed (line, column) in **code points** for CLI diagnostics.
//! - 0-indexed (line, character) in **UTF-16 units** for LSP positions.

use std::borrow::Cow;
use std::ops::{Deref, Range};

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

/// The line-start byte offsets of a text buffer.
///
/// `self[i]` is the byte offset of the first character of line `i` (0-indexed);
/// the table always starts with `0`.
///
/// It is a value of its own, separate from the text, so a live buffer can
/// [patch](Self::patch) it across an edit rather than rescanning. Building it
/// is linear in the buffer, which on a large file costs several times the
/// incremental reparse the edit goes on to trigger — see
/// `benches/line_index.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineStarts(Vec<usize>);

impl Default for LineStarts {
    /// The empty buffer's table: one line, starting at 0. Deriving this would
    /// give an empty `Vec`, which is not a line table at all.
    fn default() -> Self {
        Self(vec![0])
    }
}

impl LineStarts {
    /// Scan `text` for its line starts.
    pub fn new(text: &str) -> Self {
        // 40 bytes per line is a rough fit for Julia source; the scan itself is
        // `memchr`'s, which is about twice a hand-written byte loop.
        let mut starts = Vec::with_capacity(text.len() / 40 + 1);
        starts.push(0);
        starts.extend(memchr::memchr_iter(b'\n', text.as_bytes()).map(|at| at + 1));
        Self(starts)
    }

    /// Patch the table for a replacement of `range` with `insert`, leaving it
    /// exactly as [`new`](Self::new) would have scanned the edited text.
    ///
    /// `range` is a byte range in the *pre-edit* text. Three groups of line
    /// starts fall out of that: those at or before `range.start` are untouched
    /// (a newline ending such a line sits before the replaced bytes); those in
    /// `range.start + 1 ..= range.end` sat inside the replaced text and are
    /// gone; those past `range.end` shift by the edit's byte delta. Only the
    /// last group costs anything, and it is one add per line rather than a
    /// scan per byte.
    pub fn patch(&mut self, range: Range<usize>, insert: &str) {
        let Range { start, end } = range;
        debug_assert!(start <= end, "reversed edit range {start}..{end}");
        let first = self.0.partition_point(|&at| at <= start);
        let last = self.0.partition_point(|&at| at <= end);
        let delta = insert.len() as isize - (end - start) as isize;
        if delta != 0 {
            for at in &mut self.0[last..] {
                *at = at.wrapping_add_signed(delta);
            }
        }
        let inserted = memchr::memchr_iter(b'\n', insert.as_bytes()).map(|at| start + at + 1);
        drop(self.0.splice(first..last, inserted));
    }
}

impl Deref for LineStarts {
    type Target = [usize];

    fn deref(&self) -> &[usize] {
        &self.0
    }
}

/// A text buffer paired with its line-start table.
///
/// The table is either built for the occasion ([`new`](Self::new)) or borrowed
/// from a buffer that maintains one ([`with_starts`](Self::with_starts)).
#[derive(Debug, Clone)]
pub struct LineIndex<'a> {
    text: &'a str,
    line_starts: Cow<'a, LineStarts>,
}

impl<'a> LineIndex<'a> {
    /// Scan `text` for a one-off index. Prefer
    /// [`with_starts`](Self::with_starts) wherever a maintained table is at
    /// hand: this rescans the whole buffer.
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            line_starts: Cow::Owned(LineStarts::new(text)),
        }
    }

    /// An index over `text` reusing an already-built table.
    ///
    /// `line_starts` must be `text`'s own table, as
    /// [`crate::text::TextBuffer`] keeps it; pairing it with a different text
    /// yields wrong positions rather than a panic.
    pub fn with_starts(text: &'a str, line_starts: &'a LineStarts) -> Self {
        Self {
            text,
            line_starts: Cow::Borrowed(line_starts),
        }
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

    /// The whole point of [`LineStarts::patch`]: over every replacement of
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
                        let mut patched = LineStarts::new(text);
                        patched.patch(start..end, insert);
                        let mut edited = text.to_string();
                        edited.replace_range(start..end, insert);
                        assert_eq!(
                            patched,
                            LineStarts::new(&edited),
                            "{text:?} [{start}..{end}] -> {insert:?} gives {edited:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_borrowed_table_indexes_the_same_as_a_scanned_one() {
        let text = "ab\ncd\u{1F600}\nef";
        let starts = LineStarts::new(text);
        let borrowed = LineIndex::with_starts(text, &starts);
        let scanned = LineIndex::new(text);
        for offset in 0..=text.len() {
            if !text.is_char_boundary(offset) {
                continue;
            }
            assert_eq!(
                borrowed.byte_to_position(offset, UTF16),
                scanned.byte_to_position(offset, UTF16),
            );
            assert_eq!(borrowed.byte_to_lc(offset), scanned.byte_to_lc(offset));
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
