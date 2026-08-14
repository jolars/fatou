//! Juxtaposition — implicit multiplication with no operator between (`2x`, `2(x)`, `(x-1)y`) — and the signed-literal fold that competes with it for a glued `+`/`-`.
//!
//! Split out of `expr.rs`; see that module's docs for the parser as a whole.

use super::*;

/// Whether a `+`/`-` at `op_idx`, glued to an adjacent numeric literal, folds
/// into a single signed literal rather than a unary prefix call. Mirrors
/// JuliaSyntax `parse_unary`: the operator must be undotted (`Plus`/`Minus`, not
/// `DotPlus`/`DotMinus`) and unsuffixed, and directly followed (no whitespace) by
/// a number literal — decimal `Integer`/`Float`/`Float32` for either sign, plus
/// the unsigned `BinInt`/`HexInt`/`OctInt` for `+` only (whose sign is a no-op;
/// `-0x1` stays a prefix call). It does *not* fold when `^`/`[`/`{` follow the
/// literal, since those bind tighter than unary negation (`-2^x` is `-(2^x)`,
/// `-2[1]` is `-(2[1])`, `-2{T}` is `-(2{T})`).
pub(super) fn signed_literal_fold(ctx: &ParserCtx<'_>, op_idx: usize) -> bool {
    let Some(op) = ctx.token(op_idx) else {
        return false;
    };
    // A suffixed `+₁` is not a unary operator at all, so never folds.
    if op.text.chars().last().is_some_and(is_op_suffix_char) {
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

/// Whether `kind` is a numeric literal token (Julia's `is_number`: not chars or
/// booleans). Used to recognize a numeric-literal coefficient for juxtaposition.
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

/// Whether `kind` is a malformed numeric literal Julia keeps as a single error
/// token (`0x1p`, `0x1.8`). It still juxtaposes like a real coefficient, so a
/// stray following term joins it (`0x1p₁` ⇒
/// `(juxtapose (ErrorInvalidNumericConstant) (ErrorUnknownCharacter))`).
fn is_error_number_tok(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::ErrorInvalidNumber | TokKind::ErrorHexFloatNoP
    )
}

/// Whether `lhs` is a bare numeric literal (a single number token). A numeric
/// coefficient juxtaposes with almost any adjacent value, and a `(` glued to it
/// is a multiplication rather than a call (`2(x)` ⇒ `(juxtapose 2 x)`).
pub(super) fn lhs_is_number(ctx: &ParserCtx<'_>, lhs: &ExprParse) -> bool {
    // A bare numeric literal: a single number token. An error-numeric literal
    // (`0x1p`, `0x1.8`) juxtaposes too, so a stray trailing term joins it.
    if lhs.end == lhs.start + 1
        && ctx
            .token(lhs.start)
            .is_some_and(|t| is_number_tok(t.kind) || is_error_number_tok(t.kind))
    {
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
pub(super) fn should_juxtapose_string_error(
    ctx: &ParserCtx<'_>,
    lhs: &ExprParse,
    min_bp: u8,
) -> bool {
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
    // Splat `...` is kept out of `is_operator` (the operator loop's splat arm
    // owns it) but cannot start a value either (`"a"...` is a splat of the
    // string, not a juxtaposition).
    if k.is_trivia()
        || is_operator(k)
        || k == TokKind::DotDotDot
        || k == TokKind::At
        || is_closing_token(k)
    {
        return false;
    }
    // A glued `in`/`isa` is the *word operator*, not a juxtaposed value
    // (`"identity"in c` ⇒ `(call-i (string "identity") in c)`, `1isa Int` ⇒
    // `(call-i 1 isa Int)`). Both are lexed as identifiers, so `is_keyword`
    // above does not filter them out.
    if is_word_operator_tok(next) {
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

/// Whether `tok` is one of the word operators `in`/`isa`, which the lexer emits
/// as plain identifiers (they are ordinary names elsewhere) and the operator loop
/// picks up by text.
pub(super) fn is_word_operator_tok(tok: &Token<'_>) -> bool {
    tok.kind == TokKind::Ident && (tok.text == "in" || tok.text == "isa")
}

/// Whether `tok` is the Unicode set-membership operator `∈`, the alternate
/// spelling of the `in` iteration separator. Only `∈` is accepted there — the
/// sibling `∉` is an ordinary operator that Julia error-recovers in that position
/// (`for i ∉ xs` ⇒ `(= i (error ∉ xs))`), so it stays out.
pub(super) fn is_element_of_tok(tok: &Token<'_>) -> bool {
    tok.kind == TokKind::UniComparison && tok.text == "∈"
}

/// Whether `tok` separates the loop variable from the iterable in a `for`
/// iteration spec: the word operator `in` or its Unicode spelling `∈`. The third
/// spelling, `=`, is context-dependent (only the `outer` form leaves it loose,
/// since a plain `i = xs` spec is parsed whole as an assignment) and is checked at
/// the call sites.
pub(super) fn is_for_separator_tok(tok: &Token<'_>) -> bool {
    (tok.kind == TokKind::Ident && tok.text == "in") || is_element_of_tok(tok)
}

/// Whether the token directly after `lhs` begins a juxtaposed term — an implicit
/// multiplication with no operator between (`2x`, `2(x)`, `(x-1)y`, `1√x`).
/// Mirrors JuliaSyntax's `parse_juxtapose`/`is_juxtapose` (the non-string-literal
/// branch; string juxtaposition is error recovery and deferred).
pub(super) fn should_juxtapose(ctx: &ParserCtx<'_>, lhs: &ExprParse, min_bp: u8) -> bool {
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
    // they pass), not a closing delimiter, keyword, or macro `@`. Splat `...` is
    // kept out of `is_operator` (the operator loop's splat arm owns it) but
    // cannot start a value either (`g(x)...` splats the call result).
    if is_operator(k)
        || k == TokKind::DotDotDot
        || is_juxtapose_closing(k)
        || k.is_keyword()
        || k == TokKind::At
    {
        return false;
    }
    // A glued `in`/`isa` is a word operator, not a juxtaposed value — see
    // `should_juxtapose_string_error`, which excludes it for the same reason.
    if is_word_operator_tok(next) {
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
