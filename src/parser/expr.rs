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

        // A `.` whose right-hand side begins with `@` is a qualified macro call
        // (`Base.@time f()`): the whole `Base.@time` is the macro name and the
        // rest are its arguments — not a field access wrapping a macro call.
        if op_kind == TokKind::Dot
            && ctx.token(ctx.skip_trivia(op_idx + 1)).map(|t| t.kind) == Some(TokKind::At)
        {
            lhs = parse_qualified_macro(&ctx, lhs, op_idx, diagnostics, inside_brackets);
            continue;
        }

        // Assignment (and broadcast assignment `.=`) is right-associative and
        // the loosest operator.
        let (l_bp, r_bp) = if op_kind == TokKind::Eq || op_kind == TokKind::DotEq {
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
            TokKind::Eq | TokKind::DotEq => SyntaxKind::ASSIGNMENT_EXPR,
            TokKind::Arrow => SyntaxKind::ARROW_EXPR,
            TokKind::ColonColon => SyntaxKind::TYPE_ANNOTATION,
            TokKind::WhereKw => SyntaxKind::WHERE_EXPR,
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
        // Prefix operators: arithmetic/logical unary (`-x`, `!x`), lower-bound
        // type expressions (`<:Real` in `Array{<:Real}`), and unary `::`
        // declarations (`::Int` in a method signature `f(::Int)`).
        TokKind::Plus
        | TokKind::Minus
        | TokKind::DotPlus
        | TokKind::DotMinus
        | TokKind::Bang
        | TokKind::Subtype
        | TokKind::Supertype
        | TokKind::ColonColon => {
            let node = if tok.kind == TokKind::ColonColon {
                SyntaxKind::TYPE_ANNOTATION
            } else {
                SyntaxKind::UNARY_EXPR
            };
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
            let mut events = vec![Event::Start(node)];
            push_range(&mut events, start, operand.start);
            events.extend(operand.events);
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: operand.end,
                events,
            })
        }
        TokKind::At => Some(parse_macro(ctx, start, diagnostics, inside_brackets)),
        TokKind::LParen => parse_paren(ctx, start, diagnostics),
        TokKind::LBrace => parse_braces(ctx, start, diagnostics),
        TokKind::Ident => Some(atom(SyntaxKind::NAME, start)),
        TokKind::StringPrefix | TokKind::StringDelimOpen | TokKind::CmdDelimOpen => {
            Some(parse_string_literal(ctx, start, diagnostics))
        }
        TokKind::Integer | TokKind::Float | TokKind::Char | TokKind::TrueKw | TokKind::FalseKw => {
            Some(atom(SyntaxKind::LITERAL, start))
        }
        _ => None,
    }
}

/// Assemble a string (`"..."`) or command (`` `...` ``) literal from its flat
/// token run into a `STRING_LITERAL`/`CMD_LITERAL` node. The run is: an optional
/// prefix, an open delimiter, a sequence of content chunks and interpolations,
/// the close delimiter, and an optional suffix. An unterminated literal (no close
/// delimiter) simply stops early — every consumed token is still emitted.
fn parse_string_literal(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let mut i = start;

    // Optional non-standard literal prefix (`r`, `raw`, …).
    if ctx.token(i).map(|t| t.kind) == Some(TokKind::StringPrefix) {
        i += 1;
    }

    let node = match ctx.token(i).map(|t| t.kind) {
        Some(TokKind::CmdDelimOpen) => SyntaxKind::CMD_LITERAL,
        _ => SyntaxKind::STRING_LITERAL,
    };
    let close_kind = if node == SyntaxKind::CMD_LITERAL {
        TokKind::CmdDelimClose
    } else {
        TokKind::StringDelimClose
    };

    let mut events = vec![Event::Start(node)];
    for idx in start..=i {
        events.push(Event::Tok(idx));
    }
    i += 1; // past the open delimiter

    loop {
        match ctx.token(i).map(|t| t.kind) {
            Some(TokKind::StringContent) => {
                events.push(Event::Tok(i));
                i += 1;
            }
            Some(TokKind::Dollar) => {
                i = parse_interpolation(ctx, &mut events, i, diagnostics);
            }
            Some(k) if k == close_kind => {
                events.push(Event::Tok(i));
                i += 1;
                // Optional suffix flags (`r"pat"ims`).
                if ctx.token(i).map(|t| t.kind) == Some(TokKind::StringSuffix) {
                    events.push(Event::Tok(i));
                    i += 1;
                }
                break;
            }
            // Unterminated: anything else (incl. EOF) ends the literal.
            _ => break,
        }
    }

    events.push(Event::Finish);
    ExprParse {
        start,
        end: i,
        events,
    }
}

/// Parse one `$ident` or `$(expr)` interpolation into an `INTERPOLATION` node,
/// returning the token index just past it. `$(...)` interiors reuse the Pratt
/// parser, so they become real expression subtrees.
fn parse_interpolation(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    dollar: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    events.push(Event::Start(SyntaxKind::INTERPOLATION));
    events.push(Event::Tok(dollar)); // `$`
    let next = dollar + 1;

    match ctx.token(next).map(|t| t.kind) {
        Some(TokKind::LParen) => {
            events.push(Event::Tok(next)); // `(`
            let inner_start = ctx.skip_trivia(next + 1);
            push_range(events, next + 1, inner_start);

            if ctx.token(inner_start).map(|t| t.kind) == Some(TokKind::RParen) {
                events.push(Event::Tok(inner_start)); // empty `$()`
                events.push(Event::Finish);
                return inner_start + 1;
            }

            let Some(inner) = parse_expr_in_brackets(ctx.tokens(), inner_start, 0, diagnostics)
            else {
                events.push(Event::Finish);
                return inner_start;
            };
            events.extend(inner.events);

            let close = ctx.skip_trivia(inner.end);
            if ctx.token(close).map(|t| t.kind) == Some(TokKind::RParen) {
                push_range(events, inner.end, close);
                events.push(Event::Tok(close)); // `)`
                events.push(Event::Finish);
                close + 1
            } else {
                push_range(events, inner.end, close);
                events.push(Event::Finish);
                close
            }
        }
        Some(TokKind::Ident) => {
            events.push(Event::Tok(next));
            events.push(Event::Finish);
            next + 1
        }
        // A lone `$` (the lexer folds non-operand dollars into content, so this
        // is unreachable in practice) — emit just the sigil.
        _ => {
            events.push(Event::Finish);
            next
        }
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

    // Empty `()` is the empty tuple; a leading `;` (`(; a=1)`) is a named-tuple
    // parameter section. Both are tuples, handled by the arg-list machinery.
    match ctx.token(inner_start).map(|t| t.kind) {
        Some(TokKind::RParen) => {
            let mut events = vec![Event::Start(SyntaxKind::TUPLE_EXPR)];
            push_range(&mut events, start, inner_start + 1);
            events.push(Event::Finish);
            return Some(ExprParse {
                start,
                end: inner_start + 1,
                events,
            });
        }
        Some(TokKind::Semicolon) => {
            let (events, end) = parse_arg_list(
                ctx,
                start,
                TokKind::RParen,
                SyntaxKind::TUPLE_EXPR,
                diagnostics,
            );
            return Some(ExprParse { start, end, events });
        }
        _ => {}
    }

    let Some(inner) = parse_expr_in_brackets(ctx.tokens(), inner_start, 0, diagnostics) else {
        return Some(error_expr_with_range(start, inner_start));
    };

    // A `,` or `;` after the first element makes this a tuple (or named tuple).
    // Re-parse the whole parenthesized run as an argument list so each element
    // becomes an `ARG`/`KEYWORD_ARG` and `;` opens a `PARAMETERS` section.
    let sep = ctx.skip_trivia(inner.end);
    if matches!(
        ctx.token(sep).map(|t| t.kind),
        Some(TokKind::Comma | TokKind::Semicolon)
    ) {
        let (events, end) = parse_arg_list(
            ctx,
            start,
            TokKind::RParen,
            SyntaxKind::TUPLE_EXPR,
            diagnostics,
        );
        return Some(ExprParse { start, end, events });
    }

    // Otherwise a single parenthesized expression: `(a)` grouping.
    let mut events = vec![Event::Start(SyntaxKind::PAREN_EXPR)];
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
            // Parametric type application: `Vector{T}`, `Dict{K, V}`.
            Some(TokKind::LBrace) => {
                lhs = parse_postfix(
                    ctx,
                    lhs,
                    next,
                    TokKind::RBrace,
                    SyntaxKind::CURLY_EXPR,
                    diagnostics,
                )
            }
            // Broadcast call `f.(args)`: a `.` whose next non-space token is `(`.
            // (A `.` before an identifier is field access, handled by the infix
            // loop; before `@` it is a qualified macro — neither matches here.)
            Some(TokKind::Dot)
                if ctx.token(ctx.skip_ws(next + 1)).map(|t| t.kind) == Some(TokKind::LParen) =>
            {
                let lparen = ctx.skip_ws(next + 1);
                let (list_events, end) = parse_arg_list(
                    ctx,
                    lparen,
                    TokKind::RParen,
                    SyntaxKind::ARG_LIST,
                    diagnostics,
                );
                let mut events = vec![Event::Start(SyntaxKind::DOT_CALL_EXPR)];
                events.extend(lhs.events);
                push_range(&mut events, lhs.end, next);
                events.push(Event::Tok(next)); // `.`
                push_range(&mut events, next + 1, lparen);
                events.extend(list_events);
                events.push(Event::Finish);
                lhs = ExprParse {
                    start: lhs.start,
                    end,
                    events,
                };
            }
            // Splat/vararg `x...` is postfix and terminal: wrap and re-loop (the
            // next pass finds nothing more to chain).
            Some(TokKind::DotDotDot) => {
                let mut events = vec![Event::Start(SyntaxKind::SPLAT_EXPR)];
                events.extend(lhs.events);
                push_range(&mut events, lhs.end, next);
                events.push(Event::Tok(next));
                events.push(Event::Finish);
                lhs = ExprParse {
                    start: lhs.start,
                    end: next + 1,
                    events,
                };
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
    let (list_events, end) =
        parse_arg_list(ctx, open_idx, close, SyntaxKind::ARG_LIST, diagnostics);
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

/// Parse a macro call introduced by a leading `@` (`@m`, `@m(a, b)`, `@m a b`,
/// `@.`, `@Mod.mac x`) into a `MACRO_CALL` wrapping a `MACRO_NAME` and the
/// arguments. The `@` sits at `at_idx`.
fn parse_macro(
    ctx: &ParserCtx<'_>,
    at_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    inside_brackets: bool,
) -> ExprParse {
    let mut events = vec![Event::Start(SyntaxKind::MACRO_CALL)];
    events.push(Event::Start(SyntaxKind::MACRO_NAME));
    events.push(Event::Tok(at_idx)); // `@`
    let name_end = parse_macro_name_body(ctx, &mut events, at_idx + 1);
    events.push(Event::Finish); // close MACRO_NAME

    let end = parse_macro_args(ctx, &mut events, name_end, diagnostics, inside_brackets);
    events.push(Event::Finish); // close MACRO_CALL
    ExprParse {
        start: at_idx,
        end,
        events,
    }
}

/// Parse a qualified macro call `lhs.@mac args` (`Base.@time f()`). `lhs` is the
/// already-parsed module path; `dot_idx` is the `.` before the `@`. The
/// `MACRO_NAME` spans `lhs`, the `.`, the `@`, and the macro name body.
fn parse_qualified_macro(
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    dot_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    inside_brackets: bool,
) -> ExprParse {
    let mut events = vec![Event::Start(SyntaxKind::MACRO_CALL)];
    events.push(Event::Start(SyntaxKind::MACRO_NAME));
    events.extend(lhs.events);
    push_range(&mut events, lhs.end, dot_idx);
    events.push(Event::Tok(dot_idx)); // `.`
    let at_idx = ctx.skip_trivia(dot_idx + 1);
    push_range(&mut events, dot_idx + 1, at_idx);
    events.push(Event::Tok(at_idx)); // `@`
    let name_end = parse_macro_name_body(ctx, &mut events, at_idx + 1);
    events.push(Event::Finish); // close MACRO_NAME

    let end = parse_macro_args(ctx, &mut events, name_end, diagnostics, inside_brackets);
    events.push(Event::Finish); // close MACRO_CALL
    ExprParse {
        start: lhs.start,
        end,
        events,
    }
}

/// Emit the tokens of a macro name following the `@` sigil, starting at `start`:
/// either a lone `.` (the broadcast macro `@.`) or an identifier followed by a
/// trailing adjacent `.ident` chain (`@Mod.mac`). Returns the index just past
/// the name.
fn parse_macro_name_body(ctx: &ParserCtx<'_>, events: &mut Vec<Event>, start: usize) -> usize {
    match ctx.token(start).map(|t| t.kind) {
        // The broadcast macro `@.` — the name is the single `.` token.
        Some(TokKind::Dot) => {
            events.push(Event::Tok(start));
            start + 1
        }
        Some(TokKind::Ident) => {
            events.push(Event::Tok(start));
            let mut i = start + 1;
            // Adjacent `.ident` chain: `@Mod.mac`. No whitespace skipping — a
            // space before a `.` makes it a (broadcast) argument, not the name.
            while ctx.token(i).map(|t| t.kind) == Some(TokKind::Dot)
                && ctx.token(i + 1).map(|t| t.kind) == Some(TokKind::Ident)
            {
                events.push(Event::Tok(i)); // `.`
                events.push(Event::Tok(i + 1)); // ident
                i += 2;
            }
            i
        }
        // A bare `@` with no name — emit nothing more; the MACRO_NAME holds just
        // the sigil (still lossless).
        _ => start,
    }
}

/// Parse the arguments of a macro call after its name (which ends at `name_end`)
/// into `events`, returning the index just past the last argument. Two forms: a
/// `(` adjacent to the name opens a comma-separated `ARG_LIST` (call-like);
/// otherwise the arguments are space-separated expressions consumed to the end
/// of the line (or until a closing delimiter / separator inside brackets).
fn parse_macro_args(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    name_end: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    inside_brackets: bool,
) -> usize {
    // Paren form `@m(a, b)`: the `(` must be adjacent (no whitespace), otherwise
    // `@m (a, b)` is the space form with a single parenthesized argument.
    if ctx.token(name_end).map(|t| t.kind) == Some(TokKind::LParen) {
        let (list_events, end) = parse_arg_list(
            ctx,
            name_end,
            TokKind::RParen,
            SyntaxKind::ARG_LIST,
            diagnostics,
        );
        events.extend(list_events);
        return end;
    }

    // Space form `@m a b`: each argument is a full expression. Stop at a newline,
    // end of input, or a delimiter that closes/separates an enclosing list.
    let mut pos = name_end;
    loop {
        let next = ctx.skip_ws(pos);
        match ctx.token(next).map(|t| t.kind) {
            None
            | Some(
                TokKind::Newline
                | TokKind::Comma
                | TokKind::RParen
                | TokKind::RBracket
                | TokKind::RBrace
                | TokKind::Semicolon,
            ) => break,
            _ => {
                push_range(events, pos, next);
                match parse_expr_in(ctx.tokens(), next, 0, diagnostics, inside_brackets) {
                    Some(arg) => {
                        events.extend(arg.events);
                        pos = arg.end;
                    }
                    None => break,
                }
            }
        }
    }
    pos
}

/// Parse a standalone `{ … }` brace expression into a `BRACES` node — the
/// type-variable list of a bare `where {T, S}`. Like `PAREN_EXPR`, the braces
/// node directly holds its comma-separated items (no wrapping `ARG_LIST`).
fn parse_braces(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let (events, end) =
        parse_arg_list(ctx, start, TokKind::RBrace, SyntaxKind::BRACES, diagnostics);
    Some(ExprParse { start, end, events })
}

/// Parse a comma-separated, bracket-delimited argument list into a `list_kind`
/// node (`ARG_LIST` for calls/indices/curlies, `BRACES` for bare braces). Each
/// positional argument is wrapped in an `ARG`, each `name = value` in a
/// `KEYWORD_ARG`. A `;` opens a `PARAMETERS` node that holds the remaining
/// keyword parameters. Returns the events and the index just past the closing
/// bracket.
fn parse_arg_list(
    ctx: &ParserCtx<'_>,
    open_idx: usize,
    close: TokKind,
    list_kind: SyntaxKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> (Vec<Event>, usize) {
    let tokens = ctx.tokens();
    let mut events = vec![Event::Start(list_kind), Event::Tok(open_idx)];
    let mut i = open_idx + 1;
    let mut in_params = false;

    loop {
        // Interior trivia belongs to the current container (the list, or the
        // `PARAMETERS` section once a `;` has opened one).
        while matches!(tokens.get(i).map(|t| t.kind), Some(k) if k.is_trivia()) {
            events.push(Event::Tok(i));
            i += 1;
        }
        match tokens.get(i).map(|t| t.kind) {
            None => break, // unterminated list; still lossless
            Some(k) if k == close => {
                if in_params {
                    events.push(Event::Finish); // close PARAMETERS
                    in_params = false;
                }
                events.push(Event::Tok(i));
                i += 1;
                break;
            }
            Some(TokKind::Comma) => {
                events.push(Event::Tok(i));
                i += 1;
            }
            // `;` splits positional arguments from keyword parameters; the first
            // one opens a `PARAMETERS` node holding the rest of the list.
            Some(TokKind::Semicolon) => {
                if !in_params {
                    events.push(Event::Start(SyntaxKind::PARAMETERS));
                    in_params = true;
                }
                events.push(Event::Tok(i));
                i += 1;
            }
            Some(_) => i = parse_one_arg(ctx, &mut events, i, diagnostics),
        }
    }

    if in_params {
        events.push(Event::Finish); // close PARAMETERS on an unterminated list
    }
    events.push(Event::Finish); // close the list node
    (events, i)
}

/// Parse one argument starting at `i` into `events`, as a `KEYWORD_ARG`
/// (`name = value`) when it is a keyword argument and an `ARG` otherwise.
/// Returns the index just past the argument.
fn parse_one_arg(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    i: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let tokens = ctx.tokens();
    if let Some(eq_idx) = kwarg_eq(ctx, i) {
        events.push(Event::Start(SyntaxKind::KEYWORD_ARG));
        events.push(Event::Start(SyntaxKind::NAME));
        events.push(Event::Tok(i));
        events.push(Event::Finish);
        // Whitespace + `=` between the name and the value.
        push_range(events, i + 1, eq_idx);
        events.push(Event::Tok(eq_idx));
        let val_start = ctx.skip_trivia(eq_idx + 1);
        push_range(events, eq_idx + 1, val_start);
        let end = match parse_expr_in_brackets(tokens, val_start, 0, diagnostics) {
            Some(val) => {
                events.extend(val.events);
                val.end
            }
            None => val_start,
        };
        events.push(Event::Finish);
        end
    } else if let Some(arg) = parse_expr_in_brackets(tokens, i, 0, diagnostics) {
        events.push(Event::Start(SyntaxKind::ARG));
        events.extend(arg.events);
        events.push(Event::Finish);
        arg.end
    } else {
        events.push(Event::Tok(i));
        i + 1
    }
}

/// If the argument at `i` is a keyword argument (`name = value` — a bare
/// identifier followed on the same line by a single `=`, not `==`), return the
/// `=` token's index.
fn kwarg_eq(ctx: &ParserCtx<'_>, i: usize) -> Option<usize> {
    if ctx.token(i).map(|t| t.kind) != Some(TokKind::Ident) {
        return None;
    }
    let eq = ctx.skip_ws(i + 1);
    (ctx.token(eq).map(|t| t.kind) == Some(TokKind::Eq)).then_some(eq)
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
    matches!(kind, TokKind::Eq | TokKind::DotEq) || infix_binding_power(kind).is_some()
}

/// `(left_bp, right_bp)` for binary operators. A right-associative operator has
/// `right_bp < left_bp` (e.g. `^`); a left-associative one has `right_bp =
/// left_bp + 1`.
fn infix_binding_power(kind: TokKind) -> Option<(u8, u8)> {
    Some(match kind {
        TokKind::Arrow => (4, 3),
        TokKind::OrOr => (5, 6),
        TokKind::AndAnd => (7, 8),
        // `where` sits below the comparison tier so its right-hand side captures
        // a `<:`/`>:` bound (`A where T<:Real` → `A where (T<:Real)`), and above
        // `->`/`=` so `f(x)::T where U` groups as `((f(x)::T) where U)`.
        TokKind::WhereKw => (8, 9),
        TokKind::EqEq
        | TokKind::NotEq
        | TokKind::Lt
        | TokKind::Le
        | TokKind::Gt
        | TokKind::Ge
        | TokKind::Subtype
        | TokKind::Supertype
        | TokKind::DotEqEq
        | TokKind::DotNotEq
        | TokKind::DotLt
        | TokKind::DotLe
        | TokKind::DotGt
        | TokKind::DotGe => (10, 11),
        TokKind::PipeGt => (12, 13),
        TokKind::Colon => (14, 15),
        TokKind::Plus | TokKind::Minus | TokKind::DotPlus | TokKind::DotMinus => (20, 21),
        TokKind::Star
        | TokKind::Slash
        | TokKind::Percent
        | TokKind::DotStar
        | TokKind::DotSlash
        | TokKind::DotPercent => (24, 25),
        TokKind::Caret | TokKind::DotCaret => (32, 31),
        TokKind::ColonColon => (36, 37),
        TokKind::Dot => (40, 41),
        _ => return None,
    })
}
