//! Incremental reparse: turn a text edit plus the previous parse into a new
//! tree without re-lexing and re-parsing the whole file.
//!
//! Modelled on rust-analyzer's `reparsing.rs` and arity's `reparse.rs`: try
//! the cheapest strategy first and fall back to progressively more work, with
//! a full [`parse`](crate::parser::parse) as the always-correct last resort.
//! The staged plan lives in `TODO.md` (`### Incremental`). Two tiers are in:
//! the token tier ([`reparse_token`]) relexes an edit confined to a single
//! `Ident`, comment, or whitespace leaf in isolation and splices it into the
//! previous green tree ([`try_splice_plain_token`]), or — for a `StringContent`
//! leaf, whose lexing depends on the enclosing delimiter's mode — relexes the
//! whole enclosing string literal in isolation and requires it to reproduce its
//! own token run ([`try_splice_string_content`]); the top-level statement tier
//! ([`reparse_toplevel`]) reparses the run of `ROOT` children touching the edit
//! with the public [`parse`](crate::parser::parse) and splices the resulting
//! children and diagnostics in place, provided the boundary guards prove the
//! region decoupled from its neighbors.
//!
//! Two entry points sit above those tiers. [`reparse_edits`] takes the precise
//! chain the language server staged from a `didChange` batch and replays it one
//! edit at a time; [`reparse`] takes a single edit, which is what
//! `crate::incremental` recovers with [`diff_edit`] from a pair of whole texts
//! when no chain is available. The chain is tried first: a collapsed diff of
//! scattered edits spans everything between them, and the region that produces
//! is wide enough that [`region_is_too_wide`] declines it outright, where
//! replaying the chain splices each edit on its own. The Tenet-4 oracle
//! ([`assert_matches_full_parse`] plus `tests/incremental_reparse.rs`) checks
//! every splice against a full parse.
//!
//! **Correctness invariant (Tenet 4):** a successful reparse must yield a
//! green tree *and* diagnostics byte-identical to a full parse of the edited
//! text. A reparse is an output-pure performance hint: the salsa layer only
//! ever sees text changes, and a miss is a full parse, never an error.

use rowan::{GreenNode, GreenToken, TextRange, TextSize, TokenAtOffset};

use crate::parser::ParseDiagnostic;
use crate::parser::diagnostics::DIAGNOSTIC_STREAMS;
use crate::parser::lexer::lex;
use crate::parser::tree_builder::syntax_kind_for;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// The edit currency, defined in the leaf `crate::text` module and re-exported
/// here so `crate::parser::Edit` stays the path the parser layer uses.
pub use crate::text::{Edit, apply_edits, diff_edit, try_apply_edits};

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

/// Which strategy produced a [`Reparsed`]. Surfaced for tests and benchmarks.
/// Ordered cheapest-first, so a chain of reparses can report the most
/// expensive tier it had to reach with [`Ord::max`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
///
/// `edit` is not trusted, for the same reason [`reparse_edits`]'s chain is
/// not: one that does not fit `prev_text` — a stale range, a buffer that moved
/// underneath it — is rejected here rather than applied at offsets that would
/// panic on a slice.
pub fn reparse(
    prev_text: &str,
    prev_green: &GreenNode,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    new_text: &str,
) -> Option<Reparsed> {
    if !fits(prev_text, edit) {
        return None;
    }
    let result = reparse_impl(prev_text, prev_green, prev_diags, edit, new_text)?;
    assert_matches_full_parse(&result, new_text);
    Some(result)
}

/// Attempt an incremental reparse across a *chain* of edits, each expressed
/// against the text its predecessors produced — the shape an LSP `didChange`
/// batch arrives in (see [`crate::text::apply_content_changes`]).
///
/// This is for the case [`diff_edit`] cannot express: when the edits are
/// scattered, collapsing them into one spanning edit covers everything between
/// the first and the last change and blows both tiers' guards, while replaying
/// them one at a time keeps each splice small. Try this *before* the collapsed
/// edit: a wide span is the one shape the collapsed edit can never answer, so
/// offering it first is the difference between a hit and a
/// [`region_is_too_wide`] bail (`benches/reparse.rs`).
///
/// Fewer than two edits returns `None` immediately: a single edit's span
/// always contains [`diff_edit`]'s for the same transform, so there is nothing
/// to gain over the caller's next step.
///
/// `edits` is not trusted: a slice that is not the exact transform from
/// `prev_text` to `new_text` (a stale chain, a buffer that moved underneath
/// it) is rejected rather than applied at meaningless offsets. Each step is
/// proven to [`fits`] the text its predecessors produced *before* that text is
/// sliced, and the composed result is proven to be `new_text` before anything
/// is returned — so a stale chain costs at most the splices it got through,
/// bounded by the caller's chain budget (`incremental::MAX_CHAIN_EDITS`).
///
/// Validating step by step rather than pre-folding the whole chain with
/// [`try_apply_edits`] keeps the replay to one application: the fold would have
/// to redo every `replace_range` the loop below already does, which on a
/// 131 KB buffer is the same order as the splice it is guarding
/// (`benches/reparse.rs`, `precise_chain`).
pub fn reparse_edits(
    prev_text: &str,
    prev_green: &GreenNode,
    prev_diags: &[ParseDiagnostic],
    edits: &[Edit],
    new_text: &str,
) -> Option<Reparsed> {
    if edits.len() < 2 {
        return None;
    }

    let mut text = prev_text.to_string();
    let mut green = prev_green.clone();
    let mut diagnostics = prev_diags.to_vec();
    let mut tier = ReparseTier::Token;

    for edit in edits {
        if !fits(&text, edit) {
            return None;
        }
        let next = edit.apply(&text);
        let step = reparse_impl(&text, &green, &diagnostics, edit, &next)?;
        tier = tier.max(step.tier);
        text = next;
        green = step.green;
        diagnostics = step.diagnostics;
    }

    // The chain applied cleanly, but that alone does not make it the transform
    // the caller is asking about: a stale chain can fit and still land
    // somewhere else entirely. This check is what the Tenet-4 assert below
    // relies on to be comparing the same two texts.
    if text != new_text {
        return None;
    }

    let result = Reparsed {
        green,
        diagnostics,
        tier,
    };
    assert_matches_full_parse(&result, new_text);
    Some(result)
}

/// Tenet 4: a successful reparse yields a green tree *and* diagnostics
/// byte-identical to a full parse of the edited text.
///
/// Debug builds only — this runs a whole `parse` plus two whole-tree
/// [`fingerprint`] builds, so it is far too expensive to leave on. A chain
/// asserts once on its composed result rather than once per step, which would
/// otherwise make a debug-build language server unusable on a large file.
#[cfg(debug_assertions)]
fn assert_matches_full_parse(result: &Reparsed, new_text: &str) {
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

#[cfg(not(debug_assertions))]
fn assert_matches_full_parse(_result: &Reparsed, _new_text: &str) {}

/// Whether `edit`'s range is a well-formed byte range of `text`. Both tiers
/// slice `prev_text` at the edit's offsets, so an edit that does not fit is a
/// panic rather than a miss.
fn fits(text: &str, edit: &Edit) -> bool {
    edit.range.start <= edit.range.end
        && edit.range.end <= text.len()
        && text.is_char_boundary(edit.range.start)
        && text.is_char_boundary(edit.range.end)
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

/// Leaf kinds proved by the isolated single-token relex plus the two join
/// probes ([`try_splice_plain_token`]). Their text never spans a newline and
/// their lexing is local enough for those probes to prove a splice sound.
/// Deliberately absent: `NEWLINE` (statement structure), `CHAR` and the
/// numeric kinds (quote-context and juxtaposition traps outweigh the win), and
/// the string prefix/suffix kinds (glued to their delimiters).
/// `STRING_CONTENT` is handled too, but by a different proof — its lexing
/// depends on the enclosing delimiter's mode, so it takes the whole-literal
/// relex in [`try_splice_string_content`] instead.
const TOKEN_REPARSE_KINDS: &[SyntaxKind] = &[
    SyntaxKind::IDENT,
    SyntaxKind::COMMENT,
    SyntaxKind::BLOCK_COMMENT,
    SyntaxKind::WHITESPACE,
];

/// Literal node kinds a `STRING_CONTENT` leaf may sit directly under. Error
/// recovery can leave one under an `ERROR` node instead, which
/// [`try_splice_string_content`] rejects: the proof needs the delimiters.
const STRING_LITERAL_KINDS: &[SyntaxKind] = &[
    SyntaxKind::STRING_LITERAL,
    SyntaxKind::CMD_LITERAL,
    // `var"…"`, a non-standard *identifier*. Nothing keys on its content text
    // either, so a content edit is as safe here as in a plain string.
    SyntaxKind::NONSTANDARD_IDENTIFIER,
];

/// Identifier texts the parser treats structurally even though they lex as
/// plain `Ident`, so a same-kind relex proves nothing about the tree shape.
/// True keywords (`where`, `mutable`, …) need no entry: they lex as their own
/// kinds, and the same-kind guard rejects them.
///
/// This list is the one guard here with no compile-time link to what it
/// describes, and the Tenet-4 oracle only catches an omission if a seeded edit
/// happens to spell the missing word — so
/// [`every_contextual_ident_is_listed`](tests::every_contextual_ident_is_listed)
/// scans the grammar for `Ident`-text comparisons and fails if one names a word
/// that is not here.
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

/// Dispatch one candidate leaf to the proof its kind takes.
fn try_splice_token(
    prev_text: &str,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    token: &SyntaxToken,
) -> Option<Reparsed> {
    match token.kind() {
        SyntaxKind::STRING_CONTENT => try_splice_string_content(prev_text, prev_diags, edit, token),
        kind if TOKEN_REPARSE_KINDS.contains(&kind) => {
            try_splice_plain_token(prev_text, prev_diags, edit, token)
        }
        _ => None,
    }
}

/// Run the stage-2 guards against one candidate leaf and splice on success.
fn try_splice_plain_token(
    prev_text: &str,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    token: &SyntaxToken,
) -> Option<Reparsed> {
    // Newline ban: these kinds either cannot contain a newline or (block
    // comments) rarely see one edited, and a newline moves statement
    // boundaries, so bail early. The single-token relex guard below would
    // catch most of these anyway; this is the cheap early-out. It does not
    // apply to `STRING_CONTENT`, whose run legitimately spans newlines and
    // whose proof does not depend on line structure.
    if edit.insert.contains(['\n', '\r']) || prev_text[edit.range.clone()].contains(['\n', '\r']) {
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

/// `text` — the slice of the previous buffer starting at absolute offset
/// `base` — with `edit` applied at its relative offset.
fn edited_slice(text: &str, base: usize, edit: &Edit) -> String {
    let mut out = text.to_string();
    out.replace_range(
        (edit.range.start - base)..(edit.range.end - base),
        &edit.insert,
    );
    out
}

/// Every token under `node` as `(kind, end)`, with `end` relative to the node
/// start. Tokens tile the node's text contiguously from `0` and the tree
/// synthesizes none of its own (`tree_builder`), so this *is* the lexer's
/// token run for that span and the ends alone pin every range.
fn literal_token_ends(node: &SyntaxNode) -> Vec<(SyntaxKind, usize)> {
    let base = usize::from(node.text_range().start());
    node.descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|t| (t.kind(), usize::from(t.text_range().end()) - base))
        .collect()
}

/// Whether an isolated [`lex`] of `text` reproduces `expected`: the same kinds
/// in the same order, with the same ends, covering all of `text`. `grown`
/// names the index whose end — and every end after it — is expected to have
/// moved by that delta.
fn relex_matches(
    text: &str,
    expected: &[(SyntaxKind, usize)],
    grown: Option<(usize, isize)>,
) -> bool {
    let tokens = lex(text);
    if tokens.len() != expected.len() {
        return false;
    }
    for (i, (got, &(kind, end))) in tokens.iter().zip(expected).enumerate() {
        let shift = match grown {
            Some((from, delta)) if i >= from => delta,
            _ => 0,
        };
        // A shift that runs an end past zero cannot describe any token, so it
        // simply fails to match rather than wrapping into a huge `usize`.
        let Ok(want) = usize::try_from(end as isize + shift) else {
            return false;
        };
        if syntax_kind_for(got.kind) != kind || got.end != want {
            return false;
        }
    }
    tokens.last().is_some_and(|t| t.end == text.len())
}

/// The token tier's `STRING_CONTENT` path: relex the *whole enclosing string
/// literal* in isolation and require it to reproduce its own token run with
/// only the edited content leaf resized.
///
/// A content leaf cannot take [`try_splice_plain_token`]'s proof, because both
/// of its probes assume a leaf that lexes the same standing alone: `abc` in
/// isolation is an `Ident`, never `StringContent`, and a leaf following an
/// interpolation (`"a$x b"`) has an `Ident` before it, which blows the
/// two-token backward probe. Taking the delimiters along instead puts the
/// isolated lexer into the right mode for free, so triple/raw/prefixed need no
/// hand-written character table.
///
/// Why this is sound. Nothing outside the lexer reads `StringContent` *text*:
/// the parser only pushes the token, docstring eligibility tests the first
/// token's kind, and the flag passes are kind-based — so preserving the node's
/// token-kind sequence preserves both the tree and the diagnostics. (Nor does
/// triple-quoted dedenting intrude: that lives in the test-only JuliaSyntax
/// projector, which rebuilds the string at projection time.) Given that:
///
/// * The bytes before the node are unchanged and the lexer is a deterministic
///   left-to-right machine, so its mode stack at the node start is identical.
///   That subsumes the backward join probe.
/// * The relex proves the kinds are unchanged and that only the edited leaf's
///   end, and the ends after it, moved by the delta. The bytes after the leaf
///   *within* the node are therefore byte-identical — including the close
///   delimiter and any suffix — so the mode stack is back to its starting
///   depth at the node end and the rest of the file lexes identically, merely
///   shifted. That subsumes the forward join probe, which could not have
///   expressed a delimiter-plus-suffix tail with its one probe character
///   anyway.
///
/// The second bullet needs the literal to be *terminated*, which gets its own
/// check rather than leaning on an `UnterminatedLiteral` diagnostic's anchor.
///
/// Newlines are allowed here, unlike in [`try_splice_plain_token`]: a content
/// run spans them freely (a triple-quoted body, and a single-quoted one too —
/// no newline stops the run, which is why an unterminated literal reaches EOF),
/// and a newline *inside* one emits no `NEWLINE` token, so no statement
/// boundary moves. Diagnostics are byte offsets and do not care about lines.
/// That is what puts Enter in a docstring on this tier.
///
/// The faithfulness relex (the unedited node text must reproduce the node's own
/// tokens) is what makes the second bullet an induction rather than an
/// assumption: an isolated [`lex`] starts from an empty mode stack and no token
/// history, while in context the node may sit under a `Str`/`Interp` frame and
/// `prev_ends_value` consults the preceding token. Proving faithfulness for the
/// old text, plus an identical kind sequence, carries it to the new text.
fn try_splice_string_content(
    prev_text: &str,
    prev_diags: &[ParseDiagnostic],
    edit: &Edit,
    token: &SyntaxToken,
) -> Option<Reparsed> {
    let node = token.parent()?;
    if !STRING_LITERAL_KINDS.contains(&node.kind()) {
        return None;
    }
    let nr = node.text_range();
    let (n0, n1) = (usize::from(nr.start()), usize::from(nr.end()));

    // Bail when any diagnostic touches the literal, boundaries included. The
    // relex proves the token sequence unchanged, so the four string-related
    // kinds would in fact survive a shift — but "no diagnostic may touch the
    // literal we splice" is an invariant that keeps holding as diagnostics are
    // added, and what it gives up (`x = "abc`, `"a"b`, `var"x"y`) is the error
    // path, not the keystroke path.
    //
    // This runs *first*, before either relex: an unterminated literal runs to
    // EOF, so its node is the rest of the file, and every keystroke inside one
    // would otherwise pay two whole-file lexes to learn what a diagnostic
    // already says (`benches/reparse.rs`, `rejected_attempt`).
    if prev_diags.iter().any(|d| d.start <= n1 && d.end >= n0) {
        return None;
    }

    let expected = literal_token_ends(&node);
    let tr = token.text_range();
    let (t0, t1) = (usize::from(tr.start()), usize::from(tr.end()));
    // Ends are unique (tokens tile with positive width), so this identifies the
    // leaf by index — which the edited relex then pins as the one that resized.
    let leaf = expected
        .iter()
        .position(|&(kind, end)| kind == SyntaxKind::STRING_CONTENT && end == t1 - n0)?;

    // Terminated: a matching close delimiter must follow the leaf. A prefixed
    // literal's suffix is lexed as part of the same node, so nothing but the
    // close can end the run.
    let close = if node.kind() == SyntaxKind::CMD_LITERAL {
        SyntaxKind::CMD_DELIM_CLOSE
    } else {
        SyntaxKind::STRING_DELIM_CLOSE
    };
    if !expected[leaf + 1..].iter().any(|&(kind, _)| kind == close) {
        return None;
    }

    let old_node_text = &prev_text[n0..n1];
    if !relex_matches(old_node_text, &expected, None) {
        return None;
    }
    let delta = edit.delta();
    let edited = edited_slice(old_node_text, n0, edit);
    if !relex_matches(&edited, &expected, Some((leaf, delta))) {
        return None;
    }

    let new_leaf = &edited[(t0 - n0)..((t1 - n0) as isize + delta) as usize];

    // Every surviving diagnostic lies wholly outside the literal, so the ones
    // past it shift and the rest keep their positions.
    let shift = |pos: usize| (pos as isize + delta) as usize;
    let diagnostics = prev_diags
        .iter()
        .map(|d| {
            let mut d = d.clone();
            if d.start >= n1 {
                d.start = shift(d.start);
                d.end = shift(d.end);
            }
            d
        })
        .collect();

    let green = token.replace_with(GreenToken::new(SyntaxKind::STRING_CONTENT.into(), new_leaf));
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

/// Below this the tier always tries, whatever fraction of the file the region
/// is: on a small file every parse in play is small, and the fraction test
/// alone would refuse the common case of a file with one or two statements.
const REGION_ALWAYS_TRY_BYTES: usize = 4 * 1024;

/// Above this fraction of the file, a region is wide enough that answering it
/// costs more than the full parse it is trying to avoid.
const REGION_MAX_FRACTION: usize = 4;

/// Whether a region is too wide to be worth attempting. The tier answers a
/// region with a fragment parse of it *plus* up to three neighbor-sized
/// boundary parses, so the attempt scales with the region — and a miss pays
/// the full parse on top. A collapsed [`diff_edit`] of scattered edits spans
/// everything between the first and the last, which is exactly how a region
/// grows to most of the file; on ~131 KB that attempt cost about 12 ms against
/// a 6.3 ms full parse (`benches/reparse.rs`, `scattered_via_diff_edit`).
/// Bailing on the cheap evidence — the region's byte span, known before any
/// parse — caps the loss at the full parse the caller was going to pay anyway.
///
/// This is a performance guard only. Like every other bail here it returns the
/// caller to a full parse, so it cannot affect what a reparse yields.
fn region_is_too_wide(region_len: usize, text_len: usize) -> bool {
    region_len > REGION_ALWAYS_TRY_BYTES && region_len > text_len / REGION_MAX_FRACTION
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

/// Splice the fragment's diagnostics into the previous ones, stream by
/// stream ([`DiagnosticKind::stream`](crate::parser::diagnostics::DiagnosticKind::stream)):
/// keep those before the region, drop those the fragment reparse
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
///
/// A stream index outside [`DIAGNOSTIC_STREAMS`] aborts the splice too, rather
/// than indexing out of bounds. [`DiagnosticKind::stream`] is an exhaustive
/// match today, so this cannot fire — but a bail returns the caller to a full
/// parse, whereas a panic here takes down an analysis query, and this module
/// buys its whole safety argument from every failure being a bail.
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

    let mut before: [Vec<ParseDiagnostic>; DIAGNOSTIC_STREAMS] = Default::default();
    let mut after: [Vec<ParseDiagnostic>; DIAGNOSTIC_STREAMS] = Default::default();
    for d in prev_diags {
        let class = classify(d)?;
        let stream = d.kind.stream();
        if stream >= DIAGNOSTIC_STREAMS {
            debug_assert!(false, "diagnostic stream {stream} is out of range");
            return None;
        }
        match class {
            Class::Before => before[stream].push(d.clone()),
            Class::Drop => {}
            Class::After => {
                let mut d = d.clone();
                d.start = (d.start as isize + delta) as usize;
                d.end = (d.end as isize + delta) as usize;
                after[stream].push(d);
            }
        }
    }

    let mut out = Vec::with_capacity(prev_diags.len() + frag_diags.len());
    for stream in 0..DIAGNOSTIC_STREAMS {
        out.append(&mut before[stream]);
        out.extend(
            frag_diags
                .iter()
                .filter(|d| d.kind.stream() == stream)
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
    if region_is_too_wide(region.end - region.start, prev_text.len()) {
        return None;
    }
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

    // Cross-region coupling: when the fragment carries no significant content,
    // the two neighbors sit next to each other across it and can couple in a
    // way neither boundary parse can see, because each concatenation contains
    // only one of them. Deleting the `]` from `"doc" ]\nf() = 1` is the shape:
    // the stray closer was all that kept the string from being the statement
    // below it, and once it goes `fold_docstrings` folds the pair into a `DOC`
    // spanning both seams. Parse the neighbors together, through the real gap
    // bytes, and require both seams to survive. Only reachable when the region
    // is reduced to trivia, so the extra parse is rare and neighbor-sized.
    if !frag
        .cst
        .children_with_tokens()
        .any(|el| is_significant_child(&el))
        && let (Some(prev), Some(next)) = (&prev_node, &next_node)
    {
        let back_gap = &prev_text[usize::from(prev.text_range().end())..region.start];
        let fwd_gap = &prev_text[region.end..usize::from(next.text_range().start())];
        let guard = format!(
            "{}{}{}{}{}",
            prev.text(),
            back_gap,
            fragment,
            fwd_gap,
            next.text()
        );
        let back_seam = usize::from(prev.text_range().len()) + back_gap.len();
        let fwd_seam = back_seam + fragment.len();
        let parsed = crate::parser::parse(&guard);
        if !no_straddle(&parsed.cst, back_seam) || !no_straddle(&parsed.cst, fwd_seam) {
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

    fn edit(range: std::ops::Range<usize>, insert: &str) -> Edit {
        Edit {
            range,
            insert: insert.to_string(),
        }
    }

    /// The grammar decides some tree shapes by an identifier's *text*, and the
    /// token tier's same-kind relex cannot see such a change — so every such
    /// word has to be in [`CONTEXTUAL_IDENTS`]. Nothing in the type system says
    /// so, and the oracle only surfaces an omission by luck, so scan the
    /// grammar for the two spellings those comparisons take and check the words
    /// they name.
    ///
    /// The known words are asserted to have been *found*, not just listed: if a
    /// comparison is ever rewritten into a shape this scan does not recognize,
    /// the test fails loudly rather than quietly covering nothing.
    #[test]
    fn every_contextual_ident_is_listed() {
        // `t.kind == TokKind::Ident && t.text == "word"` and the `matches!`
        // form `matches!(t.text.as_ref(), "word" | "other")`. Both are keyed on
        // `t.text`, which is what makes them text-structural.
        fn words_after(haystack: &str, marker: &str) -> Vec<String> {
            let mut out = Vec::new();
            for tail in haystack.split(marker).skip(1) {
                // Take the literals in the comparison that follows, stopping at
                // the first token that ends it.
                let stop = tail.find([';', '{', '}']).unwrap_or(tail.len());
                let mut rest = &tail[..stop];
                while let Some(open) = rest.find('"') {
                    let Some(close) = rest[open + 1..].find('"') else {
                        break;
                    };
                    out.push(rest[open + 1..open + 1 + close].to_string());
                    rest = &rest[open + 1 + close + 1..];
                }
            }
            out
        }

        // The grammar files, read from source: this is a lint over the code,
        // not over anything the crate exposes at runtime.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/parser");
        let mut found: Vec<String> = Vec::new();
        for name in ["expr.rs", "structural.rs", "core.rs", "recovery.rs"] {
            let src = std::fs::read_to_string(dir.join(name))
                .unwrap_or_else(|e| panic!("read {name}: {e}"));
            found.extend(words_after(&src, "t.text =="));
            found.extend(words_after(&src, "t.text.as_ref(),"));
            found.extend(words_after(&src, "next.text =="));
        }
        assert!(
            !found.is_empty(),
            "the scan matched nothing — the grammar's `Ident`-text comparisons \
             have been rewritten, and this guard is no longer guarding anything"
        );

        for word in &found {
            assert!(
                CONTEXTUAL_IDENTS.contains(&word.as_str()),
                "the grammar branches on the identifier text {word:?}, so an \
                 edit into or out of it can reshape the tree under an unchanged \
                 token kind. Add it to CONTEXTUAL_IDENTS.",
            );
        }

        // Every word the scan is known to reach today. A rewrite that hides one
        // from the scan trips this rather than silently narrowing the guard.
        for expected in ["as", "abstract", "primitive", "type", "typegroup", "public"] {
            assert!(
                found.iter().any(|w| w == expected),
                "the scan no longer finds the {expected:?} comparison",
            );
        }
    }

    #[test]
    fn fingerprint_is_stable_and_discriminating() {
        let a1 = fingerprint(&crate::parser::parse("x = 1\n").cst);
        let a2 = fingerprint(&crate::parser::parse("x = 1\n").cst);
        let b = fingerprint(&crate::parser::parse("x = 2\n").cst);
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
    }

    /// A numeric literal is deliberately outside `TOKEN_REPARSE_KINDS`, and a
    /// lone `x = 1` statement is the whole `ROOT`, so the top-level tier finds
    /// no decoupled region either.
    #[test]
    fn a_number_edit_in_a_lone_statement_falls_back() {
        let parsed = crate::parser::parse("x = 1\n");
        let green = parsed.cst.green().into_owned();
        let e = diff_edit("x = 1\n", "x = 2\n");
        assert!(reparse("x = 1\n", &green, &parsed.diagnostics, &e, "x = 2\n").is_none());
    }

    /// The point of the `STRING_CONTENT` path: a docstring body edit stays at
    /// the token tier. `fold_docstrings` folds a docstring with the definition
    /// it documents into one `ROOT` child, so the statement tier would answer
    /// this by reparsing the docstring *and* the whole function under it.
    #[test]
    fn a_docstring_body_edit_stays_at_the_token_tier() {
        let old = "\"\"\"\ndoc\n\"\"\"\nfunction f(x)\n    x + 1\nend\n";
        let parsed = crate::parser::parse(old);
        let green = parsed.cst.green().into_owned();
        let e = edit(5..5, "s");
        let new = e.apply(old);

        let out = reparse(old, &green, &parsed.diagnostics, &e, &new)
            .expect("a docstring body edit is spliceable");
        assert_eq!(out.tier, ReparseTier::Token);
    }

    /// An unterminated literal runs to EOF, so its node is the rest of the
    /// file. The diagnostic bail must reject a content edit inside one — and
    /// must do so before either relex, which is why it is hoisted to the top
    /// of `try_splice_string_content`.
    #[test]
    fn a_content_edit_in_an_unterminated_literal_falls_back() {
        let old = "x = 1\ny = \"abc\nz = 2\n";
        let parsed = crate::parser::parse(old);
        let green = parsed.cst.green().into_owned();
        // Offset 12 is inside the content run, which here swallows the rest of
        // the file; `UnterminatedLiteral` is anchored at the literal's start.
        let token = SyntaxNode::new_root(green.clone())
            .token_at_offset(TextSize::new(12))
            .right_biased()
            .expect("a leaf at offset 12");
        assert_eq!(token.kind(), SyntaxKind::STRING_CONTENT);

        let e = edit(12..13, "X");
        let new = e.apply(old);
        assert!(try_splice_string_content(old, &parsed.diagnostics, &e, &token).is_none());
        // The statement tier may still answer it (the in-crate Tenet-4 assert
        // validates that splice); what matters is that the token tier declined.
        let tier = reparse(old, &green, &parsed.diagnostics, &e, &new).map(|r| r.tier);
        assert_ne!(tier, Some(ReparseTier::Token));
    }

    /// An edit that does not fit `prev_text` is rejected rather than applied at
    /// offsets that would panic on a slice. `reparse_edits` proves its chain
    /// with `try_apply_edits`; the single-edit entry point has to check too.
    #[test]
    fn reparse_rejects_an_edit_that_does_not_fit() {
        let old = "α = 1\n";
        let parsed = crate::parser::parse(old);
        let green = parsed.cst.green().into_owned();
        // `new_text` is deliberately not the edit's result: a misfitting edit
        // has none, which is the whole point.
        let run = |e: Edit| reparse(old, &green, &parsed.diagnostics, &e, old);

        // Past the end of the text.
        assert!(run(edit(99..99, "x")).is_none());
        // Off a char boundary (α is two bytes).
        assert!(run(edit(1..1, "x")).is_none());
        // Inverted range (built by hand: a literal `4..2` is a clippy error).
        let inverted = Edit {
            range: std::ops::Range { start: 4, end: 2 },
            insert: "x".to_string(),
        };
        assert!(run(inverted).is_none());
    }

    /// A region wide enough that answering it costs more than the full parse it
    /// avoids is declined on the cheap evidence, before any fragment parse. The
    /// shape is a collapsed `diff_edit` of scattered edits, which spans
    /// everything between the first and the last.
    #[test]
    fn a_region_covering_most_of_a_large_file_is_declined() {
        let mut old = String::new();
        for i in 0..2000 {
            old.push_str(&format!("var_{i} = {i}\n"));
        }
        let parsed = crate::parser::parse(&old);
        let green = parsed.cst.green().into_owned();

        // A narrow edit near the end still splices: the guard keys on the
        // region, not on the file size.
        let narrow = edit(old.len()..old.len(), "tail = 1\n");
        assert!(
            reparse(
                &old,
                &green,
                &parsed.diagnostics,
                &narrow,
                &narrow.apply(&old)
            )
            .is_some()
        );

        // The same net change seen as one collapsed edit spanning most of the
        // file: `select_region` returns nearly every child, and the tier bails.
        // Both sites are inside an identifier, and the second is expressed
        // against the text the first produced (one byte longer), so the chain
        // below stays at the token tier. The last line is `var_1999 = 1999\n`,
        // 16 bytes, whose name runs from its start.
        let scattered = vec![edit(4..4, "X"), edit(old.len() - 11..old.len() - 11, "X")];
        let new = apply_edits(&old, &scattered);
        let collapsed = diff_edit(&old, &new);
        assert!(collapsed.range.len() > old.len() / 2, "{collapsed:?}");
        assert!(reparse(&old, &green, &parsed.diagnostics, &collapsed, &new).is_none());

        // Replaying the chain still answers it at the token tier — the guard
        // costs the precise-edit path nothing.
        let out = reparse_edits(&old, &green, &parsed.diagnostics, &scattered, &new)
            .expect("the precise chain is unaffected");
        assert_eq!(out.tier, ReparseTier::Token);
    }

    /// The width guard is a fraction *and* a floor: a small file's region is
    /// always worth trying, however much of the file it covers.
    #[test]
    fn the_region_width_guard_leaves_small_files_alone() {
        assert!(!region_is_too_wide(4 * 1024, 4 * 1024));
        assert!(!region_is_too_wide(64 * 1024, 1024 * 1024));
        assert!(region_is_too_wide(64 * 1024, 128 * 1024));
    }

    /// The shape `reparse_edits` exists for: two identifier edits far apart,
    /// which `diff_edit` would collapse into one span covering both statements.
    #[test]
    fn reparse_edits_chains_scattered_edits() {
        let old = "alpha = 1\nfiller = 2\nomega = 3\n";
        let parsed = crate::parser::parse(old);
        let green = parsed.cst.green().into_owned();
        // `alpha` -> `alphaX`, then `omega` -> `omegaX` at its post-first-edit
        // offset (one byte further along).
        let edits = vec![edit(5..5, "X"), edit(27..27, "X")];
        let new = apply_edits(old, &edits);
        assert_eq!(new, "alphaX = 1\nfiller = 2\nomegaX = 3\n");

        // The collapsed diff spans from `alpha` all the way to `omega`, so the
        // cheap token tier cannot touch it and the whole run of statements has
        // to be reparsed. Replaying the chain keeps both splices at the token
        // tier. On three statements that is a wash; on a real file the
        // difference is two orders of magnitude (`benches/reparse.rs`), which
        // is why the chain is tried first.
        let collapsed = diff_edit(old, &new);
        assert!(collapsed.range.len() > 20, "{collapsed:?} should be coarse");
        let via_diff = reparse(old, &green, &parsed.diagnostics, &collapsed, &new)
            .expect("the whole-file region is still spliceable at this size");
        assert_eq!(via_diff.tier, ReparseTier::TopLevel);

        let out = reparse_edits(old, &green, &parsed.diagnostics, &edits, &new)
            .expect("two identifier edits should chain");
        assert_eq!(out.tier, ReparseTier::Token);
        assert_eq!(
            fingerprint(&SyntaxNode::new_root(out.green)),
            fingerprint(&crate::parser::parse(&new).cst),
        );
    }

    /// The chain reports the most expensive tier it had to reach.
    #[test]
    fn reparse_edits_reports_the_max_tier() {
        let old = "alpha = 1\nfiller = 2\nomega = 3\n";
        let parsed = crate::parser::parse(old);
        let green = parsed.cst.green().into_owned();
        // An ident edit (token tier) plus a whole added statement (top level),
        // appended at the post-first-edit end of the buffer.
        let edits = vec![edit(5..5, "X"), edit(32..32, "beta = 4\n")];
        let new = apply_edits(old, &edits);

        let out = reparse_edits(old, &green, &parsed.diagnostics, &edits, &new)
            .expect("ident edit plus a new statement should chain");
        assert_eq!(out.tier, ReparseTier::TopLevel);
    }

    /// Fewer than two edits is always `diff_edit`'s job: its span for the same
    /// transform is contained in the single edit's, and the caller tried it.
    #[test]
    fn reparse_edits_declines_short_chains() {
        let old = "alpha = 1\nomega = 3\n";
        let parsed = crate::parser::parse(old);
        let green = parsed.cst.green().into_owned();

        assert!(reparse_edits(old, &green, &parsed.diagnostics, &[], old).is_none());
        let one = vec![edit(5..5, "X")];
        let new = apply_edits(old, &one);
        assert!(reparse_edits(old, &green, &parsed.diagnostics, &one, &new).is_none());
    }

    /// A chain that is not the exact transform between the two texts is
    /// rejected before any offset is trusted — both when it simply does not
    /// reproduce the target, and when it does not fit the source at all.
    #[test]
    fn reparse_edits_rejects_a_stale_chain() {
        let old = "alpha = 1\nfiller = 2\nomega = 3\n";
        let parsed = crate::parser::parse(old);
        let green = parsed.cst.green().into_owned();

        // Applies cleanly, but the buffer moved on underneath it.
        let edits = vec![edit(5..5, "X"), edit(26..26, "X")];
        let elsewhere = "alphaX = 1\nfiller = 2\nomegaX = 3\nextra = 4\n";
        assert!(reparse_edits(old, &green, &parsed.diagnostics, &edits, elsewhere).is_none());

        // Does not even fit the source text.
        let past_end = vec![edit(500..500, "X"), edit(501..501, "X")];
        assert!(reparse_edits(old, &green, &parsed.diagnostics, &past_end, old).is_none());
    }

    /// One unhandleable step aborts the whole chain — the caller full-parses.
    #[test]
    fn reparse_edits_aborts_on_an_unhandleable_step() {
        let old = "alpha = 1\nfiller = 2\nomega = 3\n";
        let parsed = crate::parser::parse(old);
        let green = parsed.cst.green().into_owned();
        // The second edit opens an unterminated string, which no tier splices.
        let edits = vec![edit(5..5, "X"), edit(26..26, "\"")];
        let new = apply_edits(old, &edits);
        assert!(reparse_edits(old, &green, &parsed.diagnostics, &edits, &new).is_none());
    }
}
