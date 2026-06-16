//! Pratt (precedence-climbing) expression parser plus postfix call/index chains.
//!
//! `parse_expr` parses one expression starting at a **non-trivia** token; the
//! caller is responsible for emitting any leading trivia. Every token the
//! expression covers (operators and interior trivia included) is emitted into
//! the event stream, so the parser preserves losslessness.

use crate::parser::context::ParserCtx;
use crate::parser::diagnostics::{ParseDiagnostic, push_diagnostic};
use crate::parser::events::{Event, ExprParse, push_range};
use crate::parser::lexer::{TokKind, Token};
use crate::parser::recovery::{error_expr_to_line_end, error_expr_with_range};
use crate::parser::structural::{
    KwStmt, parse_begin_expr, parse_do_block, parse_for_expr, parse_function_expr, parse_if_expr,
    parse_keyword_stmt, parse_let_expr, parse_module_expr, parse_quote_expr, parse_struct_expr,
    parse_try_expr, parse_while_expr,
};
use crate::syntax::SyntaxKind;

/// Binding power for prefix unary operators (`+x`, `-x`, `!x`). Higher than the
/// binary arithmetic operators so `-a + b` parses as `(-a) + b`.
const PREFIX_BP: u8 = 28;

/// Parse one expression at statement scope (a newline after a complete operand
/// terminates it).
pub(crate) fn parse_expr(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    parse_expr_in(tokens, start, min_bp, diagnostics, false)
}

/// Parse one expression inside brackets (`(...)`, `[...]`), where newlines are
/// insignificant and an operator may continue onto the next line.
pub(crate) fn parse_expr_in_brackets(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    parse_expr_in(tokens, start, min_bp, diagnostics, true)
}

fn parse_expr_in(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
    inside_brackets: bool,
) -> Option<ExprParse> {
    let ctx = ParserCtx::new(tokens);

    // Leading keywords open a structural (block) form.
    match ctx.token(start).map(|t| t.kind) {
        Some(TokKind::IfKw) => return parse_if_expr(tokens, start, diagnostics),
        Some(TokKind::FunctionKw) => return parse_function_expr(tokens, start, diagnostics),
        Some(TokKind::BeginKw) => return parse_begin_expr(tokens, start, diagnostics),
        Some(TokKind::QuoteKw) => return parse_quote_expr(tokens, start, diagnostics),
        Some(TokKind::WhileKw) => return parse_while_expr(tokens, start, diagnostics),
        Some(TokKind::ForKw) => return parse_for_expr(tokens, start, diagnostics),
        Some(TokKind::LetKw) => return parse_let_expr(tokens, start, diagnostics),
        Some(TokKind::TryKw) => return parse_try_expr(tokens, start, diagnostics),
        Some(TokKind::StructKw | TokKind::MutableKw) => {
            return parse_struct_expr(tokens, start, diagnostics);
        }
        Some(TokKind::ModuleKw | TokKind::BaremoduleKw) => {
            return parse_module_expr(tokens, start, diagnostics);
        }
        Some(TokKind::ReturnKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::RETURN_EXPR,
                KwStmt::Expr,
                diagnostics,
            );
        }
        Some(TokKind::BreakKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::BREAK_EXPR,
                KwStmt::Bare,
                diagnostics,
            );
        }
        Some(TokKind::ContinueKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::CONTINUE_EXPR,
                KwStmt::Bare,
                diagnostics,
            );
        }
        Some(TokKind::ConstKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::CONST_STMT,
                KwStmt::Expr,
                diagnostics,
            );
        }
        Some(TokKind::GlobalKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::GLOBAL_STMT,
                KwStmt::Expr,
                diagnostics,
            );
        }
        Some(TokKind::LocalKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::LOCAL_STMT,
                KwStmt::Expr,
                diagnostics,
            );
        }
        Some(TokKind::ImportKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::IMPORT_STMT,
                KwStmt::Path,
                diagnostics,
            );
        }
        Some(TokKind::UsingKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::USING_STMT,
                KwStmt::Path,
                diagnostics,
            );
        }
        Some(TokKind::ExportKw) => {
            return parse_keyword_stmt(
                tokens,
                start,
                SyntaxKind::EXPORT_STMT,
                KwStmt::Path,
                diagnostics,
            );
        }
        _ => {}
    }

    let mut lhs = parse_prefix(&ctx, start, diagnostics, inside_brackets)?;

    loop {
        lhs = parse_postfix_chain(&ctx, lhs, diagnostics);

        let Some((op_idx, op_kind)) = next_operator(&ctx, lhs.end, inside_brackets) else {
            break;
        };

        // Assignment is right-associative and the loosest operator.
        let (l_bp, r_bp) = if op_kind == TokKind::Eq {
            (2, 1)
        } else {
            match infix_binding_power(op_kind) {
                Some(bp) => bp,
                None => break,
            }
        };
        if l_bp < min_bp {
            break;
        }

        let rhs_operand = ctx.skip_trivia(op_idx + 1);
        let Some(rhs) = parse_expr_in(tokens, rhs_operand, r_bp, diagnostics, inside_brackets)
        else {
            let op = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                "expected right-hand side for operator",
                op.start,
                op.end,
            );
            return Some(error_expr_to_line_end(tokens, lhs.start, op_idx + 1));
        };

        let node = match op_kind {
            TokKind::Eq => SyntaxKind::ASSIGNMENT_EXPR,
            TokKind::Arrow => SyntaxKind::ARROW_EXPR,
            _ => SyntaxKind::BINARY_EXPR,
        };
        lhs = build_binary(node, lhs, rhs);
    }

    Some(lhs)
}

/// Build a binary/assignment node from `lhs`, the gap (whitespace + operator +
/// trivia) up to `rhs`, and `rhs`.
fn build_binary(kind: SyntaxKind, lhs: ExprParse, rhs: ExprParse) -> ExprParse {
    let mut events = vec![Event::Start(kind)];
    events.extend(lhs.events);
    push_range(&mut events, lhs.end, rhs.start);
    events.extend(rhs.events);
    events.push(Event::Finish);
    ExprParse {
        start: lhs.start,
        end: rhs.end,
        events,
    }
}

fn parse_prefix(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    inside_brackets: bool,
) -> Option<ExprParse> {
    let tok = ctx.token(start)?;
    match tok.kind {
        TokKind::Plus | TokKind::Minus | TokKind::Bang => {
            let operand_start = ctx.skip_trivia(start + 1);
            let Some(operand) = parse_expr_in(
                ctx.tokens(),
                operand_start,
                PREFIX_BP,
                diagnostics,
                inside_brackets,
            ) else {
                // A bare prefix operator with no operand: wrap it as an error.
                return Some(error_expr_with_range(start, start + 1));
            };
            let mut events = vec![Event::Start(SyntaxKind::UNARY_EXPR)];
            push_range(&mut events, start, operand.start);
            events.extend(operand.events);
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: operand.end,
                events,
            })
        }
        TokKind::LParen => parse_paren(ctx, start, diagnostics),
        TokKind::Ident => Some(atom(SyntaxKind::NAME, start)),
        TokKind::Integer
        | TokKind::Float
        | TokKind::String
        | TokKind::Char
        | TokKind::TrueKw
        | TokKind::FalseKw => Some(atom(SyntaxKind::LITERAL, start)),
        _ => None,
    }
}

/// A single-token atom wrapped in `kind` (`NAME` or `LITERAL`).
fn atom(kind: SyntaxKind, idx: usize) -> ExprParse {
    ExprParse {
        start: idx,
        end: idx + 1,
        events: vec![Event::Start(kind), Event::Tok(idx), Event::Finish],
    }
}

fn parse_paren(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let inner_start = ctx.skip_trivia(start + 1);
    let mut events = vec![Event::Start(SyntaxKind::PAREN_EXPR)];

    // Empty parens `()`.
    if ctx.token(inner_start).map(|t| t.kind) == Some(TokKind::RParen) {
        push_range(&mut events, start, inner_start + 1);
        events.push(Event::Finish);
        return Some(ExprParse {
            start,
            end: inner_start + 1,
            events,
        });
    }

    let Some(inner) = parse_expr_in_brackets(ctx.tokens(), inner_start, 0, diagnostics) else {
        return Some(error_expr_with_range(start, inner_start));
    };
    push_range(&mut events, start, inner.start);
    events.extend(inner.events);

    let close = ctx.skip_trivia(inner.end);
    if ctx.token(close).map(|t| t.kind) == Some(TokKind::RParen) {
        push_range(&mut events, inner.end, close + 1);
        events.push(Event::Finish);
        Some(ExprParse {
            start,
            end: close + 1,
            events,
        })
    } else {
        let open = &ctx.tokens()[start];
        push_diagnostic(diagnostics, "unclosed `(`", open.start, open.end);
        push_range(&mut events, inner.end, close);
        events.push(Event::Finish);
        Some(ExprParse {
            start,
            end: close,
            events,
        })
    }
}

fn parse_postfix_chain(
    ctx: &ParserCtx<'_>,
    mut lhs: ExprParse,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    loop {
        // No newline between the callee and `(`/`[` — only horizontal space.
        let next = ctx.skip_ws(lhs.end);
        match ctx.token(next).map(|t| t.kind) {
            Some(TokKind::LParen) => {
                lhs = parse_postfix(
                    ctx,
                    lhs,
                    next,
                    TokKind::RParen,
                    SyntaxKind::CALL_EXPR,
                    diagnostics,
                )
            }
            Some(TokKind::LBracket) => {
                lhs = parse_postfix(
                    ctx,
                    lhs,
                    next,
                    TokKind::RBracket,
                    SyntaxKind::INDEX_EXPR,
                    diagnostics,
                )
            }
            _ => break,
        }
    }

    // A `do` block can follow a call on the same line: `f(x) do y … end`. It is
    // terminal in the postfix chain — to call its result you parenthesize.
    let next = ctx.skip_ws(lhs.end);
    if ctx.token(next).map(|t| t.kind) == Some(TokKind::DoKw) {
        lhs = parse_do_block(ctx, lhs, next, diagnostics);
    }
    lhs
}

/// Parse a `(...)` call or `[...]` index suffix into `node` wrapping `lhs` and a
/// delimited `ARG_LIST`.
fn parse_postfix(
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    open_idx: usize,
    close: TokKind,
    node: SyntaxKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let (list_events, end) = parse_arg_list(ctx, open_idx, close, diagnostics);
    let mut events = vec![Event::Start(node)];
    events.extend(lhs.events);
    push_range(&mut events, lhs.end, open_idx);
    events.extend(list_events);
    events.push(Event::Finish);
    ExprParse {
        start: lhs.start,
        end,
        events,
    }
}

/// Parse a comma-separated, bracket-delimited argument list into an `ARG_LIST`
/// node. Each argument is wrapped in an `ARG`. Returns the events and the index
/// just past the closing bracket.
fn parse_arg_list(
    ctx: &ParserCtx<'_>,
    open_idx: usize,
    close: TokKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> (Vec<Event>, usize) {
    let tokens = ctx.tokens();
    let mut events = vec![Event::Start(SyntaxKind::ARG_LIST), Event::Tok(open_idx)];
    let mut i = open_idx + 1;

    loop {
        // Interior trivia belongs to the arg list, not to any argument.
        while matches!(tokens.get(i).map(|t| t.kind), Some(k) if k.is_trivia()) {
            events.push(Event::Tok(i));
            i += 1;
        }
        match tokens.get(i).map(|t| t.kind) {
            None => break, // unterminated list; still lossless
            Some(k) if k == close => {
                events.push(Event::Tok(i));
                i += 1;
                break;
            }
            Some(TokKind::Comma) => {
                events.push(Event::Tok(i));
                i += 1;
            }
            Some(_) => {
                if let Some(arg) = parse_expr_in_brackets(tokens, i, 0, diagnostics) {
                    events.push(Event::Start(SyntaxKind::ARG));
                    events.extend(arg.events);
                    events.push(Event::Finish);
                    i = arg.end;
                } else {
                    events.push(Event::Tok(i));
                    i += 1;
                }
            }
        }
    }

    events.push(Event::Finish);
    (events, i)
}

/// Find the next infix/assignment operator after `from`, honoring newline
/// sensitivity. Returns its token index and kind.
fn next_operator(
    ctx: &ParserCtx<'_>,
    from: usize,
    inside_brackets: bool,
) -> Option<(usize, TokKind)> {
    let op_idx = ctx.skip_ws(from);
    let op = ctx.token(op_idx)?;
    if op.kind == TokKind::Newline {
        if !inside_brackets {
            return None;
        }
        let next_idx = ctx.skip_ws_and_newlines(from);
        let next = ctx.token(next_idx)?;
        return is_operator(next.kind).then_some((next_idx, next.kind));
    }
    is_operator(op.kind).then_some((op_idx, op.kind))
}

fn is_operator(kind: TokKind) -> bool {
    kind == TokKind::Eq || infix_binding_power(kind).is_some()
}

/// `(left_bp, right_bp)` for binary operators. A right-associative operator has
/// `right_bp < left_bp` (e.g. `^`); a left-associative one has `right_bp =
/// left_bp + 1`.
fn infix_binding_power(kind: TokKind) -> Option<(u8, u8)> {
    Some(match kind {
        TokKind::Arrow => (4, 3),
        TokKind::OrOr => (5, 6),
        TokKind::AndAnd => (7, 8),
        TokKind::EqEq | TokKind::NotEq | TokKind::Lt | TokKind::Le | TokKind::Gt | TokKind::Ge => {
            (10, 11)
        }
        TokKind::PipeGt => (12, 13),
        TokKind::Colon => (14, 15),
        TokKind::Plus | TokKind::Minus => (20, 21),
        TokKind::Star | TokKind::Slash | TokKind::Percent => (24, 25),
        TokKind::Caret => (32, 31),
        TokKind::ColonColon => (36, 37),
        TokKind::Dot => (40, 41),
        _ => return None,
    })
}
