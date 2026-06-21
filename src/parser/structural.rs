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
use crate::parser::expr::{
    parse_block_stmt, parse_expr, parse_prefix_interpolation, parse_quote_sym,
};
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
    parse_function_like(tokens, start, SyntaxKind::FUNCTION_DEF, diagnostics)
}

/// A `macro` definition: `macro name(args) body end`. Structurally identical to
/// a `function` definition (a call-shaped signature plus a body block), so it
/// shares [`parse_function_like`]; only the wrapper node kind differs.
pub(crate) fn parse_macro_def(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    parse_function_like(tokens, start, SyntaxKind::MACRO_DEF, diagnostics)
}

fn parse_function_like(
    tokens: &[Token],
    start: usize,
    node_kind: SyntaxKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(node_kind), Event::Tok(start)];

    // Signature, e.g. `g(x)` (a call) or `g(x)::T`.
    let sig_start = ctx.skip_ws(start + 1);
    let mut i = if let Some(sig) = parse_expr(tokens, sig_start, 0, diagnostics) {
        push_range(&mut events, start + 1, sig.start);
        events.push(Event::Start(SyntaxKind::SIGNATURE));
        let mut sig_events = sig.events;
        // An anonymous `function (args) … end` signature is a tuple of arguments,
        // not a parenthesized value: Julia models `function (x) end` as
        // `(function (tuple-p x) …)`. A lone `(x)` parses as `PAREN_EXPR`
        // (multi-element / `;` forms already become `TUPLE_EXPR`); relabel it so
        // the single-arg case joins them — unless the parenthesized expression is
        // "eventually a call" (`function (x*y) end`, `function (f()::S) end`),
        // which names a method and keeps its parens stripped. Macros take a call
        // signature, so the shared path's macro form is left alone.
        if node_kind == SyntaxKind::FUNCTION_DEF
            && matches!(
                sig_events.first(),
                Some(Event::Start(SyntaxKind::PAREN_EXPR))
            )
            && !signature_eventually_call(&sig_events, tokens)
        {
            sig_events[0] = Event::Start(SyntaxKind::TUPLE_EXPR);
        }
        events.extend(sig_events);
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

/// Skip (and emit) trivia and `;` separators up to `i`'s next significant token.
/// Inside `abstract`/`primitive type … end` a trailing `;` before `end` is an
/// insignificant separator (`abstract type A ; end` ≡ `abstract type A end`).
fn skip_trivia_and_semis(ctx: &ParserCtx<'_>, events: &mut Vec<Event>, mut i: usize) -> usize {
    loop {
        let next = ctx.skip_trivia(i);
        push_range(events, i, next);
        i = next;
        if ctx.token(i).map(|t| t.kind) == Some(TokKind::Semicolon) {
            events.push(Event::Tok(i));
            i += 1;
        } else {
            return i;
        }
    }
}

/// `abstract type Name end` — a contextual-keyword declaration. `abstract` and
/// `type` are ordinary identifiers elsewhere; here they are bare leaf tokens and
/// the type expression (`A`, `A <: B`, `A{T}`, …) is parsed into a `SIGNATURE`.
/// JuliaSyntax models this as `(abstract <spec>)`, so there is no body block.
pub(crate) fn parse_abstract_type(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::ABSTRACT_DEF), Event::Tok(start)];

    // The contextual `type` keyword (guaranteed adjacent by the caller's gate).
    let type_idx = ctx.skip_trivia(start + 1);
    push_range(&mut events, start + 1, type_idx);
    events.push(Event::Tok(type_idx));

    // The type spec is a real expression (`<:`, `curly`, `where`, …). It has no
    // block body, so trivia (including newlines) up to `end` is insignificant.
    let spec_start = ctx.skip_trivia(type_idx + 1);
    push_range(&mut events, type_idx + 1, spec_start);
    let mut i = spec_start;
    if ctx.token(spec_start).map(|t| t.kind) != Some(TokKind::EndKw)
        && let Some(expr) = parse_expr(tokens, spec_start, 0, diagnostics)
    {
        events.push(Event::Start(SyntaxKind::SIGNATURE));
        events.extend(expr.events);
        events.push(Event::Finish);
        i = expr.end;
    }
    let before_end = skip_trivia_and_semis(&ctx, &mut events, i);
    i = expect_end(&ctx, &mut events, before_end, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
}

/// `primitive type Name Bits end` — like [`parse_abstract_type`], but a size
/// expression follows the type spec on the same line. JuliaSyntax models this as
/// `(primitive <spec> <bits>)`; the spec goes in a `SIGNATURE`, the size is a
/// sibling expression node.
pub(crate) fn parse_primitive_type(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::PRIMITIVE_DEF), Event::Tok(start)];

    // The contextual `type` keyword (guaranteed adjacent by the caller's gate).
    let type_idx = ctx.skip_trivia(start + 1);
    push_range(&mut events, start + 1, type_idx);
    events.push(Event::Tok(type_idx));

    // The type spec, parsed as an expression and wrapped in a `SIGNATURE`. The
    // size is *not* swallowed because a juxtaposed name and number (`A 32`,
    // `B 8`) is not a valid expression continuation, so the spec parse stops.
    let spec_start = ctx.skip_trivia(type_idx + 1);
    push_range(&mut events, type_idx + 1, spec_start);
    let mut i = spec_start;
    if ctx.token(spec_start).map(|t| t.kind) != Some(TokKind::EndKw)
        && let Some(expr) = parse_expr(tokens, spec_start, 0, diagnostics)
    {
        events.push(Event::Start(SyntaxKind::SIGNATURE));
        events.extend(expr.events);
        events.push(Event::Finish);
        i = expr.end;
    }

    // The bit size: the next expression (possibly on a following line).
    let size_start = ctx.skip_trivia(i);
    push_range(&mut events, i, size_start);
    i = size_start;
    if ctx.token(size_start).map(|t| t.kind) != Some(TokKind::EndKw)
        && let Some(size) = parse_expr(tokens, size_start, 0, diagnostics)
    {
        events.extend(size.events);
        i = size.end;
    }

    let before_end = skip_trivia_and_semis(&ctx, &mut events, i);
    i = expect_end(&ctx, &mut events, before_end, start, diagnostics);
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
    i = run_module_block(&ctx, &mut events, i, END_ONLY, diagnostics);
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
    /// the line (`global a, b`, `local x`). A top-level comma is *not* folded
    /// into a tuple: `global`/`local` carry a bare name list (`global a, b` ⇒
    /// `(global a b)`), so each name is parsed separately.
    Expr,
    /// Like [`KwStmt::Expr`], but the operand allows a statement-level
    /// bare-comma tuple (`return x, y` ⇒ `(return (tuple x y))`, `const x, y =
    /// 1, 2` ⇒ `(const (= (tuple x y) (tuple 1 2)))`).
    ExprTuple,
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

        if !header_ends(&ctx, i) {
            let operand = match body {
                KwStmt::ExprTuple => parse_block_stmt(tokens, i, false, diagnostics),
                KwStmt::Expr => parse_expr(tokens, i, 0, diagnostics),
                _ => None,
            };
            if let Some(expr) = operand {
                events.extend(expr.events);
                i = expr.end;
            }
        }

        // Carry any remaining same-line tokens through verbatim, but build real
        // nodes for the name forms the projector models: an `INTERPOLATION` for an
        // interpolated name (`export $a, $(a*b)`) and a `MACRO_NAME` for a macro
        // name (`export @a`), so the projector reads them as `($ …)`/`@a` rather
        // than a loose `$`/`@` + operand.
        while !header_ends(&ctx, i) {
            match ctx.token(i).map(|t| t.kind) {
                Some(TokKind::Dollar) => {
                    let interp = parse_prefix_interpolation(&ctx, i, diagnostics);
                    events.extend(interp.events);
                    i = interp.end;
                }
                Some(TokKind::At) => {
                    i = push_macro_name(&ctx, &mut events, i);
                }
                _ => {
                    events.push(Event::Tok(i));
                    i += 1;
                }
            }
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

/// Emit a [`MACRO_NAME`](SyntaxKind) node for a macro name used as a directive
/// name (`export @a`, `import A.@x`): the `@` sigil at `at_idx` plus an adjacent
/// identifier. Returns the index just past the name. Unlike a macro *call*, no
/// arguments and no dotted chain are consumed — in these positions Julia treats a
/// trailing `.mac` as a separate (erroring) component, and the import-path loop
/// handles further dotted components itself.
fn push_macro_name(ctx: &ParserCtx<'_>, events: &mut Vec<Event>, at_idx: usize) -> usize {
    events.push(Event::Start(SyntaxKind::MACRO_NAME));
    events.push(Event::Tok(at_idx)); // `@`
    let mut i = at_idx + 1;
    if ctx.token(i).map(|t| t.kind) == Some(TokKind::Ident) {
        events.push(Event::Tok(i));
        i += 1;
    }
    events.push(Event::Finish);
    i
}

/// Parse a single dotted import path into an [`IMPORT_PATH`](SyntaxKind) node:
/// leading relative dots (`.`/`..`/`...`) followed by dot-separated identifier
/// components (`A.B.C`). Returns the index after the path; equal to `start` when
/// no path is recognized.
fn parse_import_path(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
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
        Some(TokKind::Dollar) => {
            // An interpolated path root (`import $A`): a real `INTERPOLATION`
            // node the projector reads as `($ A)`.
            let interp = parse_prefix_interpolation(ctx, i, diagnostics);
            body.extend(interp.events);
            i = interp.end;
        }
        Some(TokKind::At) => {
            // A macro-name path root (`import @x`, `import .@x`): a `MACRO_NAME`
            // node the projector reads as `@x`.
            i = push_macro_name(ctx, &mut body, i);
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
            (Some(TokKind::Dot), Some(TokKind::At)) => {
                // A macro-name component (`import A.@x` → `(importpath A @x)`).
                body.push(Event::Tok(i)); // separating `.`
                i = push_macro_name(ctx, &mut body, i + 1);
            }
            (Some(TokKind::Dot), Some(TokKind::Colon)) => {
                // A quoted symbol component (`A.:+` → `(quote-: +)`, `A.:(+)` →
                // `(quote-: +)`). The `:` and everything after it is a `QUOTE_SYM`.
                let Some(quote) = parse_quote_sym(ctx, i + 1, diagnostics) else {
                    break;
                };
                body.push(Event::Tok(i)); // separating `.`
                body.extend(quote.events);
                i = quote.end;
            }
            (Some(TokKind::Dot), Some(TokKind::LParen))
                if ctx.token(i + 2).map(|t| t.kind) == Some(TokKind::Colon) =>
            {
                // A parenthesized quoted symbol (`A.(:+)` → `(quote-: +)`). The
                // parens wrap a `QUOTE_SYM`; both project away to the bare quote.
                let Some(quote) = parse_quote_sym(ctx, i + 2, diagnostics) else {
                    break;
                };
                let rparen = quote.end;
                if ctx.token(rparen).map(|t| t.kind) != Some(TokKind::RParen) {
                    break;
                }
                body.push(Event::Tok(i)); // separating `.`
                body.push(Event::Start(SyntaxKind::PAREN_EXPR));
                body.push(Event::Tok(i + 1)); // `(`
                body.extend(quote.events);
                body.push(Event::Tok(rparen)); // `)`
                body.push(Event::Finish); // PAREN_EXPR
                i = rparen + 1;
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

/// Mirror of JuliaSyntax's `was_eventually_call`, over a parsed node's event
/// slice: peel `where`/`parens`/infix-`::` off the front (following the first
/// child) and report whether a call is reached. Used to decide whether a
/// parenthesized `function` signature is an anonymous argument tuple
/// (`function (x) end` → `(tuple-p x)`) or a named-function signature in parens
/// (`function (x*y) end` → `(call-i x * y)`, parens stripped). `events` must be
/// the balanced event slice of a single node (`events[0]` its `Start`).
fn signature_eventually_call(events: &[Event], tokens: &[Token]) -> bool {
    use SyntaxKind::*;
    match events.first() {
        Some(Event::Start(CALL_EXPR)) => true,
        Some(Event::Start(TYPE_ANNOTATION | WHERE_EXPR | PAREN_EXPR)) => {
            first_child_slice(events).is_some_and(|child| signature_eventually_call(child, tokens))
        }
        // A `BINARY_EXPR` is a call iff its operator is an ordinary infix-call
        // operator (`*`, `=>`, `<`, …); the short-circuit/type/arrow/field/dotted
        // operators model as their own heads, not `call` (mirrors the `CallI`
        // arms of the projector's `infix_head`).
        Some(Event::Start(BINARY_EXPR)) => {
            direct_child_operator(events, tokens).is_some_and(is_call_infix_operator)
        }
        _ => false,
    }
}

/// The balanced event slice of `events`' first child node (`events[0]` is the
/// parent `Start`), skipping any leading delimiter/trivia tokens. `None` if the
/// node has no child node.
fn first_child_slice(events: &[Event]) -> Option<&[Event]> {
    let mut depth = 0i32;
    let mut start = None;
    for (i, ev) in events.iter().enumerate() {
        match ev {
            Event::Start(_) => {
                depth += 1;
                if depth == 2 {
                    start = Some(i);
                    break;
                }
            }
            Event::Finish => depth -= 1,
            Event::Tok(_) => {}
        }
    }
    let start = start?;
    let mut depth = 0i32;
    for (i, ev) in events[start..].iter().enumerate() {
        match ev {
            Event::Start(_) => depth += 1,
            Event::Finish => {
                depth -= 1;
                if depth == 0 {
                    return Some(&events[start..=start + i]);
                }
            }
            Event::Tok(_) => {}
        }
    }
    None
}

/// The kind of a binary node's operator: its first significant direct-child
/// token (depth 1), skipping trivia. Operands are deeper child nodes.
fn direct_child_operator(events: &[Event], tokens: &[Token]) -> Option<TokKind> {
    let mut depth = 0i32;
    for ev in events {
        match ev {
            Event::Start(_) => depth += 1,
            Event::Finish => depth -= 1,
            Event::Tok(i) => {
                let kind = tokens[*i].kind;
                if depth == 1 && !kind.is_trivia() {
                    return Some(kind);
                }
            }
        }
    }
    None
}

/// Whether `kind` is an ordinary infix-call operator — one JuliaSyntax models as
/// `K"call"` (and the projector's `infix_head` maps to `CallI`). Excludes the
/// short-circuit (`&&`/`||`), type (`<:`/`>:`), `-->`, field-access `.`, and all
/// dotted/broadcast operators, which carry their own heads.
fn is_call_infix_operator(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        Plus | Minus
            | Star
            | Slash
            | SlashSlash
            | Caret
            | Percent
            | Colon
            | DotDot
            | FatArrow
            | PipeGt
            | PipeLt
            | LeftRightArrow
            | Shl
            | Shr
            | UShr
            | Amp
            | Pipe
            | EqEq
            | NotEq
            | Lt
            | Le
            | Gt
            | Ge
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
    } else if ctx.token(i).map(|t| t.kind) == Some(TokKind::Dollar) {
        // An interpolated name (`module $A end`): build a real `INTERPOLATION`
        // node so the projector reads it as `($ A)` rather than a loose name.
        let interp = parse_prefix_interpolation(ctx, i, diagnostics);
        events.extend(interp.events);
        i = interp.end;
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
    run_block_inner(ctx, events, start, terminators, false, diagnostics)
}

/// Like [`run_block`], but a module body where the contextual keyword `public`
/// opens a `PUBLIC_STMT` (parsed via [`parse_stmt`] rather than [`parse_expr`]).
fn run_module_block(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    start: usize,
    terminators: &[TokKind],
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    run_block_inner(ctx, events, start, terminators, true, diagnostics)
}

fn run_block_inner(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    start: usize,
    terminators: &[TokKind],
    public_context: bool,
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
                let parsed = parse_block_stmt(tokens, i, public_context, diagnostics);
                if let Some(stmt) = parsed {
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
