//! Incremental reparse: turn a text edit plus the previous parse into a new
//! tree without re-lexing and re-parsing the whole file.
//!
//! Modelled on rust-analyzer's `reparsing.rs` and arity's `reparse.rs`: try
//! the cheapest strategy first and fall back to progressively more work, with
//! a full [`parse`](crate::parser::parse) as the always-correct last resort.
//! The staged plan lives in `TODO.md` (`### Incremental`); this module is
//! stage 0 — the edit plumbing plus a [`reparse`] stub that always falls back,
//! so behavior is unchanged while the salsa side-channel and the oracle
//! harness land against the real API.
//!
//! **Correctness invariant (Tenet 4):** a successful reparse must yield a
//! green tree *and* diagnostics byte-identical to a full parse of the edited
//! text. A reparse is an output-pure performance hint: the salsa layer only
//! ever sees text changes, and a miss is a full parse, never an error.

use std::ops::Range;

use rowan::GreenNode;

use crate::parser::ParseDiagnostic;

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
/// predecessors produced (the shape LSP `didChange` batches arrive in).
///
/// Doubles as an apply-and-verify guard: reconstructing the current buffer
/// from an old snapshot plus an edit slice proves the slice is the exact
/// transform between them, so a stale or misaligned sequence can be rejected
/// in favor of the whole-text [`diff_edit`] fallback.
pub fn apply_edits(old: &str, edits: &[Edit]) -> String {
    edits.iter().fold(old.to_string(), |text, e| e.apply(&text))
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

/// Which strategy produced a [`Reparsed`]. Surfaced for tests and benchmarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparseTier {
    /// A single leaf token was relexed in isolation and spliced in place.
    Token,
    /// A run of top-level statements was reparsed and spliced under `ROOT`.
    TopLevel,
}

/// The result of a successful incremental reparse: the new whole-file green
/// tree and its parse diagnostics (absolute offsets in the new text).
#[derive(Debug, Clone)]
pub struct Reparsed {
    pub green: GreenNode,
    pub diagnostics: Vec<ParseDiagnostic>,
    pub tier: ReparseTier,
}

/// Attempt an incremental reparse of the previous parse (`prev_green`, parsed
/// from `prev_text` with `prev_diags`) under `edit`, which transforms
/// `prev_text` into `new_text`. Returns `None` when no incremental strategy
/// applies — the caller must then do a full parse.
///
/// Stage 0 stub: no strategy exists yet, so every call falls back.
pub fn reparse(
    _prev_text: &str,
    _prev_green: &GreenNode,
    _prev_diags: &[ParseDiagnostic],
    _edit: &Edit,
    _new_text: &str,
) -> Option<Reparsed> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(range: Range<usize>, insert: &str) -> Edit {
        Edit {
            range,
            insert: insert.to_string(),
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
    fn reparse_stub_always_falls_back() {
        let parsed = crate::parser::parse("x = 1\n");
        let green = parsed.cst.green().into_owned();
        let e = diff_edit("x = 1\n", "x = 2\n");
        assert!(reparse("x = 1\n", &green, &parsed.diagnostics, &e, "x = 2\n").is_none());
    }
}
