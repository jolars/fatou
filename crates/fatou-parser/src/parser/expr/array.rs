//! Array and brace literals: `[…]`/`{…}` bodies, n-dimensional concatenation, comprehensions, and generator clauses.
//!
//! Split out of `expr.rs`; see that module's docs for the parser as a whole.

use super::*;

/// Whether the operator at `op_idx` begins a new array element: there is
/// whitespace between the previous operand (ending at `operand_end`) and the
/// operator, but the operator is glued to its own operand (no whitespace after).
/// That makes it a prefix of the next element rather than an infix operator, so
/// `[1 +2]` is two elements while `[1 + 2]` is one.
pub(super) fn array_element_boundary(
    ctx: &ParserCtx<'_>,
    operand_end: usize,
    op_idx: usize,
) -> bool {
    let space_before = op_idx > operand_end;
    if !space_before {
        return false;
    }
    // Only an operator that can be *unary* begins a new element when glued to its
    // operand: `[a +b]` is `[a, +b]` but `[a *b]` is `[a*b]` (one element), since
    // `*` is binary-only. A suffixed operator (`+₁`) is never unary either, so
    // `[x +₁y]` stays one element. Matches JuliaSyntax's whitespace-sensitive
    // array splitting (only `is_unary`/`is_both_unary_and_binary` operators split).
    let Some(op) = ctx.token(op_idx) else {
        return false;
    };
    if !op_can_lead_array_element(op) {
        return false;
    }
    // …and the operator must be glued to its own operand (no whitespace after).
    !matches!(
        ctx.token(op_idx + 1).map(|t| t.kind),
        Some(TokKind::Whitespace | TokKind::Newline) | None
    )
}

/// Whether `op`, glued to the following operand inside an array literal, reads as
/// that operand's prefix (so it begins a new element). The leading operators are
/// the unary-and-binary infix operators `+ - +% -% & ~` (broadcast `.+ .- .~`)
/// and the symbol-quote `:` (glued `:a` is a quoted symbol). Binary-only
/// operators (`* / % *% | :: <: >:`, broadcast `.& .|`) and any *suffixed* (`+₁`,
/// never unary) stay infix and do not split. Unary-only prefixes (`! ¬ √`) have
/// no infix binding power, so they end the element naturally and are not listed
/// here; the interpolation sigil `$` does have one (Julia's old xor operator),
/// so it is listed. Mirrors JuliaSyntax's whitespace-sensitive array splitting.
fn op_can_lead_array_element(op: &Token<'_>) -> bool {
    matches!(
        op.kind,
        TokKind::Plus
            | TokKind::Minus
            | TokKind::PlusPercent
            | TokKind::MinusPercent
            | TokKind::DotPlus
            | TokKind::DotMinus
            | TokKind::Tilde
            | TokKind::DotTilde
            | TokKind::Amp
            | TokKind::Colon
            | TokKind::Dollar
    ) && !op.text.chars().last().is_some_and(is_op_suffix_char)
}

/// Parse one element of an array literal: a full expression in array mode (a
/// space-glued operator ends it) at statement-newline sensitivity (a newline is a
/// row separator handled by the caller, not part of the element). Array literals
/// are square-bracketed, so `end` is the index marker here.
pub(super) fn parse_element(
    tokens: &[Token<'_>],
    start: usize,
    // Whether `end`/`begin` are index markers in this element, inherited from an
    // enclosing indexing bracket. A bare literal (`[1 2 end]`) is an array
    // constructor where `end` is *not* a marker; only `a[[1 end]]` (a literal
    // nested inside indexing) inherits the marker.
    end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        array_mode: true,
        end_marker,
        begin_marker: end_marker,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, 0, diagnostics, flags)
}

/// Wrap a parsed element in an `ARG` node, returning the index just past it.
fn push_element_arg(events: &mut Vec<Event>, el: ExprParse) -> usize {
    let end = el.end;
    events.push(Event::Start(SyntaxKind::ARG));
    events.extend(el.events);
    events.push(Event::Finish);
    end
}

/// Whether the trivia run beginning at the newline `look` is followed by `kind`.
///
/// Two callers decide that a newline first separator inside `[…]`/`{…}` is
/// insignificant: a following `,` is the real separator of a vector, and a
/// following `for` makes a comprehension, so a blank line before it does not
/// force a `vcat` (`[x \n\n for a in as]` is `(comprehension …)`). A second
/// element is hit before either, keeping `[1\n2\nfor …]` a matrix.
fn newline_run_precedes(ctx: &ParserCtx<'_>, look: usize, kind: TokKind) -> bool {
    ctx.token(ctx.skip_trivia(look)).map(|t| t.kind) == Some(kind)
}

/// The two delimiter families sharing the concatenation grammar, `[…]` and
/// `{…}`: the closing token plus the three node kinds a body can take.
#[derive(Clone, Copy)]
pub(super) struct CatShape {
    close: TokKind,
    /// A comma-separated layout, and the empty and single-element cases.
    list: SyntaxKind,
    /// A space-, `;`-, or newline-separated concatenation.
    cat: SyntaxKind,
    /// A `for`-clause comprehension.
    comprehension: SyntaxKind,
}

pub(super) const BRACKET_CAT: CatShape = CatShape {
    close: TokKind::RBracket,
    list: SyntaxKind::VECT_EXPR,
    cat: SyntaxKind::MATRIX_EXPR,
    comprehension: SyntaxKind::COMPREHENSION,
};

pub(super) const BRACE_CAT: CatShape = CatShape {
    close: TokKind::RBrace,
    list: SyntaxKind::BRACES,
    cat: SyntaxKind::BRACESCAT_EXPR,
    comprehension: SyntaxKind::BRACES_COMPREHENSION,
};

/// Parse a `[…]` or `{…}` literal at prefix position (a postfix `[` is indexing).
/// The first separator past the opening element decides the shape: a `,`, the
/// closer, or end gives `shape.list` (reusing the arg-list machinery), a `for`
/// gives `shape.comprehension`, and anything else — a space, `;`, or newline —
/// gives `shape.cat`, a concatenation of `MATRIX_ROW`s.
pub(super) fn parse_delimited_literal(
    ctx: &ParserCtx<'_>,
    open: usize,
    // Inherited index-marker context. A bare `[…]`/`{…}` literal does not enable
    // `end`, but `a[[…]]` (a literal nested in indexing) inherits it.
    end_marker: bool,
    shape: CatShape,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let list = |diagnostics: &mut Vec<ParseDiagnostic>| {
        let (events, end) =
            parse_arg_list(ctx, open, shape.close, shape.list, end_marker, diagnostics);
        ExprParse {
            start: open,
            end,
            events,
        }
    };
    let comprehension = |first, diagnostics: &mut Vec<ParseDiagnostic>| {
        parse_comprehension(
            ctx,
            open,
            first,
            shape.comprehension,
            shape.close,
            diagnostics,
        )
    };

    let first_start = ctx.skip_trivia(open + 1);
    // Empty `[]`/`{}`, or a first element we cannot parse: the comma-list parser
    // handles both losslessly.
    if ctx.token(first_start).map(|t| t.kind) == Some(shape.close) {
        return list(diagnostics);
    }
    // An element-free `[; …]` is an empty n-dimensional concatenation
    // (`[;]` → `ncat-1`, `[;;]` → `ncat-2`, `{;}` → `(bracescat (nrow-1))`).
    if let Some(empty) = parse_empty_ncat(ctx, open, first_start, shape) {
        return empty;
    }
    let Some(first) = parse_element(ctx.tokens(), first_start, end_marker, diagnostics) else {
        return list(diagnostics);
    };

    let look = ctx.skip_ws_and_comments(first.end);
    match ctx.token(look).map(|t| t.kind) {
        Some(TokKind::ForKw) => comprehension(first, diagnostics),
        None | Some(TokKind::Comma) => list(diagnostics),
        Some(k) if k == shape.close => list(diagnostics),
        // A newline run before the comprehension `for` is insignificant, so
        // `[x \n\n for a in as]` stays a `(comprehension …)`.
        Some(TokKind::Newline) if newline_run_precedes(ctx, look, TokKind::ForKw) => {
            comprehension(first, diagnostics)
        }
        // A newline run is a row separator only if it separates two *elements*.
        // When the next significant token past the newline(s) is a `,`, the comma
        // is the real separator and the newline is insignificant whitespace, so
        // `[x\n, y]` is `(vect x y)` and `{x\n, y}` is `(braces x y)`, matching
        // Julia (`;` after a newline stays a row separator, and another element
        // keeps it a concatenation).
        Some(TokKind::Newline) if newline_run_precedes(ctx, look, TokKind::Comma) => {
            list(diagnostics)
        }
        _ => parse_matrix(ctx, open, first, shape, end_marker, diagnostics),
    }
}

/// The concatenation order of an array, as JuliaSyntax tracks it: established by
/// the first space (`RowMajor`) or `;;` (`ColumnMajor`) separator. A later
/// separator of the conflicting kind is a whitespace error (see `parse_matrix`).
#[derive(PartialEq)]
enum ArrayOrder {
    Unknown,
    RowMajor,
    ColumnMajor,
}

/// A run of separator tokens between two concatenation elements (or trailing
/// before `]`): horizontal whitespace, comments, newlines, and `;`. The
/// dimension it separates along is its semicolon count, or 1 for a row-breaking
/// newline, or 0 for plain whitespace.
struct SepRun {
    toks: Vec<usize>,
    semis: usize,
    has_newline: bool,
    /// A newline appears *after* the last `;` in the run (`;; \n` but not
    /// `\n ;;`). Only this position makes a `;;` a line continuation.
    newline_after_semis: bool,
    /// A `;;` immediately followed by a newline inside a row-major array: a line
    /// continuation that JuliaSyntax treats like a space separator
    /// (`[a b ;; \n c]` ⇒ `(hcat a b c)`), so its effective dimension is 0.
    continuation: bool,
}

impl SepRun {
    /// The dimension this separator concatenates along. A trailing separator
    /// (`between` = false) only separates via `;` — a trailing newline is just
    /// whitespace (`[x\n]` is a `vect`, not a `vcat`).
    fn dim(&self, between: bool) -> usize {
        if self.continuation {
            0
        } else if self.semis > 0 {
            self.semis
        } else if self.has_newline && between {
            1
        } else {
            0
        }
    }
}

/// Build an element-free `[; …]` concatenation (`[;]` → `ncat-1`,
/// `[;;]` → `ncat-2`): the body is only trivia and `;`. Returns `None` when a
/// real element follows (the caller then falls back to a vector).
pub(super) fn parse_empty_ncat(
    ctx: &ParserCtx<'_>,
    lbrk: usize,
    first_start: usize,
    shape: CatShape,
) -> Option<ExprParse> {
    let (close, node_kind) = (shape.close, shape.cat);
    let mut q = first_start;
    let mut saw_semi = false;
    while let Some(k) = ctx.token(q).map(|t| t.kind) {
        match k {
            TokKind::Semicolon => {
                saw_semi = true;
                q += 1;
            }
            _ if k.is_trivia() => q += 1,
            _ => break,
        }
    }
    if !saw_semi || ctx.token(q).map(|t| t.kind) != Some(close) {
        return None;
    }
    let mut events = vec![Event::Start(node_kind), Event::Tok(lbrk)];
    push_range(&mut events, lbrk + 1, q + 1);
    finish(&mut events, node_kind);
    Some(ExprParse {
        start: lbrk,
        end: q + 1,
        events,
    })
}

/// Parse the concatenation form of a `[...]` literal given its already-parsed
/// first element. Elements are separated along increasing dimensions by spaces
/// (dim 0, a `row`), single `;`/newlines (dim 1), `;;` (dim 2), and so on. The
/// CST nests groups by dimension into `MATRIX_ROW` nodes (with bare single
/// elements left unwrapped); the projector recovers each group's dimension from
/// its separator tokens and heads it `hcat`/`vcat`/`ncat-d` (top) or
/// `row`/`nrow-d` (nested).
pub(super) fn parse_matrix(
    ctx: &ParserCtx<'_>,
    lbrk: usize,
    first: ExprParse,
    shape: CatShape,
    // Inherited index-marker context for the matrix elements (see
    // [`parse_element`]).
    end_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let (close, comma_kind, matrix_kind) = (shape.close, shape.list, shape.cat);
    let tokens = ctx.tokens();
    let lead_start = first.start;
    let mut elems = vec![first];
    let mut seps: Vec<SepRun> = Vec::new();
    let mut pos = elems[0].end;

    // Scan the body into elements and the separator runs that follow each of
    // them (the final entry is the trailing run before `]`/EOF).
    let end = loop {
        let mut run = SepRun {
            toks: Vec::new(),
            semis: 0,
            has_newline: false,
            newline_after_semis: false,
            continuation: false,
        };
        let mut q = pos;
        while let Some(k) = ctx.token(q).map(|t| t.kind) {
            match k {
                TokKind::Semicolon => {
                    run.semis += 1;
                    run.newline_after_semis = false;
                }
                TokKind::Newline => {
                    run.has_newline = true;
                    if run.semis > 0 {
                        run.newline_after_semis = true;
                    }
                }
                TokKind::Whitespace | TokKind::Comment | TokKind::BlockComment => {}
                _ => break,
            }
            run.toks.push(q);
            q += 1;
        }
        match ctx.token(q).map(|t| t.kind) {
            None => {
                seps.push(run);
                break q;
            }
            Some(k) if k == close => {
                seps.push(run);
                break q + 1;
            }
            // A macro `@` glued to the preceding element (no separating
            // whitespace, `;`, or newline) is not a new row element: JuliaSyntax
            // bumps the rest of the array — every token up to the closing `]` (or
            // EOF) — as one flat trailing-junk run (`[x@y]` ⇒
            // `(hcat x (error-t ✘ y))`, `[a b@c]` ⇒ `(hcat a b (error-t ✘ c))`).
            // A spaced `@` (`[x @y]`) keeps a real separator run and stays a
            // macrocall element, so the run must be empty to trigger this.
            // A misplaced `end` keyword as a non-leading array element in a *plain*
            // array literal: `end` is a valid index marker only as the sole/leading
            // element, so once another element precedes it (`[1 2 end]`, `[1; end]`)
            // JuliaSyntax stops the array, splices a zero-width `(error-t)` after the
            // last real element, and bumps the `end` plus the remaining closers up as
            // a trailing-junk run handled by the top-level leftover driver. We stop
            // the array *before* the `end` (without consuming the closer) and record
            // a `MatrixKeywordRecovery` diagnostic at the last element's end; the
            // projector splices the marker and the leftover driver renders the run.
            // Inside an indexing bracket (`end_marker`), though, `end` is the
            // index-end marker at *any* position (`a[[1; end]]` ⇒ `(ref a (vcat 1
            // end))`), so it falls through to the ordinary element parse below.
            Some(TokKind::EndKw) if !end_marker => {
                seps.push(run);
                let anchor = tokens[elems[elems.len() - 1].end - 1].end;
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::MatrixKeywordRecovery,
                    "misplaced `end` in array",
                    anchor,
                    anchor,
                );
                break q;
            }
            Some(TokKind::At) if run.toks.is_empty() => {
                seps.push(run);
                let mut j = q;
                while let Some(k) = ctx.token(j).map(|t| t.kind) {
                    if k == close {
                        break;
                    }
                    j += 1;
                }
                let mut events = vec![Event::Start(SyntaxKind::ERROR)];
                push_range(&mut events, q, j);
                events.push(Event::Finish);
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::TrailingJunk,
                    "trailing tokens in array",
                    tokens[q].start,
                    tokens[q].end,
                );
                elems.push(ExprParse {
                    start: q,
                    end: j,
                    events,
                });
                pos = j;
            }
            _ => {
                seps.push(run);
                let el = match parse_element(tokens, q, end_marker, diagnostics) {
                    Some(el) => el,
                    None => ExprParse {
                        start: q,
                        end: q + 1,
                        events: vec![Event::Tok(q)],
                    },
                };
                pos = el.end;
                elems.push(el);
            }
        }
    };

    let n = elems.len();
    let close_idx = if ctx.token(end.saturating_sub(1)).map(|t| t.kind) == Some(close) {
        Some(end - 1)
    } else {
        None
    };

    // JuliaSyntax establishes an array "order" from the first space/`;;`
    // separator (a space makes it row-major; a `;;` makes it column-major) and
    // then flags any *conflicting* later separator — a `;;` in a row-major array
    // or a space in a column-major one — as a whitespace error, splicing a
    // zero-width `(error-t)` right after the element preceding the offending
    // separator (`[a b ;; c]` ⇒ `(ncat-2 (row a b (error-t)) c)`, `[a ;; b c]` ⇒
    // `(ncat-2 a (row b (error-t) c))`). Only `;` runs of exactly two count;
    // single `;`, newlines, and `;;;`-or-longer runs are order-neutral. We record
    // each conflict as a diagnostic at the element's end byte; the projector
    // reconstructs the marker. (A `;;` immediately followed by a newline is a line
    // continuation collapsing to `hcat` rather than a conflict: a `;;` directly
    // followed by a newline inside an already-row-major array behaves exactly
    // like a space separator (`[a b ;; \n c]` ⇒ `(hcat a b c)`), so we mark it a
    // continuation — dimension 0, no conflict.)
    let mut order = ArrayOrder::Unknown;
    for k in 0..n.saturating_sub(1) {
        let is_space = seps[k].semis == 0 && !seps[k].has_newline;
        let is_double_semi = seps[k].semis == 2;
        let newline_after_semis = seps[k].newline_after_semis;
        let mut continuation = false;
        let conflict = match order {
            ArrayOrder::Unknown => {
                if is_space {
                    order = ArrayOrder::RowMajor;
                } else if is_double_semi {
                    order = ArrayOrder::ColumnMajor;
                }
                false
            }
            ArrayOrder::RowMajor => {
                if is_double_semi && newline_after_semis {
                    continuation = true;
                    false
                } else {
                    is_double_semi
                }
            }
            ArrayOrder::ColumnMajor => is_space,
        };
        seps[k].continuation = continuation;
        if conflict {
            let anchor = tokens[elems[k].end - 1].end;
            push_diagnostic(
                diagnostics,
                DiagnosticKind::ArraySeparatorMismatch,
                "cannot mix space and `;;` separators in an array",
                anchor,
                anchor,
            );
        }
    }

    // The top-level dimension: the largest `between`-element separator, plus any
    // trailing semicolon run (`[x;]` is a `vcat`).
    let top_d = (0..n.saturating_sub(1))
        .map(|k| seps[k].dim(true))
        .chain(std::iter::once(seps[n - 1].dim(false)))
        .max()
        .unwrap_or(0);

    // A lone element with no real separator (only a trailing newline) is a
    // vector, matching JuliaSyntax (`[x\n]` → `(vect x)`).
    let node_kind = if n == 1 && top_d == 0 {
        comma_kind
    } else {
        matrix_kind
    };

    let mut events = vec![Event::Start(node_kind), Event::Tok(lbrk)];
    push_range(&mut events, lbrk + 1, lead_start);
    emit_cat_groups(&mut events, &elems, &seps, 0, n, top_d);
    // Trailing separator run, then the closing bracket.
    for &t in &seps[n - 1].toks {
        events.push(Event::Tok(t));
    }
    if let Some(close_idx) = close_idx {
        events.push(Event::Tok(close_idx));
    }
    finish(&mut events, node_kind);
    ExprParse {
        start: lbrk,
        end,
        events,
    }
}

/// Emit the children of a concatenation group spanning elements `lo..hi`,
/// splitting at separators of dimension `split_d` and emitting their tokens
/// between children.
fn emit_cat_groups(
    events: &mut Vec<Event>,
    elems: &[ExprParse],
    seps: &[SepRun],
    lo: usize,
    hi: usize,
    split_d: usize,
) {
    let mut g = lo;
    for k in lo..hi {
        let is_boundary = k + 1 < hi && seps[k].dim(true) == split_d;
        if is_boundary {
            emit_cat_child(events, elems, seps, g, k + 1);
            for &t in &seps[k].toks {
                events.push(Event::Tok(t));
            }
            g = k + 1;
        } else if k + 1 == hi {
            emit_cat_child(events, elems, seps, g, hi);
        }
    }
}

/// Emit one concatenation child spanning elements `lo..hi`. A single bare
/// element is emitted unwrapped (inside its `ARG`); a multi-element group is
/// wrapped in a `MATRIX_ROW` and split along its own maximum internal dimension.
fn emit_cat_child(
    events: &mut Vec<Event>,
    elems: &[ExprParse],
    seps: &[SepRun],
    lo: usize,
    hi: usize,
) {
    if hi - lo == 1 {
        push_element_arg(events, elems[lo].clone());
        return;
    }
    let inner_d = (lo..hi - 1).map(|k| seps[k].dim(true)).max().unwrap_or(0);
    events.push(Event::Start(SyntaxKind::MATRIX_ROW));
    emit_cat_groups(events, elems, seps, lo, hi, inner_d);
    finish(events, SyntaxKind::MATRIX_ROW);
}

/// Parse the trailing `for <specs> [if <cond>]` clauses of a comprehension or
/// generator, given `pos` just past the already-emitted body. Each `for` becomes
/// a `FOR_BINDING` and each trailing `if` a `COMPREHENSION_IF` wrapping the
/// preceding clause. Returns the index past the last clause; the caller owns any
/// surrounding delimiters. Shared by the bracketed comprehension driver and the
/// delimiter-less generator-argument path (`f(a, x for x in xs)`).
pub(super) fn parse_generator_clauses(
    ctx: &ParserCtx<'_>,
    mut pos: usize,
    events: &mut Vec<Event>,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    loop {
        let for_idx = ctx.skip_trivia(pos);
        if ctx.token(for_idx).map(|t| t.kind) != Some(TokKind::ForKw) {
            break;
        }
        // JuliaSyntax requires whitespace before a comprehension/generator `for`;
        // when it is glued to the preceding element (`[(x)for x in xs]`), record a
        // `GluedFor` diagnostic at the `for`, projected as a `(error-t)` between
        // the body and the iteration clause.
        if for_idx == pos {
            let for_tok = &ctx.tokens()[for_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::GluedFor,
                "expected whitespace before `for`",
                for_tok.start,
                for_tok.start,
            );
        }
        push_range(events, pos, for_idx);
        events.push(Event::Start(SyntaxKind::FOR_BINDING));
        events.push(Event::Tok(for_idx)); // `for`
        pos = parse_for_specs(ctx, for_idx + 1, events, true, diagnostics);
        finish(events, SyntaxKind::FOR_BINDING);

        // Optional `if <cond>` filter on this clause.
        let if_idx = ctx.skip_trivia(pos);
        if ctx.token(if_idx).map(|t| t.kind) == Some(TokKind::IfKw) {
            push_range(events, pos, if_idx);
            events.push(Event::Start(SyntaxKind::COMPREHENSION_IF));
            events.push(Event::Tok(if_idx)); // `if`
            pos = if_idx + 1;
            let cond_start = ctx.skip_trivia(pos);
            push_range(events, pos, cond_start);
            if let Some(cond) =
                parse_expr_in_brackets(ctx.tokens(), cond_start, 0, false, diagnostics)
            {
                events.extend(cond.events);
                pos = cond.end;
            } else {
                pos = cond_start;
            }
            finish(events, SyntaxKind::COMPREHENSION_IF);
        }
    }
    pos
}

/// Parse a comprehension `[elem for v in iter if cond]` or generator
/// `(elem for v in iter)` given the already-parsed `elem` and the open delimiter
/// at `open` (closing kind `close`). Each `for` becomes a `FOR_BINDING` and each
/// trailing `if` a `COMPREHENSION_IF` wrapping the preceding clause. Multiple
/// `for` clauses (`for a in as for b in bs`) and comma-separated iteration specs
/// within one clause (`for a in as, b in bs`) are both handled. `in` is matched
/// as a bare `in` identifier, mirroring `for`/`while` loops; the `a = as` spec
/// form is parsed as a plain assignment.
pub(super) fn parse_comprehension(
    ctx: &ParserCtx<'_>,
    open: usize,
    elem: ExprParse,
    node_kind: SyntaxKind,
    close: TokKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let tokens = ctx.tokens();
    let mut events = vec![Event::Start(node_kind), Event::Tok(open)];
    push_range(&mut events, open + 1, elem.start);
    let mut pos = elem.end;
    events.extend(elem.events);

    // One or more `for <specs> [if <cond>]` clauses.
    pos = parse_generator_clauses(ctx, pos, &mut events, diagnostics);

    // Closing delimiter.
    let close_idx = ctx.skip_trivia(pos);
    push_range(&mut events, pos, close_idx);
    let end = if ctx.token(close_idx).map(|t| t.kind) == Some(close) {
        events.push(Event::Tok(close_idx));
        close_idx + 1
    } else {
        let tok = &tokens[open];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::UnclosedComprehension,
            "unclosed comprehension",
            tok.start,
            tok.end,
        );
        close_idx
    };
    finish(&mut events, node_kind);
    ExprParse {
        start: open,
        end,
        events,
    }
}

/// Whether the token at `idx` is the contextual `outer` keyword of an iteration
/// spec rather than the loop variable itself. It is the keyword only when a
/// whole second pattern follows it — `for outer i in xs` rebinds the enclosing
/// `i`, while `for outer in xs` loops over a variable named `outer`.
///
/// Deciding that needs the precedence table, not a token whitelist, because an
/// operator or a *glued* opener continues the `outer` expression instead of
/// starting a new one: `outer $ i`, `outer[1]`, `outer(1)`, `outer.x` and
/// `outer::Int` are all plain loop variables, whereas the space-separated
/// `outer (i, j)` and `outer [i]` are the keyword plus a destructuring pattern.
/// So probe by parsing: `outer` is the keyword exactly when it parses as a
/// complete expression on its own (`array_mode` supplies the spaced-opener
/// boundary) and a pattern parses after it.
///
/// Returns the index the pattern starts at.
fn is_outer_marker(ctx: &ParserCtx<'_>, idx: usize, flags: ExprFlags) -> Option<usize> {
    if !ctx
        .token(idx)
        .is_some_and(|t| t.kind == TokKind::Ident && t.text == "outer")
    {
        return None;
    }
    // A separator right after is the plain form (`for outer in xs`, `for outer
    // = xs`); checked first so the probe below never sees the iterable.
    let next = ctx.skip_trivia(idx + 1);
    if ctx
        .token(next)
        .is_some_and(|t| t.kind == TokKind::Eq || is_for_separator_tok(t))
    {
        return None;
    }
    // Speculative parses: their diagnostics belong to the caller's real parse of
    // whichever reading wins, so they are discarded here.
    let mut probe = Vec::new();
    let probe_flags = ExprFlags {
        array_mode: true,
        ..flags
    };
    if parse_expr_in(ctx.tokens(), idx, COMMA_ITEM_BP, &mut probe, probe_flags)
        .is_none_or(|lone| lone.end != idx + 1)
    {
        return None;
    }
    // No pattern after it (`for outer end`, `for outer, j in xs`) is recovery;
    // leave those to the plain reading rather than invent an error shape.
    let pattern_start = ctx.skip_trivia(idx + 1);
    parse_expr_in(
        ctx.tokens(),
        pattern_start,
        COMMA_ITEM_BP,
        &mut probe,
        flags,
    )?;
    Some(pattern_start)
}

/// Parse the comma-separated iteration specs of one `for` clause, starting just
/// past the `for` keyword. Each spec is `var in iter`/`var ∈ iter` (the `in`
/// matched as a bare identifier) or the assignment form `var = iter` (parsed
/// whole as an `ASSIGNMENT_EXPR`). Commas are kept as tokens so the projector can
/// group multiple specs into a `cartesian_iterator`. Returns the index past the
/// last spec.
///
/// `bracketed` selects the scope: a comprehension/generator clause (`[x for i in
/// xs]`) parses inside brackets, where newlines are insignificant; a statement
/// `for`-loop binding (`for i in xs … end`) parses at statement scope, so the
/// iterable stops at the end of the line and a same-line body (`for i in xs y
/// end`) falls through to the loop block rather than being swallowed.
pub(crate) fn parse_for_specs(
    ctx: &ParserCtx<'_>,
    mut pos: usize,
    events: &mut Vec<Event>,
    bracketed: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let tokens = ctx.tokens();
    loop {
        let var_start = ctx.skip_trivia(pos);
        push_range(events, pos, var_start);
        // `for_spec_var` keeps a following `in`/`∈`/`isa` as the iteration
        // separator (handled below) rather than swallowing it as an operator.
        let var_flags = ExprFlags {
            inside_brackets: bracketed,
            for_spec_var: true,
            ..ExprFlags::default()
        };
        // `outer` is contextual: it marks the spec as rebinding an enclosing
        // scope's variable only when a whole pattern follows it.
        let outer = is_outer_marker(ctx, var_start, var_flags);
        // The loop variable (or, absent `outer`, the whole `=`-form spec as one
        // assignment). Under `outer` the pattern parses at `COMMA_ITEM_BP` so a
        // spec `=` stays out of it and is consumed as a separator below, giving
        // `outer` the variable alone — JuliaSyntax nests `(= (outer i) xs)`, not
        // `(outer (= i xs))`.
        let (var_bp, pattern_start) = match outer {
            Some(after_kw) => {
                events.push(Event::Start(SyntaxKind::OUTER_BINDING));
                events.push(Event::Tok(var_start));
                push_range(events, var_start + 1, after_kw);
                (COMMA_ITEM_BP, after_kw)
            }
            None => (0, var_start),
        };
        if let Some(var) = parse_expr_in(tokens, pattern_start, var_bp, diagnostics, var_flags) {
            events.extend(var.events);
            pos = var.end;
        } else {
            pos = pattern_start;
        }
        if outer.is_some() {
            finish(events, SyntaxKind::OUTER_BINDING);
        }

        // `in`/`∈` (and, under `outer`, `=`) separate the variable from the
        // iterator; without `outer` the `=` form is already complete, consumed
        // above as an assignment.
        let in_idx = ctx.skip_trivia(pos);
        if ctx
            .token(in_idx)
            .is_some_and(|t| is_for_separator_tok(t) || (outer.is_some() && t.kind == TokKind::Eq))
        {
            push_range(events, pos, in_idx);
            events.push(Event::Tok(in_idx));
            pos = in_idx + 1;
            let iter_start = ctx.skip_trivia(pos);
            push_range(events, pos, iter_start);
            let iter = if bracketed {
                parse_expr_in_brackets(tokens, iter_start, 0, false, diagnostics)
            } else {
                parse_expr(tokens, iter_start, 0, diagnostics)
            };
            if let Some(iter) = iter {
                events.extend(iter.events);
                pos = iter.end;
            } else {
                pos = iter_start;
            }
        }

        // Another comma-separated spec in the same clause?
        let comma_idx = ctx.skip_trivia(pos);
        if ctx.token(comma_idx).map(|t| t.kind) == Some(TokKind::Comma) {
            push_range(events, pos, comma_idx);
            events.push(Event::Tok(comma_idx));
            pos = comma_idx + 1;
            continue;
        }
        break;
    }
    pos
}
