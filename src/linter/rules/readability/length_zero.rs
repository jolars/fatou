//! `length-zero`: comparing `length(x)` against `0` or `1` to ask whether `x`
//! is empty, where Julia spells the question `isempty(x)`.
//!
//! A length is a non-negative integer, so six two-operand comparisons collapse
//! to one of two tests, and each has a mirrored spelling with the literal
//! first:
//!
//! | emptiness            | non-emptiness        |
//! |----------------------|----------------------|
//! | `length(x) == 0`     | `length(x) != 0`     |
//! | `length(x) <= 0`     | `length(x) > 0`      |
//! | `length(x) < 1`      | `length(x) >= 1`     |
//!
//! `isempty` is not merely shorter. It is the emptiness test collections
//! actually implement — for an iterator that only knows how to produce its next
//! element, `isempty` asks exactly that, while `length` must walk the whole
//! collection to count what is thrown away. The two agree on every collection
//! whose `length` and iteration agree, which is every collection that is not
//! already broken.
//!
//! Only the bare two-operand form is considered. A comparison *chain*
//! (`0 < length(x) < 3`) folds into a `COMPARISON_EXPR` rather than a
//! `BINARY_EXPR`, so it never reaches the check — the same shape rule
//! [`NothingComparison`](crate::linter::rules::suspicious::NothingComparison)
//! follows. `===` and the broadcast `.==` / `.<` carry their own operator
//! kinds and are left alone: an elementwise comparison over a container is a
//! different question, and `===` is the deliberate identity spelling.
//!
//! The bound must be the integer literal `0` or `1` written as such. The
//! comparisons that are constant by construction (`length(x) >= 0`,
//! `length(x) < 0`) are not emptiness tests and belong to no rule here, and
//! `length(x) == 1` asks a different question entirely.
//!
//! Two namespace gates, not one. The callee must be confirmed to be Base's
//! `length` via [`RuleContext::resolves_to_base`], the conservative gate every
//! idiom rule opens with. The *fix* additionally needs `isempty` to mean Base's
//! `isempty` at the rewrite site
//! ([`RuleContext::name_resolves_to_base`]) — it is a name the source does not
//! contain yet, and splicing it into a file that defines its own `isempty`
//! would call that one. The finding still stands when that second gate fails;
//! only the fix is withheld.
//!
//! **The fix is `Safe`.** It replaces the whole comparison with
//! `isempty(<arg>)` (or `!isempty(<arg>)`), reusing the argument's own source
//! text, so whatever it contains survives byte-for-byte. No parenthesization
//! question arises: the argument lands back inside a call's parentheses, and
//! both replacements bind at least as tightly as the comparison they replace,
//! so no surrounding operator can rebind.
//!
//! The fix is **withheld** (the finding still stands) when a comment sits in
//! the replaced span *outside* the argument — around the operator, or between
//! `length` and its argument — which the rewrite would drop. A comment inside
//! the argument travels with it and costs nothing.

use crate::ast::{AstNode, AstToken, BinaryExpr, CallExpr, Expr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::matchers;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct LengthZero;

impl Rule for LengthZero {
    fn id(&self) -> &'static str {
        "length-zero"
    }

    fn description(&self) -> &'static str {
        "Flag a comparison of `length(x)` against `0` or `1` that asks whether \
         `x` is empty. A length is a non-negative integer, so `length(x) == 0`, \
         `length(x) <= 0`, and `length(x) < 1` all spell `isempty(x)`, while \
         `length(x) != 0`, `length(x) > 0`, and `length(x) >= 1` spell \
         `!isempty(x)`; each mirrored spelling with the literal first \
         (`0 < length(x)`) collapses the same way. `isempty` is the test \
         collections actually implement, and it need not count what it throws \
         away.\n\n\
         The already-deliberate `===`, the broadcast `.==` / `.<`, and a \
         comparison chain are left alone, as are the bounds that ask a \
         different question (`length(x) == 1`) or none at all \
         (`length(x) >= 0`, which is always true). The callee must be confirmed \
         to be Base's `length`, so a local shadow, a qualified `Base.length`, \
         or a file whose imports cannot be resolved reports nothing.\n\n\
         The rule reports a safe fix replacing the comparison with \
         `isempty(x)` or `!isempty(x)`, reusing the argument's own source text. \
         The fix is withheld — the finding still stands — when the file's \
         `isempty` is not Base's, or when a comment sits in the rewritten span \
         outside the argument."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "Comparing a length against zero is an emptiness test:",
                source: "if length(xs) == 0\n    println(\"nothing to do\")\nend\n",
            },
            Example {
                caption: "The non-emptiness spellings, including the mirrored ones:",
                source: "while 0 < length(queue)\n    pop!(queue)\nend\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::BINARY_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(bin) = el.as_node().cloned().and_then(BinaryExpr::cast) else {
            return;
        };
        let Some(op) = bin.op() else { return };
        let (Some(lhs), Some(rhs)) = (bin.lhs(), bin.rhs()) else {
            return;
        };

        // Orient the comparison as `length(x) <op> <literal>`, mirroring the
        // operator when the source wrote the bound first.
        let (call, coll, bound, op) = match base_arity_length(&lhs) {
            Some((call, coll)) => (call, coll, &rhs, op.syntax().kind()),
            None => {
                let (Some((call, coll)), Some(op)) =
                    (base_arity_length(&rhs), mirror(op.syntax().kind()))
                else {
                    return;
                };
                (call, coll, &lhs, op)
            }
        };
        let Some(empty) = emptiness_test(op, bound) else {
            return;
        };
        if !ctx.resolves_to_base(&call) {
            return;
        }

        let coll_text = coll.syntax().text().to_string();
        let bound_text = bound.syntax().text().to_string();
        let negation = if empty { "" } else { "!" };
        let message = format!(
            "test `{negation}isempty({coll_text})` instead of comparing \
             `length({coll_text})` to `{bound_text}`"
        );
        let mut diag = Diagnostic::new(self.id(), bin.syntax().text_range(), message);
        if let Some(fix) = isempty_rewrite(ctx, &bin, &coll, empty) {
            diag.fixes.push(fix);
        }
        sink.push(diag);
    }
}

/// `expr` as a `length` call at Base's arity — exactly one positional
/// argument, no keywords, no splat, no `do` block — together with that
/// argument.
fn base_arity_length(expr: &Expr) -> Option<(CallExpr, Expr)> {
    let Expr::CallExpr(call) = expr else {
        return None;
    };
    let (call, mut args) = matchers::plain_call(call.syntax(), "length", 1)?;
    Some((call, args.pop()?))
}

/// The operator as it would read with the operands swapped, so the mirrored
/// spelling `0 < length(x)` is decided by the same table as `length(x) > 0`.
/// `==` and `!=` are symmetric; every other operator kind (including `===` and
/// the broadcast forms) is out of scope and answers `None`.
fn mirror(op: SyntaxKind) -> Option<SyntaxKind> {
    Some(match op {
        SyntaxKind::EQ_EQ => SyntaxKind::EQ_EQ,
        SyntaxKind::NOT_EQ => SyntaxKind::NOT_EQ,
        SyntaxKind::LT => SyntaxKind::GT,
        SyntaxKind::GT => SyntaxKind::LT,
        SyntaxKind::LE => SyntaxKind::GE,
        SyntaxKind::GE => SyntaxKind::LE,
        _ => return None,
    })
}

/// Whether `length(x) <op> <bound>` is an emptiness test (`Some(true)`), a
/// non-emptiness test (`Some(false)`), or neither.
///
/// `bound` must be the integer literal `0` or `1` written as such: a float
/// (`0.0`), another base (`0x0`), or any non-literal expression leaves the
/// comparison alone, since none of them is the idiom this rule rewrites.
fn emptiness_test(op: SyntaxKind, bound: &Expr) -> Option<bool> {
    let Expr::Literal(literal) = bound else {
        return None;
    };
    let token = literal.numeric_token()?;
    if token.kind() != SyntaxKind::INTEGER {
        return None;
    }
    Some(match (op, token.text()) {
        (SyntaxKind::EQ_EQ, "0") | (SyntaxKind::LE, "0") | (SyntaxKind::LT, "1") => true,
        (SyntaxKind::NOT_EQ, "0") | (SyntaxKind::GT, "0") | (SyntaxKind::GE, "1") => false,
        _ => return None,
    })
}

/// The safe fix replacing the whole comparison with an `isempty` test.
///
/// Withheld when `isempty` would not mean Base's at this point in the file, or
/// when a comment sits in the replaced span outside `coll` — the only tokens
/// there are `length`, the parentheses, the operator, and the literal, so a
/// `#` outside `coll` is always a comment and never a string.
fn isempty_rewrite(
    ctx: &RuleContext<'_>,
    bin: &BinaryExpr,
    coll: &Expr,
    empty: bool,
) -> Option<Fix> {
    let range = bin.syntax().text_range();
    if !ctx.name_resolves_to_base("isempty", range.start()) {
        return None;
    }
    if comment_outside(bin, coll) {
        return None;
    }
    let coll = coll.syntax().text().to_string();
    let content = if empty {
        format!("isempty({coll})")
    } else {
        format!("!isempty({coll})")
    };
    Some(Fix {
        description: "Rewrite as an `isempty` test".to_string(),
        content,
        start: range.start().into(),
        end: range.end().into(),
        applicability: Applicability::Safe,
    })
}

/// Whether a `#` sits anywhere in `bin` but outside `coll`. The two offsets
/// are token boundaries within `bin`'s own text, so the slicing is
/// char-boundary safe.
fn comment_outside(bin: &BinaryExpr, coll: &Expr) -> bool {
    let outer = bin.syntax().text_range();
    let inner = coll.syntax().text_range();
    let text = bin.syntax().text().to_string();
    let head = usize::from(inner.start() - outer.start());
    let tail = usize::from(inner.end() - outer.start());
    text[..head].contains('#') || text[tail..].contains('#')
}
