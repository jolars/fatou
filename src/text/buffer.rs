//! A live text buffer that carries its own line-start table.
//!
//! The language server resolves LSP positions against a document many times per
//! keystroke: once to splice the `didChange` in, then once more in every read
//! handler that answers against the buffer. Scanning the text for its line
//! starts each of those times is linear in the *buffer*, not in the edit, and
//! on a large file that scan costs several times the incremental reparse it
//! precedes (`benches/line_index.rs`).
//!
//! [`TextBuffer`] is the fix: the table lives beside the text and is patched
//! across each edit, so a keystroke costs one memmove plus one add per line
//! after the edit site, and every reader shares the result.

use std::ops::{Deref, Range};

use super::line_index::{LineIndex, LineStarts};

/// Text plus the line-start table that indexes it, kept in step.
///
/// Derefs to `str`, so anything that just wants the text — `parse`, the
/// formatter, the linter — takes it unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextBuffer {
    text: String,
    line_starts: LineStarts,
}

impl TextBuffer {
    /// Take ownership of `text`, scanning it once for its line starts.
    pub fn new(text: String) -> Self {
        let line_starts = LineStarts::new(&text);
        Self { text, line_starts }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The maintained table, for a caller building its own [`LineIndex`].
    pub fn line_starts(&self) -> &LineStarts {
        &self.line_starts
    }

    /// An index over this buffer. Unlike [`LineIndex::new`] this reuses the
    /// maintained table instead of rescanning, which is the whole point of the
    /// type: call it freely.
    pub fn line_index(&self) -> LineIndex<'_> {
        LineIndex::with_starts(&self.text, &self.line_starts)
    }

    /// Replace the bytes in `range` with `insert`, patching the line table
    /// rather than rebuilding it.
    ///
    /// Panics on a range that is out of bounds or not on a char boundary, as
    /// [`String::replace_range`] does.
    pub fn replace_range(&mut self, range: Range<usize>, insert: &str) {
        self.line_starts.patch(range.clone(), insert);
        self.text.replace_range(range, insert);
        self.debug_assert_in_step();
    }

    /// Replace the whole buffer, rescanning. This is the `didChange`-without-a-
    /// range case and the `didOpen` case; there is no edit to patch with.
    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.line_starts = LineStarts::new(&self.text);
    }

    pub fn into_string(self) -> String {
        self.text
    }

    /// The invariant the type exists to uphold: the table is always exactly
    /// what a rescan would produce. Debug-only — it is linear in the buffer,
    /// which is the cost [`replace_range`](Self::replace_range) avoids.
    fn debug_assert_in_step(&self) {
        debug_assert!(
            self.line_starts == LineStarts::new(&self.text),
            "line table drifted from the buffer"
        );
    }
}

impl Deref for TextBuffer {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl From<String> for TextBuffer {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for TextBuffer {
    fn from(text: &str) -> Self {
        Self::new(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_buffer_has_one_line() {
        let buffer = TextBuffer::default();
        assert_eq!(buffer.line_index().line_count(), 1);
        assert_eq!(&*buffer, "");
    }

    #[test]
    fn edits_keep_the_table_in_step() {
        let mut buffer = TextBuffer::from("ab\ncd\nef");
        buffer.replace_range(2..2, "\nxy");
        assert_eq!(&*buffer, "ab\nxy\ncd\nef");
        assert_eq!(buffer.line_starts(), &LineStarts::new(&buffer));
        // A deletion spanning the newlines it introduced.
        buffer.replace_range(2..9, "");
        assert_eq!(&*buffer, "abef");
        assert_eq!(buffer.line_starts(), &LineStarts::new(&buffer));
        assert_eq!(buffer.line_index().line_count(), 1);
    }

    #[test]
    fn set_text_rescans() {
        let mut buffer = TextBuffer::from("one line");
        buffer.set_text("two\nlines".to_string());
        assert_eq!(buffer.line_index().line_count(), 2);
        assert_eq!(buffer.line_starts(), &LineStarts::new(&buffer));
    }
}
