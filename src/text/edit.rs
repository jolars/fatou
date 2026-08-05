//! Byte-range text edits, and applying LSP `didChange` content changes to a
//! text buffer.
//!
//! [`Edit`] is the crate's one edit currency: a byte range in some old text
//! plus the string that replaces it. It lives here rather than beside its
//! busiest consumer (`crate::parser::reparse`, which re-exports it) because
//! `crate::text` is a leaf module shared by the CLI, diagnostics, and the
//! language server, and these operations are pure text manipulation with no
//! parser content.

use std::ops::Range;

use lsp_types::TextDocumentContentChangeEvent;

use super::{LineIndex, PositionEncoding};

/// A single contiguous text edit: replace `range` (a byte range in the *old*
/// text) with `insert`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub insert: String,
}

impl Edit {
    /// The signed length change this edit applies to text after `range`.
    pub fn delta(&self) -> isize {
        self.insert.len() as isize - (self.range.end - self.range.start) as isize
    }

    /// Apply the edit to `old`, producing the new text.
    pub fn apply(&self, old: &str) -> String {
        let mut out =
            String::with_capacity(old.len().saturating_sub(self.range.len()) + self.insert.len());
        out.push_str(&old[..self.range.start]);
        out.push_str(&self.insert);
        out.push_str(&old[self.range.end..]);
        out
    }
}

/// Apply `edits` to `old` left-to-right, each expressed against the text its
/// predecessors produced (the shape LSP `didChange` batches arrive in). Folds
/// in place, so peak memory is one text rather than one per edit.
///
/// # Panics
///
/// If any edit's range is out of bounds or off a char boundary of the text its
/// predecessors produced. Use [`try_apply_edits`] for a slice that may be
/// stale.
pub fn apply_edits(old: &str, edits: &[Edit]) -> String {
    try_apply_edits(old, edits).expect("apply_edits: edit slice does not fit the text")
}

/// [`apply_edits`] for an edit slice of unproven provenance: [`None`] when any
/// edit's range does not fit the text its predecessors produced.
///
/// This is the apply-and-verify guard the incremental reparse leans on.
/// Reconstructing the current buffer from an old snapshot plus an edit slice
/// proves the slice is the exact transform between them, so a stale or
/// misaligned sequence rejects — either here, or by comparing the result
/// against the buffer — in favor of the whole-text [`diff_edit`] fallback.
pub fn try_apply_edits(old: &str, edits: &[Edit]) -> Option<String> {
    let mut text = old.to_string();
    for e in edits {
        if e.range.start > e.range.end
            || e.range.end > text.len()
            || !text.is_char_boundary(e.range.start)
            || !text.is_char_boundary(e.range.end)
        {
            return None;
        }
        text.replace_range(e.range.clone(), &e.insert);
    }
    Some(text)
}

/// Recover a single contiguous [`Edit`] from a pair of full texts by stripping
/// the common prefix and suffix. Multiple disjoint edits collapse into one
/// spanning edit — still a correct transform, just coarser. Boundaries are
/// clamped to char boundaries of both texts.
pub fn diff_edit(old: &str, new: &str) -> Edit {
    let ob = old.as_bytes();
    let nb = new.as_bytes();

    let mut prefix = 0;
    let max_prefix = ob.len().min(nb.len());
    while prefix < max_prefix && ob[prefix] == nb[prefix] {
        prefix += 1;
    }
    while prefix > 0 && !old.is_char_boundary(prefix) {
        prefix -= 1;
    }

    let mut suffix = 0;
    let max_suffix = (ob.len() - prefix).min(nb.len() - prefix);
    while suffix < max_suffix && ob[ob.len() - 1 - suffix] == nb[nb.len() - 1 - suffix] {
        suffix += 1;
    }
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
    }

    Edit {
        range: prefix..(old.len() - suffix),
        insert: new[prefix..(new.len() - suffix)].to_string(),
    }
}

/// Apply a `didChange` batch to `text` in place, interpreting range positions
/// in the negotiated `encoding`, and return the byte [`Edit`]s that describe
/// the transform.
///
/// Changes apply sequentially: each range is interpreted against the text as
/// it stands after the previous change, so the line table is rebuilt per
/// ranged change. A change without a range replaces the whole buffer (legal
/// from clients even under incremental sync), so application starts at the
/// last such change and everything before it is skipped. Out-of-range
/// positions clamp to the end of the line or buffer.
///
/// The returned edits share that left-to-right convention, so
/// `apply_edits(old_text, &edits)` reproduces the new buffer exactly — which
/// is what lets the incremental reparse consume them (`TODO.md`,
/// `### Incremental`). A batch containing a whole-buffer replacement returns
/// [`None`]: the transform from the previous buffer is then unknown, and the
/// reparse layer must fall back to a whole-text diff.
pub fn apply_content_changes(
    text: &mut String,
    changes: Vec<TextDocumentContentChangeEvent>,
    encoding: PositionEncoding,
) -> Option<Vec<Edit>> {
    let start = changes
        .iter()
        .rposition(|change| change.range.is_none())
        .unwrap_or(0);
    let mut edits = Vec::with_capacity(changes.len() - start);
    let mut replaced = false;
    for change in &changes[start..] {
        match change.range {
            Some(range) => {
                let index = LineIndex::new(text);
                let start = index.position_to_byte(range.start, encoding);
                let end = index.position_to_byte(range.end, encoding);
                text.replace_range(start..end, &change.text);
                edits.push(Edit {
                    range: start..end,
                    insert: change.text.clone(),
                });
            }
            None => {
                text.clear();
                text.push_str(&change.text);
                replaced = true;
            }
        }
    }
    (!replaced).then_some(edits)
}

#[cfg(test)]
mod tests {
    use lsp_types::{Position, Range};

    use super::*;

    fn ranged(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_string(),
        }
    }

    fn full(text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_string(),
        }
    }

    fn apply(initial: &str, changes: Vec<TextDocumentContentChangeEvent>) -> String {
        let mut text = initial.to_string();
        let edits = apply_content_changes(&mut text, changes, PositionEncoding::Utf16);
        // Whatever the batch was, the edits it reports must reproduce it.
        if let Some(edits) = edits {
            assert_eq!(
                apply_edits(initial, &edits),
                text,
                "reported edits: {edits:?}"
            );
        }
        text
    }

    fn edit(range: std::ops::Range<usize>, insert: &str) -> Edit {
        Edit {
            range,
            insert: insert.to_string(),
        }
    }

    #[test]
    fn insert_delete_replace_on_one_line() {
        assert_eq!(apply("ab", vec![ranged((0, 1), (0, 1), "x")]), "axb");
        assert_eq!(apply("axb", vec![ranged((0, 1), (0, 2), "")]), "ab");
        assert_eq!(apply("abc", vec![ranged((0, 1), (0, 2), "xy")]), "axyc");
    }

    #[test]
    fn sequential_changes_see_prior_edits() {
        // The second range is only correct against the post-first-change text:
        // (0, 3)..(0, 3) lands after "xyz" only once the first insert applied.
        let changes = vec![ranged((0, 0), (0, 0), "xyz"), ranged((0, 3), (0, 3), "!")];
        assert_eq!(apply("ab", changes), "xyz!ab");
    }

    #[test]
    fn edit_spanning_a_newline() {
        assert_eq!(apply("ab\ncd", vec![ranged((0, 1), (1, 1), "-")]), "a-d");
    }

    #[test]
    fn insert_adding_lines_shifts_later_ranges() {
        // The second change targets line 2, which only exists after the first
        // change inserts a newline: the line table must be rebuilt in between.
        let changes = vec![ranged((0, 2), (0, 2), "\nnew"), ranged((1, 3), (1, 3), "!")];
        assert_eq!(apply("ab\ncd", changes), "ab\nnew!\ncd");
    }

    #[test]
    fn utf16_offsets_after_surrogate_pair() {
        // U+1F600 is 2 UTF-16 units, so character 2 is just past the emoji.
        assert_eq!(
            apply("\u{1F600}x", vec![ranged((0, 2), (0, 3), "y")]),
            "\u{1F600}y"
        );
    }

    #[test]
    fn utf8_offsets_after_surrogate_pair() {
        // Under the negotiated utf-8 encoding, U+1F600 is 4 units (bytes), so
        // character 4 is just past the emoji.
        let mut text = "\u{1F600}x".to_string();
        apply_content_changes(
            &mut text,
            vec![ranged((0, 4), (0, 5), "y")],
            PositionEncoding::Utf8,
        );
        assert_eq!(text, "\u{1F600}y");
    }

    #[test]
    fn full_replacement() {
        assert_eq!(apply("old", vec![full("new")]), "new");
    }

    #[test]
    fn ranged_changes_report_their_byte_edits() {
        let mut text = "ab\ncd".to_string();
        let edits = apply_content_changes(
            &mut text,
            vec![ranged((0, 1), (0, 1), "x"), ranged((1, 1), (1, 1), "y")],
            PositionEncoding::Utf16,
        );
        // The second edit's offset is against the post-first-change text, where
        // line 1 starts one byte later than it did in the original buffer.
        assert_eq!(edits, Some(vec![edit(1..1, "x"), edit(5..5, "y")]));
        assert_eq!(text, "axb\ncyd");
    }

    #[test]
    fn a_full_replacement_reports_no_edits() {
        let mut text = "old".to_string();
        assert_eq!(
            apply_content_changes(
                &mut text,
                vec![full("base\n"), ranged((0, 4), (0, 4), "!")],
                PositionEncoding::Utf16,
            ),
            None,
        );
        assert_eq!(text, "base!\n");
    }

    #[test]
    fn an_empty_batch_reports_an_empty_edit_slice() {
        let mut text = "x = 1\n".to_string();
        assert_eq!(
            apply_content_changes(&mut text, vec![], PositionEncoding::Utf16),
            Some(vec![]),
        );
        assert_eq!(text, "x = 1\n");
    }

    #[test]
    fn diff_edit_recovers_a_noop() {
        assert_eq!(diff_edit("x = 1\n", "x = 1\n"), edit(6..6, ""));
    }

    #[test]
    fn diff_edit_recovers_an_insertion() {
        let e = diff_edit("x = 1\n", "x = 12\n");
        assert_eq!(e, edit(5..5, "2"));
        assert_eq!(e.apply("x = 1\n"), "x = 12\n");
        assert_eq!(e.delta(), 1);
    }

    #[test]
    fn diff_edit_recovers_a_deletion() {
        let e = diff_edit("x = 12\n", "x = 1\n");
        assert_eq!(e, edit(5..6, ""));
        assert_eq!(e.apply("x = 12\n"), "x = 1\n");
        assert_eq!(e.delta(), -1);
    }

    #[test]
    fn diff_edit_recovers_a_replacement() {
        let e = diff_edit("f(a, b)\n", "f(a, c)\n");
        assert_eq!(e, edit(5..6, "c"));
        assert_eq!(e.apply("f(a, b)\n"), "f(a, c)\n");
        assert_eq!(e.delta(), 0);
    }

    #[test]
    fn diff_edit_collapses_disjoint_edits_into_one_span() {
        // Two edits (a→z at 0, c→z at 4) collapse into one spanning edit.
        let e = diff_edit("a b c\n", "z b z\n");
        assert_eq!(e, edit(0..5, "z b z"));
        assert_eq!(e.apply("a b c\n"), "z b z\n");
    }

    #[test]
    fn diff_edit_handles_whole_replacement_and_empty_texts() {
        assert_eq!(diff_edit("", "x = 1\n"), edit(0..0, "x = 1\n"));
        assert_eq!(diff_edit("x = 1\n", ""), edit(0..6, ""));
        assert_eq!(diff_edit("abc", "xyz"), edit(0..3, "xyz"));
    }

    #[test]
    fn diff_edit_clamps_to_char_boundaries() {
        // α (0xCE 0xB1) and β (0xCE 0xB2) share their lead byte; a naive byte
        // prefix would split the code point.
        let e = diff_edit("α = 1\n", "β = 1\n");
        assert_eq!(e, edit(0..2, "β"));
        assert_eq!(e.apply("α = 1\n"), "β = 1\n");

        // Shared trail bytes: ά vs ᾶ end in different code points whose UTF-8
        // shares a final byte pattern only mid-character.
        let old = "x\u{3AC}";
        let new = "x\u{1FB6}";
        let e = diff_edit(old, new);
        assert!(old.is_char_boundary(e.range.start));
        assert!(old.is_char_boundary(e.range.end));
        assert_eq!(e.apply(old), new);
    }

    #[test]
    fn apply_edits_chains_left_to_right() {
        let edits = vec![edit(0..1, "y"), edit(4..5, "2")];
        assert_eq!(apply_edits("x = 1\n", &edits), "y = 2\n");
        assert_eq!(apply_edits("x = 1\n", &[]), "x = 1\n");
    }

    #[test]
    fn try_apply_edits_rejects_a_slice_that_does_not_fit() {
        // Past the end of the text.
        assert_eq!(try_apply_edits("x = 1\n", &[edit(9..9, "!")]), None);
        // Inverted range (built by hand: a literal `4..2` is a clippy error).
        let inverted = Edit {
            range: std::ops::Range { start: 4, end: 2 },
            insert: "!".to_string(),
        };
        assert_eq!(try_apply_edits("x = 1\n", &[inverted]), None);
        // Off a char boundary (α is two bytes).
        assert_eq!(try_apply_edits("α", &[edit(1..1, "!")]), None);
        // Only the *second* edit misfits: a chain rejects as a whole.
        let edits = vec![edit(0..6, ""), edit(3..3, "!")];
        assert_eq!(try_apply_edits("x = 1\n", &edits), None);
    }

    #[test]
    fn changes_before_a_full_replacement_are_skipped() {
        let changes = vec![
            ranged((5, 0), (9, 0), "junk that must not apply"),
            full("base\n"),
            ranged((0, 4), (0, 4), "!"),
        ];
        assert_eq!(apply("ab", changes), "base!\n");
    }

    #[test]
    fn out_of_range_positions_clamp() {
        assert_eq!(apply("ab\ncd", vec![ranged((0, 9), (9, 9), "!")]), "ab!");
        assert_eq!(apply("", vec![ranged((3, 1), (4, 2), "x")]), "x");
    }
}
