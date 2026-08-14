//! Byte-range text edits.
//!
//! [`Edit`] is the parser's one edit currency: a byte range in some old text
//! plus the string that replaces it. These operations are pure text
//! manipulation with no parser content; `super::reparse` re-exports them so
//! `parser::Edit` stays the path the parser layer uses. Conversion from LSP
//! `didChange` content changes lives host-side (`fatou::text`), keeping this
//! crate free of protocol dependencies.

use ropey::Rope;
use std::ops::Range;

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

    /// Apply the edit to `old` in place on a `Rope`. The rope's copy-on-write
    /// edit is O(log n + edit size), not a whole-buffer copy — this is the form
    /// the incremental reparse chain uses so replaying each edit stays cheap.
    pub fn apply_rope(&self, old: &Rope) -> Rope {
        let mut out = old.clone();
        out.remove(self.range.clone());
        out.insert(self.range.start, &self.insert);
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

/// [`diff_edit`] over a pair of [`Rope`]s, comparing bytes chunk-wise so the
/// common prefix/suffix is found without flattening either rope. Byte-identical
/// to [`diff_edit`] on the same texts.
pub fn diff_edit_rope(old: &Rope, new: &Rope) -> Edit {
    let mut prefix = 0;
    for (o, n) in old.bytes().zip(new.bytes()) {
        if o != n {
            break;
        }
        prefix += 1;
    }
    while prefix > 0 && !old.is_char_boundary(prefix) {
        prefix -= 1;
    }

    let max_suffix = (old.len() - prefix).min(new.len() - prefix);
    let mut suffix = 0;
    let mut old_back = old.bytes_at(old.len());
    let mut new_back = new.bytes_at(new.len());
    while suffix < max_suffix {
        match (old_back.prev(), new_back.prev()) {
            (Some(o), Some(n)) if o == n => suffix += 1,
            _ => break,
        }
    }
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
    }

    Edit {
        range: prefix..(old.len() - suffix),
        insert: String::from(new.slice(prefix..(new.len() - suffix))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(range: std::ops::Range<usize>, insert: &str) -> Edit {
        Edit {
            range,
            insert: insert.to_string(),
        }
    }

    /// The rope variants are byte-identical to the `&str` ones: `diff_edit_rope`
    /// recovers the same edit, and `apply_rope` reproduces the same text. The
    /// corpus exercises multi-byte chars and the common-prefix/suffix clamp.
    #[test]
    fn rope_edits_match_the_string_ones() {
        let pairs: &[(&str, &str)] = &[
            ("x = 1\n", "x = 1\n"),
            ("x = 1\n", "x = 12\n"),
            ("x = 12\n", "x = 1\n"),
            ("f(a, b)\n", "f(a, c)\n"),
            ("", "x = 1\n"),
            ("x = 1\n", ""),
            ("abc", "xyz"),
            ("α = 1\n", "β = 1\n"),
            ("x\u{3AC}", "x\u{1FB6}"),
            ("a b c\n", "z b z\n"),
        ];
        for &(a, b) in pairs {
            let e = diff_edit(a, b);
            let er = diff_edit_rope(&Rope::from(a), &Rope::from(b));
            assert_eq!(er, e, "diff_edit_rope({a:?}, {b:?})");
            assert_eq!(er.apply(a), b);
            assert_eq!(er.apply_rope(&Rope::from(a)), Rope::from(b));
        }
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
}
