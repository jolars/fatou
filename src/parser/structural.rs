//! Recursive-descent parsing for Julia's block forms: `if/elseif/else … end`,
//! `function … end`, and `begin … end`. Each keyword opens a node, parses its
//! clauses and a statement block, and closes on `end`.

use crate::parser::context::ParserCtx;
use crate::parser::diagnostics::{ParseDiagnostic, push_diagnostic};
use crate::parser::events::{Event, ExprParse, push_range};
use crate::parser::expr::parse_expr;
use crate::parser::lexer::{TokKind, Token};
use crate::syntax::SyntaxKind;

/// Keywords that terminate a statement block.
const IF_TERMINATORS: &[TokKind] = &[TokKind::EndKw, TokKind::ElseifKw, TokKind::ElseKw];
const END_ONLY: &[TokKind] = &[TokKind::EndKw];

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
    let ctx = ParserCtx::new(tokens);
    let mut events = vec![Event::Start(SyntaxKind::BEGIN_EXPR), Event::Tok(start)];

    let mut i = run_block(&ctx, &mut events, start + 1, END_ONLY, diagnostics);
    i = expect_end(&ctx, &mut events, i, start, diagnostics);
    events.push(Event::Finish);
    Some(ExprParse {
        start,
        end: i,
        events,
    })
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
