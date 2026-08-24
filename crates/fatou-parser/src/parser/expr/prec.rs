//! Pratt binding powers and operator classification.
//!
//! Keeping the table and the predicates that consume it together makes the
//! expression parser’s precedence decisions auditable in one place.

use crate::parser::context::ParserCtx;
use crate::parser::lexer::{TokKind, Token, is_op_suffix_char};
use crate::parser::structural::is_op_name;

use super::juxtapose::is_word_operator_tok;

/// Binding power for prefix unary operators (`+x`, `-x`, `!x`). Higher than the
/// binary arithmetic operators so `-a + b` parses as `(-a) + b`.
pub(super) const PREFIX_BP: u8 = 28;

/// Fire gate for the ternary `? :`. Just above assignment (`Eq` at `(2, 1)`), so
/// a whole ternary can be an assignment's right-hand side (`w = a ? b : c`) while
/// the `?` still fires whenever `TERNARY_L >= min_bp`. The branches themselves
/// parse at [`TERNARY_BRANCH_BP`] (below assignment), which is what admits an
/// unparenthesized assignment *inside* a branch and nests a right-hand ternary.
pub(super) const TERNARY_L: u8 = 3;

/// Binding power at which each ternary *branch* is parsed. JuliaSyntax parses the
/// true- and false-branches with `parse_eq*`, so an unparenthesized assignment is
/// allowed inside a branch (`a ? b = c : d` ⇒ `(? a (= b c) d)`, `a ? b : c = d`
/// ⇒ `(? a b (= c d))`). That means dropping below the assignment tier (`=` at
/// `(2, 1)`) — `COMMA_BP` — while the branch flags clear `stmt_comma` so a bare
/// comma still does not build a tuple (`a ? b, c : d` stays a ternary that then
/// errors on the missing `:`, as in Julia).
pub(super) const TERNARY_BRANCH_BP: u8 = COMMA_BP;

/// Binding powers for numeric-literal-coefficient juxtaposition (`2x`, `(x-1)y`,
/// `1√x`). Julia binds juxtaposition tighter than `*`/`//`/`<<` but looser than
/// `^`: `2x^2` ⇒ `(juxtapose 2 (x^2))` (left binds into a following `^`), while
/// `2^2x` ⇒ `2^(2x)` (it binds into `^`'s right operand). So the left power must
/// out-bind `^`'s right (`33`), and the right operand captures only `^` (`34`)
/// and tighter — keeping `*` (`24`), `//` (`28`), and `where` (`31`) out.
/// Right-associative (`L > R`), like `^`.
pub(super) const JUXTAPOSE_L: u8 = 34;
pub(super) const JUXTAPOSE_R: u8 = 33;

/// Binding power gate for the `where` clause. `where` binds tighter than every
/// binary operator (so `A << B where C` ⇒ `(call-i A << (where B C))`) but looser
/// than `^`/juxtaposition/`.` (so `A^B where C` ⇒ `(where (call-i A ^ B) C)`),
/// matching JuliaSyntax, where `parse_where` sits between `parse_shift` and
/// `parse_juxtapose`. The chain fires whenever `WHERE_BP >= min_bp`; the shift
/// tier's right power is `31`, so this is `31` while `^`/juxtaposition sit at
/// `33`/`34`. The `::` annotation captures its own trailing `where` separately
/// (see the operator loop), since it parses its operands through `parse_where`.
pub(super) const WHERE_BP: u8 = 31;

/// The precedence at which a `where` bound is parsed (JuliaSyntax parses it with
/// `parse_comparison`): the comparison tier's left power, so the bound captures a
/// `<:`/`>:`/comparison operator and everything tighter (`A where T<:S` ⇒
/// `(where A (<: T S))`) but stops before `&&`/`||`/`->`/`=`.
pub(super) const WHERE_BOUND_BP: u8 = 10;

/// Binding powers for the word operators `in`/`isa`. They share the comparison
/// tier (the symbolic comparisons `< == …` are `(10, 11)`) and are
/// left-associative.
pub(super) const WORD_OP_L: u8 = 10;
pub(super) const WORD_OP_R: u8 = 11;

/// The loose end of the precedence range at which a statement-level bare comma
/// builds a tuple. A comma binds *tighter* than assignment (`=` at `(2, 1)`), so
/// `a, b = c, d` ⇒ `(= (tuple a b) (tuple c d))`: the tuple forms first and the
/// assignment binds the two tuples. It fires only while parsing at `min_bp <=
/// COMMA_BP` (toplevel `0`, an assignment right-hand side `1`), so it stays inert
/// once inside a comma item. Each item is parsed at `COMMA_ITEM_BP` — one tighter
/// — which excludes assignment (`2 < 3`) and the comma itself but keeps the
/// ternary (`3`) and everything tighter (`a => b, c` ⇒ `(tuple (=> a b) c)`).
pub(super) const COMMA_BP: u8 = 2;
pub(super) const COMMA_ITEM_BP: u8 = 3;

/// Left binding power of the postfix splat/vararg `...`. JuliaSyntax parses `...`
/// between `parse_pipe_lt` and `parse_range`, so it binds looser than the
/// colon/range tier (`x:y...` ⇒ `(... (call-i x : y))`) but tighter than the
/// pipes and everything looser (`a|>b...` ⇒ `(call-i a |> (... b))`,
/// `a&&b...` ⇒ `(&& a (... b))`). Colon's right power is `15` and `|>`'s is `14`,
/// so a left power of `14` binds inside a pipe's right operand (`14 >= 14`) but
/// not inside colon's (`14 < 15`).
pub(super) const SPLAT_BP: u8 = 14;

pub(super) fn is_comparison_op(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::EqEq
            | TokKind::NotEq
            | TokKind::EqEqEq
            | TokKind::NotEqEq
            | TokKind::Lt
            | TokKind::Le
            | TokKind::Gt
            | TokKind::Ge
            | TokKind::Subtype
            | TokKind::Supertype
            | TokKind::DotEqEq
            | TokKind::DotNotEq
            | TokKind::DotEqEqEq
            | TokKind::DotNotEqEq
            | TokKind::DotLt
            | TokKind::DotLe
            | TokKind::DotGt
            | TokKind::DotGe
            | TokKind::DotSubtype
            | TokKind::DotSupertype
            | TokKind::UniComparison
    )
}

pub(super) fn is_flat_arith_op(tok: &Token) -> bool {
    matches!(
        tok.kind,
        TokKind::Plus
            | TokKind::Star
            | TokKind::PlusPercent
            | TokKind::StarPercent
            | TokKind::PlusPlus
    ) && !tok.text.chars().any(is_op_suffix_char)
}

pub(super) fn is_lone_error_operator(kind: TokKind) -> bool {
    use TokKind::*;
    is_assignment_op(kind) || matches!(kind, AndAnd | OrOr | Arrow | DotDotDot)
}

pub(super) fn is_unary_prefix_op(kind: TokKind, text: &str) -> bool {
    use TokKind::*;
    matches!(
        kind,
        Plus | Minus
            | PlusPercent
            | MinusPercent
            | DotPlus
            | DotMinus
            | Bang
            | DotBang
            | Tilde
            | DotTilde
            | Amp
            | Subtype
            | Supertype
            | ColonColon
            | UniRadical
    ) || (matches!(kind, UniPlus | UniTimes) && matches!(text, "±" | "∓" | "⋆"))
}

pub(super) fn is_value_operator(kind: TokKind) -> bool {
    use TokKind::*;
    (is_op_name(kind) && !matches!(kind, AndAnd | OrOr | Arrow))
        || matches!(
            kind,
            Colon
                | DotDot
                | UniAssign
                | UniRadical
                | UniArrow
                | UniComparison
                | UniColon
                | UniPlus
                | UniTimes
                | UniPower
                | DotPlus
                | DotMinus
                | DotStar
                | DotSlash
                | DotBackslash
                | DotSlashSlash
                | DotCaret
                | DotPercent
                | DotTilde
                | DotEqEq
                | DotNotEq
                | DotEqEqEq
                | DotNotEqEq
                | DotLt
                | DotLe
                | DotGt
                | DotGe
                | DotShl
                | DotShr
                | DotUShr
                | DotSubtype
                | DotSupertype
                | DotFatArrow
                | DotLongArrow
                | DotLeftLongArrow
                | DotLeftRightArrow
                | DotPipeGt
                | DotAmp
                | DotPipe
        )
}

pub(super) fn is_quotable_operator(kind: TokKind) -> bool {
    use TokKind::*;
    matches!(
        kind,
        Dot | DotDot
            | UniRadical
            | UniArrow
            | UniComparison
            | UniColon
            | UniPlus
            | UniTimes
            | UniPower
            | Question
            // The assignment-tier `:=` (`ColonEq`) and the Unicode assignment
            // operators `≔ ≕ ⩴` (`UniAssign`) are symbols under a quote (`:≔`,
            // `:(:=)` ⇒ `(quote-: :=)`); both are kept out of `is_assignment_op`
            // so their operator parse keeps its own head, so name them here.
            | ColonEq
            | UniAssign
            // `$` is a plus-tier operator in Julia's kind table, so a quote
            // takes it as a symbol (`x.head !== :$` ⇒ `(quote-: $)`) even though
            // an unquoted `$` is an interpolation sigil.
            | Dollar
    )
}

pub(super) fn next_operator(
    ctx: &ParserCtx<'_>,
    from: usize,
    inside_brackets: bool,
) -> Option<(usize, TokKind)> {
    let op_idx = ctx.skip_ws_and_block_comments(from);
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

pub(super) fn word_operator(
    ctx: &ParserCtx<'_>,
    from: usize,
    inside_brackets: bool,
) -> Option<usize> {
    let op_idx = ctx.skip_ws_and_block_comments(from);
    let op = ctx.token(op_idx)?;
    let op_idx = if op.kind == TokKind::Newline {
        if !inside_brackets {
            return None;
        }
        ctx.skip_ws_and_newlines(from)
    } else {
        op_idx
    };
    is_word_operator_tok(ctx.token(op_idx)?).then_some(op_idx)
}

pub(super) fn is_operator(kind: TokKind) -> bool {
    matches!(kind, TokKind::Question | TokKind::WhereKw)
        || is_assignment_op(kind)
        || infix_binding_power(kind).is_some()
}

pub(super) fn is_assignment_op(kind: TokKind) -> bool {
    matches!(
        kind,
        TokKind::Eq
            | TokKind::DotEq
            | TokKind::PlusEq
            | TokKind::MinusEq
            | TokKind::StarEq
            | TokKind::SlashEq
            | TokKind::BackslashEq
            | TokKind::SlashSlashEq
            | TokKind::CaretEq
            | TokKind::PercentEq
            | TokKind::PlusPercentEq
            | TokKind::MinusPercentEq
            | TokKind::StarPercentEq
            | TokKind::PipeEq
            | TokKind::DollarEq
            | TokKind::AmpEq
            | TokKind::ShlEq
            | TokKind::ShrEq
            | TokKind::UShrEq
            | TokKind::DivEq
            | TokKind::XorEq
            | TokKind::DotPlusEq
            | TokKind::DotMinusEq
            | TokKind::DotStarEq
            | TokKind::DotSlashEq
            | TokKind::DotBackslashEq
            | TokKind::DotSlashSlashEq
            | TokKind::DotCaretEq
            | TokKind::DotPercentEq
            | TokKind::DotAmpEq
            | TokKind::DotPipeEq
            | TokKind::DotShlEq
            | TokKind::DotShrEq
            | TokKind::DotUShrEq
            | TokKind::DotDivEq
            | TokKind::DotXorEq
    )
}

pub(super) fn infix_binding_power(kind: TokKind) -> Option<(u8, u8)> {
    Some(match kind {
        // `~` (and broadcast `.~`) sits at the assignment tier: right-associative
        // and as loose as `=` (`a ~ b = c` ⇒ `(~ a (= b c))`, `x = a ~ b` ⇒
        // `(= x (~ a b))`), but builds an ordinary `(call-i a ~ b)`, not an
        // assignment. Handled here (not `is_assignment_op`) so the node stays
        // `BINARY_EXPR`.
        TokKind::Tilde | TokKind::DotTilde => (2, 1),
        // `:=` sits at the assignment tier like the Unicode `≔`: right-associative
        // and as loose as `=`, but builds an ordinary `BINARY_EXPR` that keeps its
        // own head (`(:= a b)`) rather than lowering to an assignment.
        TokKind::ColonEq => (2, 1),
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
        // The lambda arrow `->` is *not* an ordinary arrow-tier operator: Julia
        // parses it with a high left binding power and a very low right one, so it
        // binds a tight left operand but sweeps everything looser into its body
        // (`1 + 2 -> 3` ⇒ `(call-i 1 + (-> 2 3))`, `x |> y -> y + 1` ⇒
        // `(call-i x |> (-> y (call-i y + 1)))`, `a -> b = c` ⇒ `(-> a (= b c))`).
        // The left binding power sits just below `::`/`.`/postfix `'` (so
        // `a::b -> c` ⇒ `(-> (::-i a b) c)`) and above `^` (so `2 ^ 3 -> 4` ⇒
        // `2 ^ (3 -> 4)`); the right one is below assignment/ternary so the body
        // absorbs them. Right-associative (`a -> b -> c` ⇒ `(-> a (-> b c))`).
        // The other arrows (`-->`, `→`, `<--`, …) and the pair `=>` stay ordinary
        // arrow-tier operators below.
        TokKind::Arrow => (35, 1),
        // The pair `=>` shares the arrow/ternary tier: right-associative, looser
        // than `||` and tighter than `=` (`a || b => c = d` ⇒ `(= (=> (|| a b) c) d)`).
        TokKind::FatArrow
        | TokKind::DotFatArrow
        | TokKind::LongArrow
        | TokKind::LeftRightArrow
        | TokKind::LeftLongArrow
        | TokKind::DotLongArrow
        | TokKind::DotLeftLongArrow
        | TokKind::DotLeftRightArrow => (4, 3),
        // Short-circuit `||`/`&&` (and broadcast `.||`/`.&&`) are
        // right-associative (`a && b && c` ⇒ `(&& a (&& b c))`), so `r_bp <
        // l_bp`; `&&` binds tighter than `||`, both looser than the comparisons.
        TokKind::OrOr | TokKind::DotOrOr => (6, 5),
        TokKind::AndAnd | TokKind::DotAndAnd => (8, 7),
        // `where` is not an ordinary infix operator: it is a left-associative
        // chain handled directly in the operator loop (see `parse_where_chain`),
        // binding tighter than every binary operator but looser than
        // `^`/juxtaposition/`.`. It returns `None` here so the generic path stops
        // at it.
        TokKind::EqEq
        | TokKind::NotEq
        | TokKind::EqEqEq
        | TokKind::NotEqEq
        | TokKind::Lt
        | TokKind::Le
        | TokKind::Gt
        | TokKind::Ge
        | TokKind::Subtype
        | TokKind::Supertype
        | TokKind::DotEqEq
        | TokKind::DotNotEq
        | TokKind::DotEqEqEq
        | TokKind::DotNotEqEq
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
        // The invalid doubled operators `**`/`--` (and broadcast `.**`/`.--`)
        // sit at their own low tier, looser than `+` and tighter than `:`/`==`
        // (`a+b**c` ⇒ `(a+b)**c`, `a**b:c` ⇒ `(a**b):c`), left-associative.
        TokKind::StarStar | TokKind::MinusMinus | TokKind::DotStarStar | TokKind::DotMinusMinus => {
            (18, 19)
        }
        // Bitwise-or `|` shares the `+` (plus) precedence family, left-associative
        // (`a | b & c` ⇒ `(a | (b & c))`, `a & b | c` ⇒ `((a & b) | c)`).
        // The wrapping `+%`/`-%` share the `+` tier (JuliaSyntax `is_prec_plus`).
        // `$` is two operators in one spelling: the interpolation sigil in atom
        // position (handled by `parse_prefix_interpolation`) and Julia's old xor
        // operator in infix position, also at the `+` tier — the Pratt loop only
        // consults this table in infix position, so the two never collide.
        TokKind::Dollar
        | TokKind::Plus
        | TokKind::Minus
        | TokKind::PlusPercent
        | TokKind::MinusPercent
        | TokKind::PlusPlus
        | TokKind::DotPlus
        | TokKind::DotMinus
        | TokKind::Pipe
        | TokKind::DotPipe => (20, 21),
        // Bitwise-and `&` shares the `*` (times) precedence family, left-associative
        // (`a & b * c` ⇒ `((a & b) * c)`, `a + b & c` ⇒ `(a + (b & c))`).
        // The wrapping `*%` shares the `*` tier (JuliaSyntax `is_prec_times`).
        TokKind::Star
        | TokKind::Slash
        | TokKind::Backslash
        | TokKind::Percent
        | TokKind::StarPercent
        | TokKind::Amp
        | TokKind::DotAmp
        | TokKind::DotStar
        | TokKind::DotSlash
        | TokKind::DotBackslash
        | TokKind::DotPercent => (24, 25),
        // Rational `//` (and broadcast `.//`) bind tighter than `*`/`/` but
        // looser than `^`, and are left-associative (`a//b//c` ⇒ `(a//b)//c`).
        TokKind::SlashSlash | TokKind::DotSlashSlash => (28, 29),
        // Bitshift `<< >> >>>` binds tighter than `//` and looser than `^`
        // (Julia precedence 14), left-associative.
        TokKind::Shl | TokKind::Shr | TokKind::UShr => (30, 31),
        TokKind::DotShl | TokKind::DotShr | TokKind::DotUShr => (30, 31),
        TokKind::Caret | TokKind::DotCaret => (34, 33),
        TokKind::ColonColon => (36, 37),
        TokKind::Dot => (40, 41),
        _ => return None,
    })
}
