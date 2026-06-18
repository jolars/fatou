//! Recursive-descent parsing for Julia's `… end` block forms: `if/elseif/else`,
//! `function`, `begin`, `quote`, `while`, `for`, `let`, `try/catch/else/finally`,
//! `struct`/`mutable struct`, and `module`/`baremodule`. Each keyword opens a
//! node, parses its clauses/header and a statement block, and closes on `end`.
//!
//! The `do` block (`f(x) do y … end`) is the one form not opened by a leading
//! keyword: it is postfix on a call, so [`parse_do_block`] wraps an
//! already-parsed expression and is driven from the postfix chain in `expr`.
//!
//! Two more leading-keyword families live here even though they have no `end`:
//! the simple statement forms parsed by [`parse_keyword_stmt`] — control flow
//! (`return`/`break`/`continue`), declarations (`const`/`global`/`local`), and
//! module directives (`import`/`using`/`export`).

use crate::parser::context::ParserCtx;
use crate::parser::diagnostics::{ParseDiagnostic, push_diagnostic};
use crate::parser::events::{Event, ExprParse, push_range};
use crate::parser::expr::parse_expr;
use crate::parser::lexer::{TokKind, Token};
use crate::syntax::SyntaxKind;

/// Keywords that terminate a statement block.
const IF_TERMINATORS: &[TokKind] = &[TokKind::EndKw, TokKind::ElseifKw, TokKind::ElseKw];
const END_ONLY: &[TokKind] = &[TokKind::EndKw];
const TRY_TERMINATORS: &[TokKind] = &[
    TokKind::EndKw,
    TokKind::CatchKw,
    TokKind::ElseKw,
    TokKind::FinallyKw,
];

pub(crate) fn parse_if_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::IF_EXPR), Event::Tok(start)];

    let mut i = parse_condition(&ctx, &mut events, start + 1, diagnostics);
    i = run_block(&ctx, &mut events, i, IF_TERMINATORS, diagnostics);

    loop {
        match ctx.token(i).map(|t| t.kind) {
            Some(TokKind::ElseifKw) => {
                events.push(Event::Start(SyntaxKind::ELSEIF_CLAUSE));
                events.push(Event::Tok(i));
                let cond_end = parse_condition(&ctx, &mut events, i + 1, diagnostics);
                i = run_block(&ctx, &mut events, cond_end, IF_TERMINATORS, diagnostics);
                events.push(Event::Finish);
            }
            Some(TokKind::ElseKw) => {
                events.push(Event::Start(SyntaxKind::ELSE_CLAUSE));
                events.push(Event::Tok(i));
                i = run_block(&ctx, &mut events, i + 1, END_ONLY, diagnostics);
                events.push(Event::Finish);
                break;
            }
            _ => break,
        }
    }

    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

pub(crate) fn parse_function_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::FUNCTION_DEF), Event::Tok(start)];

    // Signature, e.g. `g(x)` (a call) or `g(x)::T`.
    let sig_start = ctx.skip_ws(start + 1);
    let mut i = if let Some(sig) = parse_expr(tokens, sig_start, 0, diagnostics) {
        push_range(&mut events, start + 1, sig.start);
        events.push(Event::Start(SyntaxKind::SIGNATURE));
        events.extend(sig.events);
        events.push(Event::Finish);
        sig.end
    } else {
        start + 1
    };

    i = run_block(&ctx, &mut events, i, END_ONLY, diagnostics);
    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

pub(crate) fn parse_begin_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    parse_block_only(tokens, start, SyntaxKind::BEGIN_EXPR, diagnostics)
}

pub(crate) fn parse_quote_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    parse_block_only(tokens, start, SyntaxKind::QUOTE_EXPR, diagnostics)
}

/// A keyword form whose body is a bare statement block: `begin … end` and
/// `quote … end`. The keyword opens `node_kind`, a block runs to `end`.
fn parse_block_only(
    tokens: &[Token],
    start: usize,
    node_kind: SyntaxKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(node_kind), Event::Tok(start)];

    let mut i = run_block(&ctx, &mut events, start + 1, END_ONLY, diagnostics);
    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

pub(crate) fn parse_while_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::WHILE_EXPR), Event::Tok(start)];

    let mut i = parse_condition(&ctx, &mut events, start + 1, diagnostics);
    i = run_block(&ctx, &mut events, i, END_ONLY, diagnostics);
    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

pub(crate) fn parse_for_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::FOR_EXPR), Event::Tok(start)];

    let mut i = parse_header(
        &ctx,
        &mut events,
        start + 1,
        SyntaxKind::FOR_BINDING,
        true,
        diagnostics,
    );
    i = run_block(&ctx, &mut events, i, END_ONLY, diagnostics);
    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

pub(crate) fn parse_let_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::LET_EXPR), Event::Tok(start)];

    let mut i = parse_header(
        &ctx,
        &mut events,
        start + 1,
        SyntaxKind::LET_BINDINGS,
        true,
        diagnostics,
    );
    i = run_block(&ctx, &mut events, i, END_ONLY, diagnostics);
    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

pub(crate) fn parse_try_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::TRY_EXPR), Event::Tok(start)];

    let mut i = run_block(&ctx, &mut events, start + 1, TRY_TERMINATORS, diagnostics);

    loop {
        match ctx.token(i).map(|t| t.kind) {
            Some(TokKind::CatchKw) => {
                events.push(Event::Start(SyntaxKind::CATCH_CLAUSE));
                events.push(Event::Tok(i));
                // Optional exception variable on the `catch` line (`catch e`).
                let var_start = ctx.skip_ws(i + 1);
                push_range(&mut events, i + 1, var_start);
                let mut j = var_start;
                if !header_ends(&ctx, var_start)
                    && let Some(var) = parse_expr(tokens, var_start, 0, diagnostics)
                {
                    events.extend(var.events);
                    j = var.end;
                }
                i = run_block(&ctx, &mut events, j, TRY_TERMINATORS, diagnostics);
                events.push(Event::Finish);
            }
            Some(TokKind::ElseKw) => {
                events.push(Event::Start(SyntaxKind::ELSE_CLAUSE));
                events.push(Event::Tok(i));
                i = run_block(&ctx, &mut events, i + 1, TRY_TERMINATORS, diagnostics);
                events.push(Event::Finish);
            }
            Some(TokKind::FinallyKw) => {
                events.push(Event::Start(SyntaxKind::FINALLY_CLAUSE));
                events.push(Event::Tok(i));
                i = run_block(&ctx, &mut events, i + 1, END_ONLY, diagnostics);
                events.push(Event::Finish);
                break;
            }
            _ => break,
        }
    }

    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

/// `struct Name … end` and `mutable struct Name … end`. Dispatched on either the
/// `struct` or the (contextual) `mutable` keyword.
pub(crate) fn parse_struct_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::STRUCT_DEF)];

    // Optional leading `mutable`.
    let mut i = start;
    if ctx.token(i).map(|t| t.kind) == Some(TokKind::MutableKw) {
        events.push(Event::Tok(i));
        let next = ctx.skip_ws(i + 1);
        push_range(&mut events, i + 1, next);
        i = next;
    }

    // The `struct` keyword.
    if ctx.token(i).map(|t| t.kind) == Some(TokKind::StructKw) {
        events.push(Event::Tok(i));
        i += 1;
    } else {
        let kw = &ctx.tokens()[start];
        push_diagnostic(diagnostics, "expected `struct`", kw.start, kw.end);
    }

    i = parse_header(
        &ctx,
        &mut events,
        i,
        SyntaxKind::SIGNATURE,
        false,
        diagnostics,
    );
    i = run_block(&ctx, &mut events, i, END_ONLY, diagnostics);
    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

/// `module Name … end` and `baremodule Name … end`.
pub(crate) fn parse_module_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::MODULE_DEF), Event::Tok(start)];

    let mut i = parse_header(
        &ctx,
        &mut events,
        start + 1,
        SyntaxKind::SIGNATURE,
        false,
        diagnostics,
    );
    i = run_block(&ctx, &mut events, i, END_ONLY, diagnostics);
    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

/// The shape of a simple keyword statement's body — the part (if any) that
/// follows the keyword on its line.
pub(crate) enum KwStmt {
    /// Just the keyword (`break`, `continue`); any trailing trivia is left to
    /// the enclosing block loop, exactly like a single-token atom.
    Bare,
    /// An optional leading expression, then verbatim passthrough of the rest of
    /// the line (`return [expr]`, `const x = 1`, `global a, b`).
    Expr,
    /// Pure verbatim passthrough of the rest of the line (`import A: b`,
    /// `using A.B`, `export a, b`). The module-path/name grammar uses `:`/`.`/`,`
    /// that have no dedicated trees yet, so parsing it as an expression would
    /// misread `:` as a range and `.` as field access (see `TODO.md`).
    Path,
}

/// Parse a simple keyword-led statement that is not a `… end` block form. The
/// keyword at `start` opens `node_kind`; `body` selects what follows it on the
/// line. Losslessness holds: every same-line token is either parsed into a
/// subtree or carried through verbatim.
pub(crate) fn parse_keyword_stmt(
    tokens: &[Token],
    start: usize,
    node_kind: SyntaxKind,
    body: KwStmt,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(node_kind), Event::Tok(start)];

    let mut i = start + 1;
    if !matches!(body, KwStmt::Bare) {
        let operand_start = ctx.skip_ws(i);
        push_range(&mut events, i, operand_start);
        i = operand_start;

        if matches!(body, KwStmt::Expr)
            && !header_ends(&ctx, i)
            && let Some(expr) = parse_expr(tokens, i, 0, diagnostics)
        {
            events.extend(expr.events);
            i = expr.end;
        }

        // Carry any remaining same-line tokens through verbatim.
        while !header_ends(&ctx, i) {
            events.push(Event::Tok(i));
            i += 1;
        }
    }

    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

/// Parse an `import`/`using` directive into a real path tree. The keyword at
/// `start` opens `node_kind`; what follows is a comma-separated list of clauses,
/// optionally split by a top-level `:` into a base path and a list of imported
/// names (`import A: x, y`). Each clause is an [`IMPORT_PATH`](SyntaxKind) —
/// leading relative dots plus dot-separated name components — optionally wrapped
/// in an [`IMPORT_ALIAS`](SyntaxKind) for an `as` rename. The `:`/`,` separators
/// are kept as tokens so the projector can group base-vs-names. Anything the path
/// grammar doesn't recognize (operator names, `@macro`/`$interp` paths) is carried
/// through verbatim to preserve losslessness; those remain divergences for now.
pub(crate) fn parse_import_stmt(
    tokens: &[Token],
    start: usize,
    node_kind: SyntaxKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(node_kind), Event::Tok(start)];

    let mut i = parse_import_clause(&ctx, &mut events, start + 1, diagnostics);

    // Comma-separated further clauses, plus an optional single `:` that switches
    // from the base path to the list of imported names. Both separators feed the
    // same clause parser; the projector reads the `:` to group base vs. names.
    loop {
        let sep = ctx.skip_ws(i);
        match ctx.token(sep).map(|t| t.kind) {
            Some(TokKind::Comma | TokKind::Colon) => {
                push_range(&mut events, i, sep);
                events.push(Event::Tok(sep));
                i = parse_import_clause(&ctx, &mut events, sep + 1, diagnostics);
            }
            _ => break,
        }
    }

    // Carry any remaining same-line tokens through verbatim (unrecognized path
    // forms), keeping losslessness.
    while !header_ends(&ctx, i) {
        events.push(Event::Tok(i));
        i += 1;
    }

    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

/// Parse one import clause: an [`IMPORT_PATH`](SyntaxKind), optionally followed by
/// `as <name>` (wrapping the path in an [`IMPORT_ALIAS`](SyntaxKind)). Emits the
/// leading whitespace before the path, then the path subtree. Returns the index
/// after the clause (unchanged if no path is recognized, so the caller's verbatim
/// passthrough takes over).
fn parse_import_clause(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    after_sep: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let path_start = ctx.skip_ws(after_sep);

    let mut path_events = Vec::new();
    let path_end = parse_import_path(ctx, &mut path_events, path_start, diagnostics);
    if path_end == path_start {
        // Nothing recognized as a path; leave it (and its leading whitespace) to
        // the caller's verbatim passthrough.
        return after_sep;
    }
    // Commit the leading whitespace only now that a path was recognized, so the
    // failure path above doesn't double-emit it.
    push_range(events, after_sep, path_start);

    // `as <name>` rename — `as` is a contextual identifier.
    let as_idx = ctx.skip_ws(path_end);
    if is_as_kw(ctx, as_idx) {
        let alias_start = ctx.skip_ws(as_idx + 1);
        if matches!(ctx.token(alias_start).map(|t| t.kind), Some(TokKind::Ident)) {
            events.push(Event::Start(SyntaxKind::IMPORT_ALIAS));
            events.extend(path_events);
            push_range(events, path_end, as_idx);
            events.push(Event::Tok(as_idx)); // `as`
            push_range(events, as_idx + 1, alias_start);
            events.push(Event::Tok(alias_start)); // alias name
            events.push(Event::Finish);
            return alias_start + 1;
        }
    }

    events.extend(path_events);
    path_end
}

/// Parse a single dotted import path into an [`IMPORT_PATH`](SyntaxKind) node:
/// leading relative dots (`.`/`..`/`...`) followed by dot-separated identifier
/// components (`A.B.C`). Returns the index after the path; equal to `start` when
/// no path is recognized.
fn parse_import_path(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    start: usize,
    _diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let mut i = start;
    let mut body = Vec::new();

    // Leading relative dots: `.`/`..`/`...`, consumed greedily before any name.
    while matches!(
        ctx.token(i).map(|t| t.kind),
        Some(TokKind::Dot | TokKind::DotDot | TokKind::DotDotDot)
    ) {
        body.push(Event::Tok(i));
        i += 1;
    }

    // First name component: an identifier, or a bare operator symbol when the
    // clause's whole path is an operator (`import A: +`, `import A: +, ==`).
    match ctx.token(i).map(|t| t.kind) {
        Some(TokKind::Ident) => {
            body.push(Event::Tok(i));
            i += 1;
        }
        Some(k) if is_op_name(k) => {
            body.push(Event::Tok(i));
            i += 1;
        }
        _ => {
            // No name: a bare relative path (`import .`) keeps just the dots;
            // nothing at all means no path here.
            if body.is_empty() {
                return start;
            }
            events.push(Event::Start(SyntaxKind::IMPORT_PATH));
            events.extend(body);
            events.push(Event::Finish);
            return i;
        }
    }

    // Further `.component` parts, kept tight (no internal whitespace). A
    // component is an identifier (`A.B`), a fused dotted operator (`A.==`, lexed
    // as one `.==` token whose leading dot is the separator), or a quoted
    // operator symbol (`A.:+` → `.` `:` `+` → `(quote-: +)`).
    loop {
        match (
            ctx.token(i).map(|t| t.kind),
            ctx.token(i + 1).map(|t| t.kind),
        ) {
            (Some(TokKind::Dot), Some(TokKind::Ident)) => {
                body.push(Event::Tok(i)); // separating `.`
                body.push(Event::Tok(i + 1)); // name
                i += 2;
            }
            (Some(TokKind::Dot), Some(TokKind::Colon)) if matches!(ctx.token(i + 2).map(|t| t.kind), Some(k) if is_op_name(k)) =>
            {
                body.push(Event::Tok(i)); // separating `.`
                body.push(Event::Start(SyntaxKind::QUOTE_SYM));
                body.push(Event::Tok(i + 1)); // `:`
                body.push(Event::Tok(i + 2)); // operator
                body.push(Event::Finish);
                i += 3;
            }
            (Some(k), _) if is_dotted_op_name(k) => {
                body.push(Event::Tok(i));
                i += 1;
            }
            _ => break,
        }
    }

    events.push(Event::Start(SyntaxKind::IMPORT_PATH));
    events.extend(body);
    events.push(Event::Finish);
    i
}

/// An undotted operator symbol usable as a bare import-path name (`import A: +`,
/// `import A.:+`, `import A: +, ==`). Excludes the `:` list separator, the
/// relative-dot tokens, and assignment forms (`=`, `+=`).
pub(super) fn is_op_name(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        Plus | Minus
            | Star
            | Slash
            | SlashSlash
            | Caret
            | Percent
            | EqEq
            | NotEq
            | Lt
            | Le
            | Gt
            | Ge
            | AndAnd
            | OrOr
            | Subtype
            | Supertype
            | Arrow
            | LongArrow
            | LeftRightArrow
            | FatArrow
            | Shl
            | Shr
            | UShr
            | PipeGt
            | PipeLt
            | Bang
            | Amp
            | Pipe
            | Tilde
    )
}

/// A fused dotted (broadcast) operator token (`.+`, `.==`). In an import path
/// these encode a separator dot fused to an operator name, so the projector
/// strips the leading dot (`import A.==` → `(importpath A ==)`).
fn is_dotted_op_name(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        DotPlus
            | DotMinus
            | DotStar
            | DotSlash
            | DotSlashSlash
            | DotCaret
            | DotPercent
            | DotEqEq
            | DotNotEq
            | DotLt
            | DotLe
            | DotGt
            | DotGe
            | DotAndAnd
            | DotOrOr
            | DotTilde
            | DotFatArrow
            | DotLongArrow
            | DotPipeGt
    )
}

/// Whether the token at `i` is the contextual `as` keyword (a plain identifier
/// whose text is `as`).
fn is_as_kw(ctx: &ParserCtx<'_>, i: usize) -> bool {
    matches!(ctx.token(i), Some(t) if t.kind == TokKind::Ident && t.text == "as")
}

/// Wrap an already-parsed call expression `lhs` in a `DO_EXPR` for the postfix
/// `do` block form (`f(x) do y … end`). `do_idx` is the `do` keyword's token
/// index (the caller has verified it sits on `lhs`'s line). The optional
/// parameters on the `do` line reuse the generic header passthrough, and the
/// body is a plain statement block closed by `end`.
pub(crate) fn parse_do_block(
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    do_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let mut events = vec![Event::Start(SyntaxKind::DO_EXPR)];
    events.extend(lhs.events);
    // Whitespace between the call and `do`, then the `do` keyword itself.
    push_range(&mut events, lhs.end, do_idx);
    events.push(Event::Tok(do_idx));

    let mut i = parse_header(
        ctx,
        &mut events,
        do_idx + 1,
        SyntaxKind::DO_PARAMS,
        true,
        diagnostics,
    );
    i = run_block(ctx, &mut events, i, END_ONLY, diagnostics);
    i = expect_end(ctx, &mut events, i, do_idx, diagnostics);
    events.push(Event::Finish);
    ExprParse {
        start: lhs.start,
        end: i,
        events,
    }
}

/// Parse a `CONDITION` node (the test of an `if`/`elseif`). The condition lives
/// on the keyword's line, so a newline ends it. Emits the trivia between the
/// keyword and the condition first. Returns the index after the condition.
fn parse_condition(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    after_kw: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let cond_start = ctx.skip_ws(after_kw);
    push_range(events, after_kw, cond_start);
    match parse_expr(ctx.tokens(), cond_start, 0, diagnostics) {
        Some(cond) => {
            events.push(Event::Start(SyntaxKind::CONDITION));
            events.extend(cond.events);
            events.push(Event::Finish);
            cond.end
        }
        None => {
            let tok = &ctx.tokens()[after_kw.min(ctx.tokens().len() - 1)];
            push_diagnostic(diagnostics, "expected a condition", tok.start, tok.end);
            cond_start
        }
    }
}

/// Parse the header that sits on a block keyword's line, wrapping it in
/// `node_kind`. When `run_expr` is set, an expression is parsed first (so
/// `for i = 1:10` yields an assignment); any remaining tokens on the line are
/// then carried through verbatim. This keeps losslessness without committing to
/// dedicated `in`/`∈`/`<:` operators yet (those land with the operators and
/// parametric-type work; see `TODO.md`). An empty header emits no node. Returns
/// the index after the header.
fn parse_header(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    after_kw: usize,
    node_kind: SyntaxKind,
    run_expr: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let header_start = ctx.skip_ws(after_kw);
    push_range(events, after_kw, header_start);

    if header_ends(ctx, header_start) {
        return header_start;
    }

    events.push(Event::Start(node_kind));
    let mut i = header_start;
    if run_expr && let Some(expr) = parse_expr(ctx.tokens(), header_start, 0, diagnostics) {
        events.extend(expr.events);
        i = expr.end;
    }
    while !header_ends(ctx, i) {
        events.push(Event::Tok(i));
        i += 1;
    }
    events.push(Event::Finish);
    i
}

/// Whether the keyword-line header ends at `i`: at a newline, a `;`, a block
/// terminator keyword (so one-liners like `struct Foo end` stop correctly), or
/// end of input.
fn header_ends(ctx: &ParserCtx<'_>, i: usize) -> bool {
    match ctx.token(i).map(|t| t.kind) {
        None => true,
        Some(k) => {
            matches!(k, TokKind::Newline | TokKind::Semicolon)
                || matches!(
                    k,
                    TokKind::EndKw
                        | TokKind::ElseifKw
                        | TokKind::ElseKw
                        | TokKind::CatchKw
                        | TokKind::FinallyKw
                )
        }
    }
}

/// Parse a `BLOCK` of statements starting at `start`, stopping (without
/// consuming) at the first `terminators` keyword or end of input. Appends the
/// block's events. Returns the index of the terminator (or EOF).
fn run_block(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    start: usize,
    terminators: &[TokKind],
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let tokens = ctx.tokens();
    events.push(Event::Start(SyntaxKind::BLOCK));
    let mut i = start;

    loop {
        // Trivia and `;` statement separators belong to the block.
        while matches!(
            tokens.get(i).map(|t| t.kind),
            Some(k) if k.is_trivia() || k == TokKind::Semicolon
        ) {
            events.push(Event::Tok(i));
            i += 1;
        }
        match tokens.get(i).map(|t| t.kind) {
            None => break,
            Some(k) if terminators.contains(&k) => break,
            Some(_) => {
                if let Some(stmt) = parse_expr(tokens, i, 0, diagnostics) {
                    events.extend(stmt.events);
                    i = stmt.end;
                } else {
                    events.push(Event::Tok(i));
                    i += 1;
                }
            }
        }
    }

    events.push(Event::Finish);
    i
}

/// Emit the closing `end` keyword, or a diagnostic if it is missing.
fn expect_end(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    i: usize,
    open_start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    if ctx.token(i).map(|t| t.kind) == Some(TokKind::EndKw) {
        events.push(Event::Tok(i));
        i + 1
    } else {
        let kw = &ctx.tokens()[open_start];
        push_diagnostic(diagnostics, "expected `end`", kw.start, kw.end);
        i
    }
}
