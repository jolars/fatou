//! Incremental reparse: turn a text edit plus the previous parse into a new
//! tree without re-lexing and re-parsing the whole file.
//!
//! Modelled on rust-analyzer's `reparsing.rs` and arity's `reparse.rs`: try
//! the cheapest strategy first and fall back to progressively more work, with
//! a full [`parse`](crate::parser::parse) as the always-correct last resort.
//! The staged plan lives in `TODO.md` (`### Incremental`). The token tier
//! ([`reparse_token`]) is in: an edit confined to a single `Ident`, comment,
//! or whitespace leaf is relexed in isolation and spliced into the previous
//! green tree; everything else falls back until the top-level statement tier
//! lands. The Tenet-4 oracle (the `debug_assert` below plus
//! `tests/incremental_reparse.rs`) checks every splice against a full parse.
//!
//! **Correctness invariant (Tenet 4):** a successful reparse must yield a
//! green tree *and* diagnostics byte-identical to a full parse of the edited
//! text. A reparse is an output-pure performance hint: the salsa layer only
//! ever sees text changes, and a miss is a full parse, never an error.

use std::ops::Range;

use rowan::{GreenNode, GreenToken, TextRange, TextSize, TokenAtOffset};

use crate::parser::ParseDiagnostic;
use crate::parser::lexer::lex;
use crate::parser::tree_builder::syntax_kind_for;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Structural fingerprint of a tree: one line per descendant element with
/// `kind@range` plus the token text (empty for nodes). Two trees with equal
/// fingerprints are byte-identical. Oracle/debug support: the Tenet-4 assert
/// in [`reparse`] and the `tests/incremental_reparse.rs` harness share this
/// definition so they can never diverge.
pub fn fingerprint(node: &SyntaxNode) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    for el in node.descendants_with_tokens() {
        let text = el
            .as_token()
            .map(|t| t.text().to_string())
            .unwrap_or_default();
        let _ = writeln!(out, "{:?}@{:?} {:?}", el.kind(), el.text_range(), text);
    }
    out
}

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
/// In debug builds every successful reparse is checked against a full parse
/// of `new_text` (Tenet 4): tree fingerprint and diagnostics vector must be
/// identical.
pub fn reparse(
    prev_text: &str,
    prev_green: &GreenNode,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    new_text: &str,
) -> Option<Reparsed> {
    let result = reparse_impl(prev_text, prev_green, prev_diags, edit, new_text)?;

    #[cfg(debug_assertions)]
    {
        let full = crate::parser::parse(new_text);
        debug_assert_eq!(
            fingerprint(&SyntaxNode::new_root(result.green.clone())),
            fingerprint(&full.cst),
            "Tenet 4: reparse ({:?}) tree differs from full parse",
            result.tier,
        );
        debug_assert_eq!(
            result.diagnostics, full.diagnostics,
            "Tenet 4: reparse ({:?}) diagnostics differ from full parse",
            result.tier,
        );
    }

    Some(result)
}

/// Tier dispatch: cheapest first. The top-level statement tier (stage 3) will
/// slot in after the token tier.
fn reparse_impl(
    prev_text: &str,
    prev_green: &GreenNode,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    _new_text: &str,
) -> Option<Reparsed> {
    reparse_token(prev_text, prev_green, prev_diags, edit)
}

/// Leaf kinds the token tier may splice. Their text never spans a newline and
/// their lexing is local enough for the join guards to prove a splice sound.
/// Deliberately absent: `STRING_CONTENT` (lexing depends on the enclosing
/// delimiter's mode), `NEWLINE` (statement structure), `CHAR` and the numeric
/// kinds (quote-context and juxtaposition traps outweigh the win), and the
/// string prefix/suffix kinds (glued to their delimiters).
const TOKEN_REPARSE_KINDS: &[SyntaxKind] = &[
    SyntaxKind::IDENT,
    SyntaxKind::COMMENT,
    SyntaxKind::BLOCK_COMMENT,
    SyntaxKind::WHITESPACE,
];

/// Identifier texts the parser treats structurally even though they lex as
/// plain `Ident`, so a same-kind relex proves nothing about the tree shape.
/// True keywords (`where`, `mutable`, …) need no entry: they lex as their own
/// kinds, and the same-kind guard rejects them. The oracle harness surfaces
/// omissions.
const CONTEXTUAL_IDENTS: &[&str] = &[
    "as",        // import alias (structural.rs)
    "abstract",  // `abstract type` (expr.rs)
    "primitive", // `primitive type` (expr.rs)
    "type",      // the `type` in `abstract`/`primitive` type decls (expr.rs)
    "typegroup", // `typegroup` declaration (expr.rs)
    "public",    // top-level `public` statement (expr.rs)
    "var",       // `var"..."` quoting prefix (expr.rs)
    "in",        // iteration spec / infix operator (expr.rs)
    "∈",         // Unicode `in` (expr.rs)
    "isa",       // infix operator (expr.rs)
    "doc",       // `doc"..."` / `@doc` marker (expr.rs)
    "outer",     // future `for outer i` support
];

/// The token tier: when the edit is confined to a single eligible leaf, relex
/// that leaf in isolation and splice it via [`SyntaxToken::replace_with`].
///
/// A pure insertion at a token boundary is ambiguous (it could extend either
/// neighbor), so both candidates are tried, left first; whichever passes the
/// guards yields the same new file text, and the guards plus the Tenet-4
/// oracle arbitrate tree equality. Any guard failure returns `None`, which
/// the caller answers with a full parse, so every bail here is safe.
fn reparse_token(
    prev_text: &str,
    prev_green: &GreenNode,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
) -> Option<Reparsed> {
    // Newline ban: the eligible kinds either cannot contain a newline or
    // (block comments) rarely see one edited; a newline also moves statement
    // boundaries, so bail early. The single-token relex guard below would
    // catch most of these anyway; this is the cheap early-out.
    if edit.insert.contains(['\n', '\r']) || prev_text[edit.range.clone()].contains(['\n', '\r']) {
        return None;
    }

    let root = SyntaxNode::new_root(prev_green.clone());
    let (s, e) = (edit.range.start, edit.range.end);
    let mut candidates: Vec<SyntaxToken> = Vec::with_capacity(2);
    if s == e {
        match root.token_at_offset(TextSize::new(s as u32)) {
            TokenAtOffset::None => return None,
            TokenAtOffset::Single(t) => candidates.push(t),
            TokenAtOffset::Between(l, r) => candidates.extend([l, r]),
        }
    } else {
        // A range spanning a token boundary covers a node, not a token, and
        // bails here.
        let range = TextRange::new(TextSize::new(s as u32), TextSize::new(e as u32));
        candidates.push(root.covering_element(range).into_token()?);
    }

    candidates
        .into_iter()
        .find_map(|token| try_splice_token(prev_text, prev_diags, edit, &token))
}

/// Run the stage-2 guards against one candidate leaf and splice on success.
fn try_splice_token(
    prev_text: &str,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    token: &SyntaxToken,
) -> Option<Reparsed> {
    if !TOKEN_REPARSE_KINDS.contains(&token.kind()) {
        return None;
    }
    let tr = token.text_range();
    let (t0, t1) = (usize::from(tr.start()), usize::from(tr.end()));

    // The leaf's new text, with the edit applied at its relative offset. A
    // whole-leaf deletion leaves it empty, which the relex guard rejects.
    let mut new_leaf = token.text().to_string();
    new_leaf.replace_range((edit.range.start - t0)..(edit.range.end - t0), &edit.insert);

    // Isolated relex: still exactly one token, same kind, spanning all of it.
    // This alone rejects kind flips (ident ⇒ keyword, whitespace ⇒ newline)
    // and splits (`=#` typed into a block comment).
    let relexed = lex(&new_leaf);
    let [only] = relexed.as_slice() else {
        return None;
    };
    if only.start != 0 || only.end != new_leaf.len() || syntax_kind_for(only.kind) != token.kind() {
        return None;
    }

    // Contextual identifiers parse structurally, so text changes into or out
    // of one can reshape the tree under an unchanged token kind.
    if token.kind() == SyntaxKind::IDENT
        && (CONTEXTUAL_IDENTS.contains(&token.text())
            || CONTEXTUAL_IDENTS.contains(&new_leaf.as_str()))
    {
        return None;
    }

    // Forward join: the next source character must not extend the token
    // (`r` + `"…"` fusing into a prefixed string, an unterminated block
    // comment swallowing its neighbor). At EOF probe with `\n`, which no
    // eligible kind can absorb, so a leaf left unterminated at EOF (`#=` typed
    // into the last block comment) still fails here.
    let next_char = prev_text[t1..].chars().next().unwrap_or('\n');
    let mut probe = new_leaf.clone();
    probe.push(next_char);
    let first = lex(&probe).into_iter().next()?;
    if first.end != new_leaf.len() || syntax_kind_for(first.kind) != token.kind() {
        return None;
    }

    // Backward join: the previous leaf plus the new text must relex to the
    // same two tokens. Catches juxtaposition fusing (`2` + `e10` ⇒ one
    // `Float`), and conservatively bails whenever the previous leaf's
    // isolated lex diverges from its in-context one (e.g. a closing string
    // delimiter reopening as a string).
    if let Some(prev) = token.prev_token() {
        let mut probe = prev.text().to_string();
        let boundary = probe.len();
        probe.push_str(&new_leaf);
        let relexed = lex(&probe);
        let [a, b] = relexed.as_slice() else {
            return None;
        };
        if a.end != boundary
            || syntax_kind_for(a.kind) != prev.kind()
            || b.end != probe.len()
            || syntax_kind_for(b.kind) != token.kind()
        {
            return None;
        }
    }

    // Keep diagnostic remapping trivial: bail when any diagnostic touches the
    // leaf, boundaries included. A clean same-kind relex introduces none of
    // its own, so the rest keep their meaning and only need shifting.
    if prev_diags.iter().any(|d| d.start <= t1 && d.end >= t0) {
        return None;
    }
    let delta = edit.delta();
    let shift = |pos: usize| (pos as isize + delta) as usize;
    let diagnostics = prev_diags
        .iter()
        .map(|d| {
            let mut d = d.clone();
            if d.start >= t1 {
                d.start = shift(d.start);
                d.end = shift(d.end);
            }
            d
        })
        .collect();

    let green = token.replace_with(GreenToken::new(token.kind().into(), &new_leaf));
    Some(Reparsed {
        green,
        diagnostics,
        tier: ReparseTier::Token,
    })
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
    fn fingerprint_is_stable_and_discriminating() {
        let a1 = fingerprint(&crate::parser::parse("x = 1\n").cst);
        let a2 = fingerprint(&crate::parser::parse("x = 1\n").cst);
        let b = fingerprint(&crate::parser::parse("x = 2\n").cst);
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
    }

    #[test]
    fn reparse_stub_always_falls_back() {
        let parsed = crate::parser::parse("x = 1\n");
        let green = parsed.cst.green().into_owned();
        let e = diff_edit("x = 1\n", "x = 2\n");
        assert!(reparse("x = 1\n", &green, &parsed.diagnostics, &e, "x = 2\n").is_none());
    }
}
