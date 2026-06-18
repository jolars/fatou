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
    KwStmt, is_op_name, parse_begin_expr, parse_do_block, parse_for_expr, parse_function_expr,
    parse_if_expr, parse_import_stmt, parse_keyword_stmt, parse_let_expr, parse_macro_def,
    parse_module_expr, parse_quote_expr, parse_struct_expr, parse_try_expr, parse_while_expr,
};
use crate::syntax::SyntaxKind;

/// Context flags threaded through the Pratt parser. All default to `false`, the
/// statement-scope context; bracketed, array, ternary, and indexing contexts flip
/// the relevant ones as they recurse.
#[derive(Clone, Copy, Default)]
struct ExprFlags {
    /// Inside `(…)`/`[…]`/`{…}`: newlines are insignificant and an operator may
    /// continue onto the next line (see [`next_operator`]).
    inside_brackets: bool,
    /// A bare `:` terminates the expression (a ternary true-branch separator)
    /// rather than being parsed as a range operator.
    no_range: bool,
    /// Parsing one element of an array literal: an operator with whitespace before
    /// it but none after begins a new element, so `[1 +2]` is two elements while
    /// `[1 + 2]` is one (see [`array_element_boundary`]).
    array_mode: bool,
    /// A bare `end` is the index-end marker (an `END_MARKER` atom) rather than a
    /// block terminator. Enabled only inside square brackets (`a[end]`, `[end]`);
    /// parens and braces leave it off, matching Julia's `end`-symbol scope.
    end_marker: bool,
    /// A bare `begin` is the index-begin marker (a `BEGIN_MARKER` atom) rather
    /// than a block opener. Enabled only inside an *indexing* `a[…]` (not vector
    /// literals, where `[begin … end]` is a block), matching Julia: `begin` is a
    /// first-index marker only in `ref` position.
    begin_marker: bool,
    /// At toplevel or module-block statement position, where the contextual
    /// keyword `public` opens a `PUBLIC_STMT`. Off everywhere else (so `public`
    /// stays a plain identifier in sub-expressions and non-module blocks),
    /// matching Julia, which only parses `public` as a keyword at file/module
    /// level.
    public_context: bool,
}

/// Binding power for prefix unary operators (`+x`, `-x`, `!x`). Higher than the
/// binary arithmetic operators so `-a + b` parses as `(-a) + b`.
const PREFIX_BP: u8 = 28;

/// Binding powers for the ternary `? :`. Right-associative (`l == r`) and just
/// above assignment (`Eq` at `(2, 1)`), so a whole ternary can be an
/// assignment's right-hand side while keeping `=` out of an unparenthesized
/// branch. Both branches parse at `TERNARY_R`, capturing everything tighter than
/// the ternary (including `||`/`&&`/comparisons) and nesting right-associatively.
const TERNARY_L: u8 = 3;
const TERNARY_R: u8 = 3;

/// Parse one expression at statement scope (a newline after a complete operand
/// terminates it).
pub(crate) fn parse_expr(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    parse_expr_in(tokens, start, min_bp, diagnostics, ExprFlags::default())
}

/// Parse one statement at toplevel or module-block scope, where the contextual
/// keyword `public` opens a `PUBLIC_STMT`. Identical to [`parse_expr`] otherwise.
pub(crate) fn parse_stmt(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        public_context: true,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, 0, diagnostics, flags)
}

/// Parse one expression inside brackets (`(...)`, `[...]`), where newlines are
/// insignificant and an operator may continue onto the next line. Note: this does
/// *not* enable the `end` index marker — that is specific to square brackets and
/// is threaded separately (see [`ExprFlags::end_marker`]).
pub(crate) fn parse_expr_in_brackets(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        inside_brackets: true,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, min_bp, diagnostics, flags)
}

fn parse_expr_in(
    tokens: &[Token],
    start: usize,
    min_bp: u8,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> Option<ExprParse> {
    // `end_marker` is consumed by `parse_prefix` (via `flags`); the rest steer the
    // operator loop directly.
    let ExprFlags {
        inside_brackets,
        no_range,
        array_mode,
        end_marker: _,
        begin_marker,
        public_context,
    } = flags;
    let ctx = ParserCtx::new(tokens);

    // The contextual keyword `public` (a plain identifier elsewhere) opens a
    // `PUBLIC_STMT` at toplevel/module-block statement position, *unless* the next
    // significant token is `(`, `=`, or `[` — those keep `public` an identifier
    // (a call `public(x)`, an assignment `public = 1`, an index `public[i]`),
    // matching JuliaSyntax's `parse_public` compatibility shim.
    if public_context && is_public_keyword(&ctx, start) {
        return parse_keyword_stmt(
            tokens,
            start,
            SyntaxKind::PUBLIC_STMT,
            KwStmt::Path,
            diagnostics,
        );
    }

    // Leading keywords open a structural (block) form. Inside an indexing `a[…]`
    // a leading `begin` is instead the index-begin marker (handled in
    // `parse_prefix`), so skip the block dispatch there.
    match ctx.token(start).map(|t| t.kind) {
        Some(TokKind::IfKw) => return parse_if_expr(tokens, start, diagnostics),
        Some(TokKind::FunctionKw) => return parse_function_expr(tokens, start, diagnostics),
        Some(TokKind::MacroKw) => return parse_macro_def(tokens, start, diagnostics),
        Some(TokKind::BeginKw) if !begin_marker => {
            return parse_begin_expr(tokens, start, diagnostics);
        }
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
            return parse_import_stmt(tokens, start, SyntaxKind::IMPORT_STMT, diagnostics);
        }
        Some(TokKind::UsingKw) => {
            return parse_import_stmt(tokens, start, SyntaxKind::USING_STMT, diagnostics);
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

    let mut lhs = parse_prefix(&ctx, start, diagnostics, flags)?;

    loop {
        lhs = parse_postfix_chain(&ctx, lhs, diagnostics);

        let Some((op_idx, op_kind)) = next_operator(&ctx, lhs.end, inside_brackets) else {
            break;
        };

        // Inside an array literal, an operator glued to the start of the next
        // operand (space before, none after) is that operand's prefix, marking a
        // new element rather than an infix operator. End this element here.
        if array_mode && array_element_boundary(&ctx, lhs.end, op_idx) {
            break;
        }

        // A `.` whose right-hand side begins with `@` is a qualified macro call
        // (`Base.@time f()`): the whole `Base.@time` is the macro name and the
        // rest are its arguments — not a field access wrapping a macro call.
        if op_kind == TokKind::Dot
            && ctx.token(ctx.skip_trivia(op_idx + 1)).map(|t| t.kind) == Some(TokKind::At)
        {
            lhs = parse_qualified_macro(&ctx, lhs, op_idx, diagnostics, inside_brackets);
            continue;
        }

        // In a ternary true-branch a bare `:` is the separator, not a range.
        if no_range && op_kind == TokKind::Colon {
            break;
        }

        // Ternary `cond ? then : else` — right-associative, just above
        // assignment and below `||`. Handled specially (like assignment) so the
        // `:` separator is consumed here rather than parsed as a range operator.
        if op_kind == TokKind::Question {
            if TERNARY_L < min_bp {
                break;
            }
            lhs = match parse_ternary(&ctx, lhs, op_idx, diagnostics, flags) {
                Ok(node) => node,
                Err(done) => return Some(done),
            };
            continue;
        }

        // Assignment (`=`, `.=`, and augmented `+=`/`.+=`/…) is right-associative
        // and the loosest operator.
        let (l_bp, r_bp) = if is_assignment_op(op_kind) {
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
        let Some(rhs) = parse_expr_in(tokens, rhs_operand, r_bp, diagnostics, flags) else {
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
            k if is_assignment_op(k) => SyntaxKind::ASSIGNMENT_EXPR,
            TokKind::Arrow => SyntaxKind::ARROW_EXPR,
            TokKind::ColonColon => SyntaxKind::TYPE_ANNOTATION,
            TokKind::WhereKw => SyntaxKind::WHERE_EXPR,
            _ => SyntaxKind::BINARY_EXPR,
        };
        lhs = build_binary(node, lhs, rhs);
    }

    Some(lhs)
}

/// Whether the identifier `public` at `start` opens a `PUBLIC_STMT`. True when
/// the token is the identifier `public` and the next significant token exists and
/// is not `(`, `=`, or `[` — those three keep `public` an ordinary identifier (a
/// call, assignment, or index), matching JuliaSyntax's `parse_public`.
fn is_public_keyword(ctx: &ParserCtx<'_>, start: usize) -> bool {
    match ctx.token(start) {
        Some(t) if t.kind == TokKind::Ident && t.text == "public" => {}
        _ => return false,
    }
    match ctx.token(ctx.skip_trivia(start + 1)).map(|t| t.kind) {
        Some(TokKind::LParen | TokKind::Eq | TokKind::LBracket) | None => false,
        Some(_) => true,
    }
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
    flags: ExprFlags,
) -> Option<ExprParse> {
    let tok = ctx.token(start)?;
    match tok.kind {
        // An operator glued to `{` is a parametric callee: `+{T}` → `(curly + T)`,
        // `*{T}(x)` → `(call (curly * T) x)`. We return the operator as a bare leaf
        // and let the postfix chain build the `CURLY_EXPR` (and any trailing call),
        // exactly as for an identifier callee `f{T}`. Excludes `::`/`&`/`:`, which
        // Julia keeps as prefixes over the braces (`::{T}` → `(::-pre (braces T))`).
        k if is_curly_operator_name(k)
            && ctx.token(start + 1).map(|t| t.kind) == Some(TokKind::LBrace) =>
        {
            Some(ExprParse {
                start,
                end: start + 1,
                events: vec![Event::Tok(start)],
            })
        }
        // A bare `end` inside square brackets is the index-end marker (`a[end]`,
        // `a[end - 1]`); elsewhere `end` is a block terminator and not an atom.
        TokKind::EndKw if flags.end_marker => Some(atom(SyntaxKind::END_MARKER, start)),
        // A bare `begin` inside an indexing `a[…]` is the index-begin marker
        // (`a[begin]`, `a[begin + 1]`); elsewhere `begin` opens a block.
        TokKind::BeginKw if flags.begin_marker => Some(atom(SyntaxKind::BEGIN_MARKER, start)),
        // Prefix operators: arithmetic/logical unary (`-x`, `!x`), lower-bound
        // type expressions (`<:Real` in `Array{<:Real}`), and unary `::`
        // declarations (`::Int` in a method signature `f(::Int)`).
        TokKind::Plus
        | TokKind::Minus
        | TokKind::DotPlus
        | TokKind::DotMinus
        | TokKind::Bang
        | TokKind::Tilde
        | TokKind::DotTilde
        | TokKind::Subtype
        | TokKind::Supertype
        | TokKind::ColonColon => {
            // A unary arithmetic/logical operator glued to a `(` is a call when
            // the parens look like an argument list (`+(x, y)` → `(call + x y)`,
            // `+(a...)` → `(call + (... a))`, `+(a; b, c)` → `(call + a
            // (parameters b c))`). A single bare operand stays a prefix
            // application (`+(x)` → `(call-pre + x)`). Mirrors JuliaSyntax's
            // paren-call heuristic. The type operators `<:`/`>:` follow the same
            // rule (`<:(a, b)` -> `(<: a b)`, `<:(a)` -> `(<:-pre a)`); the
            // projector heads the call node with the operator. Unary `::` keeps
            // its prefix handling (its paren-call shape differs and is deferred).
            if matches!(
                tok.kind,
                TokKind::Plus
                    | TokKind::Minus
                    | TokKind::DotPlus
                    | TokKind::DotMinus
                    | TokKind::Bang
                    | TokKind::Tilde
                    | TokKind::DotTilde
                    | TokKind::Subtype
                    | TokKind::Supertype
            ) && ctx.token(start + 1).map(|t| t.kind) == Some(TokKind::LParen)
                && unary_op_paren_is_call(ctx, start + 1)
            {
                let (list_events, end) = parse_arg_list(
                    ctx,
                    start + 1,
                    TokKind::RParen,
                    SyntaxKind::ARG_LIST,
                    diagnostics,
                );
                let mut events = vec![Event::Start(SyntaxKind::CALL_EXPR), Event::Tok(start)];
                events.extend(list_events);
                events.push(Event::Finish);
                return Some(ExprParse { start, end, events });
            }
            let node = if tok.kind == TokKind::ColonColon {
                SyntaxKind::TYPE_ANNOTATION
            } else {
                SyntaxKind::UNARY_EXPR
            };
            let operand_start = ctx.skip_trivia(start + 1);
            // PREFIX_BP is above the range colon and the array-element boundary
            // only matters at low binding powers, so neither `no_range` nor
            // `array_mode` changes the operand here; carry the rest through.
            let operand_flags = ExprFlags {
                no_range: false,
                array_mode: false,
                ..flags
            };
            let Some(operand) = parse_expr_in(
                ctx.tokens(),
                operand_start,
                PREFIX_BP,
                diagnostics,
                operand_flags,
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
        // A non-unary operator glued to a `(` is a call with the operator as the
        // callee: `*(x)` → `(call * x)`, `.*(a, b)` → `(call (. *) a b)`. Only the
        // adjacent form is a call (`* (x)` is an error); a space would leave the
        // `(` to be parsed separately. Unary operators (`+`, `-`, `!`, `~`) keep
        // their prefix-application handling above.
        k if is_operator_call_name(k)
            && ctx.token(start + 1).map(|t| t.kind) == Some(TokKind::LParen) =>
        {
            let (list_events, end) = parse_arg_list(
                ctx,
                start + 1,
                TokKind::RParen,
                SyntaxKind::ARG_LIST,
                diagnostics,
            );
            let mut events = vec![Event::Start(SyntaxKind::CALL_EXPR), Event::Tok(start)];
            events.extend(list_events);
            events.push(Event::Finish);
            Some(ExprParse { start, end, events })
        }
        // A prefix `:` quotes a symbol (`:foo`, `:end`) or expression (`:(x+1)`).
        // A bare `:` not followed by something quotable (`a[:]`) is not a quote;
        // `parse_quote_sym` returns `None` so it falls through.
        TokKind::Colon => parse_quote_sym(ctx, start, diagnostics),
        // A prefix `$` is an interpolation (`$x`, `$(x + y)`). It parses
        // everywhere — Julia only rejects it outside a quote during lowering,
        // not at parse time — so the field-access right-hand side (`f.$x`) and
        // quoted contexts (`:($x)`) reuse the same node.
        TokKind::Dollar => Some(parse_prefix_interpolation(ctx, start, diagnostics)),
        TokKind::At => Some(parse_macro(ctx, start, diagnostics, flags.inside_brackets)),
        TokKind::LParen => parse_paren(ctx, start, diagnostics),
        TokKind::LBracket => Some(parse_bracket_literal(ctx, start, diagnostics)),
        TokKind::LBrace => parse_braces(ctx, start, diagnostics),
        TokKind::Ident => Some(atom(SyntaxKind::NAME, start)),
        TokKind::StringPrefix | TokKind::StringDelimOpen | TokKind::CmdDelimOpen => {
            Some(parse_string_literal(ctx, start, diagnostics))
        }
        TokKind::Integer
        | TokKind::BinInt
        | TokKind::OctInt
        | TokKind::HexInt
        | TokKind::Float
        | TokKind::Float32
        | TokKind::Char
        | TokKind::TrueKw
        | TokKind::FalseKw => Some(atom(SyntaxKind::LITERAL, start)),
        _ => None,
    }
}

/// Parse a prefix `:` quote into a `QUOTE_SYM` node: `:name`/`:end` (a symbol)
/// or `:(expr)` (a quoted expression). Returns `None` for a bare `:` that is not
/// followed by a quotable token (e.g. the index colon in `a[:]`), so the caller
/// falls through to its normal handling.
pub(super) fn parse_quote_sym(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let next = ctx.skip_trivia(start + 1);
    let mut events = vec![Event::Start(SyntaxKind::QUOTE_SYM), Event::Tok(start)];
    push_range(&mut events, start + 1, next);
    match ctx.token(next).map(|t| t.kind)? {
        // `:(op)` — a lone operator quoted in parens, e.g. `:(=)`, `:(::)`,
        // `:(:)`, `:(+)`. In a quote context a bare operator (including the
        // syntactic `=`/`::`/`:` that are errors in value position) is a symbol.
        // Build a `PAREN_EXPR` wrapping the operator token; the projector reads a
        // lone-operator paren as the operator's text.
        TokKind::LParen
            if {
                let op = ctx.skip_trivia(next + 1);
                is_paren_quotable_op(ctx.token(op).map(|t| t.kind))
                    && ctx.token(ctx.skip_trivia(op + 1)).map(|t| t.kind) == Some(TokKind::RParen)
            } =>
        {
            let op = ctx.skip_trivia(next + 1);
            let rparen = ctx.skip_trivia(op + 1);
            events.push(Event::Start(SyntaxKind::PAREN_EXPR));
            push_range(&mut events, next, rparen + 1);
            events.push(Event::Finish); // PAREN_EXPR
            events.push(Event::Finish); // QUOTE_SYM
            Some(ExprParse {
                start,
                end: rparen + 1,
                events,
            })
        }
        // `:(expr)` — the parenthesized expression is the quoted form.
        TokKind::LParen => {
            let paren = parse_paren(ctx, next, diagnostics)?;
            let end = paren.end;
            events.extend(paren.events);
            events.push(Event::Finish);
            Some(ExprParse { start, end, events })
        }
        // `:name` — an identifier symbol.
        TokKind::Ident => {
            events.push(Event::Start(SyntaxKind::NAME));
            events.push(Event::Tok(next));
            events.push(Event::Finish);
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // `:+`, `:<:`, `:+=`, … — a symbolic operator used as a symbol. Restricted
        // to undotted operator names (`is_op_name`) plus assignment operators;
        // broadcast forms like `:.+` quote to `(. +)` and are not handled here.
        k if is_op_name(k) || is_assignment_op(k) => {
            events.push(Event::Tok(next));
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // `:end`, `:function`, … — a keyword used as a symbol.
        k if k.is_keyword() => {
            events.push(Event::Tok(next));
            events.push(Event::Finish);
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // A bare `:` (e.g. `a[:]`) is not a quote.
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

/// Parse a standalone `$…` interpolation in expression position. `$ident` and
/// `$(expr)` reuse the string-context [`parse_interpolation`]; any other operand
/// (`$$a`, `$[1, 2]`, `$"s"`) binds `$` to the next *prefix atom* — tightly, with
/// no postfix — so `$a.b` is `(. ($ a) …)` and `$$a` is `($ ($ a))`.
pub(super) fn parse_prefix_interpolation(
    ctx: &ParserCtx<'_>,
    dollar: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let next = dollar + 1;
    if matches!(
        ctx.token(next).map(|t| t.kind),
        Some(TokKind::LParen | TokKind::Ident)
    ) {
        let mut events = Vec::new();
        let end = parse_interpolation(ctx, &mut events, dollar, diagnostics);
        return ExprParse {
            start: dollar,
            end,
            events,
        };
    }

    let mut events = vec![Event::Start(SyntaxKind::INTERPOLATION), Event::Tok(dollar)];
    match parse_prefix(ctx, next, diagnostics, ExprFlags::default()) {
        Some(operand) => {
            push_range(&mut events, next, operand.start);
            let end = operand.end;
            events.extend(operand.events);
            events.push(Event::Finish);
            ExprParse {
                start: dollar,
                end,
                events,
            }
        }
        // A bare `$` with no operand: emit just the sigil.
        None => {
            events.push(Event::Finish);
            ExprParse {
                start: dollar,
                end: next,
                events,
            }
        }
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

    // `(op)` — a lone non-syntactic operator in parens is the operator as a
    // value, e.g. `(+)` → `+`, `(:)` → `:`, `(<:)` → `<:`. Build a `PAREN_EXPR`
    // wrapping the bare operator token (the projector reads a lone-operator paren
    // as the operator's text). Postfix application (`(+)(a, b)`) then makes it a
    // call callee. Whitespace-insensitive: `( + )` is the same.
    if is_paren_value_op(ctx.token(inner_start).map(|t| t.kind)) {
        let close = ctx.skip_trivia(inner_start + 1);
        if ctx.token(close).map(|t| t.kind) == Some(TokKind::RParen) {
            let mut events = vec![Event::Start(SyntaxKind::PAREN_EXPR)];
            push_range(&mut events, start, close + 1);
            events.push(Event::Finish);
            return Some(ExprParse {
                start,
                end: close + 1,
                events,
            });
        }
    }

    let Some(inner) = parse_expr_in_brackets(ctx.tokens(), inner_start, 0, diagnostics) else {
        return Some(error_expr_with_range(start, inner_start));
    };

    // `(x for x in xs)` is a generator expression.
    let sep = ctx.skip_trivia(inner.end);
    if ctx.token(sep).map(|t| t.kind) == Some(TokKind::ForKw) {
        return Some(parse_comprehension(
            ctx,
            start,
            inner,
            SyntaxKind::GENERATOR,
            TokKind::RParen,
            diagnostics,
        ));
    }

    // A `,` or `;` after the first element makes this a tuple (or named tuple).
    // Re-parse the whole parenthesized run as an argument list so each element
    // becomes an `ARG`/`KEYWORD_ARG` and `;` opens a `PARAMETERS` section.
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

/// Whether the operator at `op_idx` begins a new array element: there is
/// whitespace between the previous operand (ending at `operand_end`) and the
/// operator, but the operator is glued to its own operand (no whitespace after).
/// That makes it a prefix of the next element rather than an infix operator, so
/// `[1 +2]` is two elements while `[1 + 2]` is one.
fn array_element_boundary(ctx: &ParserCtx<'_>, operand_end: usize, op_idx: usize) -> bool {
    let space_before = op_idx > operand_end;
    space_before
        && !matches!(
            ctx.token(op_idx + 1).map(|t| t.kind),
            Some(TokKind::Whitespace | TokKind::Newline) | None
        )
}

/// Parse one element of an array literal: a full expression in array mode (a
/// space-glued operator ends it) at statement-newline sensitivity (a newline is a
/// row separator handled by the caller, not part of the element). Array literals
/// are square-bracketed, so `end` is the index marker here.
fn parse_element(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        array_mode: true,
        end_marker: true,
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

/// Parse a `[...]` literal at prefix position (postfix `[` is indexing). A `,`
/// after the first element (or an empty/single `[x]`) is a `VECT_EXPR`, reusing
/// the arg-list machinery; a space-, `;`-, or newline-separated layout is a
/// `MATRIX_EXPR` of `MATRIX_ROW`s.
fn parse_bracket_literal(
    ctx: &ParserCtx<'_>,
    lbrk: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let tokens = ctx.tokens();
    let vect = |diagnostics: &mut Vec<ParseDiagnostic>| {
        let (events, end) = parse_arg_list(
            ctx,
            lbrk,
            TokKind::RBracket,
            SyntaxKind::VECT_EXPR,
            diagnostics,
        );
        ExprParse {
            start: lbrk,
            end,
            events,
        }
    };

    let first_start = ctx.skip_trivia(lbrk + 1);
    // Empty `[]`, or a first element we cannot parse: the comma-list parser
    // handles both losslessly.
    if ctx.token(first_start).map(|t| t.kind) == Some(TokKind::RBracket) {
        return vect(diagnostics);
    }
    let Some(first) = parse_element(tokens, first_start, diagnostics) else {
        return vect(diagnostics);
    };

    // Look at the first separator (past horizontal whitespace and comments, but
    // not a newline — a newline is a significant row separator). A `,`, `]`, or
    // end means a vector; anything else (`;`, newline, or another element) means
    // a matrix.
    let mut look = first.end;
    while matches!(
        ctx.token(look).map(|t| t.kind),
        Some(TokKind::Whitespace | TokKind::Comment | TokKind::BlockComment)
    ) {
        look += 1;
    }
    match ctx.token(look).map(|t| t.kind) {
        Some(TokKind::ForKw) => parse_comprehension(
            ctx,
            lbrk,
            first,
            SyntaxKind::COMPREHENSION,
            TokKind::RBracket,
            diagnostics,
        ),
        None | Some(TokKind::RBracket | TokKind::Comma) => vect(diagnostics),
        _ => parse_matrix(ctx, lbrk, first, diagnostics),
    }
}

/// Parse the matrix form of a `[...]` literal given its already-parsed first
/// element. Elements within a row are space-separated; rows are separated by `;`
/// or a newline. Each element is an `ARG` inside a `MATRIX_ROW`.
fn parse_matrix(
    ctx: &ParserCtx<'_>,
    lbrk: usize,
    first: ExprParse,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    let tokens = ctx.tokens();
    let mut events = vec![Event::Start(SyntaxKind::MATRIX_EXPR), Event::Tok(lbrk)];
    push_range(&mut events, lbrk + 1, first.start);
    events.push(Event::Start(SyntaxKind::MATRIX_ROW));
    let mut pos = push_element_arg(&mut events, first);

    loop {
        // Scan past horizontal whitespace and comments to the next significant
        // token (a newline stays significant as a row separator).
        let mut look = pos;
        while matches!(
            ctx.token(look).map(|t| t.kind),
            Some(TokKind::Whitespace | TokKind::Comment | TokKind::BlockComment)
        ) {
            look += 1;
        }

        match ctx.token(look).map(|t| t.kind) {
            // End of the literal: close the open row, emit trailing trivia + `]`.
            None | Some(TokKind::RBracket) => {
                events.push(Event::Finish); // MATRIX_ROW
                push_range(&mut events, pos, look);
                let end = if ctx.token(look).map(|t| t.kind) == Some(TokKind::RBracket) {
                    events.push(Event::Tok(look));
                    look + 1
                } else {
                    look
                };
                events.push(Event::Finish); // MATRIX_EXPR
                return ExprParse {
                    start: lbrk,
                    end,
                    events,
                };
            }
            // Row separator: close the row, consume the `;`/newline/trivia run,
            // then either close (a trailing separator) or open the next row.
            Some(TokKind::Semicolon | TokKind::Newline) => {
                events.push(Event::Finish); // MATRIX_ROW
                let mut q = pos;
                loop {
                    while matches!(ctx.token(q).map(|t| t.kind), Some(k) if k.is_trivia()) {
                        events.push(Event::Tok(q));
                        q += 1;
                    }
                    if ctx.token(q).map(|t| t.kind) == Some(TokKind::Semicolon) {
                        events.push(Event::Tok(q));
                        q += 1;
                        continue;
                    }
                    break;
                }
                match ctx.token(q).map(|t| t.kind) {
                    None => {
                        events.push(Event::Finish); // MATRIX_EXPR
                        return ExprParse {
                            start: lbrk,
                            end: q,
                            events,
                        };
                    }
                    Some(TokKind::RBracket) => {
                        events.push(Event::Tok(q));
                        events.push(Event::Finish); // MATRIX_EXPR
                        return ExprParse {
                            start: lbrk,
                            end: q + 1,
                            events,
                        };
                    }
                    _ => {
                        events.push(Event::Start(SyntaxKind::MATRIX_ROW));
                        pos = match parse_element(tokens, q, diagnostics) {
                            Some(el) => push_element_arg(&mut events, el),
                            None => {
                                events.push(Event::Tok(q));
                                q + 1
                            }
                        };
                    }
                }
            }
            // Another element in the current row (whitespace is the separator).
            _ => {
                push_range(&mut events, pos, look);
                pos = match parse_element(tokens, look, diagnostics) {
                    Some(el) => push_element_arg(&mut events, el),
                    None => {
                        events.push(Event::Tok(look));
                        look + 1
                    }
                };
            }
        }
    }
}

/// Parse a comprehension `[elem for v in iter if cond]` or generator
/// `(elem for v in iter)` given the already-parsed `elem` and the open delimiter
/// at `open` (closing kind `close`). Each `for` becomes a `FOR_BINDING` and each
/// trailing `if` a `COMPREHENSION_IF` wrapping the preceding clause. Multiple
/// `for` clauses (`for a in as for b in bs`) and comma-separated iteration specs
/// within one clause (`for a in as, b in bs`) are both handled. `in` is matched
/// as a bare `in` identifier, mirroring `for`/`while` loops; the `a = as` spec
/// form is parsed as a plain assignment.
fn parse_comprehension(
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
    loop {
        let for_idx = ctx.skip_trivia(pos);
        if ctx.token(for_idx).map(|t| t.kind) != Some(TokKind::ForKw) {
            break;
        }
        push_range(&mut events, pos, for_idx);
        events.push(Event::Start(SyntaxKind::FOR_BINDING));
        events.push(Event::Tok(for_idx)); // `for`
        pos = parse_for_specs(ctx, for_idx + 1, &mut events, diagnostics);
        events.push(Event::Finish); // FOR_BINDING

        // Optional `if <cond>` filter on this clause.
        let if_idx = ctx.skip_trivia(pos);
        if ctx.token(if_idx).map(|t| t.kind) == Some(TokKind::IfKw) {
            push_range(&mut events, pos, if_idx);
            events.push(Event::Start(SyntaxKind::COMPREHENSION_IF));
            events.push(Event::Tok(if_idx)); // `if`
            pos = if_idx + 1;
            let cond_start = ctx.skip_trivia(pos);
            push_range(&mut events, pos, cond_start);
            if let Some(cond) = parse_expr_in_brackets(tokens, cond_start, 0, diagnostics) {
                events.extend(cond.events);
                pos = cond.end;
            } else {
                pos = cond_start;
            }
            events.push(Event::Finish); // COMPREHENSION_IF
        }
    }

    // Closing delimiter.
    let close_idx = ctx.skip_trivia(pos);
    push_range(&mut events, pos, close_idx);
    let end = if ctx.token(close_idx).map(|t| t.kind) == Some(close) {
        events.push(Event::Tok(close_idx));
        close_idx + 1
    } else {
        let tok = &tokens[open];
        push_diagnostic(diagnostics, "unclosed comprehension", tok.start, tok.end);
        close_idx
    };
    events.push(Event::Finish); // node_kind
    ExprParse {
        start: open,
        end,
        events,
    }
}

/// Parse the comma-separated iteration specs of one `for` clause, starting just
/// past the `for` keyword. Each spec is `var in iter`/`var ∈ iter` (the `in`
/// matched as a bare identifier) or the assignment form `var = iter` (parsed
/// whole as an `ASSIGNMENT_EXPR`). Commas are kept as tokens so the projector can
/// group multiple specs into a `cartesian_iterator`. Returns the index past the
/// last spec.
fn parse_for_specs(
    ctx: &ParserCtx<'_>,
    mut pos: usize,
    events: &mut Vec<Event>,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let tokens = ctx.tokens();
    loop {
        // The loop variable (or, for the `=` form, the whole spec assignment).
        let var_start = ctx.skip_trivia(pos);
        push_range(events, pos, var_start);
        if let Some(var) = parse_expr_in_brackets(tokens, var_start, 0, diagnostics) {
            events.extend(var.events);
            pos = var.end;
        } else {
            pos = var_start;
        }

        // `in`/`∈` form needs an explicit iterator; the `=` form is already
        // complete (consumed above as an assignment).
        let in_idx = ctx.skip_trivia(pos);
        if ctx
            .token(in_idx)
            .is_some_and(|t| t.kind == TokKind::Ident && (t.text == "in" || t.text == "∈"))
        {
            push_range(events, pos, in_idx);
            events.push(Event::Tok(in_idx));
            pos = in_idx + 1;
            let iter_start = ctx.skip_trivia(pos);
            push_range(events, pos, iter_start);
            if let Some(iter) = parse_expr_in_brackets(tokens, iter_start, 0, diagnostics) {
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
            // Postfix transpose/adjoint `A'`. Wraps the operand and re-loops, so
            // it chains (`A''`) and composes with later suffixes (`A'[i]`). The
            // lexer only emits `Transpose` when it directly abuts a value, so the
            // operator is always adjacent (no newline between operand and `'`).
            Some(TokKind::Transpose) => {
                let mut events = vec![Event::Start(SyntaxKind::POSTFIX_EXPR)];
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
    // A single element followed by `for` is a generator argument:
    // `sum(x for x in xs)` (a call whose sole argument is a generator) or
    // `T[x for x in xs]` (a typed comprehension). The delimiters belong to the
    // outer node; the generator clauses reuse the comprehension machinery.
    let first_start = ctx.skip_trivia(open_idx + 1);
    if ctx.token(first_start).map(|t| t.kind) != Some(close) {
        let end_marker = close == TokKind::RBracket;
        let flags = ExprFlags {
            inside_brackets: true,
            end_marker,
            begin_marker: end_marker,
            ..ExprFlags::default()
        };
        let diag_mark = diagnostics.len();
        if let Some(first) = parse_expr_in(ctx.tokens(), first_start, 0, diagnostics, flags)
            && ctx.token(ctx.skip_trivia(first.end)).map(|t| t.kind) == Some(TokKind::ForKw)
        {
            let generator = parse_comprehension(
                ctx,
                open_idx,
                first,
                SyntaxKind::GENERATOR,
                close,
                diagnostics,
            );
            let node_kind = if node == SyntaxKind::CALL_EXPR {
                SyntaxKind::CALL_EXPR
            } else {
                SyntaxKind::TYPED_COMPREHENSION
            };
            let mut events = vec![Event::Start(node_kind)];
            events.extend(lhs.events);
            push_range(&mut events, lhs.end, open_idx);
            events.extend(generator.events);
            events.push(Event::Finish);
            return ExprParse {
                start: lhs.start,
                end: generator.end,
                events,
            };
        }
        diagnostics.truncate(diag_mark);
    }

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
                let arg_flags = ExprFlags {
                    inside_brackets,
                    ..ExprFlags::default()
                };
                match parse_expr_in(ctx.tokens(), next, 0, diagnostics, arg_flags) {
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
    // `end` is the index marker only inside square brackets — indexing (`a[end]`)
    // and vector literals (`[end]`), both of which close with `]`.
    let end_marker = close == TokKind::RBracket;
    // `begin` is the index marker only in *indexing* position, which is the sole
    // `ARG_LIST` closed by `]` (vector literals build a `VECT_EXPR`, calls close
    // with `)`); a vector literal's `[begin … end]` stays a block.
    let begin_marker = end_marker && list_kind == SyntaxKind::ARG_LIST;
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
            Some(_) => {
                i = parse_one_arg(ctx, &mut events, i, end_marker, begin_marker, diagnostics)
            }
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
    end_marker: bool,
    begin_marker: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    let tokens = ctx.tokens();
    let flags = ExprFlags {
        inside_brackets: true,
        end_marker,
        begin_marker,
        ..ExprFlags::default()
    };
    let parse_arg_expr = |tokens: &[Token], start, diagnostics: &mut Vec<ParseDiagnostic>| {
        parse_expr_in(tokens, start, 0, diagnostics, flags)
    };
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
        let end = match parse_arg_expr(tokens, val_start, diagnostics) {
            Some(val) => {
                events.extend(val.events);
                val.end
            }
            None => val_start,
        };
        events.push(Event::Finish);
        end
    } else if let Some(arg) = parse_arg_expr(tokens, i, diagnostics) {
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
    matches!(kind, TokKind::Question)
        || is_assignment_op(kind)
        || infix_binding_power(kind).is_some()
}

/// Plain/broadcast assignment (`=`, `.=`) and augmented assignment (`+=`, `.+=`,
/// …): the loosest, right-associative tier, all modeled as `ASSIGNMENT_EXPR`.
fn is_assignment_op(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::Eq
            | TokKind::DotEq
            | TokKind::PlusEq
            | TokKind::MinusEq
            | TokKind::StarEq
            | TokKind::SlashEq
            | TokKind::SlashSlashEq
            | TokKind::CaretEq
            | TokKind::PercentEq
            | TokKind::PipeEq
            | TokKind::AmpEq
            | TokKind::DotPlusEq
            | TokKind::DotMinusEq
            | TokKind::DotStarEq
            | TokKind::DotSlashEq
            | TokKind::DotSlashSlashEq
            | TokKind::DotCaretEq
            | TokKind::DotPercentEq
    )
}

/// A binary operator that, glued to a `(`, names a function call: `*(x)`,
/// `==(a, b)`, `.*(a, b)`. These are the operators that are *not* unary in Julia
/// (so the parens form an argument list, never a prefix application) and not
/// syntactic (`&`, `:`, `::`, `&&`, `||`, `->` route elsewhere). The unary
/// operators (`+`, `-`, `!`, `~`) and type operators (`<:`, `>:`) are excluded;
/// they keep their prefix-application parse.
/// Whether a unary operator's adjacent parens form an argument list — making
/// `+(...)` a call (`(call + …)`) rather than a parenthesized operand (a prefix
/// application `+(x)` → `(call-pre + x)`). Mirrors JuliaSyntax: the parens are a
/// call when empty (`+()`), opened by a leading `;` (a parameters section,
/// `+(; a)`), or when — at the top level — they contain a comma (`+(x, y)`) or a
/// splat `...` (`+(a...)`). A lone interior expression, or a non-leading `;`
/// block (`+(a; b)`), stays a prefix operand.
fn unary_op_paren_is_call(ctx: &ParserCtx<'_>, lparen_idx: usize) -> bool {
    let first = ctx.skip_trivia(lparen_idx + 1);
    match ctx.token(first).map(|t| t.kind) {
        Some(TokKind::RParen) => return true,
        Some(TokKind::Semicolon) => return true,
        _ => {}
    }
    let mut depth = 0i32;
    let mut i = first;
    while let Some(tok) = ctx.token(i) {
        match tok.kind {
            TokKind::LParen | TokKind::LBracket | TokKind::LBrace => depth += 1,
            TokKind::RParen | TokKind::RBracket | TokKind::RBrace => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            TokKind::Comma | TokKind::DotDotDot if depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

fn is_operator_call_name(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        Star | Slash
            | SlashSlash
            | Caret
            | Percent
            | EqEq
            | NotEq
            | Lt
            | Le
            | Gt
            | Ge
            | Pipe
            | Shl
            | Shr
            | UShr
            | PipeGt
            | PipeLt
            | FatArrow
            | LongArrow
            | LeftRightArrow
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
            | DotFatArrow
            | DotLongArrow
            | DotPipeGt
    )
}

/// Whether `kind` is an operator that, glued to `{`, names a parametric callee
/// (`+{T}` → `(curly + T)`). This is the operator-call set (binary operators)
/// plus the unary arithmetic/logical and type operators. `::`, `&`, and `:` are
/// excluded: Julia keeps them as prefixes over the braces, and the syntactic
/// `&&`/`||`/`->` produce error-shape callees and stay unsupported.
fn is_curly_operator_name(kind: TokKind) -> bool {
    use TokKind::*;
    is_operator_call_name(kind)
        || matches!(
            kind,
            Plus | Minus | DotPlus | DotMinus | Bang | Tilde | DotTilde | Subtype | Supertype
        )
}

/// A lone operator that may be quoted inside parens, `:(op)`. Accepts undotted
/// operator names, undotted augmented/plain assignment operators, and the
/// syntactic `::`/`:` — all of which are valid symbols in a quote context (even
/// `=`/`::`, which are errors in value position). Broadcast forms (`.+`, `.=`)
/// quote to a `(. op)` shape and are excluded here.
/// Whether `kind` is an operator that, alone inside parens in *value* position,
/// is the operator as a value (`(+)` → `+`, `(:)` → `:`, `(<:)` → `<:`). This is
/// the non-syntactic subset: `is_op_name` minus the syntactic `&&`/`||`/`->`
/// (which Julia reports as errors in value position) plus `:`. Broadcast forms
/// (`(.+)` → `(. +)`) and the erroring syntactic ops (`=`, `::`, assignment, `?`,
/// `...`) are deliberately excluded.
fn is_paren_value_op(kind: Option<TokKind>) -> bool {
    let Some(k) = kind else { return false };
    use TokKind::*;
    (is_op_name(k) && !matches!(k, AndAnd | OrOr | Arrow)) || k == Colon
}

fn is_paren_quotable_op(kind: Option<TokKind>) -> bool {
    let Some(k) = kind else { return false };
    use TokKind::*;
    is_op_name(k)
        || matches!(
            k,
            Eq | PlusEq
                | MinusEq
                | StarEq
                | SlashEq
                | SlashSlashEq
                | CaretEq
                | PercentEq
                | PipeEq
                | AmpEq
                | ColonColon
                | Colon
        )
}

/// Parse the `then : else` tail of a ternary whose `?` sits at `q_idx`, given the
/// already-parsed condition `cond`. Returns `Ok(node)` with the assembled
/// `TERNARY_EXPR` (the caller continues its operator loop), or `Err(recovered)`
/// when a branch or the `:` separator is missing (the caller returns it as-is).
fn parse_ternary(
    ctx: &ParserCtx<'_>,
    cond: ExprParse,
    q_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> Result<ExprParse, ExprParse> {
    let tokens = ctx.tokens();
    let inside_brackets = flags.inside_brackets;

    // True-branch: `no_range` so a bare `:` ends it (the ternary separator). A
    // real range in the true-branch must therefore be parenthesized, as in Julia.
    let then_start = ctx.skip_trivia(q_idx + 1);
    let then_flags = ExprFlags {
        no_range: true,
        array_mode: false,
        ..flags
    };
    let Some(then_br) = parse_expr_in(tokens, then_start, TERNARY_R, diagnostics, then_flags)
    else {
        let op = &tokens[q_idx];
        push_diagnostic(
            diagnostics,
            "expected expression after `?`",
            op.start,
            op.end,
        );
        return Err(error_expr_to_line_end(tokens, cond.start, q_idx + 1));
    };

    // The `:` separator (newlines insignificant inside brackets, like operators).
    let colon = if inside_brackets {
        ctx.skip_ws_and_newlines(then_br.end)
    } else {
        ctx.skip_ws(then_br.end)
    };
    if ctx.token(colon).map(|t| t.kind) != Some(TokKind::Colon) {
        let op = &tokens[q_idx];
        push_diagnostic(
            diagnostics,
            "expected `:` in ternary expression",
            op.start,
            op.end,
        );
        // Recover: a ternary holding the condition, `?`, and true-branch only.
        let mut events = vec![Event::Start(SyntaxKind::TERNARY_EXPR)];
        events.extend(cond.events);
        push_range(&mut events, cond.end, q_idx);
        events.push(Event::Tok(q_idx));
        push_range(&mut events, q_idx + 1, then_br.start);
        events.extend(then_br.events);
        events.push(Event::Finish);
        return Ok(ExprParse {
            start: cond.start,
            end: then_br.end,
            events,
        });
    }

    // False-branch: inherit `no_range` so an enclosing ternary's `:` still ends
    // it (`a ? b ? c : d : e`), while a top-level else may hold a range.
    let else_start = ctx.skip_trivia(colon + 1);
    let else_flags = ExprFlags {
        array_mode: false,
        ..flags
    };
    let Some(else_br) = parse_expr_in(tokens, else_start, TERNARY_R, diagnostics, else_flags)
    else {
        let op = &tokens[colon];
        push_diagnostic(
            diagnostics,
            "expected expression after `:`",
            op.start,
            op.end,
        );
        return Err(error_expr_to_line_end(tokens, cond.start, colon + 1));
    };

    let mut events = vec![Event::Start(SyntaxKind::TERNARY_EXPR)];
    events.extend(cond.events);
    push_range(&mut events, cond.end, q_idx);
    events.push(Event::Tok(q_idx)); // `?`
    push_range(&mut events, q_idx + 1, then_br.start);
    events.extend(then_br.events);
    push_range(&mut events, then_br.end, colon);
    events.push(Event::Tok(colon)); // `:`
    push_range(&mut events, colon + 1, else_br.start);
    events.extend(else_br.events);
    events.push(Event::Finish);
    Ok(ExprParse {
        start: cond.start,
        end: else_br.end,
        events,
    })
}

/// `(left_bp, right_bp)` for binary operators. A right-associative operator has
/// `right_bp < left_bp` (e.g. `^`); a left-associative one has `right_bp =
/// left_bp + 1`.
fn infix_binding_power(kind: TokKind) -> Option<(u8, u8)> {
    Some(match kind {
        // `~` (and broadcast `.~`) sits at the assignment tier: right-associative
        // and as loose as `=` (`a ~ b = c` ⇒ `(~ a (= b c))`, `x = a ~ b` ⇒
        // `(= x (~ a b))`), but builds an ordinary `(call-i a ~ b)`, not an
        // assignment. Handled here (not `is_assignment_op`) so the node stays
        // `BINARY_EXPR`.
        TokKind::Tilde | TokKind::DotTilde => (2, 1),
        // The pair `=>` shares the arrow/ternary tier: right-associative, looser
        // than `||` and tighter than `=` (`a || b => c = d` ⇒ `(= (=> (|| a b) c) d)`).
        TokKind::Arrow
        | TokKind::FatArrow
        | TokKind::DotFatArrow
        | TokKind::LongArrow
        | TokKind::LeftRightArrow
        | TokKind::DotLongArrow => (4, 3),
        TokKind::OrOr | TokKind::DotOrOr => (5, 6),
        TokKind::AndAnd | TokKind::DotAndAnd => (7, 8),
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
        // The pipe operators share Julia's pipe precedence: `<|` (left-pipe) is
        // looser and right-associative, `|>` (right-pipe, also broadcast `.|>`)
        // is tighter and left-associative (`a <| b |> c` ⇒ `a <| (b |> c)`).
        TokKind::PipeLt => (12, 11),
        TokKind::PipeGt | TokKind::DotPipeGt => (13, 14),
        // The range operator `..` shares the colon tier (Julia gives both
        // precedence 10) and is left-associative, building an ordinary
        // `(call-i a .. b)`.
        TokKind::Colon | TokKind::DotDot => (14, 15),
        TokKind::Plus | TokKind::Minus | TokKind::DotPlus | TokKind::DotMinus => (20, 21),
        TokKind::Star
        | TokKind::Slash
        | TokKind::Percent
        | TokKind::DotStar
        | TokKind::DotSlash
        | TokKind::DotPercent => (24, 25),
        // Rational `//` (and broadcast `.//`) bind tighter than `*`/`/` but
        // looser than `^`, and are left-associative (`a//b//c` ⇒ `(a//b)//c`).
        TokKind::SlashSlash | TokKind::DotSlashSlash => (28, 29),
        // Bitshift `<< >> >>>` binds tighter than `//` and looser than `^`
        // (Julia precedence 14), left-associative.
        TokKind::Shl | TokKind::Shr | TokKind::UShr => (30, 31),
        TokKind::Caret | TokKind::DotCaret => (32, 31),
        TokKind::ColonColon => (36, 37),
        TokKind::Dot => (40, 41),
        _ => return None,
    })
}
