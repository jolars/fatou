//! Pratt (precedence-climbing) expression parser plus postfix call/index chains.
//!
//! `parse_expr` parses one expression starting at a **non-trivia** token; the
//! caller is responsible for emitting any leading trivia. Every token the
//! expression covers (operators and interior trivia included) is emitted into
//! the event stream, so the parser preserves losslessness.

use crate::parser::context::ParserCtx;
use crate::parser::diagnostics::{DiagnosticKind, ParseDiagnostic, push_diagnostic};
use crate::parser::events::{Event, ExprParse, push_range};
use crate::parser::lexer::{TokKind, Token, is_op_suffix_char};
use crate::parser::recovery::{error_expr_to_line_end, error_expr_with_range};
use crate::parser::structural::{
    KwStmt, is_op_name, parse_abstract_type, parse_begin_expr, parse_do_block, parse_for_expr,
    parse_function_expr, parse_if_expr, parse_import_stmt, parse_keyword_stmt, parse_let_expr,
    parse_macro_def, parse_module_expr, parse_name_list_stmt, parse_primitive_type,
    parse_quote_expr, parse_struct_expr, parse_try_expr, parse_while_expr,
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
    /// At statement position (toplevel, module/block statements, and the operand
    /// of `return`/`const`): a top-level comma collects a bare-comma tuple
    /// (`a, b` ⇒ `(tuple a b)`). Off inside brackets and sub-expressions, where
    /// commas are argument/element separators handled by the container parsers.
    stmt_comma: bool,
    /// Suppress the word operators `in`/`isa` (lexed as identifiers, comparison
    /// precedence). Set only while parsing a `for`/generator loop variable, where
    /// a following `in` is the iteration separator rather than a comparison
    /// operator (`for i in xs` keeps `i` the loop variable, not `(i in xs)`).
    no_word_op: bool,
    /// Suppress the `where` clause. Set only while parsing a `where` bound, so a
    /// chain stays left-nested (`A where B where C` ⇒ `(where (where A B) C)`,
    /// not right-nested) and the bound captures only comparison-and-tighter
    /// (mirrors JuliaSyntax's `where_enabled=false` inside `parse_where_chain`).
    no_where: bool,
    /// Suppress the `::` annotation pulling a trailing `where` into its right
    /// operand. Set only for the top level of a long-form `function`/`macro`
    /// signature, where the return type is a bare call-level type and a trailing
    /// `where` binds the whole signature (`function f()::S where T end` ⇒
    /// `(function (where (::-i (call f) S) T) …)`), unlike a value-position `::`
    /// (`f(x)::T where U` ⇒ `(::-i (call f x) (where T U))`). Resets inside
    /// brackets, so argument annotations still capture their own `where`.
    no_decl_where: bool,
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

/// Binding powers for numeric-literal-coefficient juxtaposition (`2x`, `(x-1)y`,
/// `1√x`). Julia binds juxtaposition tighter than `*`/`//`/`<<` but looser than
/// `^`: `2x^2` ⇒ `(juxtapose 2 (x^2))` (left binds into a following `^`), while
/// `2^2x` ⇒ `2^(2x)` (it binds into `^`'s right operand). So the left power must
/// out-bind `^`'s right (`33`), and the right operand captures only `^` (`34`)
/// and tighter — keeping `*` (`24`), `//` (`28`), and `where` (`31`) out.
/// Right-associative (`L > R`), like `^`.
const JUXTAPOSE_L: u8 = 34;
const JUXTAPOSE_R: u8 = 33;

/// Binding power gate for the `where` clause. `where` binds tighter than every
/// binary operator (so `A << B where C` ⇒ `(call-i A << (where B C))`) but looser
/// than `^`/juxtaposition/`.` (so `A^B where C` ⇒ `(where (call-i A ^ B) C)`),
/// matching JuliaSyntax, where `parse_where` sits between `parse_shift` and
/// `parse_juxtapose`. The chain fires whenever `WHERE_BP >= min_bp`; the shift
/// tier's right power is `31`, so this is `31` while `^`/juxtaposition sit at
/// `33`/`34`. The `::` annotation captures its own trailing `where` separately
/// (see the operator loop), since it parses its operands through `parse_where`.
const WHERE_BP: u8 = 31;

/// The precedence at which a `where` bound is parsed (JuliaSyntax parses it with
/// `parse_comparison`): the comparison tier's left power, so the bound captures a
/// `<:`/`>:`/comparison operator and everything tighter (`A where T<:S` ⇒
/// `(where A (<: T S))`) but stops before `&&`/`||`/`->`/`=`.
const WHERE_BOUND_BP: u8 = 10;

/// Binding powers for the word operators `in`/`isa`. They share the comparison
/// tier (the symbolic comparisons `< == …` are `(10, 11)`) and are
/// left-associative.
const WORD_OP_L: u8 = 10;
const WORD_OP_R: u8 = 11;

/// The loose end of the precedence range at which a statement-level bare comma
/// builds a tuple. A comma binds *tighter* than assignment (`=` at `(2, 1)`), so
/// `a, b = c, d` ⇒ `(= (tuple a b) (tuple c d))`: the tuple forms first and the
/// assignment binds the two tuples. It fires only while parsing at `min_bp <=
/// COMMA_BP` (toplevel `0`, an assignment right-hand side `1`), so it stays inert
/// once inside a comma item. Each item is parsed at `COMMA_ITEM_BP` — one tighter
/// — which excludes assignment (`2 < 3`) and the comma itself but keeps the
/// ternary (`3`) and everything tighter (`a => b, c` ⇒ `(tuple (=> a b) c)`).
const COMMA_BP: u8 = 2;
const COMMA_ITEM_BP: u8 = 3;

/// Left binding power of the postfix splat/vararg `...`. JuliaSyntax parses `...`
/// between `parse_pipe_lt` and `parse_range`, so it binds looser than the
/// colon/range tier (`x:y...` ⇒ `(... (call-i x : y))`) but tighter than the
/// pipes and everything looser (`a|>b...` ⇒ `(call-i a |> (... b))`,
/// `a&&b...` ⇒ `(&& a (... b))`). Colon's right power is `15` and `|>`'s is `14`,
/// so a left power of `14` binds inside a pipe's right operand (`14 >= 14`) but
/// not inside colon's (`14 < 15`).
const SPLAT_BP: u8 = 14;

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
    parse_block_stmt(tokens, start, true, diagnostics)
}

/// Parse one statement inside a block body, where a top-level comma builds a
/// bare-comma tuple (`a, b` ⇒ `(tuple a b)`). `public_context` is true only at
/// toplevel/module scope (where `public` opens a `PUBLIC_STMT`), false in inner
/// blocks (where `public` stays an ordinary identifier).
pub(crate) fn parse_block_stmt(
    tokens: &[Token],
    start: usize,
    public_context: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        public_context,
        stmt_comma: true,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, 0, diagnostics, flags)
}

/// Parse a `for`-loop binding (`for i in xs`), where a following `in`/`isa` is
/// the iteration separator handled by the caller rather than a comparison
/// operator. The `=` form (`for i = 1:3`) is still parsed whole as an
/// `ASSIGNMENT_EXPR`. See [`ExprFlags::no_word_op`].
pub(crate) fn parse_for_binding(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        no_word_op: true,
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

/// Parse the top-level signature of a long-form `function`/`macro` definition.
/// Like [`parse_expr`] but with `no_decl_where` set, so a `::` return type stays
/// a bare call-level annotation and a trailing `where` binds the whole signature
/// (`function f()::S where T end` ⇒ `(where (::-i (call f) S) T)`), matching
/// JuliaSyntax's `parse_function_signature`.
pub(crate) fn parse_signature_expr(
    tokens: &[Token],
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let flags = ExprFlags {
        no_decl_where: true,
        ..ExprFlags::default()
    };
    parse_expr_in(tokens, start, 0, diagnostics, flags)
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
        stmt_comma,
        no_word_op,
        no_where,
        no_decl_where,
    } = flags;
    let ctx = ParserCtx::new(tokens);

    // The contextual keyword `public` (a plain identifier elsewhere) opens a
    // `PUBLIC_STMT` at toplevel/module-block statement position, *unless* the next
    // significant token is `(`, `=`, or `[` — those keep `public` an identifier
    // (a call `public(x)`, an assignment `public = 1`, an index `public[i]`),
    // matching JuliaSyntax's `parse_public` compatibility shim.
    if public_context && is_public_keyword(&ctx, start) {
        return parse_name_list_stmt(tokens, start, SyntaxKind::PUBLIC_STMT, diagnostics);
    }

    // Value-producing block forms (`begin…end`, `if`, `for`, `while`, `let`,
    // `try`, `function`/`macro`, `quote`, `struct`, `module`, and the contextual
    // `abstract type`/`primitive type` declarations) are operands: Julia lets a
    // trailing infix operator take the whole block form as its left side
    // (`begin x end::T` ⇒ `(::-i (block x) T)`, `if c x end + 1`). So they fall
    // through into the operator loop as `lhs` rather than returning early, with
    // postfix chaining and juxtaposition suppressed (Julia errors on `begin x
    // end(y)` / `begin x end y`). Inside an indexing `a[…]` a leading `begin` is
    // instead the index-begin marker (handled in `parse_prefix`).
    //
    // The contextual `abstract`/`primitive` words (ordinary identifiers
    // elsewhere) open a type declaration only when immediately followed by the
    // contextual `type`; the pair of adjacent identifiers is unambiguous, so this
    // fires in any expression position (`x = abstract type A end`).
    let block_form = if let Some(decl_word) = type_decl_keyword(&ctx, start) {
        Some(match decl_word {
            TypeDecl::Abstract => parse_abstract_type(tokens, start, diagnostics),
            TypeDecl::Primitive => parse_primitive_type(tokens, start, diagnostics),
        })
    } else {
        match ctx.token(start).map(|t| t.kind) {
            Some(TokKind::IfKw) => Some(parse_if_expr(tokens, start, diagnostics)),
            Some(TokKind::FunctionKw) => Some(parse_function_expr(tokens, start, diagnostics)),
            Some(TokKind::MacroKw) => Some(parse_macro_def(tokens, start, diagnostics)),
            Some(TokKind::BeginKw) if !begin_marker => {
                Some(parse_begin_expr(tokens, start, diagnostics))
            }
            Some(TokKind::QuoteKw) => Some(parse_quote_expr(tokens, start, diagnostics)),
            Some(TokKind::WhileKw) => Some(parse_while_expr(tokens, start, diagnostics)),
            Some(TokKind::ForKw) => Some(parse_for_expr(tokens, start, diagnostics)),
            Some(TokKind::LetKw) => Some(parse_let_expr(tokens, start, diagnostics)),
            Some(TokKind::TryKw) => Some(parse_try_expr(tokens, start, diagnostics)),
            Some(TokKind::StructKw | TokKind::MutableKw) => {
                Some(parse_struct_expr(tokens, start, diagnostics))
            }
            Some(TokKind::ModuleKw | TokKind::BaremoduleKw) => {
                Some(parse_module_expr(tokens, start, diagnostics))
            }
            _ => None,
        }
    };

    // Statement keywords consume their own operand through the expression loop
    // internally, so they return directly (`return x::T` ⇒ `(return (::-i x T))`,
    // not `(::-i (return x) T)`).
    if block_form.is_none() {
        match ctx.token(start).map(|t| t.kind) {
            Some(TokKind::ReturnKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::RETURN_EXPR,
                    KwStmt::ExprTuple,
                    true,
                    diagnostics,
                );
            }
            Some(TokKind::BreakKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::BREAK_EXPR,
                    KwStmt::Bare,
                    false,
                    diagnostics,
                );
            }
            Some(TokKind::ContinueKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::CONTINUE_EXPR,
                    KwStmt::Bare,
                    false,
                    diagnostics,
                );
            }
            Some(TokKind::ConstKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::CONST_STMT,
                    KwStmt::ExprTuple,
                    false,
                    diagnostics,
                );
            }
            Some(TokKind::GlobalKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::GLOBAL_STMT,
                    KwStmt::Expr,
                    false,
                    diagnostics,
                );
            }
            Some(TokKind::LocalKw) => {
                return parse_keyword_stmt(
                    tokens,
                    start,
                    SyntaxKind::LOCAL_STMT,
                    KwStmt::Expr,
                    false,
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
                return parse_name_list_stmt(tokens, start, SyntaxKind::EXPORT_STMT, diagnostics);
            }
            _ => {}
        }
    }

    // A block form is an operand whose own postfix (`.f`, `(y)`, `[y]`, `{T}`) and
    // juxtaposition are errors in Julia; only infix operators take it as a left
    // side. `lhs_is_block_keyword` suppresses those two checks for the bare block
    // form (the first loop iteration) and is cleared once any operator builds a
    // binary node on top of it.
    let (mut lhs, mut lhs_is_block_keyword) = match block_form {
        Some(parsed) => (parsed?, true),
        None => (parse_prefix(&ctx, start, diagnostics, flags)?, false),
    };

    loop {
        if !lhs_is_block_keyword {
            lhs = parse_postfix_chain(&ctx, lhs, array_mode, diagnostics);
        }

        // Invalid string juxtaposition (`"a"x`, `"a""b"`, `2"a"`): a string
        // literal glued to another term (or a term glued to a string) is an error
        // in Julia, recovered as `(juxtapose lhs (error-t) rhs)`. Checked before
        // the numeric case so a string operand takes the error-bearing shape; the
        // right operand is parsed identically (at `JUXTAPOSE_R`).
        if !lhs_is_block_keyword
            && should_juxtapose_string_error(&ctx, &lhs, min_bp)
            && let Some(rhs) = parse_expr_in(tokens, lhs.end, JUXTAPOSE_R, diagnostics, flags)
        {
            let pos = tokens[lhs.end - 1].end;
            push_diagnostic(
                diagnostics,
                DiagnosticKind::StringJuxtapose,
                "invalid juxtaposition",
                pos,
                pos,
            );
            lhs = build_binary(SyntaxKind::JUXTAPOSE_EXPR, lhs, rhs);
            continue;
        }

        // Numeric-literal-coefficient juxtaposition (`2x`, `2(x)`, `(x-1)y`,
        // `1√x`): an adjacent value with no operator between is an implicit
        // multiplication binding tighter than `*` and looser than `^`. The right
        // operand is parsed at `JUXTAPOSE_R` (capturing a trailing `^` but not a
        // `*`), and the whole thing re-enters the loop so a following operator
        // (`2x*y` ⇒ `(2x)*y`) attaches outside.
        if !lhs_is_block_keyword
            && should_juxtapose(&ctx, &lhs, min_bp)
            && let Some(rhs) = parse_expr_in(tokens, lhs.end, JUXTAPOSE_R, diagnostics, flags)
        {
            lhs = build_binary(SyntaxKind::JUXTAPOSE_EXPR, lhs, rhs);
            continue;
        }

        // Past the postfix/juxtaposition checks the bare block form is fully
        // formed; any further iterations see an ordinary operand.
        lhs_is_block_keyword = false;

        // Splat/vararg `x...` is a postfix operator (left power `SPLAT_BP = 14`),
        // not part of the postfix chain: it binds looser than the colon/range
        // tier (`x:y...` ⇒ `(... (call-i x : y))`) but tighter than the pipes
        // and everything looser (`a|>b...` ⇒ `(call-i a |> (... b))`). It wraps
        // `lhs` and re-loops; `...` is not in `is_operator`, so when it does not
        // bind (`SPLAT_BP < min_bp`, e.g. inside colon's right operand) the loop
        // simply breaks and an enclosing parse consumes it.
        if SPLAT_BP >= min_bp {
            let splat_idx = ctx.skip_ws(lhs.end);
            if ctx.token(splat_idx).map(|t| t.kind) == Some(TokKind::DotDotDot) {
                let mut events = vec![Event::Start(SyntaxKind::SPLAT_EXPR)];
                events.extend(lhs.events);
                push_range(&mut events, lhs.end, splat_idx);
                events.push(Event::Tok(splat_idx));
                events.push(Event::Finish);
                lhs = ExprParse {
                    start: lhs.start,
                    end: splat_idx + 1,
                    events,
                };
                continue;
            }
        }

        // `where` clause: a left-associative chain (`A where B where C` ⇒
        // `(where (where A B) C)`) binding tighter than every binary operator but
        // looser than `^`/juxtaposition/`.` (handled above). Each bound is parsed
        // at comparison precedence with `where` itself suppressed (`no_where`), so
        // `A where T<:S` captures the `<:` bound while a trailing `where` stays in
        // this chain. Suppressed while parsing a bound. The `::` annotation
        // captures its own trailing `where` (below), so a `where` reaching here
        // belongs to `lhs`, not to a pending `::` right operand.
        if !no_where
            && WHERE_BP >= min_bp
            && let Some((where_idx, TokKind::WhereKw)) =
                next_operator(&ctx, lhs.end, inside_brackets)
        {
            lhs = parse_where_chain(tokens, &ctx, lhs, where_idx, diagnostics, flags);
            continue;
        }

        // Statement-level bare-comma tuple: at statement scope a top-level comma
        // collects the operands into a `BARE_TUPLE_EXPR`. Comma is not a Pratt
        // operator (it never reaches `next_operator`); it is handled here, looser
        // than every real operator but tighter than assignment, so a following `=`
        // binds the whole tuple (`a, b = c, d` ⇒ `(= (tuple a b) (tuple c d))`).
        // The `min_bp` guard keeps it inert once we are inside a comma item.
        if stmt_comma
            && min_bp <= COMMA_BP
            && ctx.token(ctx.skip_ws(lhs.end)).map(|t| t.kind) == Some(TokKind::Comma)
        {
            lhs = parse_comma_tuple(tokens, &ctx, lhs, diagnostics, flags);
            continue;
        }

        // Word operators `in`/`isa` (lexed as identifiers) act as infix operators
        // at comparison precedence (`i in rhs` ⇒ `(call-i i in rhs)`, `x isa T` ⇒
        // `(call-i x isa T)`). Like the comparison operators, they are
        // left-associative and chains stay nested (a recorded modeling
        // divergence). Suppressed while parsing a loop variable, where `in` is the
        // for-spec separator. Checked after juxtaposition (so an adjacent `2in`
        // still juxtaposes) and before the symbolic operators.
        if !no_word_op && let Some(op_idx) = word_operator(&ctx, lhs.end, inside_brackets) {
            if WORD_OP_L < min_bp {
                break;
            }
            let rhs_operand = ctx.skip_trivia(op_idx + 1);
            let Some(rhs) = parse_expr_in(tokens, rhs_operand, WORD_OP_R, diagnostics, flags)
            else {
                let op = &tokens[op_idx];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::MissingOperand,
                    "expected right-hand side for operator",
                    op.start,
                    op.end,
                );
                return Some(error_expr_to_line_end(tokens, lhs.start, op_idx + 1));
            };
            lhs = build_binary(SyntaxKind::BINARY_EXPR, lhs, rhs);
            continue;
        }

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

        // Range `:` collapses a stepped chain into a single 3-operand call
        // (`a:b:c` ⇒ `(call-i a : b c)`, `a:b:c:d:e` ⇒ `(call-i (call-i a : b c)
        // : d e)`), exactly as JuliaSyntax's `parse_range`, so it is handled
        // before the generic left-associative path.
        if op_kind == TokKind::Colon {
            let (l_bp, _) = infix_binding_power(TokKind::Colon).expect("colon binds");
            if l_bp < min_bp {
                break;
            }
            lhs = parse_colon_range(tokens, &ctx, lhs, op_idx, diagnostics, flags);
            continue;
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
        // Field access `a.b`: the right operand is an atom (the field name), not a
        // postfix-chained expression. A trailing `()`/`[]`/`{}` binds to the whole
        // field access (`A.f()` is `(A.f)()`, `a.b{T}` is `(a.b){T}`), so parse the
        // RHS prefix-only and let the outer postfix chain attach any suffix. Other
        // operators parse a full right operand at their binding power.
        let rhs_result = if op_kind == TokKind::Dot {
            parse_prefix(&ctx, rhs_operand, diagnostics, flags)
        } else {
            parse_expr_in(tokens, rhs_operand, r_bp, diagnostics, flags)
        };
        let Some(mut rhs) = rhs_result else {
            let op = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingOperand,
                "expected right-hand side for operator",
                op.start,
                op.end,
            );
            // JuliaSyntax keeps the operator node and synthesizes a zero-width
            // `(error)` for the absent operand (`x =` ⇒ `(= x (error))`, `a +`
            // ⇒ `(call-i a + (error))`, `a &&` ⇒ `(&& a (error))`) rather than
            // discarding the whole construct. Build the node with only the LHS
            // and the operator; the projector replays the `(error)` from the
            // `MissingOperand` diagnostic anchored at the operator.
            lhs = build_binary_missing_rhs(operator_node_kind(op_kind), lhs, rhs_operand);
            continue;
        };

        // A `::` annotation captures a trailing `where` in its right operand
        // (JuliaSyntax parses the annotation through `parse_where`): `A::B where C`
        // ⇒ `(:: A (where B C))`. `where` binds tighter than `::` itself, so the
        // chain wraps the annotation type, not the whole `::`. Suppressed inside a
        // `where` bound, where `::` does not pull in a following `where`.
        if op_kind == TokKind::ColonColon
            && !no_where
            && !no_decl_where
            && let Some((where_idx, TokKind::WhereKw)) =
                next_operator(&ctx, rhs.end, inside_brackets)
        {
            rhs = parse_where_chain(tokens, &ctx, rhs, where_idx, diagnostics, flags);
        }

        let node = operator_node_kind(op_kind);
        // Whitespace before a field-access dot is disallowed: JuliaSyntax keeps
        // the `(. lhs (quote rhs))` shape but flags it (`x .y` ⇒
        // `(. x (error-t) (quote y))`). We record a `DotWhitespace` diagnostic at
        // the dot's end; the projector replays the `(error-t)`. A broadcast
        // operator `.+` lexes as a single token (not `Dot`), so this never fires
        // for `a .+ b`.
        if op_kind == TokKind::Dot && op_idx > lhs.end {
            let op = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::DotWhitespace,
                "whitespace before `.`",
                op.end,
                op.end,
            );
        }
        lhs = build_binary(node, lhs, rhs);
    }

    Some(lhs)
}

/// Consume a left-associative `where` chain onto `lhs`, starting at the `where`
/// token `where_idx`. Each iteration parses the bound at `WHERE_BOUND_BP`
/// (comparison precedence) with `where` suppressed, then wraps the running
/// expression in a `WHERE_EXPR` (`A where B where C` ⇒ `(where (where A B) C)`).
/// Mirrors JuliaSyntax's `parse_where_chain` (`while peek == where`, the bound
/// parsed by `parse_comparison` with `where_enabled=false`).
fn parse_where_chain(
    tokens: &[Token],
    ctx: &ParserCtx<'_>,
    mut lhs: ExprParse,
    mut where_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> ExprParse {
    let bound_flags = ExprFlags {
        no_where: true,
        ..flags
    };
    loop {
        let bound_start = ctx.skip_trivia(where_idx + 1);
        let Some(bound) = parse_expr_in(
            tokens,
            bound_start,
            WHERE_BOUND_BP,
            diagnostics,
            bound_flags,
        ) else {
            let op = &tokens[where_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingWhereBound,
                "expected type bound after `where`",
                op.start,
                op.end,
            );
            return error_expr_to_line_end(tokens, lhs.start, where_idx + 1);
        };
        lhs = build_binary(SyntaxKind::WHERE_EXPR, lhs, bound);
        match next_operator(ctx, lhs.end, flags.inside_brackets) {
            Some((idx, TokKind::WhereKw)) => where_idx = idx,
            _ => return lhs,
        }
    }
}

/// Whether the identifier `public` at `start` opens a `PUBLIC_STMT`. True when
/// the token is the identifier `public` and the next significant token exists and
/// is not `(`, `=`, or `[` — those three keep `public` an ordinary identifier (a
/// call, assignment, or index), matching JuliaSyntax's `parse_public`.
enum TypeDecl {
    Abstract,
    Primitive,
}

/// Detect a contextual `abstract type`/`primitive type` opener: an identifier
/// `abstract`/`primitive` immediately followed (across trivia only) by the
/// identifier `type`. Returns `None` for the plain-identifier uses (`abstract`,
/// `abstract = 1`, `abstract(x)`).
fn type_decl_keyword(ctx: &ParserCtx<'_>, start: usize) -> Option<TypeDecl> {
    let word = match ctx.token(start) {
        Some(t) if t.kind == TokKind::Ident && t.text == "abstract" => TypeDecl::Abstract,
        Some(t) if t.kind == TokKind::Ident && t.text == "primitive" => TypeDecl::Primitive,
        _ => return None,
    };
    let next = ctx.token(ctx.skip_trivia(start + 1))?;
    (next.kind == TokKind::Ident && next.text == "type").then_some(word)
}

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

/// The CST node kind for an infix operator: an assignment, an anonymous-function
/// `->`, a `::` type annotation, or a plain binary expression.
fn operator_node_kind(op_kind: TokKind) -> SyntaxKind {
    match op_kind {
        k if is_assignment_op(k) => SyntaxKind::ASSIGNMENT_EXPR,
        TokKind::Arrow => SyntaxKind::ARROW_EXPR,
        TokKind::ColonColon => SyntaxKind::TYPE_ANNOTATION,
        _ => SyntaxKind::BINARY_EXPR,
    }
}

/// Build an operator node whose right operand is absent: `lhs`, then the gap
/// (whitespace + operator + trailing trivia) up to `gap_end`, and no RHS. The
/// projector replays JuliaSyntax's zero-width `(error)` operand from the
/// `MissingOperand` diagnostic recorded at the operator.
fn build_binary_missing_rhs(kind: SyntaxKind, lhs: ExprParse, gap_end: usize) -> ExprParse {
    let mut events = vec![Event::Start(kind)];
    let start = lhs.start;
    events.extend(lhs.events);
    push_range(&mut events, lhs.end, gap_end);
    events.push(Event::Finish);
    ExprParse {
        start,
        end: gap_end,
        events,
    }
}

/// Collect a statement-level bare-comma tuple. The first operand `first` is
/// already parsed and the caller has confirmed a comma follows it. Each further
/// operand is parsed at [`COMMA_ITEM_BP`] (so it stops before the next comma and
/// before any assignment), and the comma tokens and surrounding trivia are kept
/// in the gaps. A trailing comma with no operand after it (`x, = xs` ⇒
/// `(tuple x)`, `x, y, = a`) leaves a tuple with the operands gathered so far,
/// mirroring JuliaSyntax's `parse_comma`.
fn parse_comma_tuple(
    tokens: &[Token],
    ctx: &ParserCtx<'_>,
    first: ExprParse,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> ExprParse {
    let start = first.start;
    let mut events = vec![Event::Start(SyntaxKind::BARE_TUPLE_EXPR)];
    let mut end = first.end;
    events.extend(first.events);

    loop {
        let comma_idx = ctx.skip_ws(end);
        if ctx.token(comma_idx).map(|t| t.kind) != Some(TokKind::Comma) {
            break;
        }
        push_range(&mut events, end, comma_idx);
        events.push(Event::Tok(comma_idx));
        end = comma_idx + 1;

        let item_start = ctx.skip_ws(end);
        // A trailing comma before an assignment-family operator is a 1-tuple the
        // assignment then binds (`x, = xs` ⇒ `(= (tuple x) xs)`): the operator is
        // not a tuple element, so stop here and let the operator loop take it,
        // rather than collecting it as a `(error op)` atom.
        if ctx
            .token(item_start)
            .is_some_and(|t| is_lone_error_operator(t.kind))
        {
            break;
        }
        match parse_expr_in(tokens, item_start, COMMA_ITEM_BP, diagnostics, flags) {
            Some(item) => {
                push_range(&mut events, end, item.start);
                events.extend(item.events);
                end = item.end;
            }
            // Trailing comma: nothing follows that can start an operand.
            None => break,
        }
    }

    events.push(Event::Finish);
    ExprParse { start, end, events }
}

/// Build a stepped range `(a : b : c)` from its three operands, capturing the two
/// colon tokens (and surrounding trivia) in the gaps between operands.
fn build_range3(a: ExprParse, b: ExprParse, c: ExprParse) -> ExprParse {
    let mut events = vec![Event::Start(SyntaxKind::RANGE_EXPR)];
    events.extend(a.events);
    push_range(&mut events, a.end, b.start);
    events.extend(b.events);
    push_range(&mut events, b.end, c.start);
    events.extend(c.events);
    events.push(Event::Finish);
    ExprParse {
        start: a.start,
        end: c.end,
        events,
    }
}

/// Parse a range `:` chain starting at the colon `first_colon` (the first operand
/// `lhs` is already parsed and the caller has cleared the binding-power check).
/// Mirrors JuliaSyntax's `parse_range`: every second colon folds three operands
/// into one `RANGE_EXPR` (`a:b:c`), and a further colon nests the folded range as
/// the left operand of the next chain (`(a:b:c):d:e`). An odd trailing colon
/// (`a:b:c:d`) leaves an ordinary two-operand `BINARY_EXPR`.
fn parse_colon_range(
    tokens: &[Token],
    ctx: &ParserCtx<'_>,
    lhs: ExprParse,
    first_colon: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
    flags: ExprFlags,
) -> ExprParse {
    let (_, r_bp) = infix_binding_power(TokKind::Colon).expect("colon binds");
    let mut head = lhs;
    // The operand awaiting a step partner (JuliaSyntax's open colon count).
    let mut step: Option<ExprParse> = None;
    let mut op_idx = first_colon;
    loop {
        let rhs_operand = ctx.skip_trivia(op_idx + 1);
        let Some(rhs) = parse_expr_in(tokens, rhs_operand, r_bp, diagnostics, flags) else {
            let op = &tokens[op_idx];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingOperand,
                "expected right-hand side for operator",
                op.start,
                op.end,
            );
            return error_expr_to_line_end(tokens, head.start, op_idx + 1);
        };
        let last_end = rhs.end;
        match step.take() {
            Some(mid) => head = build_range3(head, mid, rhs),
            None => step = Some(rhs),
        }
        // Continue the chain only on another range colon at the same level: not a
        // ternary separator (`no_range`) and not an array-element boundary
        // (`[1 :2]` splits into elements rather than ranging).
        let continues = match next_operator(ctx, last_end, flags.inside_brackets) {
            Some((idx, TokKind::Colon)) if !flags.no_range => {
                let split = flags.array_mode && array_element_boundary(ctx, last_end, idx);
                (!split).then_some(idx)
            }
            _ => None,
        };
        match continues {
            Some(idx) => op_idx = idx,
            None => break,
        }
    }
    match step {
        Some(mid) => build_binary(SyntaxKind::BINARY_EXPR, head, mid),
        None => head,
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
        // Signed numeric literal: a `+`/`-` glued to an adjacent number folds into
        // a single signed literal rather than a unary prefix call (`-2` → `-2`,
        // `+2.0` → `2.0`, `-2*x` → `(call-i -2 * x)`). Mirrors JuliaSyntax
        // `parse_unary`; see `signed_literal_fold` for the exact conditions.
        k if matches!(k, TokKind::Plus | TokKind::Minus) && signed_literal_fold(ctx, start) => {
            let num = start + 1;
            Some(ExprParse {
                start,
                end: num + 1,
                events: vec![
                    Event::Start(SyntaxKind::LITERAL),
                    Event::Tok(start),
                    Event::Tok(num),
                    Event::Finish,
                ],
            })
        }
        // Prefix operators: arithmetic/logical unary (`-x`, `!x`), the address-of
        // `&x` (a syntactic prefix heading the node with `&`, not `call-pre`),
        // lower-bound type expressions (`<:Real` in `Array{<:Real}`), and unary
        // `::` declarations (`::Int` in a method signature `f(::Int)`).
        TokKind::Plus
        | TokKind::Minus
        | TokKind::DotPlus
        | TokKind::DotMinus
        | TokKind::Bang
        | TokKind::Tilde
        | TokKind::DotTilde
        | TokKind::Amp
        | TokKind::Subtype
        | TokKind::Supertype
        | TokKind::ColonColon
        // Prefix-only Unicode radicals `√ ∛ ∜` and logical-not `¬`: a unary
        // application heading a `UNARY_EXPR` (`√x` → `(call-pre √ x)`), with the
        // same precedence as `-`/`+` (binds looser than `^`, tighter than `*`).
        | TokKind::UniRadical => {
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
            // A value-form prefix operator (`+ - ! ~ <: >: .+ .- .~`, the
            // radicals) directly followed by a bare `=` is not a prefix call:
            // the operator is used as a value and `=` is the assignment
            // (`<: =` ⇒ `(= <: (error))`, `+ = x` ⇒ `(= + x)`). The purely
            // syntactic prefixes `&`/`::` instead consume the `=` as an error
            // operand (`& =` ⇒ `(& (error =))`), so they are excluded. Fall
            // through to the bare-value atom; the operator loop forms the
            // assignment (and `error_operator_atom`'s `=` RHS, or its absence).
            if ctx.token(operand_start).map(|t| t.kind) == Some(TokKind::Eq)
                && !matches!(tok.kind, TokKind::Amp | TokKind::ColonColon)
            {
                return Some(atom(SyntaxKind::OPERATOR_ATOM, start));
            }
            // The type operators `<:`/`>:` parse their operand at the `where`
            // tier so a trailing `where` clause attaches to the operand rather
            // than the whole prefix (`<: A where B` ⇒ `(<:-pre (where A B))`,
            // JuliaSyntax issue #21545); the arithmetic/logical prefixes keep the
            // tighter `PREFIX_BP` and suppress `where` in their operand, so a
            // trailing `where` binds the whole prefix instead (`+ <: A where B` ⇒
            // `(where (call-pre + (<:-pre A)) B)`, mirroring JuliaSyntax's
            // `parse_unary` operand sitting below `parse_where`).
            let is_subtype = matches!(tok.kind, TokKind::Subtype | TokKind::Supertype);
            // PREFIX_BP is above the range colon and the array-element boundary
            // only matters at low binding powers, so neither `no_range` nor
            // `array_mode` changes the operand here; carry the rest through.
            let operand_flags = ExprFlags {
                no_range: false,
                array_mode: false,
                no_where: !is_subtype || flags.no_where,
                ..flags
            };
            let operand_bp = if is_subtype { WHERE_BP } else { PREFIX_BP };
            let Some(operand) = parse_expr_in(
                ctx.tokens(),
                operand_start,
                operand_bp,
                diagnostics,
                operand_flags,
            ) else {
                // A bare prefix operator with no operand is the operator used as
                // a value atom (`+` → `+`, `<:` → `<:`, `.+` → `(. +)`). The
                // syntactic `::` has no value form and stays an error (Julia:
                // `::` → `(::-pre (error))`).
                if tok.kind == TokKind::ColonColon {
                    return Some(error_expr_with_range(start, start + 1));
                }
                return Some(atom(SyntaxKind::OPERATOR_ATOM, start));
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
        // A bare `:` not followed by something quotable (`a[:]`, `[:]`, a lone
        // `:`) is the Colon value atom, not a quote: `parse_quote_sym` returns
        // `None` and we fall through to an `OPERATOR_ATOM` (`a[:]` ⇒ `(ref a :)`,
        // `:` ⇒ `:`). Without the fallthrough the bare `:` token is dropped by the
        // projector's delimiter filter.
        TokKind::Colon => parse_quote_sym(ctx, start, diagnostics)
            .or_else(|| Some(atom(SyntaxKind::OPERATOR_ATOM, start))),
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
        // A lone syntactic operator (`=`, an assignment op, `&&`/`||`/`->`/`...`)
        // has no value meaning, so JuliaSyntax emits `(error op)` wherever an atom
        // is expected (`=` ⇒ `(error =)`, `.+=` ⇒ `(error (. +=))`, `[=]` ⇒
        // `(vect (error =))`). It consumes only the operator; any following operand
        // is left to the caller — the toplevel trailing-junk driver
        // (`= x` ⇒ `(error =) (error-t x)`) or the operator loop's RHS
        // (`a + =` ⇒ `(call-i a + (error =))`). Unlike `?`/binary-only operators
        // below, it never applies as a prefix call.
        k if is_lone_error_operator(k) => {
            let op = &ctx.tokens()[start];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::LoneOperator,
                "operator is not a valid value",
                op.start,
                op.end,
            );
            Some(error_operator_atom(start))
        }
        // A binary-only operator in prefix position is invalid: JuliaSyntax
        // error-wraps the operator and applies it as a prefix call
        // (`/x` ⇒ `(call-pre (error /) x)`, `.*x` ⇒ `(dotcall-pre (error (. *)) x)`,
        // `?x` ⇒ `(call-pre (error ?) x)`). With nothing parseable following, a
        // value operator stays a bare value atom (`*` ⇒ `*`, `.&` ⇒ `(. &)`,
        // `=>` ⇒ `=>`) but a bare `?` is itself the error (`?` ⇒ `(error ?)`); the
        // unary value operators (`+ - ! ~ <: >:`) are folded above and never reach
        // here. Lone syntactic operators are handled by the arm above.
        k if is_value_operator(k) || k == TokKind::Question => {
            let operand_start = ctx.skip_trivia(start + 1);
            // A value operator directly followed by a bare `=` is its value form
            // with `=` the assignment, not an invalid prefix call (`* =` ⇒
            // `(= * (error))`, `/ = x` ⇒ `(= / x)`). `?` is excluded — it keeps
            // its prefix-call handling. Fall through to the bare-value atom.
            if k != TokKind::Question
                && ctx.token(operand_start).map(|t| t.kind) == Some(TokKind::Eq)
            {
                return Some(atom(SyntaxKind::OPERATOR_ATOM, start));
            }
            // The operand binds at `PREFIX_BP` — tighter than the arithmetic
            // tiers (`/x + y` ⇒ `(call-i (call-pre (error /) x) + y)`) but below
            // `^` (`/x^2` ⇒ `(call-pre (error /) (call-i x ^ 2))`). The
            // array-element boundary never applies to a prefix operand.
            let operand_flags = ExprFlags {
                no_range: false,
                array_mode: false,
                ..flags
            };
            match parse_expr_in(
                ctx.tokens(),
                operand_start,
                PREFIX_BP,
                diagnostics,
                operand_flags,
            ) {
                Some(operand) => {
                    let op = &ctx.tokens()[start];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::InvalidPrefixOperator,
                        "invalid operator in prefix position",
                        op.start,
                        op.end,
                    );
                    // Wrap the operator in an `ERROR` (an `OPERATOR_ATOM` so a
                    // broadcast operator still projects to `(. op)`), then the
                    // operand, under a `UNARY_EXPR` the projector renders as a
                    // prefix call with the error-wrapped operator as callee.
                    let mut events = vec![
                        Event::Start(SyntaxKind::UNARY_EXPR),
                        Event::Start(SyntaxKind::ERROR),
                        Event::Start(SyntaxKind::OPERATOR_ATOM),
                        Event::Tok(start),
                        Event::Finish, // OPERATOR_ATOM
                        Event::Finish, // ERROR
                    ];
                    push_range(&mut events, start + 1, operand.start);
                    events.extend(operand.events);
                    events.push(Event::Finish); // UNARY_EXPR
                    Some(ExprParse {
                        start,
                        end: operand.end,
                        events,
                    })
                }
                None if matches!(ctx.tokens()[start].kind, TokKind::Question) => {
                    let op = &ctx.tokens()[start];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::LoneOperator,
                        "operator is not a valid value",
                        op.start,
                        op.end,
                    );
                    Some(error_operator_atom(start))
                }
                None => Some(atom(SyntaxKind::OPERATOR_ATOM, start)),
            }
        }
        _ => None,
    }
}

/// An operator token wrapped as `ERROR > OPERATOR_ATOM > op` — JuliaSyntax's
/// `(error op)` atom for a syntactic operator used where a value is expected. The
/// `OPERATOR_ATOM` keeps the broadcast projection (`.+=` ⇒ `(. +=)`).
fn error_operator_atom(start: usize) -> ExprParse {
    ExprParse {
        start,
        end: start + 1,
        events: vec![
            Event::Start(SyntaxKind::ERROR),
            Event::Start(SyntaxKind::OPERATOR_ATOM),
            Event::Tok(start),
            Event::Finish, // OPERATOR_ATOM
            Event::Finish, // ERROR
        ],
    }
}

/// Whether `kind` is a syntactic operator that has no value meaning and so, where
/// an atom is expected, is JuliaSyntax's `(error op)` — the assignment operators
/// (`=`, `+=`, `.+=`, …), the short-circuits `&&`/`||`, the anonymous-function
/// `->`, and the splat `...`. `?` is *not* here: it applies as a prefix call when
/// an operand follows (handled in the value-operator arm).
fn is_lone_error_operator(kind: TokKind) -> bool {
    use TokKind::*;
    is_assignment_op(kind) || matches!(kind, AndAnd | OrOr | Arrow | DotDotDot)
}

/// Whether `kind` is an operator that, alone in value position, is the operator
/// used as a value atom (`+` → `+`, `.&` → `(. &)`, `:` → `:`). This is the
/// non-syntactic operator set: undotted operator names (minus the syntactic
/// `&&`/`||`/`->`), the broadcast forms, plus `:`/`..` and the Unicode radicals.
/// The erroring syntactic operators (`= :: && || -> ? . ...` and assignment)
/// are excluded — Julia reports them as errors in value position.
fn is_value_operator(kind: TokKind) -> bool {
    use TokKind::*;
    (is_op_name(kind) && !matches!(kind, AndAnd | OrOr | Arrow))
        || matches!(
            kind,
            Colon
                | DotDot
                | UniRadical
                | DotPlus
                | DotMinus
                | DotStar
                | DotSlash
                | DotSlashSlash
                | DotCaret
                | DotPercent
                | DotTilde
                | DotEqEq
                | DotNotEq
                | DotLt
                | DotLe
                | DotGt
                | DotGe
                | DotSubtype
                | DotSupertype
                | DotFatArrow
                | DotLongArrow
                | DotPipeGt
                | DotAmp
                | DotPipe
        )
}

/// Whether `kind` is an operator that a prefix `:` quotes into a symbol but that
/// is *not* already covered by `is_op_name`/`is_assignment_op`: the range `..`,
/// the Unicode operators and radicals, and the ternary `?`. Julia quotes all of
/// these (`:..` ⇒ `(quote-: ..)`, `:√` ⇒ `(quote-: √)`, `:?` ⇒ `(quote-: ?)`).
/// The broadcast dotted operators are handled by their own quote arm; the
/// syntactic sigils `$`/`.`/`...` are deferred (Julia quotes the sigil alone and
/// drops any operand to an `error-t`, an error-shape we don't model yet).
fn is_quotable_operator(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        DotDot
            | UniRadical
            | UniArrow
            | UniComparison
            | UniColon
            | UniPlus
            | UniTimes
            | UniPower
            | Question
    )
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
    // Whitespace (or a newline) between the `:` and the quoted symbol is
    // disallowed: it records a `QuoteColonWhitespace` diagnostic at the `:`'s end,
    // projected as a leading `(error-t)` (`: foo` ⇒ `(quote-: (error-t) foo)`,
    // `A.: +` ⇒ `(. A (quote-: (error-t) +))`). `:foo` (glued) has no diagnostic.
    if next > start + 1 {
        let colon = &ctx.tokens()[start];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::QuoteColonWhitespace,
            "whitespace after `:`",
            colon.end,
            colon.end,
        );
    }
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
        // `:.+`, `:.&`, `:.=`, `:.&&`, `:.+=` — a quoted *dotted* (broadcast)
        // operator. Julia models the dotted operator as a `(. op)` access, so
        // `:.+` ⇒ `(quote-: (. +))`. Wrap the token in an `OPERATOR_ATOM` (Fatou's
        // operator-as-value node), which the projector splits the broadcast dot
        // off of. The `..`/`...` range/splat operators are not broadcasts and
        // fall through to the bare-operator arm below (`:..` ⇒ `(quote-: ..)`).
        _ if ctx
            .token(next)
            .is_some_and(|t| is_dotted_broadcast_text(&t.text)) =>
        {
            events.push(Event::Start(SyntaxKind::OPERATOR_ATOM));
            events.push(Event::Tok(next));
            events.push(Event::Finish); // OPERATOR_ATOM
            events.push(Event::Finish); // QUOTE_SYM
            Some(ExprParse {
                start,
                end: next + 1,
                events,
            })
        }
        // `:+`, `:<:`, `:+=`, `:..`, `:√`, `:⊕`, `:?`, … — a symbolic operator
        // used as a symbol. Covers undotted operator names (`is_op_name`),
        // assignment operators, and the remaining value/syntactic operators Julia
        // still quotes (`..`, the Unicode operators and radicals, the ternary `?`);
        // broadcast forms like `:.+` are handled by the dotted-operator arm above.
        // The token text is emitted verbatim; the projector reads it back.
        k if is_op_name(k) || is_assignment_op(k) || is_quotable_operator(k) => {
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

    // Optional non-standard literal prefix (`r`, `raw`, …). The `var` prefix is
    // special: `var"…"` (single-quoted only) is a non-standard *identifier*, not
    // a string macro — Julia models it as `(var name)`. Triple-quoted `var"""…"""`
    // stays an ordinary `@var_str` macrocall.
    let mut var_prefix = false;
    let mut has_prefix = false;
    if ctx.token(i).map(|t| t.kind) == Some(TokKind::StringPrefix) {
        has_prefix = true;
        var_prefix = ctx.token(i).map(|t| t.text.as_str()) == Some("var");
        i += 1;
    }

    let single_quote_open = matches!(
        ctx.token(i),
        Some(t) if t.kind == TokKind::StringDelimOpen && t.text.len() == 1
    );
    let node = match ctx.token(i).map(|t| t.kind) {
        Some(TokKind::CmdDelimOpen) => SyntaxKind::CMD_LITERAL,
        Some(TokKind::StringDelimOpen) if var_prefix && single_quote_open => {
            SyntaxKind::NONSTANDARD_IDENTIFIER
        }
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
                let suffix = ctx.token(i).map(|t| t.kind);
                // A `var"…"` non-standard identifier takes no flags: a glued
                // suffix (a flag-like alpha run lexed as `StringSuffix`, or a
                // digit-led numeric literal) is junk. Consume it as a child
                // token and record a `StringSuffixSpace` diagnostic (projected
                // `(error-t)`: `var"x"y`/`var"x"1`/`var"x"end` ⇒
                // `(var x (error-t))`). A glued postfix opener (`[ ( { ' .`) or
                // operator is *not* a suffix here — it chains/binds in the outer
                // parser, so only these atom-like kinds trigger recovery.
                if node == SyntaxKind::NONSTANDARD_IDENTIFIER {
                    if matches!(
                        suffix,
                        Some(
                            TokKind::StringSuffix
                                | TokKind::Integer
                                | TokKind::Float
                                | TokKind::Float32
                                | TokKind::BinInt
                                | TokKind::OctInt
                                | TokKind::HexInt
                        )
                    ) {
                        events.push(Event::Tok(i));
                        i += 1;
                        let lit = &ctx.tokens()[start];
                        push_diagnostic(
                            diagnostics,
                            DiagnosticKind::StringSuffixSpace,
                            "invalid string-macro suffix",
                            lit.start,
                            lit.start,
                        );
                    }
                    break;
                }
                // Optional suffix glued after the close delimiter of a string
                // macro: a flag run (`r"pat"ims` → `"ims"`) or a numeric literal
                // (`x"s"2` → an extra `2` macrocall argument). A digit-led suffix
                // is lexed as an ordinary number, so capture it into the literal
                // node here; the projector renders it as the trailing argument.
                let is_flag = suffix == Some(TokKind::StringSuffix);
                let is_numeric = has_prefix
                    && node == SyntaxKind::STRING_LITERAL
                    && matches!(
                        suffix,
                        Some(TokKind::Integer | TokKind::Float | TokKind::Float32)
                    );
                if is_flag || is_numeric {
                    events.push(Event::Tok(i));
                    i += 1;
                }
                break;
            }
            // Unterminated: anything else (incl. EOF) ends the literal. A
            // string/command/`var"…"` literal with no closing delimiter records an
            // `UnterminatedLiteral` diagnostic, projected as a truncation
            // `(error-t)` inside its body (`"str` → `(string "str" (error-t))`,
            // `var"x` → `(var x (error-t))`).
            _ => {
                let lit = &ctx.tokens()[start];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::UnterminatedLiteral,
                    "unterminated literal",
                    lit.start,
                    lit.start,
                );
                break;
            }
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
            // The parenthesized interpolation operand parses exactly like any
            // other parenthesized expression: a single expression (`$(x+y)`) is a
            // `PAREN_EXPR` the projector unwraps, while the multi-value forms
            // `$(x;y)` (`PAREN_BLOCK`), `$(x,y)` (`TUPLE_EXPR`), `$(x for …)`
            // (`GENERATOR`), and the empty `$()` (`TUPLE_EXPR`) are what
            // JuliaSyntax rejects as a `(error …)` interpolation.
            let Some(inner) = parse_paren(ctx, next, diagnostics) else {
                events.push(Event::Finish);
                return next + 1;
            };
            if matches!(
                inner.events.first(),
                Some(Event::Start(
                    SyntaxKind::PAREN_BLOCK | SyntaxKind::TUPLE_EXPR | SyntaxKind::GENERATOR
                ))
            ) {
                let dollar_tok = &ctx.tokens()[dollar];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::InvalidInterpolation,
                    "interpolation expects a single expression",
                    dollar_tok.start,
                    dollar_tok.end,
                );
            }
            let end = inner.end;
            events.extend(inner.events);
            events.push(Event::Finish);
            end
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
                paren_list_kind(ctx, start),
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
            paren_list_kind(ctx, start),
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
        push_diagnostic(
            diagnostics,
            DiagnosticKind::UnclosedParen,
            "unclosed `(`",
            open.start,
            open.end,
        );
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
/// the unary-and-binary infix operators `+ - & ~` (broadcast `.+ .- .~`) and the
/// symbol-quote `:` (glued `:a` is a quoted symbol). Binary-only operators
/// (`* / % | :: <: >:`, broadcast `.& .|`) and any *suffixed* operator (`+₁`,
/// never unary) stay infix and do not split. Unary-only prefixes (`! ¬ √ $`) have
/// no infix binding power, so they end the element naturally and are not listed
/// here. Mirrors JuliaSyntax's whitespace-sensitive array splitting.
fn op_can_lead_array_element(op: &Token) -> bool {
    matches!(
        op.kind,
        TokKind::Plus
            | TokKind::Minus
            | TokKind::DotPlus
            | TokKind::DotMinus
            | TokKind::Tilde
            | TokKind::DotTilde
            | TokKind::Amp
            | TokKind::Colon
    ) && !op.text.chars().next_back().is_some_and(is_op_suffix_char)
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

/// Whether the trivia run beginning at the newline `look` (newlines, horizontal
/// whitespace, and comments) is followed by a `,`. Used to decide that a newline
/// first separator inside `[…]` is insignificant because a comma — the real
/// separator of a vector — comes next.
fn newline_run_precedes_comma(ctx: &ParserCtx<'_>, look: usize) -> bool {
    let mut peek = look;
    while matches!(
        ctx.token(peek).map(|t| t.kind),
        Some(TokKind::Whitespace | TokKind::Comment | TokKind::BlockComment | TokKind::Newline)
    ) {
        peek += 1;
    }
    ctx.token(peek).map(|t| t.kind) == Some(TokKind::Comma)
}

/// Whether the trivia run beginning at the newline `look` is followed by a
/// `for`. A blank line (or any newlines) before the comprehension `for` is
/// insignificant: `[x \n\n for a in as]` is `(comprehension …)`, not a `vcat`.
/// A second element before the `for` is hit first, keeping `[1\n2\nfor …]` a
/// matrix.
fn newline_run_precedes_for(ctx: &ParserCtx<'_>, look: usize) -> bool {
    let mut peek = look;
    while matches!(
        ctx.token(peek).map(|t| t.kind),
        Some(TokKind::Whitespace | TokKind::Comment | TokKind::BlockComment | TokKind::Newline)
    ) {
        peek += 1;
    }
    ctx.token(peek).map(|t| t.kind) == Some(TokKind::ForKw)
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
    // An element-free `[; …]` is an empty n-dimensional concatenation
    // (`[;]` → `ncat-1`, `[;;]` → `ncat-2`), not a vector.
    if let Some(empty) = parse_empty_ncat(
        ctx,
        lbrk,
        first_start,
        TokKind::RBracket,
        SyntaxKind::MATRIX_EXPR,
    ) {
        return empty;
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
        // A newline run before the comprehension `for` is insignificant, so
        // `[x \n\n for a in as]` stays a `(comprehension …)`.
        Some(TokKind::Newline) if newline_run_precedes_for(ctx, look) => parse_comprehension(
            ctx,
            lbrk,
            first,
            SyntaxKind::COMPREHENSION,
            TokKind::RBracket,
            diagnostics,
        ),
        // A newline run is a row separator only if it separates two *elements*.
        // When the next significant token past the newline(s) is a `,`, the comma
        // is the real separator and the newline is insignificant whitespace, so
        // `[x\n, y]` is `(vect x y)`, matching Julia (`;` after a newline stays a
        // matrix row separator, and another element keeps it a matrix).
        Some(TokKind::Newline) if newline_run_precedes_comma(ctx, look) => vect(diagnostics),
        _ => parse_matrix(
            ctx,
            lbrk,
            first,
            TokKind::RBracket,
            SyntaxKind::VECT_EXPR,
            SyntaxKind::MATRIX_EXPR,
            diagnostics,
        ),
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
}

impl SepRun {
    /// The dimension this separator concatenates along. A trailing separator
    /// (`between` = false) only separates via `;` — a trailing newline is just
    /// whitespace (`[x\n]` is a `vect`, not a `vcat`).
    fn dim(&self, between: bool) -> usize {
        if self.semis > 0 {
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
fn parse_empty_ncat(
    ctx: &ParserCtx<'_>,
    lbrk: usize,
    first_start: usize,
    close: TokKind,
    node_kind: SyntaxKind,
) -> Option<ExprParse> {
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
    events.push(Event::Finish); // node_kind
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
fn parse_matrix(
    ctx: &ParserCtx<'_>,
    lbrk: usize,
    first: ExprParse,
    close: TokKind,
    comma_kind: SyntaxKind,
    matrix_kind: SyntaxKind,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
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
        };
        let mut q = pos;
        while let Some(k) = ctx.token(q).map(|t| t.kind) {
            match k {
                TokKind::Semicolon => run.semis += 1,
                TokKind::Newline => run.has_newline = true,
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
                let el = match parse_element(tokens, q, diagnostics) {
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
    // continuation collapsing to `hcat` rather than a conflict; that structural
    // case is not handled here, so `[a b ;; \n c]` stays divergent.)
    let mut order = ArrayOrder::Unknown;
    for k in 0..n.saturating_sub(1) {
        let sep = &seps[k];
        let is_space = sep.semis == 0 && !sep.has_newline;
        let is_double_semi = sep.semis == 2;
        let conflict = match order {
            ArrayOrder::Unknown => {
                if is_space {
                    order = ArrayOrder::RowMajor;
                } else if is_double_semi {
                    order = ArrayOrder::ColumnMajor;
                }
                false
            }
            ArrayOrder::RowMajor => is_double_semi,
            ArrayOrder::ColumnMajor => is_space,
        };
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
    events.push(Event::Finish); // node_kind
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
    events.push(Event::Finish); // MATRIX_ROW
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
        push_diagnostic(
            diagnostics,
            DiagnosticKind::UnclosedComprehension,
            "unclosed comprehension",
            tok.start,
            tok.end,
        );
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
        // `no_word_op` keeps a following `in`/`isa` as the iteration separator
        // (handled below) rather than swallowing it as a comparison operator.
        let var_start = ctx.skip_trivia(pos);
        push_range(events, pos, var_start);
        let var_flags = ExprFlags {
            inside_brackets: true,
            no_word_op: true,
            ..ExprFlags::default()
        };
        if let Some(var) = parse_expr_in(tokens, var_start, 0, diagnostics, var_flags) {
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
    array_mode: bool,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> ExprParse {
    loop {
        // No newline between the callee and `(`/`[` — only horizontal space.
        let next = ctx.skip_ws(lhs.end);
        // Inside an array literal, a `(`/`[`/`{` with whitespace before it begins a
        // new concatenation element rather than chaining as a call/index/curly:
        // `[f (x)]` is `(hcat f x)` (two elements), while `[f(x)]` is `(vect (call
        // f x))`. Mirrors JuliaSyntax's whitespace-sensitive array splitting.
        if array_mode
            && next > lhs.end
            && matches!(
                ctx.token(next).map(|t| t.kind),
                Some(TokKind::LParen | TokKind::LBracket | TokKind::LBrace)
            )
        {
            break;
        }
        // Juxtaposition with a numeric literal is multiplication, not a call: a
        // `(` glued to a number (`2(x)`) is left for the juxtaposition check in
        // the operator loop to consume as a `(juxtapose 2 x)`, not a `(call 2 x)`.
        // (A `[` glued to a number stays an index, `2[1]` ⇒ `(ref 2 1)`, matching
        // JuliaSyntax's `parse_call_chain` guard.)
        if ctx.token(next).map(|t| t.kind) == Some(TokKind::LParen) && lhs_is_number(ctx, &lhs) {
            break;
        }
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
                // Whitespace before the `(` of a broadcast call is disallowed:
                // `f. (x)` → `(dotcall f (error-t) x)`, mirroring the glued
                // postfix-opener error above.
                if lparen > next + 1 {
                    let opener = &ctx.tokens()[lparen];
                    push_diagnostic(
                        diagnostics,
                        DiagnosticKind::OpenerWhitespace,
                        "whitespace before opener",
                        opener.start,
                        opener.start,
                    );
                }
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

    // A space-, `;`-, or newline-separated bracket body after a value is a typed
    // concatenation (`T[x y]` → `(typed_hcat T x y)`), not an index. A comma
    // list, single element, or empty `T[]` stays an `INDEX_EXPR`.
    if close == TokKind::RBracket
        && let Some(typed) = parse_typed_concat(ctx, &lhs, open_idx, diagnostics)
    {
        return typed;
    }

    let (list_events, end) =
        parse_arg_list(ctx, open_idx, close, SyntaxKind::ARG_LIST, diagnostics);
    let mut events = vec![Event::Start(node)];
    events.extend(lhs.events);
    push_range(&mut events, lhs.end, open_idx);
    // Whitespace before a glued postfix opener is disallowed: JuliaSyntax keeps
    // the call/index/curly shape but flags the space. We record an
    // `OpenerWhitespace` diagnostic at the opener's start, projected as a
    // `(error-t)` before the arguments (`f (a)` → `(call f (error-t) a)`,
    // `a [i]` → `(ref a (error-t) i)`, `S {a}` → `(curly S (error-t) a)`).
    if open_idx > lhs.end {
        let opener = &ctx.tokens()[open_idx];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::OpenerWhitespace,
            "whitespace before opener",
            opener.start,
            opener.start,
        );
    }
    events.extend(list_events);
    events.push(Event::Finish);
    ExprParse {
        start: lhs.start,
        end,
        events,
    }
}

/// Detect and parse a typed concatenation `T[...]`: a bracket body after a value
/// that is space-, `;`-, or newline-separated (or an element-free `;`-only
/// `T[;]`). Returns `None` for a comma list, single element, empty `T[]`, or a
/// comprehension, leaving the caller to build an `INDEX_EXPR`. The result wraps
/// the type expression `lhs` and a `MATRIX_EXPR` body in a `TYPED_MATRIX_EXPR`.
fn parse_typed_concat(
    ctx: &ParserCtx<'_>,
    lhs: &ExprParse,
    open_idx: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let diag_mark = diagnostics.len();
    let first_start = ctx.skip_trivia(open_idx + 1);
    // `T[]` is an empty index, not a concatenation.
    if ctx.token(first_start).map(|t| t.kind) == Some(TokKind::RBracket) {
        return None;
    }
    let wrap = |body: ExprParse| {
        let mut events = vec![Event::Start(SyntaxKind::TYPED_MATRIX_EXPR)];
        events.extend(lhs.events.iter().cloned());
        push_range(&mut events, lhs.end, open_idx);
        let end = body.end;
        events.extend(body.events);
        events.push(Event::Finish);
        ExprParse {
            start: lhs.start,
            end,
            events,
        }
    };
    // An element-free `T[; …]` is an empty n-dimensional concatenation.
    if let Some(empty) = parse_empty_ncat(
        ctx,
        open_idx,
        first_start,
        TokKind::RBracket,
        SyntaxKind::MATRIX_EXPR,
    ) {
        return Some(wrap(empty));
    }
    let first = parse_element(ctx.tokens(), first_start, diagnostics)?;
    // Look at the first separator: a `,`, `]`, end, or `for` means this is an
    // index/comprehension, not a concatenation.
    let mut look = first.end;
    while matches!(
        ctx.token(look).map(|t| t.kind),
        Some(TokKind::Whitespace | TokKind::Comment | TokKind::BlockComment)
    ) {
        look += 1;
    }
    match ctx.token(look).map(|t| t.kind) {
        None | Some(TokKind::RBracket | TokKind::Comma | TokKind::ForKw) => {
            diagnostics.truncate(diag_mark);
            None
        }
        _ => {
            let body = parse_matrix(
                ctx,
                open_idx,
                first,
                TokKind::RBracket,
                SyntaxKind::VECT_EXPR,
                SyntaxKind::MATRIX_EXPR,
                diagnostics,
            );
            // A lone element with only a trailing newline collapses to the
            // comma kind (`T[x\n]` → `(ref T x)`), so it stays an index.
            if matches!(
                body.events.first(),
                Some(Event::Start(SyntaxKind::VECT_EXPR))
            ) {
                diagnostics.truncate(diag_mark);
                None
            } else {
                Some(wrap(body))
            }
        }
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
    let name_end = parse_macro_name_body(ctx, &mut events, at_idx + 1, diagnostics);
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
    let name_end = parse_macro_name_body(ctx, &mut events, at_idx + 1, diagnostics);
    events.push(Event::Finish); // close MACRO_NAME

    let end = parse_macro_args(ctx, &mut events, name_end, diagnostics, inside_brackets);
    events.push(Event::Finish); // close MACRO_CALL
    ExprParse {
        start: lhs.start,
        end,
        events,
    }
}

/// If `i` begins a `var"…"` single-quoted non-standard identifier — the macro
/// name in `@var"#"` (`(var @#)`) — append its `NONSTANDARD_IDENTIFIER` node to
/// `events` and return the index past it. Otherwise return `None`. Triple-quoted
/// `var"""…"""` is an ordinary `@var_str` macro, not a name, so it is excluded.
pub(crate) fn push_var_macro_name(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    i: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<usize> {
    let is_var = ctx.token(i).map(|t| t.kind) == Some(TokKind::StringPrefix)
        && ctx.token(i).map(|t| t.text.as_str()) == Some("var");
    let single_quote = matches!(
        ctx.token(i + 1),
        Some(t) if t.kind == TokKind::StringDelimOpen && t.text.len() == 1
    );
    if is_var && single_quote {
        let lit = parse_string_literal(ctx, i, diagnostics);
        let end = lit.end;
        events.extend(lit.events);
        Some(end)
    } else {
        None
    }
}

/// Emit the tokens of a macro name following the `@` sigil, starting at `start`:
/// either a lone `.` (the broadcast macro `@.`), an identifier followed by a
/// trailing adjacent `.ident` chain (`@Mod.mac`), or a `var"…"` non-standard
/// identifier (`@var"#"`). Returns the index just past the name.
fn parse_macro_name_body(
    ctx: &ParserCtx<'_>,
    events: &mut Vec<Event>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> usize {
    if let Some(end) = push_var_macro_name(ctx, events, start, diagnostics) {
        return end;
    }
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
        // A parenthesized macro name `@(A)`: a single identifier wrapped in
        // parens unwraps to the bare name `@A` (interior whitespace is allowed:
        // `@( A )`). The parens are kept in the CST for losslessness; the
        // projector reads only the identifier component. Anything other than a
        // lone identifier (`@(A.b)`, `@(f(x))`) is left for the paren-arg form to
        // handle (it stays a divergence, matching Julia's error recovery).
        Some(TokKind::LParen) => {
            let inner = ctx.skip_ws(start + 1);
            if ctx.token(inner).map(|t| t.kind) == Some(TokKind::Ident) {
                let after = ctx.skip_ws(inner + 1);
                if ctx.token(after).map(|t| t.kind) == Some(TokKind::RParen) {
                    push_range(events, start, after + 1);
                    return after + 1;
                }
            }
            start
        }
        // An operator, `$`, or keyword directly after `@` names the macro
        // (`@+`, `@!`, `@..`, `@$`, `@end`). A lone `:` (`@:`) is left to error.
        Some(k) if is_macro_name_token(k) => {
            events.push(Event::Tok(start));
            start + 1
        }
        // A bare `@` with no name — emit nothing more; the MACRO_NAME holds just
        // the sigil (still lossless).
        _ => start,
    }
}

/// Whether `kind`, directly after `@`, names the macro: any operator name, the
/// `$` sigil, or a keyword (`@+`, `@!`, `@..`, `@$`, `@end`). `.` is excluded —
/// it is the broadcast macro `@.`, handled before this — and `:` is excluded so
/// `@:` falls through to error recovery (Julia rejects it).
fn is_macro_name_token(kind: TokKind) -> bool {
    !matches!(kind, TokKind::Dot | TokKind::Colon)
        && (is_op_name(kind)
            || is_value_operator(kind)
            || kind == TokKind::Dollar
            || kind.is_keyword())
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

    // Bracket form `@m[a]`/`@m{a}`: a `[`/`{` adjacent to the macro name (no
    // whitespace, else `name_end` points at the whitespace token) is the single
    // argument. Postfix operators chain onto the whole macrocall, not the bracket
    // (`@m[a].b` ⇒ `(. (macrocall @m (vect a)) (quote b))`, `@m[a](x)` ⇒
    // `(call (macrocall @m (vect a)) x)`), so parse only the bracket prefix here
    // and let the outer postfix chain attach any suffix.
    if matches!(
        ctx.token(name_end).map(|t| t.kind),
        Some(TokKind::LBracket | TokKind::LBrace)
    ) {
        let arg_flags = ExprFlags {
            inside_brackets,
            ..ExprFlags::default()
        };
        if let Some(arg) = parse_prefix(ctx, name_end, diagnostics, arg_flags) {
            events.extend(arg.events);
            return arg.end;
        }
    }

    // Space form `@m a b`: each argument is a full expression. Stop at a newline,
    // end of input, or a delimiter that closes/separates an enclosing list.
    let mut pos = name_end;
    let mut n_args = 0;
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
                        n_args += 1;
                    }
                    None => break,
                }
            }
        }
    }

    // `@doc` extension: when the doc macro takes exactly one space-separated
    // argument and the next line carries another expression, it is consumed as a
    // second argument (`@doc x\ny` ⇒ `(macrocall @doc x y)`). A blank line, a
    // closing token, or end of input on the next line stops it. Matches
    // JuliaSyntax's doc-macro rule; the name's leaf identifier must be `doc`
    // (`@doc`, `A.@doc`, `@A.doc`).
    if n_args == 1 && macro_leaf_is_doc(ctx, name_end) {
        let nl = ctx.skip_ws(pos);
        if ctx.token(nl).map(|t| t.kind) == Some(TokKind::Newline) {
            let after = ctx.skip_ws(nl + 1);
            let extend = !matches!(
                ctx.token(after).map(|t| t.kind),
                None | Some(
                    TokKind::Newline
                        | TokKind::Comma
                        | TokKind::Semicolon
                        | TokKind::RParen
                        | TokKind::RBracket
                        | TokKind::RBrace
                        | TokKind::EndKw
                        | TokKind::ElseKw
                        | TokKind::ElseifKw
                        | TokKind::CatchKw
                        | TokKind::FinallyKw
                )
            );
            if extend {
                push_range(events, pos, after);
                let arg_flags = ExprFlags {
                    inside_brackets,
                    stmt_comma: true,
                    ..ExprFlags::default()
                };
                if let Some(arg) = parse_expr_in(ctx.tokens(), after, 0, diagnostics, arg_flags) {
                    events.extend(arg.events);
                    pos = arg.end;
                }
            }
        }
    }
    pos
}

/// Whether the macro name ending at `name_end` has the leaf identifier `doc` —
/// the special doc macro (`@doc`, `A.@doc`, `@A.doc`), whose single-argument
/// form is extended with the next line's expression in [`parse_macro_args`].
fn macro_leaf_is_doc(ctx: &ParserCtx<'_>, name_end: usize) -> bool {
    name_end > 0
        && ctx
            .token(name_end - 1)
            .is_some_and(|t| t.kind == TokKind::Ident && t.text == "doc")
}

/// Parse a standalone `{ … }` brace expression. A comma-separated (or single,
/// empty) layout is a `BRACES` node holding its items directly (no wrapping
/// `ARG_LIST`), matching `where {T, S}`. A space-, `;`-, or newline-separated
/// layout is a `BRACESCAT_EXPR` of `MATRIX_ROW`s — the same nesting the
/// projector reads for `[...]`, but always headed `bracescat`.
fn parse_braces(
    ctx: &ParserCtx<'_>,
    start: usize,
    diagnostics: &mut Vec<ParseDiagnostic>,
) -> Option<ExprParse> {
    let tokens = ctx.tokens();
    let braces = |diagnostics: &mut Vec<ParseDiagnostic>| {
        let (events, end) =
            parse_arg_list(ctx, start, TokKind::RBrace, SyntaxKind::BRACES, diagnostics);
        ExprParse { start, end, events }
    };

    let first_start = ctx.skip_trivia(start + 1);
    if ctx.token(first_start).map(|t| t.kind) == Some(TokKind::RBrace) {
        return Some(braces(diagnostics));
    }
    // An element-free `{; …}` is an empty n-dimensional concatenation
    // (`{;}` → `(bracescat (nrow-1))`), not a brace list.
    if let Some(empty) = parse_empty_ncat(
        ctx,
        start,
        first_start,
        TokKind::RBrace,
        SyntaxKind::BRACESCAT_EXPR,
    ) {
        return Some(empty);
    }
    let Some(first) = parse_element(tokens, first_start, diagnostics) else {
        return Some(braces(diagnostics));
    };

    // The first separator decides comma list vs concatenation, mirroring
    // `parse_bracket_literal` (a newline is a significant row separator).
    let mut look = first.end;
    while matches!(
        ctx.token(look).map(|t| t.kind),
        Some(TokKind::Whitespace | TokKind::Comment | TokKind::BlockComment)
    ) {
        look += 1;
    }
    match ctx.token(look).map(|t| t.kind) {
        Some(TokKind::ForKw) => Some(parse_comprehension(
            ctx,
            start,
            first,
            SyntaxKind::BRACES_COMPREHENSION,
            TokKind::RBrace,
            diagnostics,
        )),
        None | Some(TokKind::RBrace | TokKind::Comma) => Some(braces(diagnostics)),
        _ => Some(parse_matrix(
            ctx,
            start,
            first,
            TokKind::RBrace,
            SyntaxKind::BRACES,
            SyntaxKind::BRACESCAT_EXPR,
            diagnostics,
        )),
    }
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
            None => {
                // Unterminated list (EOF before the closing delimiter). Record an
                // `UnterminatedArgList` diagnostic at the opener, projected as a
                // trailing `(error-t)` (`f(a` → `(call f a (error-t))`, `[x` →
                // `(vect x (error-t))`).
                if in_params {
                    events.push(Event::Finish); // close PARAMETERS first
                    in_params = false;
                }
                let opener = &tokens[open_idx];
                push_diagnostic(
                    diagnostics,
                    DiagnosticKind::UnterminatedArgList,
                    "unterminated argument list",
                    opener.start,
                    opener.start,
                );
                break;
            }
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
            // `;` splits positional arguments from keyword parameters, and each
            // subsequent `;` starts a fresh `PARAMETERS` group: `(a; b; c,d)` ⇒
            // `a (parameters b) (parameters c d)`. Close the open group before
            // opening the next so the groups stay siblings.
            Some(TokKind::Semicolon) => {
                if in_params {
                    events.push(Event::Finish); // close previous PARAMETERS
                }
                events.push(Event::Start(SyntaxKind::PARAMETERS));
                in_params = true;
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

/// If an `in`/`isa` word operator (lexed as an identifier, comparison
/// precedence) immediately follows the operand ending at `from`, return its
/// token index. Honors newline sensitivity exactly like [`next_operator`]: a
/// newline ends the expression at statement scope, but inside brackets the
/// operator may continue onto the next line.
fn word_operator(ctx: &ParserCtx<'_>, from: usize, inside_brackets: bool) -> Option<usize> {
    let op_idx = ctx.skip_ws(from);
    let op = ctx.token(op_idx)?;
    let op_idx = if op.kind == TokKind::Newline {
        if !inside_brackets {
            return None;
        }
        ctx.skip_ws_and_newlines(from)
    } else {
        op_idx
    };
    let op = ctx.token(op_idx)?;
    (op.kind == TokKind::Ident && (op.text == "in" || op.text == "isa")).then_some(op_idx)
}

fn is_operator(kind: TokKind) -> bool {
    matches!(kind, TokKind::Question | TokKind::WhereKw)
        || is_assignment_op(kind)
        || infix_binding_power(kind).is_some()
}

/// Whether `kind` is a numeric literal token (Julia's `is_number`: not chars or
/// booleans). Used to recognize a numeric-literal coefficient for juxtaposition.
/// Whether a `+`/`-` at `op_idx`, glued to an adjacent numeric literal, folds
/// into a single signed literal rather than a unary prefix call. Mirrors
/// JuliaSyntax `parse_unary`: the operator must be undotted (`Plus`/`Minus`, not
/// `DotPlus`/`DotMinus`) and unsuffixed, and directly followed (no whitespace) by
/// a number literal — decimal `Integer`/`Float`/`Float32` for either sign, plus
/// the unsigned `BinInt`/`HexInt`/`OctInt` for `+` only (whose sign is a no-op;
/// `-0x1` stays a prefix call). It does *not* fold when `^`/`[`/`{` follow the
/// literal, since those bind tighter than unary negation (`-2^x` is `-(2^x)`,
/// `-2[1]` is `-(2[1])`, `-2{T}` is `-(2{T})`).
fn signed_literal_fold(ctx: &ParserCtx<'_>, op_idx: usize) -> bool {
    let Some(op) = ctx.token(op_idx) else {
        return false;
    };
    // A suffixed `+₁` is not a unary operator at all, so never folds.
    if op.text.chars().next_back().is_some_and(is_op_suffix_char) {
        return false;
    }
    let Some(num) = ctx.token(op_idx + 1) else {
        return false;
    };
    let folds = match op.kind {
        TokKind::Minus => matches!(
            num.kind,
            TokKind::Integer | TokKind::Float | TokKind::Float32
        ),
        TokKind::Plus => is_number_tok(num.kind),
        _ => return false,
    };
    if !folds {
        return false;
    }
    // `^`/`[`/`{` after the literal bind tighter than unary negation.
    let k3 = ctx.token(ctx.skip_ws(op_idx + 2)).map(|t| t.kind);
    !matches!(
        k3,
        Some(
            TokKind::Caret
                | TokKind::DotCaret
                | TokKind::UniPower
                | TokKind::LBracket
                | TokKind::LBrace
        )
    )
}

fn is_number_tok(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::Integer
            | TokKind::BinInt
            | TokKind::OctInt
            | TokKind::HexInt
            | TokKind::Float
            | TokKind::Float32
    )
}

/// Whether `lhs` is a bare numeric literal (a single number token). A numeric
/// coefficient juxtaposes with almost any adjacent value, and a `(` glued to it
/// is a multiplication rather than a call (`2(x)` ⇒ `(juxtapose 2 x)`).
fn lhs_is_number(ctx: &ParserCtx<'_>, lhs: &ExprParse) -> bool {
    // A bare numeric literal: a single number token.
    if lhs.end == lhs.start + 1 && ctx.token(lhs.start).is_some_and(|t| is_number_tok(t.kind)) {
        return true;
    }
    // A folded signed literal (`-2`, `+2.0`): a `LITERAL` wrapping a `+`/`-` sign
    // token and the adjacent number, so the coefficient still juxtaposes (`-2x`,
    // `-2(x)`) and a glued `(` is multiplication rather than a call.
    matches!(lhs.events.first(), Some(Event::Start(SyntaxKind::LITERAL)))
        && lhs.end == lhs.start + 2
        && ctx
            .token(lhs.start + 1)
            .is_some_and(|t| is_number_tok(t.kind))
}

/// Whether `lhs` is a closed value that may carry a non-numeric juxtaposed term
/// (`(x-1)y`, `f(x)y`, `[1,2]x`, `x'y`) — a parenthesized/bracketed expression,
/// a call/index/curly suffix, or a transpose. Other left operands (bare names,
/// block forms, prefixed terms) never start a juxtaposition.
fn lhs_value_close(lhs: &ExprParse) -> bool {
    matches!(
        lhs.events.first(),
        Some(Event::Start(
            SyntaxKind::PAREN_EXPR
                | SyntaxKind::CALL_EXPR
                | SyntaxKind::INDEX_EXPR
                | SyntaxKind::CURLY_EXPR
                | SyntaxKind::VECT_EXPR
                | SyntaxKind::MATRIX_EXPR
                | SyntaxKind::TYPED_MATRIX_EXPR
                | SyntaxKind::BRACESCAT_EXPR
                | SyntaxKind::POSTFIX_EXPR
        ))
    )
}

/// Whether `lhs` is a *parenthesized block form* — a `PAREN_EXPR` whose first
/// inner node is a value-producing block-keyword form (`begin`, `if`, `let`,
/// `quote`, `struct`, …). The paren is transparent (it projects to the inner
/// block), and like the bare block forms such a value never juxtaposes:
/// `(begin end)x` is two statements, the trailing `x` recovered as a leftover
/// `(error-t x)` by the driver, not `(juxtapose (block) x)`. A paren wrapping an
/// ordinary value (`(a)`, `(x-1)`) still juxtaposes, so this consults only the
/// juxtaposition checks; postfix and infix operators apply to a paren-block
/// regardless (`(begin end).x`, `(begin end)+1`).
fn lhs_is_paren_block(lhs: &ExprParse) -> bool {
    if !matches!(
        lhs.events.first(),
        Some(Event::Start(SyntaxKind::PAREN_EXPR))
    ) {
        return false;
    }
    // The first node inside the paren is the second `Start` event in the
    // preorder stream (the first being the `PAREN_EXPR` itself).
    lhs.events
        .iter()
        .filter_map(|e| match e {
            Event::Start(k) => Some(*k),
            _ => None,
        })
        .nth(1)
        .is_some_and(is_block_form_kind)
}

/// The value-producing block-keyword forms (mirrors the `block_form` dispatch in
/// [`parse_expr_in`]). A bare such form suppresses postfix/juxtaposition via
/// `lhs_is_block_keyword`; a parenthesized one is detected by [`lhs_is_paren_block`].
fn is_block_form_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::IF_EXPR
            | SyntaxKind::FUNCTION_DEF
            | SyntaxKind::MACRO_DEF
            | SyntaxKind::BEGIN_EXPR
            | SyntaxKind::QUOTE_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::LET_EXPR
            | SyntaxKind::TRY_EXPR
            | SyntaxKind::STRUCT_DEF
            | SyntaxKind::MODULE_DEF
            | SyntaxKind::ABSTRACT_DEF
            | SyntaxKind::PRIMITIVE_DEF
    )
}

/// A closing delimiter that ends the surrounding container rather than starting
/// a juxtaposed term.
fn is_juxtapose_closing(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::RParen | TokKind::RBracket | TokKind::RBrace | TokKind::Comma | TokKind::Semicolon
    )
}

/// A token that closes the surrounding construct (JuliaSyntax `is_closing_token`):
/// a closing delimiter/separator or a block-closing keyword (`end`, `else`,
/// `elseif`, `catch`, `finally`). Such a token after a value never begins a
/// juxtaposed term — a trailing `end` is leftover-junk (`"a"end`), not a juxtapose.
fn is_closing_token(kind: TokKind) -> bool {
    is_juxtapose_closing(kind)
        || matches!(
            kind,
            TokKind::EndKw
                | TokKind::ElseKw
                | TokKind::ElseifKw
                | TokKind::CatchKw
                | TokKind::FinallyKw
        )
}

/// Whether `lhs` is a plain (non-prefixed) string literal — a `STRING_LITERAL`
/// node whose first token is not a `STRING_PREFIX`. A prefixed string is a string
/// macro (`r"…"`), which absorbs a glued suffix as a flag rather than juxtaposing.
fn lhs_is_plain_string(ctx: &ParserCtx<'_>, lhs: &ExprParse) -> bool {
    if !matches!(
        lhs.events.first(),
        Some(Event::Start(SyntaxKind::STRING_LITERAL))
    ) {
        return false;
    }
    let first_tok = lhs.events.iter().find_map(|e| match e {
        Event::Tok(idx) => Some(*idx),
        _ => None,
    });
    match first_tok {
        Some(idx) => ctx.token(idx).map(|t| t.kind) != Some(TokKind::StringPrefix),
        None => true,
    }
}

/// Whether the glued term after `lhs` forms an *invalid* string juxtaposition,
/// which JuliaSyntax recovers as `(juxtapose lhs (error-t) rhs)`. Mirrors
/// `parse_juxtapose`'s `prev_k == K"string" || is_string_delim(t)` branch: it
/// fires when the left operand is a plain string literal (and the glued term is
/// any non-number value) or when the glued term is itself a string literal (and
/// the left operand is a value that would otherwise juxtapose). Adjacency,
/// operator/`@`/closing-token, and `min_bp` gating match the numeric juxtaposition
/// in [`should_juxtapose`].
fn should_juxtapose_string_error(ctx: &ParserCtx<'_>, lhs: &ExprParse, min_bp: u8) -> bool {
    if JUXTAPOSE_L < min_bp {
        return false;
    }
    // A parenthesized block form (`(begin end)`) never juxtaposes — the glued
    // term is leftover junk, not a string juxtaposition.
    if lhs_is_paren_block(lhs) {
        return false;
    }
    let Some(next) = ctx.token(lhs.end) else {
        return false;
    };
    let k = next.kind;
    // The term must be adjacent and must start a value: not an operator (radicals
    // are not `is_operator`, so they pass), not a macro `@`, not a closing token.
    if k.is_trivia() || is_operator(k) || k == TokKind::At || is_closing_token(k) {
        return false;
    }
    if lhs_is_plain_string(ctx, lhs) {
        // `prev == string`: juxtaposes with any non-number term (a glued number
        // after a string is a docstring target, `"a"2` ⇒ `(doc (string "a") 2)`).
        return !is_number_tok(k);
    }
    // `is_string_delim(t)`: the glued term is itself a string literal. It
    // juxtaposes with the left operand whenever a numeric one would (`2"a"`,
    // `(x)"a"`) — i.e. a bare number or a closed value.
    matches!(k, TokKind::StringDelimOpen | TokKind::CmdDelimOpen)
        && (lhs_is_number(ctx, lhs) || lhs_value_close(lhs))
}

/// Whether the token directly after `lhs` begins a juxtaposed term — an implicit
/// multiplication with no operator between (`2x`, `2(x)`, `(x-1)y`, `1√x`).
/// Mirrors JuliaSyntax's `parse_juxtapose`/`is_juxtapose` (the non-string-literal
/// branch; string juxtaposition is error recovery and deferred).
fn should_juxtapose(ctx: &ParserCtx<'_>, lhs: &ExprParse, min_bp: u8) -> bool {
    if JUXTAPOSE_L < min_bp {
        return false;
    }
    let Some(next) = ctx.token(lhs.end) else {
        return false;
    };
    let k = next.kind;
    // The term must be adjacent — no intervening whitespace, newline, or comment.
    if k.is_trivia() {
        return false;
    }
    // It must start a value: not an operator (radicals are not `is_operator`, so
    // they pass), not a closing delimiter, keyword, or macro `@`.
    if is_operator(k) || is_juxtapose_closing(k) || k.is_keyword() || k == TokKind::At {
        return false;
    }
    // A numeric coefficient juxtaposes with any such value.
    if lhs_is_number(ctx, lhs) {
        return true;
    }
    // A non-numeric value juxtaposes only with a non-numeric term (`f(2)2` is a
    // call, not juxtaposition) and only when the left operand is a closed value.
    // A parenthesized block form (`(begin end)x`) is excluded: it does not
    // juxtapose, leaving the glued term as a leftover `(error-t …)`.
    !is_number_tok(k) && lhs_value_close(lhs) && !lhs_is_paren_block(lhs)
}

/// Plain/broadcast assignment (`=`, `.=`) and augmented assignment (`+=`, `.+=`,
/// …): the loosest, right-associative tier, all modeled as `ASSIGNMENT_EXPR`.
/// Whether an operator token's text is a *dotted* (broadcast) operator — it leads
/// with a broadcast `.` (`.+`, `.&`, `.=`, `.&&`, `.+=`). The range/splat
/// operators `..`/`...` lead with a *doubled* dot and are not broadcasts, so they
/// are excluded; bare field-access `.` is excluded by the length check.
fn is_dotted_broadcast_text(text: &str) -> bool {
    text.as_bytes().first() == Some(&b'.') && text.len() > 1 && text.as_bytes()[1] != b'.'
}

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
        // A leading `;` opens a parameters group (`+(; a=1)` → `(call + (parameters
        // (= a 1)))`), so it is a call — unless the parens form a *block*
        // (`+(;;)` → `(call-pre + (block-p))`), where the unary operator instead
        // prefixes the parenthesized block. Mirrors JuliaSyntax resolving the
        // empty all-semicolon group `(;;)` to a block rather than an arglist.
        Some(TokKind::Semicolon) => return !paren_is_block(ctx, lparen_idx),
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

/// The node kind for a parenthesized run carrying a `;`: a `PAREN_BLOCK`
/// (projects `(block-p …)`) or a `TUPLE_EXPR` (projects `(tuple-p …)`). A pure
/// comma list (no semicolons) always stays a tuple.
fn paren_list_kind(ctx: &ParserCtx<'_>, lparen_idx: usize) -> SyntaxKind {
    if paren_is_block(ctx, lparen_idx) {
        SyntaxKind::PAREN_BLOCK
    } else {
        SyntaxKind::TUPLE_EXPR
    }
}

/// Whether a `;`-bearing parenthesized run is a block rather than a tuple,
/// mirroring JuliaSyntax `parse_paren`/`parse_brackets`:
///
/// ```text
/// is_tuple = had_commas || (had_splat && num_semis >= 1) ||
///            (initial_semi && (num_semis == 1 || num_subexprs > 0))
/// is_block = !is_tuple && num_semis > 0
/// ```
///
/// So `(a; b)`, `(a=1;)`, `(a;b;;c)`, and `(;;)` are blocks (`block-p`), while
/// `(a, b)`, `(; a=1)`, `(; a=1; b=2)`, and `(x...; y)` are tuples (`tuple-p`).
/// Flags are gathered by a depth-0 token scan from just after the `(`.
fn paren_is_block(ctx: &ParserCtx<'_>, lparen_idx: usize) -> bool {
    let first = ctx.skip_trivia(lparen_idx + 1);
    let initial_semi = ctx.token(first).map(|t| t.kind) == Some(TokKind::Semicolon);
    let mut depth = 0i32;
    let mut had_commas = false;
    let mut had_splat = false;
    let mut num_semis = 0u32;
    let mut num_subexprs = 0u32;
    let mut in_subexpr = false;
    let mut i = first;
    while let Some(tok) = ctx.token(i) {
        match tok.kind {
            TokKind::LParen | TokKind::LBracket | TokKind::LBrace => {
                if !in_subexpr {
                    num_subexprs += 1;
                    in_subexpr = true;
                }
                depth += 1;
            }
            TokKind::RParen | TokKind::RBracket | TokKind::RBrace => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            TokKind::Comma if depth == 0 => {
                had_commas = true;
                in_subexpr = false;
            }
            TokKind::Semicolon if depth == 0 => {
                num_semis += 1;
                in_subexpr = false;
            }
            TokKind::DotDotDot if depth == 0 => had_splat = true,
            k if !k.is_trivia() && !in_subexpr => {
                num_subexprs += 1;
                in_subexpr = true;
            }
            _ => {}
        }
        i += 1;
    }
    let is_tuple = had_commas
        || (had_splat && num_semis >= 1)
        || (initial_semi && (num_semis == 1 || num_subexprs > 0));
    !is_tuple && num_semis > 0
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
            | DotSubtype
            | DotSupertype
            | DotFatArrow
            | DotLongArrow
            | DotPipeGt
            | DotAmp
            | DotPipe
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

    // JuliaSyntax requires whitespace on both sides of `?` and `:`; each missing
    // side records one `(error-t)` marker (as a diagnostic). For `?` the markers
    // sit between the condition and the true-branch (`a? b : c` ⇒
    // `(? a (error-t) b c)`); a glued `?` on both sides yields two.
    let ws_after = |idx: usize| ctx.token(idx + 1).is_some_and(|t| t.kind.is_trivia());
    let q_errors = usize::from(q_idx == cond.end) + usize::from(!ws_after(q_idx));

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
            DiagnosticKind::MissingTernaryTrue,
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
    let has_colon = ctx.token(colon).map(|t| t.kind) == Some(TokKind::Colon);

    // A present `:` counts its surrounding whitespace; a missing `:` is itself one
    // marker (`a ? b c` ⇒ `(? a b (error-t) c)`), with the false-branch beginning
    // right after the true-branch.
    let (colon_errors, else_start) = if has_colon {
        let errors = usize::from(colon == then_br.end) + usize::from(!ws_after(colon));
        (errors, ctx.skip_trivia(colon + 1))
    } else {
        (1, ctx.skip_trivia(then_br.end))
    };

    // False-branch: inherit `no_range` so an enclosing ternary's `:` still ends
    // it (`a ? b ? c : d : e`), while a top-level else may hold a range.
    let else_flags = ExprFlags {
        array_mode: false,
        ..flags
    };
    let Some(else_br) = parse_expr_in(tokens, else_start, TERNARY_R, diagnostics, else_flags)
    else {
        if has_colon {
            let op = &tokens[colon];
            push_diagnostic(
                diagnostics,
                DiagnosticKind::MissingTernaryFalse,
                "expected expression after `:`",
                op.start,
                op.end,
            );
            return Err(error_expr_to_line_end(tokens, cond.start, colon + 1));
        }
        // No `:` and no false-branch: recover with the condition and true-branch.
        let op = &tokens[q_idx];
        push_diagnostic(
            diagnostics,
            DiagnosticKind::MissingTernaryColon,
            "expected `:` in ternary expression",
            op.start,
            op.end,
        );
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
    };

    // Whitespace errors around `?`/`:` are recorded as diagnostics: `q_errors`
    // copies anchored at the `?`'s end, `colon_errors` at the true-branch's end.
    // The projector replays the counts as `(error-t)` markers.
    let q_end = tokens[q_idx].end;
    for _ in 0..q_errors {
        push_diagnostic(
            diagnostics,
            DiagnosticKind::TernaryQWhitespace,
            "whitespace around `?`",
            q_end,
            q_end,
        );
    }
    let then_end = tokens[then_br.end - 1].end;
    for _ in 0..colon_errors {
        push_diagnostic(
            diagnostics,
            DiagnosticKind::TernaryColonWhitespace,
            "whitespace around `:`",
            then_end,
            then_end,
        );
    }

    let mut events = vec![Event::Start(SyntaxKind::TERNARY_EXPR)];
    events.extend(cond.events);
    push_range(&mut events, cond.end, q_idx);
    events.push(Event::Tok(q_idx)); // `?`
    push_range(&mut events, q_idx + 1, then_br.start);
    events.extend(then_br.events);
    if has_colon {
        push_range(&mut events, then_br.end, colon);
        events.push(Event::Tok(colon)); // `:`
        push_range(&mut events, colon + 1, else_br.start);
    } else {
        push_range(&mut events, then_br.end, else_br.start);
    }
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
        // Unicode operators share the tier of their ASCII precedence class. The
        // assignment-tier ops (`≔ ≕ ⩴`) are right-associative like `~`; the arrow
        // tier (`→ ← ↔ …`) is right-associative like `=>`/`-->`; the rest mirror
        // their ASCII siblings (comparison/colon/plus/times left-associative,
        // power right-associative).
        TokKind::UniAssign => (2, 1),
        TokKind::UniArrow => (4, 3),
        TokKind::UniComparison => (10, 11),
        TokKind::UniColon => (14, 15),
        TokKind::UniPlus => (20, 21),
        TokKind::UniTimes => (24, 25),
        TokKind::UniPower => (34, 33),
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
        // `where` is not an ordinary infix operator: it is a left-associative
        // chain handled directly in the operator loop (see `parse_where_chain`),
        // binding tighter than every binary operator but looser than
        // `^`/juxtaposition/`.`. It returns `None` here so the generic path stops
        // at it.
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
        | TokKind::DotGe
        | TokKind::DotSubtype
        | TokKind::DotSupertype => (10, 11),
        // The pipe operators share Julia's pipe precedence: `<|` (left-pipe) is
        // looser and right-associative, `|>` (right-pipe, also broadcast `.|>`)
        // is tighter and left-associative (`a <| b |> c` ⇒ `a <| (b |> c)`).
        TokKind::PipeLt => (12, 11),
        TokKind::PipeGt | TokKind::DotPipeGt => (13, 14),
        // The range operator `..` shares the colon tier (Julia gives both
        // precedence 10) and is left-associative, building an ordinary
        // `(call-i a .. b)`.
        TokKind::Colon | TokKind::DotDot => (14, 15),
        // Bitwise-or `|` shares the `+` (plus) precedence family, left-associative
        // (`a | b & c` ⇒ `(a | (b & c))`, `a & b | c` ⇒ `((a & b) | c)`).
        TokKind::Plus
        | TokKind::Minus
        | TokKind::DotPlus
        | TokKind::DotMinus
        | TokKind::Pipe
        | TokKind::DotPipe => (20, 21),
        // Bitwise-and `&` shares the `*` (times) precedence family, left-associative
        // (`a & b * c` ⇒ `((a & b) * c)`, `a + b & c` ⇒ `(a + (b & c))`).
        TokKind::Star
        | TokKind::Slash
        | TokKind::Percent
        | TokKind::Amp
        | TokKind::DotAmp
        | TokKind::DotStar
        | TokKind::DotSlash
        | TokKind::DotPercent => (24, 25),
        // Rational `//` (and broadcast `.//`) bind tighter than `*`/`/` but
        // looser than `^`, and are left-associative (`a//b//c` ⇒ `(a//b)//c`).
        TokKind::SlashSlash | TokKind::DotSlashSlash => (28, 29),
        // Bitshift `<< >> >>>` binds tighter than `//` and looser than `^`
        // (Julia precedence 14), left-associative.
        TokKind::Shl | TokKind::Shr | TokKind::UShr => (30, 31),
        TokKind::Caret | TokKind::DotCaret => (34, 33),
        TokKind::ColonColon => (36, 37),
        TokKind::Dot => (40, 41),
        _ => return None,
    })
}
