//! Incremental reparse: turn a text edit plus the previous parse into a new
//! tree without re-lexing and re-parsing the whole file.
//!
//! Modelled on rust-analyzer's `reparsing.rs` and arity's `reparse.rs`: try
//! the cheapest strategy first and fall back to progressively more work, with
//! a full [`parse`](crate::parser::parse) as the always-correct last resort.
//! The staged plan lives in `TODO.md` (`### Incremental`). Two tiers are in:
//! the token tier ([`reparse_token`]) relexes an edit confined to a single
//! `Ident`, comment, or whitespace leaf in isolation and splices it into the
//! previous green tree; the top-level statement tier ([`reparse_toplevel`])
//! reparses the run of `ROOT` children touching the edit with the public
//! [`parse`](crate::parser::parse) and splices the resulting children and
//! diagnostics in place, provided the boundary guards prove the region
//! decoupled from its neighbors. The Tenet-4 oracle (the `debug_assert`
//! below plus `tests/incremental_reparse.rs`) checks every splice against a
//! full parse.
//!
//! **Correctness invariant (Tenet 4):** a successful reparse must yield a
//! green tree *and* diagnostics byte-identical to a full parse of the edited
//! text. A reparse is an output-pure performance hint: the salsa layer only
//! ever sees text changes, and a miss is a full parse, never an error.

use std::ops::Range;

use rowan::{GreenNode, GreenToken, TextRange, TextSize, TokenAtOffset};

use crate::parser::ParseDiagnostic;
use crate::parser::diagnostics::DiagnosticKind;
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

/// Tier dispatch: cheapest first.
fn reparse_impl(
    prev_text: &str,
    prev_green: &GreenNode,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    new_text: &str,
) -> Option<Reparsed> {
    reparse_token(prev_text, prev_green, prev_diags, edit)
        .or_else(|| reparse_toplevel(prev_text, prev_green, prev_diags, edit, new_text))
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

/// Whether a token kind is trivia. Mirrors `TokKind::is_trivia` on the CST
/// side.
fn is_trivia_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::WHITESPACE
            | SyntaxKind::NEWLINE
            | SyntaxKind::COMMENT
            | SyntaxKind::BLOCK_COMMENT
    )
}

/// Whether a `ROOT` child carries statement-level content: a positive-width
/// node, or a loose non-trivia token (the unparseable-token recovery emits
/// bare tokens at the root, without an `ERROR` wrapper). Zero-width nodes and
/// trivia are gap material.
fn is_significant_child(el: &crate::syntax::SyntaxElement) -> bool {
    match el {
        rowan::NodeOrToken::Node(n) => !n.text_range().is_empty(),
        rowan::NodeOrToken::Token(t) => !is_trivia_kind(t.kind()),
    }
}

/// The reparse region at the top-level tier: a contiguous run of `ROOT`
/// children (statement nodes and loose trivia tokens both), identified by
/// child index and old-text byte range. The boundary child kinds
/// disambiguate zero-width diagnostics anchored exactly on a boundary.
struct Region {
    first: usize,
    last: usize,
    start: usize,
    end: usize,
    first_is_node: bool,
    last_is_node: bool,
}

/// Select the contiguous run of `ROOT` children whose closed range touches
/// the edit. Closed-interval touch makes boundary insertions extend into the
/// neighbor they abut (blank-line typing selects the surrounding newline
/// tokens; deleting a newline pulls in both neighbor statements). A
/// zero-width child abutting the region start is absorbed: stray-closer
/// recovery pairs a zero-width `ERROR` with its byte-bearing run at the same
/// offset, and the fragment reparse regenerates both, so leaving it outside
/// would duplicate it. `None` when the run covers every child (the fragment
/// reparse would be the full parse) or `ROOT` is empty.
fn select_region(root: &SyntaxNode, edit: &Edit) -> Option<Region> {
    let (s, e) = (edit.range.start, edit.range.end);
    let children: Vec<_> = root.children_with_tokens().collect();
    let touches = |el: &crate::syntax::SyntaxElement| {
        let r = el.text_range();
        usize::from(r.start()) <= e && usize::from(r.end()) >= s
    };
    let mut first = children.iter().position(touches)?;
    let last = children.len() - 1 - children.iter().rev().position(touches)?;

    let start = usize::from(children[first].text_range().start());
    while first > 0
        && children[first - 1].text_range() == TextRange::empty(TextSize::new(start as u32))
    {
        first -= 1;
    }

    if first == 0 && last == children.len() - 1 {
        return None;
    }
    Some(Region {
        first,
        last,
        start,
        end: usize::from(children[last].text_range().end()),
        first_is_node: children[first].as_node().is_some(),
        last_is_node: children[last].as_node().is_some(),
    })
}

/// The nearest positive-width `ROOT` child nodes outside the region. Trivia
/// tokens are gap material, and a zero-width `ERROR` node contributes no text
/// (taking one as the forward neighbor would make the boundary guard
/// vacuous), so both are skipped.
fn sibling_nodes(root: &SyntaxNode, region: &Region) -> (Option<SyntaxNode>, Option<SyntaxNode>) {
    let positive_node =
        |el: crate::syntax::SyntaxElement| el.into_node().filter(|n| !n.text_range().is_empty());
    let prev = root
        .children_with_tokens()
        .take(region.first)
        .filter_map(positive_node)
        .last();
    let next = root
        .children_with_tokens()
        .skip(region.last + 1)
        .find_map(positive_node);
    (prev, next)
}

/// Whether no `ROOT` child (node or loose token) of `cst` straddles `seam`.
/// Children tile the text, so no-straddle is equivalent to a boundary
/// existing exactly at the seam: the two sides parse independently. A
/// straddling node is a cross-boundary statement (trailing-operator
/// continuation, a docstring fold, an unterminated block absorbing its
/// neighbor); a straddling token is a lexical fusion (`#=` swallowing
/// trailing trivia, an unterminated string).
fn no_straddle(cst: &SyntaxNode, seam: usize) -> bool {
    let seam = TextSize::new(seam as u32);
    cst.children_with_tokens().all(|el| {
        let r = el.text_range();
        !(r.start() < seam && seam < r.end())
    })
}

/// The five ordered diagnostic streams of a [`parse`](crate::parser::parse):
/// drive-loop/`parse_stmt` emission (0), then the four flag passes in their
/// fixed order. Each flag kind is pushed only by its pass, so the stream of
/// any diagnostic is recoverable from its kind, and the full vector is the
/// streams concatenated in this order.
fn stream_of(kind: DiagnosticKind) -> usize {
    match kind {
        DiagnosticKind::ConstNotAssignment => 1,
        DiagnosticKind::InvalidFunctionSignature => 2,
        DiagnosticKind::CatchVarNotIdentifier => 3,
        DiagnosticKind::InvalidExportItem => 4,
        _ => 0,
    }
}

/// Splice the fragment's diagnostics into the previous ones, stream by
/// stream: keep those before the region, drop those the fragment reparse
/// regenerates, and shift those after by the edit delta. Emission order
/// within a stream is positional across statements (the drive loop is
/// per-line sequential, the flag passes walk in document order), so
/// `before ++ fragment ++ after` per stream, concatenated in stream order,
/// reproduces a full parse's vector.
///
/// A zero-width diagnostic exactly on a region boundary is ambiguous by
/// position alone and is disambiguated by the boundary child's kind: a
/// statement node owns a start-anchored marker at `start` (`const x` at the
/// region's front) and an end-anchored one at `end`, while a trivia boundary
/// child owns nothing, so the marker belongs to the statement just outside.
/// This is sound only because the decoupling guards already rejected
/// zero-gap node adjacency at the boundaries. A diagnostic genuinely
/// straddling a boundary aborts the splice (`None`); regions are whole
/// children and diagnostics never outgrow their statement, so this arm
/// should be unreachable and is purely defensive.
fn splice_diagnostics(
    prev_diags: &[ParseDiagnostic],
    frag_diags: &[ParseDiagnostic],
    region: &Region,
    delta: isize,
) -> Option<Vec<ParseDiagnostic>> {
    enum Class {
        Before,
        Drop,
        After,
    }
    let classify = |d: &ParseDiagnostic| -> Option<Class> {
        let (rs, re) = (region.start, region.end);
        if d.start == d.end && d.start == rs {
            return Some(if region.first_is_node {
                Class::Drop
            } else {
                Class::Before
            });
        }
        if d.start == d.end && d.start == re {
            return Some(if region.last_is_node {
                Class::Drop
            } else {
                Class::After
            });
        }
        if d.end <= rs {
            return Some(Class::Before);
        }
        if d.start >= re {
            return Some(Class::After);
        }
        if d.start >= rs && d.end <= re {
            return Some(Class::Drop);
        }
        None
    };

    let mut before: [Vec<ParseDiagnostic>; 5] = Default::default();
    let mut after: [Vec<ParseDiagnostic>; 5] = Default::default();
    for d in prev_diags {
        match classify(d)? {
            Class::Before => before[stream_of(d.kind)].push(d.clone()),
            Class::Drop => {}
            Class::After => {
                let mut d = d.clone();
                d.start = (d.start as isize + delta) as usize;
                d.end = (d.end as isize + delta) as usize;
                after[stream_of(d.kind)].push(d);
            }
        }
    }

    let mut out = Vec::with_capacity(prev_diags.len() + frag_diags.len());
    for stream in 0..5 {
        out.append(&mut before[stream]);
        out.extend(
            frag_diags
                .iter()
                .filter(|d| stream_of(d.kind) == stream)
                .map(|d| {
                    let mut d = d.clone();
                    d.start += region.start;
                    d.end += region.start;
                    d
                }),
        );
        out.append(&mut after[stream]);
    }
    Some(out)
}

/// The top-level statement tier: reparse the run of `ROOT` children touching
/// the edit with the public [`parse`](crate::parser::parse) (which reruns
/// `fold_docstrings` and the flag passes on the fragment) and splice the
/// resulting children and diagnostics in place.
///
/// The guards, in order: decoupling scans require a newline between each
/// untouched sibling statement and the fragment's non-trivia content, because
/// the drive loop's per-line junk recovery couples same-line siblings; the
/// boundary parses then prove the seam survives — the neighbor plus the
/// actual gap bytes plus the fragment must parse with a boundary exactly at
/// the old one, which rejects trailing-operator continuation, docstring
/// folds, forward absorption by an unterminated block, and lexical fusion.
/// Diagnostics splice last (see [`splice_diagnostics`]).
fn reparse_toplevel(
    prev_text: &str,
    prev_green: &GreenNode,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    new_text: &str,
) -> Option<Reparsed> {
    let root = SyntaxNode::new_root(prev_green.clone());
    let region = select_region(&root, edit)?;
    let delta = edit.delta();
    let frag_end = (region.end as isize + delta) as usize;
    let fragment = &new_text[region.start..frag_end];

    let frag = crate::parser::parse(fragment);
    let (prev_node, next_node) = sibling_nodes(&root, &region);

    let positive_nodes = |node: &SyntaxNode| {
        node.children_with_tokens()
            .filter_map(|el| el.into_node())
            .filter(|n| !n.text_range().is_empty())
    };

    // Backward decoupling: a newline must separate the last significant
    // content before the region (a statement node or a loose junk token, not
    // just `prev_node`) from the fragment's first significant content (or
    // all of a trivia-only fragment). The drive loop's per-line junk
    // recovery couples same-line siblings, and the newline often lives in
    // the unchanged gap outside the region, so scan `new_text`.
    let prev_significant_end = root
        .children_with_tokens()
        .take(region.first)
        .filter(is_significant_child)
        .last()
        .map(|el| usize::from(el.text_range().end()));
    if let Some(from) = prev_significant_end {
        let anchor = frag
            .cst
            .children_with_tokens()
            .find(is_significant_child)
            .map(|el| usize::from(el.text_range().start()))
            .unwrap_or(fragment.len());
        if !new_text[from..region.start + anchor].contains('\n') {
            return None;
        }
    }

    // Forward decoupling, in both texts: the next statement is kept, not
    // reparsed, so its line must have been decoupled from the old region
    // content too (deleting `x` from `x y` puts the kept junk `y` at line
    // start, where a full parse reads it as a plain statement). The new-text
    // scan anchors at the fragment's last statement *node* end, not its last
    // token: an unterminated bracket consumes the separating newline into
    // the statement, and a consumed newline decouples nothing.
    if let Some(next) = &next_node {
        let anchor = frag
            .cst
            .children_with_tokens()
            .filter(is_significant_child)
            .last()
            .map(|el| usize::from(el.text_range().end()))
            .unwrap_or(0);
        let to = (usize::from(next.text_range().start()) as isize + delta) as usize;
        if !new_text[region.start + anchor..to].contains('\n') {
            return None;
        }

        let old_anchor = root
            .children_with_tokens()
            .skip(region.first)
            .take(region.last + 1 - region.first)
            .filter(is_significant_child)
            .last()
            .map(|el| usize::from(el.text_range().end()))
            .or(prev_significant_end)
            .unwrap_or(0);
        if !prev_text[old_anchor..usize::from(next.text_range().start())].contains('\n') {
            return None;
        }
    }

    // A zero-width diagnostic exactly on a guard seam is disambiguated by
    // the fragment's own boundary child, mirroring `splice_diagnostics`: a
    // statement node owns a start-anchored marker at its start and an
    // end-anchored one at its end; a trivia boundary child owns nothing.
    let frag_starts_with_node = frag
        .cst
        .children_with_tokens()
        .next()
        .is_some_and(|el| el.as_node().is_some());
    let frag_ends_with_node = frag
        .cst
        .children_with_tokens()
        .last()
        .is_some_and(|el| el.as_node().is_some());

    // Backward boundary parse: previous statement + gap trivia + fragment
    // must keep a boundary exactly at the old one, and the fragment must
    // parse in context with exactly the diagnostics it produced in
    // isolation.
    if let Some(prev) = &prev_node {
        let back_gap = &prev_text[usize::from(prev.text_range().end())..region.start];
        let guard = format!("{}{}{}", prev.text(), back_gap, fragment);
        let seam = guard.len() - fragment.len();
        let parsed = crate::parser::parse(&guard);
        if !no_straddle(&parsed.cst, seam) {
            return None;
        }
        let guard_frag: Vec<ParseDiagnostic> = parsed
            .diagnostics
            .iter()
            .filter(|d| {
                d.start > seam
                    || (d.start == seam && d.end > seam)
                    || (d.start == seam && d.end == seam && frag_starts_with_node)
            })
            .map(|d| {
                let mut d = d.clone();
                d.start -= seam;
                d.end -= seam;
                d
            })
            .collect();
        if guard_frag != frag.diagnostics {
            return None;
        }
    }

    // Forward boundary parse: fragment + gap trivia + next statement (or the
    // trailing trivia to EOF when no statement follows). Three checks
    // against one parse of the concatenation: no element straddles the seam;
    // the fragment reparses exactly as it did in isolation (an EOF-adjacent
    // junk run wraps differently once text follows it); and the kept next
    // statement reappears untouched — same start, kind, and length — since a
    // fragment ending in an unterminated bracket can consume the separating
    // newline and demote the next line to flat junk without anything
    // straddling the seam.
    let tail = match &next_node {
        Some(next) => {
            let gap = &prev_text[region.end..usize::from(next.text_range().start())];
            format!("{}{}", gap, next.text())
        }
        None => prev_text[region.end..].to_string(),
    };
    if !tail.is_empty() {
        let guard = format!("{fragment}{tail}");
        let seam = fragment.len();
        let parsed = crate::parser::parse(&guard);
        if !no_straddle(&parsed.cst, seam) {
            return None;
        }
        let signature = |el: crate::syntax::SyntaxElement| {
            (
                usize::from(el.text_range().start()),
                el.as_node().is_some(),
                el.kind(),
                usize::from(el.text_range().len()),
            )
        };
        let frag_sig: Vec<_> = frag.cst.children_with_tokens().map(signature).collect();
        let pre_seam: Vec<_> = parsed
            .cst
            .children_with_tokens()
            .filter(|el| {
                let r = el.text_range();
                // A zero-width child exactly at the seam pairs with a junk
                // run on the far side; it belongs to the tail.
                usize::from(r.end()) <= seam && !(r.is_empty() && usize::from(r.start()) == seam)
            })
            .map(signature)
            .collect();
        if pre_seam != frag_sig {
            return None;
        }
        // The fragment's in-context diagnostics must also match: a trailing
        // token at the fragment's EOF can emit differently once text follows
        // it (`:` with a newline after it flags `QuoteColonWhitespace`; at
        // EOF it does not).
        let guard_frag: Vec<ParseDiagnostic> = parsed
            .diagnostics
            .iter()
            .filter(|d| {
                d.end < seam
                    || (d.end == seam && d.start < seam)
                    || (d.start == seam && d.end == seam && frag_ends_with_node)
            })
            .cloned()
            .collect();
        if guard_frag != frag.diagnostics {
            return None;
        }
        if let Some(next) = &next_node {
            let gap_len = usize::from(next.text_range().start()) - region.end;
            let reparsed_next =
                positive_nodes(&parsed.cst).find(|n| usize::from(n.text_range().start()) >= seam);
            let intact = reparsed_next.is_some_and(|n| {
                usize::from(n.text_range().start()) == seam + gap_len
                    && n.kind() == next.kind()
                    && n.text_range().len() == next.text_range().len()
            });
            if !intact {
                return None;
            }
        }
    }

    let diagnostics = splice_diagnostics(prev_diags, &frag.diagnostics, &region, delta)?;

    let frag_green = frag.cst.green();
    let green = prev_green.splice_children(
        region.first..=region.last,
        frag_green.children().map(|c| c.to_owned()),
    );
    Some(Reparsed {
        green,
        diagnostics,
        tier: ReparseTier::TopLevel,
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
