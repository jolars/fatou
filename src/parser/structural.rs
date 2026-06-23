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
use crate::parser::diagnostics::{DiagnosticKind, ParseDiagnostic, push_diagnostic};
use crate::parser::events::{Event, ExprParse, push_range};
use crate::parser::expr::{
    parse_block_stmt, parse_expr, parse_for_binding, parse_prefix_interpolation, parse_quote_sym,
    parse_signature_expr, push_var_macro_name,
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

    // Signature, e.g. `g(x)` (a call) or `g(x)::T`. A `::` return type stays a
    // bare annotation and a trailing `where` binds the whole signature (rather
    // than the return type), so parse it with `no_decl_where`.
    let sig_start = ctx.skip_ws(start + 1);
    let mut i = if let Some(sig) = parse_signature_expr(tokens, sig_start, diagnostics) {
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
        if node_kind == SyntaxKind::FUNCTION_DEF {
            match sig_events.first() {
                Some(Event::Start(SyntaxKind::PAREN_EXPR))
                    if !signature_eventually_call(&sig_events, tokens) =>
                {
                    sig_events[0] = Event::Start(SyntaxKind::TUPLE_EXPR);
                }
                // A `;`-bearing signature (`function (x; y) end`) parses as a
                // `PAREN_BLOCK` in value position, but in a signature the parens
                // are a parameter list, not a block — relabel to the tuple it
                // already shares its `ARG`/`PARAMETERS` shape with.
                Some(Event::Start(SyntaxKind::PAREN_BLOCK)) => {
                    sig_events[0] = Event::Start(SyntaxKind::TUPLE_EXPR);
                }
                _ => {}
            }
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

    // A `try` requires a `catch` or `finally`; with neither, JuliaSyntax splices
    // a truncation marker for the missing handler (`try x end` ⇒ `(try (block x)
    // (error-t))`). `else` does not satisfy the requirement.
    let mut saw_handler = false;
    let mut saw_catch = false;
    loop {
        match ctx.token(i).map(|t| t.kind) {
            Some(TokKind::CatchKw) => {
                saw_handler = true;
                saw_catch = true;
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
                if saw_catch {
                    i = run_block(&ctx, &mut events, i + 1, TRY_TERMINATORS, diagnostics);
                } else {
                    // `else` before any `catch` is invalid; JuliaSyntax wraps the
                    // else block in an `(error …)` node (`try x else y end` ⇒
                    // `(try (block x) (else (error (block y))) (error-t))`).
                    let kw = &ctx.tokens()[i];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::ElseWithoutCatch,
                        "`else` without `catch`",
                        kw.start,
                        kw.end,
                    );
                    events.push(Event::Start(SyntaxKind::ERROR));
                    i = run_block(&ctx, &mut events, i + 1, TRY_TERMINATORS, diagnostics);
                    events.push(Event::Finish);
                }
                events.push(Event::Finish);
            }
            Some(TokKind::FinallyKw) => {
                saw_handler = true;
                events.push(Event::Start(SyntaxKind::FINALLY_CLAUSE));
                events.push(Event::Tok(i));
                i = run_block(&ctx, &mut events, i + 1, TRY_TERMINATORS, diagnostics);
                events.push(Event::Finish);
                // Julia accepts `catch` after `finally` (either clause order);
                // any other clause keyword here is an error, so stop and let
                // `expect_end` recover.
                if ctx.token(i).map(|t| t.kind) != Some(TokKind::CatchKw) {
                    break;
                }
            }
            _ => break,
        }
    }

    if !saw_handler {
        let kw = &ctx.tokens()[start];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::MissingTryHandler,
            "expected `catch` or `finally`",
            kw.start,
            kw.end,
        );
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
        push_diagnostic(
            diagnostics,
            DiagnosticKind::MissingStruct,
            "expected `struct`",
            kw.start,
            kw.end,
        );
    }

    i = parse_signature(&ctx, &mut events, i, diagnostics);
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

    let mut i = parse_signature(&ctx, &mut events, start + 1, diagnostics);
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
    optional_value: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(node_kind), Event::Tok(start)];

    let mut i = start + 1;
    if !matches!(body, KwStmt::Bare) {
        let operand_start = ctx.skip_ws(i);
        // A keyword whose value is optional (`return`) ends right after the
        // keyword when its operand position is a stray closing delimiter
        // (`return)`, `return ]`): the empty form `(return)` is emitted and the
        // delimiter is left for the toplevel-leftover driver to wrap as `✘`,
        // matching `break)`. Value-required keywords (`const`/`global`/`local`)
        // instead need an inner `(error)` synthesis and so do not take this path.
        if optional_value
            && ctx
                .token(operand_start)
                .is_some_and(|t| is_close_delimiter_tok(t.kind))
        {
            events.push(Event::Finish);
            return Some(ExprParse {
                start,
                end: i,
                events,
            });
        }
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
                    i = push_macro_name(&ctx, &mut events, i, diagnostics);
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

/// Parse an `export`/`public` name-list statement. The keyword at `start` opens
/// `node_kind`, followed by a comma-separated list of exported names. A name is a
/// bare identifier, an operator used as a name (`export +, ==`, `export ⊕`), an
/// interpolated name (`export $a, $(a*b)`), or a macro name (`export @a`,
/// `export @var"#"`). A newline directly after the keyword or after a comma
/// continues the list onto the next line (`export a, \n b`); a newline after a
/// complete name ends the statement (`export a \n b` is two statements). Every
/// token is either parsed into a name subtree or carried through verbatim, so
/// losslessness holds.
pub(crate) fn parse_name_list_stmt(
    tokens: &[Token],
    start: usize,
    node_kind: SyntaxKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(node_kind), Event::Tok(start)];

    // `public` is a names-only compatibility shim: after a complete name it
    // continues only across a comma, ending the statement at any other following
    // token (`public x = 1` ⇒ `(public x) (error-t = 1)`, `public a b` ⇒
    // `(public a) (error-t b)`). `export` instead re-enters the operator parser
    // (`export x = 1` ⇒ `(= (export x) 1)`), so it keeps carrying every same-line
    // token; the leftover that a stopped `public` leaves behind is recovered by
    // the toplevel trailing-junk driver.
    let is_public = node_kind == SyntaxKind::PUBLIC_STMT;

    // A newline directly after the keyword continues onto the next line.
    let mut i = ctx.skip_ws_and_newlines(start + 1);
    push_range(&mut events, start + 1, i);

    while !header_ends(&ctx, i) {
        let mut consumed_name = false;
        match ctx.token(i).map(|t| t.kind) {
            // An interpolated name (`export $a, $(a*b)`) → `($ …)`.
            Some(TokKind::Dollar) => {
                let interp = parse_prefix_interpolation(&ctx, i, diagnostics);
                events.extend(interp.events);
                i = interp.end;
                consumed_name = true;
            }
            // A macro name (`export @a`, `export @var"#"`) → `@a`.
            Some(TokKind::At) => {
                i = push_macro_name(&ctx, &mut events, i, diagnostics);
                consumed_name = true;
            }
            // A comma separates names and allows the list to continue onto the
            // next line (a newline right after the comma is skipped).
            Some(TokKind::Comma) => {
                events.push(Event::Tok(i));
                let next = ctx.skip_ws_and_newlines(i + 1);
                push_range(&mut events, i + 1, next);
                i = next;
            }
            // A bare identifier, an operator name, or any other same-line token
            // (parens around an interpolation, intervening whitespace) carried
            // through verbatim.
            _ => {
                let is_ws = ctx.token(i).is_some_and(|t| t.kind.is_trivia());
                events.push(Event::Tok(i));
                i += 1;
                consumed_name = !is_ws;
            }
        }
        // `public` stops once a complete name is not followed by a comma.
        if is_public && consumed_name {
            let sep = ctx.skip_ws(i);
            if !matches!(ctx.token(sep).map(|t| t.kind), Some(TokKind::Comma)) {
                break;
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

    // An `as` rename is valid in an `import` base path (`import A as B`) and in
    // any name list after the top-level `:` (`using A: x as y`), but invalid in a
    // `using` base path (`using A as B` ⇒ `(error (as …))`) and in an `import`
    // base path when a `: names` list follows (`import A as B: x` ⇒ `(error (as
    // …))`). A `using` base alias that *also* has a following `:` stacks both
    // invalidities (`using A as B: x` ⇒ `(error (error (as …)))`). So each
    // alias's error-wrap depth = (in a `using` base) + (a valid `:` follows).
    let is_using = node_kind == SyntaxKind::USING_STMT;

    // A top-level `:` is the base/names split only when it is the *first*
    // separator (directly after the base path). After a comma, any `:` is
    // recovery. Probe the base clause's following separator to learn which.
    let valid_colon = {
        let (mut probe, mut probe_diags) = (Vec::new(), Vec::new());
        let base_end = parse_import_clause(&ctx, &mut probe, start + 1, 0, &mut probe_diags);
        let sep = ctx.skip_ws(base_end);
        ctx.token(sep).map(|t| t.kind) == Some(TokKind::Colon)
    };

    let base_wraps = usize::from(is_using) + usize::from(valid_colon);
    let mut i = parse_import_clause(&ctx, &mut events, start + 1, base_wraps, diagnostics);

    // Comma-separated further clauses, plus an optional single `:` that switches
    // from the base path to the list of imported names. Both separators feed the
    // same clause parser; the projector reads the `:` to group base vs. names.
    let mut valid_colon_consumed = false;
    let mut first_sep = true;
    loop {
        let sep = ctx.skip_ws(i);
        match ctx.token(sep).map(|t| t.kind) {
            Some(kind @ (TokKind::Comma | TokKind::Colon)) => {
                push_range(&mut events, i, sep);
                events.push(Event::Tok(sep));
                let is_valid_colon = kind == TokKind::Colon && first_sep && valid_colon;
                first_sep = false;
                if is_valid_colon {
                    valid_colon_consumed = true;
                    i = parse_import_clause(&ctx, &mut events, sep + 1, 0, diagnostics);
                } else if kind == TokKind::Colon {
                    // A `:` that is not the base/names split is recovery: the
                    // clause after it is wrapped in an `ERROR` node (projected
                    // `(error-t …)` via the recorded diagnostic) and grouping stops.
                    events.push(Event::Start(SyntaxKind::ERROR));
                    i = parse_import_clause(&ctx, &mut events, sep + 1, 0, diagnostics);
                    events.push(Event::Finish);
                    if i > sep + 1 {
                        let last = &ctx.tokens()[i - 1];
                        push_diagnostic(
                            diagnostics,
                            DiagnosticKind::ImportRecoveryColon,
                            "recovery `:` in import",
                            last.start,
                            last.end,
                        );
                    }
                    break;
                } else {
                    let wraps = usize::from(is_using && !valid_colon_consumed);
                    i = parse_import_clause(&ctx, &mut events, sep + 1, wraps, diagnostics);
                }
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
    error_wraps: usize,
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
            // An invalid alias is wrapped in `(error …)` so the projector emits
            // `(error (as …))`; a `using` base alias that also precedes a valid
            // `:` stacks two wraps (`(error (error (as …)))`).
            for _ in 0..error_wraps {
                events.push(Event::Start(SyntaxKind::ERROR));
            }
            events.push(Event::Start(SyntaxKind::IMPORT_ALIAS));
            events.extend(path_events);
            push_range(events, path_end, as_idx);
            events.push(Event::Tok(as_idx)); // `as`
            push_range(events, as_idx + 1, alias_start);
            events.push(Event::Tok(alias_start)); // alias name
            events.push(Event::Finish);
            for _ in 0..error_wraps {
                events.push(Event::Finish);
            }
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
fn push_macro_name(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    at_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    events.push(Event::Start(SyntaxKind::MACRO_NAME));
    events.push(Event::Tok(at_idx)); // `@`
    let mut i = at_idx + 1;
    if let Some(end) = push_var_macro_name(ctx, events, i, diagnostics) {
        // A `var"…"` non-standard identifier name (`export @var"#"`).
        i = end;
    } else if ctx.token(i).map(|t| t.kind) == Some(TokKind::Ident) {
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

    // Leading relative dots: `.`/`..`/`...`, optionally separated by whitespace
    // (`import . .A` ⇒ two leading dots). The separating whitespace is carried
    // into the path verbatim so the projector (which counts dots) stays faithful.
    loop {
        let dot = ctx.skip_ws(i);
        if matches!(
            ctx.token(dot).map(|t| t.kind),
            Some(TokKind::Dot | TokKind::DotDot | TokKind::DotDotDot)
        ) {
            push_range(&mut body, i, dot);
            body.push(Event::Tok(dot));
            i = dot + 1;
        } else {
            break;
        }
    }

    // First name component: an identifier, or a bare operator symbol when the
    // clause's whole path is an operator (`import A: +`, `import .⋆`, `import ⋆`).
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
            i = push_macro_name(ctx, &mut body, i, diagnostics);
        }
        Some(k) if is_op_name(k) || is_unicode_op_name(k) || is_dotted_op_name(k) => {
            // A leading operator name. A fused dotted operator (`import .==`,
            // `import .⋆`) carries a relative-import `.` the projector splits out.
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
                i = push_macro_name(ctx, &mut body, i + 1, diagnostics);
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
            (Some(TokKind::DotDotDot), _) => {
                // A `..` range-operator component (`import A...` → `(importpath A
                // ..)`): the `...` is the separator dot fused with the `..` name.
                body.push(Event::Tok(i));
                i += 1;
            }
            (Some(k), _) if is_dotted_op_name(k) || is_unicode_op_name(k) => {
                // A fused dotted operator component: the separator dot fused to an
                // operator name. ASCII (`import A.==` → `(importpath A ==)`) and
                // single-codepoint unicode (`import A.⋆.f` → `(importpath A ⋆ f)`)
                // both arrive as one token whose leading `.` the projector strips.
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

/// A single-codepoint Unicode operator usable as an import-path name component
/// (`import ⋆`, `import A.⋆.f`). Each lexes as its own token (no leading-dot
/// fusion), so the import path threads it through verbatim like an identifier.
fn is_unicode_op_name(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        UniArrow
            | UniComparison
            | UniColon
            | UniPlus
            | UniTimes
            | UniPower
            | UniAssign
            | UniRadical
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

    let mut i = parse_do_params(ctx, &mut events, do_idx + 1, diagnostics);
    i = run_block(ctx, &mut events, i, END_ONLY, diagnostics);
    i = expect_end(ctx, &mut events, i, do_idx, diagnostics);
    events.push(Event::Finish);
    ExprParse {
        start: lhs.start,
        end: i,
        events,
    }
}

/// Parse the optional argument tuple on a `do` line into a `DO_PARAMS` node.
/// JuliaSyntax parses these as a comma-separated list (`parse_comma_separated`),
/// so the list ends at the first non-comma token — anything after the last arg
/// (`f(x) do y body end` ⇒ args `y`, block `body`) falls through to the block.
/// An empty arg line (`do\n …` / `do; …`) emits no node so the projector heads a
/// bare `(tuple)`. Returns the index after the params.
fn parse_do_params(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    after_kw: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let start = ctx.skip_ws(after_kw);
    push_range(events, after_kw, start);

    if header_ends(ctx, start) {
        return start;
    }

    events.push(Event::Start(SyntaxKind::DO_PARAMS));
    let mut i = match parse_expr(ctx.tokens(), start, 0, diagnostics) {
        Some(expr) => {
            events.extend(expr.events);
            expr.end
        }
        None => start,
    };
    // Continue the list only across commas; the first non-comma ends the args.
    loop {
        let next = ctx.skip_ws(i);
        if ctx.token(next).map(|t| t.kind) != Some(TokKind::Comma) {
            break;
        }
        push_range(events, i, next + 1);
        let arg_start = ctx.skip_ws(next + 1);
        push_range(events, next + 1, arg_start);
        match parse_expr(ctx.tokens(), arg_start, 0, diagnostics) {
            Some(expr) => {
                events.extend(expr.events);
                i = expr.end;
            }
            None => {
                i = arg_start;
                break;
            }
        }
    }
    events.push(Event::Finish);
    i
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
            // Anchor at the opening keyword (`if`/`elseif`/`while`), mirroring
            // `MissingEnd`, so the projector can reconstruct the zero-width
            // `(error)` JuliaSyntax emits in the empty condition slot via
            // `diag_count_from(keyword_start(node), …)`.
            let kw = &ctx.tokens()[after_kw - 1];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingCondition,
                "expected a condition",
                kw.start,
                kw.end,
            );
            cond_start
        }
    }
}

/// Parse a `struct`/`module` signature: a single type/name expression wrapped in
/// a `SIGNATURE` node. Unlike [`parse_header`], it stops right after that
/// expression rather than gobbling the rest of the line, so same-line body
/// statements (`struct A const a end`, `module A x end`) fall through to the
/// block. An empty signature (the keyword's line ends immediately) emits no node.
/// Returns the index after the signature.
fn parse_signature(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    after_kw: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let sig_start = ctx.skip_ws(after_kw);
    push_range(events, after_kw, sig_start);

    if header_ends(ctx, sig_start) {
        return sig_start;
    }

    events.push(Event::Start(SyntaxKind::SIGNATURE));
    let i = if let Some(expr) = parse_expr(ctx.tokens(), sig_start, 0, diagnostics) {
        events.extend(expr.events);
        expr.end
    } else if ctx.token(sig_start).map(|t| t.kind) == Some(TokKind::Dollar) {
        // An interpolated name (`module $A end`): a real `INTERPOLATION` node.
        let interp = parse_prefix_interpolation(ctx, sig_start, diagnostics);
        events.extend(interp.events);
        interp.end
    } else {
        sig_start
    };
    events.push(Event::Finish);
    i
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
    // A `for`-loop binding suppresses the `in`/`isa` word operators so a following
    // `in` is the iteration separator (consumed as a loose token below and split
    // out by the projector), not a comparison. Other headers (`while` conditions,
    // `let` bindings) keep `in`/`isa` as ordinary comparison operators.
    let header_expr = run_expr.then(|| {
        if node_kind == SyntaxKind::FOR_BINDING {
            parse_for_binding(ctx.tokens(), header_start, diagnostics)
        } else {
            parse_expr(ctx.tokens(), header_start, 0, diagnostics)
        }
    });
    if let Some(Some(expr)) = header_expr {
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
/// Whether `kind` is a closing bracket token (`)`, `]`, `}`). Used to detect a
/// stray closer where a keyword's optional value would otherwise begin.
fn is_close_delimiter_tok(kind: TokKind) -> bool {
    matches!(kind, TokKind::RParen | TokKind::RBracket | TokKind::RBrace)
}

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
    let mut parsed_any = false;

    loop {
        // Trivia and `;` statement separators belong to the block. A newline or
        // `;` marks a statement boundary; a following statement with neither
        // between it and the previous one is glued trailing junk, not a new
        // statement.
        let mut saw_separator = false;
        while let Some(k) = tokens.get(i).map(|t| t.kind) {
            if k.is_trivia() || k == TokKind::Semicolon {
                if matches!(k, TokKind::Newline | TokKind::Semicolon) {
                    saw_separator = true;
                }
                events.push(Event::Tok(i));
                i += 1;
            } else {
                break;
            }
        }
        match tokens.get(i).map(|t| t.kind) {
            None => break,
            Some(k) if terminators.contains(&k) => break,
            // A statement glued to the previous one with no `;`/newline between
            // is trailing junk: JuliaSyntax (`parse_Nary`) ends the block here and
            // leaves the remainder for the closing recovery (`expect_end`), which
            // bumps it as flat error tokens up to the closing keyword
            // (`begin x y end` ⇒ `(block x (error-t y))`).
            Some(_) if parsed_any && !saw_separator => break,
            Some(_) => {
                let parsed = parse_block_stmt(tokens, i, public_context, diagnostics);
                if let Some(stmt) = parsed {
                    events.extend(stmt.events);
                    i = stmt.end;
                    parsed_any = true;
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

/// Close a block form: recover any trailing junk, then consume the `end`.
///
/// This mirrors JuliaSyntax's `bump_closing_token`. There are three cases:
///
/// - The `end` is right here — consume it.
/// - A non-closer token is glued before the `end` (the block stopped at
///   separator-less junk) — bump the run as flat error tokens up to the closing
///   keyword (a byte-bearing `ERROR` node the projector renders `(error-t …)`),
///   then consume the `end` if one follows. JuliaSyntax emits *only* this marker,
///   so no missing-`end` marker is added when the run reaches EOF.
/// - Neither — the form was truncated before its `end` (EOF or a foreign closer
///   like `)`); record a zero-width `MissingEnd` diagnostic at the opening keyword
///   (no node, per the rust-analyzer model), which the projector replays as the
///   truncation `(error-t)` (`if c\n x` ⇒ `(if c (block x) (error-t))`).
fn expect_end(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    i: usize,
    open_start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let mut i = i;
    match ctx.token(i).map(|t| t.kind) {
        Some(TokKind::EndKw) => {
            events.push(Event::Tok(i));
            i + 1
        }
        Some(k) if !is_block_junk_stopper(k) => {
            i = collect_block_junk(ctx, events, i, diagnostics);
            if ctx.token(i).map(|t| t.kind) == Some(TokKind::EndKw) {
                events.push(Event::Tok(i));
                i + 1
            } else {
                i
            }
        }
        _ => {
            let kw = &ctx.tokens()[open_start];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingEnd,
                "expected `end`",
                kw.start,
                kw.end,
            );
            i
        }
    }
}

/// Tokens that halt a trailing-junk recovery run: the closing keywords of every
/// block form (`end`, `else`, `elseif`, `catch`, `finally`) and the bracket
/// closers — JuliaSyntax's `is_closing_token` minus `,`/`;` (which a run
/// swallows). EOF also halts the run (the `while let` bound in
/// [`collect_block_junk`]).
fn is_block_junk_stopper(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::EndKw
            | TokKind::ElseKw
            | TokKind::ElseifKw
            | TokKind::CatchKw
            | TokKind::FinallyKw
            | TokKind::RParen
            | TokKind::RBracket
            | TokKind::RBrace
    )
}

/// Bump a trailing-junk run — every token up to (but not including) the next
/// block/bracket closer — into one `ERROR` node, flagged with a `TrailingJunk`
/// diagnostic so the projector renders it `(error-t …)`. The caller guarantees
/// the first token is not a stopper, so the node is never zero-width.
fn collect_block_junk(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    mut i: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let tokens = ctx.tokens();
    let first = i;
    events.push(Event::Start(SyntaxKind::ERROR));
    while let Some(k) = tokens.get(i).map(|t| t.kind) {
        if is_block_junk_stopper(k) {
            break;
        }
        events.push(Event::Tok(i));
        i += 1;
    }
    events.push(Event::Finish);
    push_diagnostic(
        diagnostics,
        DiagnosticKind::TrailingJunk,
        "trailing tokens after statement",
        tokens[first].start,
        tokens[first].end,
    );
    i
}
