//! `typeof-comparison`: `typeof(x) == T` (and `typeof(x) != T`, and both
//! mirrored spellings) asks whether `x`'s type is *exactly* `T`, silently
//! answering `false` for every subtype of `T`.
//!
//! That is almost never the question. `typeof(x) == Integer` is `false` for
//! every integer, since `typeof` returns the concrete `Int64`; `typeof(x) ==
//! AbstractString` is `false` for every string. What is meant is `x isa T`,
//! which is the subtype test, dispatches without allocating a type object, and
//! is the spelling Julia's style guide uses. The exact-type question is real
//! but rare, so this is `suspicious` rather than `readability`: the `==` form
//! is much more often a bug than an idiom.
//!
//! Four shapes are deliberately left alone:
//!
//! - `===` / `!==` carry their own operator kinds and *are* the spelling for
//!   exact-type identity — flagging them would be backwards.
//! - `.==` / `.!=` are distinct kinds too: an elementwise comparison over a
//!   container is a different question from the scalar type test.
//! - A comparison *chain* (`a < typeof(x) == T`) folds into a
//!   `COMPARISON_EXPR` rather than a `BINARY_EXPR`, so only the bare
//!   two-operand form reaches the check at all — the same shape rule
//!   [`super::NothingComparison`] follows.
//! - `typeof(a) == typeof(b)` compares two *dynamic* types. There is no `isa`
//!   rewrite for it (`a isa typeof(b)` would accept subtypes of `b`'s type,
//!   which is a different question), and comparing two runtime types for
//!   equality is the legitimate use of the `==` form.
//!
//! The callee must be confirmed to be Base's `typeof` via
//! [`RuleContext::resolves_to_base`], the conservative namespace gate every
//! idiom rule opens with: a local shadow, a qualified callee (`Base.typeof`),
//! or a file whose imports cannot be resolved reports nothing.
//!
//! **The fix is `Unsafe`.** Rewriting to `x isa T` widens the test from the
//! exact type to the whole subtype tree, and a caller who genuinely wanted
//! exact-type identity — `typeof(x) == DataType`, a dispatch-free type
//! discriminator — would be changed by it. That widening is the whole point of
//! the rule, but it is a behavior change, so it waits for `--unsafe-fixes`.
//!
//! The fix is **withheld** (the finding still stands) for two shapes it could
//! not edit correctly by construction. First, when a comment sits inside the
//! replaced span, which the rewrite would drop. Second, when `typeof`'s
//! argument does not bind at least as tightly as `isa` does: splicing
//! `a ? b : c` in as `isa`'s left operand would rebind the `:` arm
//! (`a ? b : (c isa T)`), and a macro call would slurp the `isa` into its own
//! arguments. The *other* operand never needs that check — it is already an
//! operand of a comparison-tier `==`, so it binds at least as tightly as the
//! `isa` replacing it.

use crate::ast::{AstNode, AstToken, BinaryExpr, CallExpr, Expr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::matchers;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct TypeofComparison;

impl Rule for TypeofComparison {
    fn id(&self) -> &'static str {
        "typeof-comparison"
    }

    fn description(&self) -> &'static str {
        "Flag `typeof(x) == T` / `typeof(x) != T` (in either operand order), \
         which tests for the *exact* type and so answers `false` for every \
         subtype of `T` — `typeof(x) == Integer` is `false` for every integer, \
         since `typeof` returns the concrete `Int64`. Use `x isa T`, the \
         subtype test.\n\n\
         The already-correct `===` / `!==`, the broadcast `.==` / `.!=`, and a \
         comparison chain are left alone, as is `typeof(a) == typeof(b)`, which \
         compares two runtime types and has no `isa` spelling. The callee must \
         be confirmed to be Base's `typeof`, so a local shadow, a qualified \
         `Base.typeof`, or a file whose imports cannot be resolved reports \
         nothing.\n\n\
         The rule reports an unsafe fix rewriting the comparison to `x isa T` \
         (`!(x isa T)` for `!=`): widening the exact-type test to the subtype \
         tree is the intent, but it is still a change in behavior, and a caller \
         may genuinely want exact-type identity. The fix is withheld — the \
         finding still stands — when a comment sits inside the rewritten span, \
         or when `typeof`'s argument binds more loosely than `isa` and so \
         cannot be spliced in without parentheses."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`typeof(x) == Integer` is `false` for every integer:",
            source: "if typeof(x) == Integer\n    1\nend\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::BINARY_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(bin) = el.as_node().cloned().and_then(BinaryExpr::cast) else {
            return;
        };
        // Only `==` / `!=`; `===` / `!==` and the broadcast forms carry their
        // own operator kinds and are out of scope.
        let Some(op) = bin.op() else { return };
        let negated = match op.syntax().kind() {
            SyntaxKind::EQ_EQ => false,
            SyntaxKind::NOT_EQ => true,
            _ => return,
        };
        let (Some(lhs), Some(rhs)) = (bin.lhs(), bin.rhs()) else {
            return;
        };

        // Which side is the `typeof` call — and, when both are, bail: comparing
        // two dynamic types is the legitimate use of the `==` form.
        let (call_side, ty) = match (names_typeof(&lhs), names_typeof(&rhs)) {
            (true, true) | (false, false) => return,
            (true, false) => (&lhs, &rhs),
            (false, true) => (&rhs, &lhs),
        };
        let Some((call, value)) = base_arity_typeof(call_side) else {
            return;
        };
        if !ctx.resolves_to_base(&call) {
            return;
        }

        let form = if negated {
            "`typeof(x) != T` matches only the exact type; use `!(x isa T)`, which includes subtypes"
        } else {
            "`typeof(x) == T` matches only the exact type; use `x isa T`, which includes subtypes"
        };
        let mut diag = Diagnostic::new(self.id(), bin.syntax().text_range(), form.to_string());
        if let Some(fix) = isa_rewrite(&bin, &value, ty, negated) {
            diag.fixes.push(fix);
        }
        sink.push(diag);
    }
}

/// Whether `expr` is a call whose callee is the bare name `typeof`, with no
/// claim about its arguments. The cheap check the `typeof(a) == typeof(b)`
/// bail-out needs, before either side is matched strictly.
fn names_typeof(expr: &Expr) -> bool {
    let Expr::CallExpr(call) = expr else {
        return false;
    };
    matchers::call_named(call.syntax(), "typeof").is_some()
}

/// `expr` as a `typeof` call at Base's arity — exactly one positional
/// argument, no keywords, no splat, no `do` block — together with that
/// argument.
fn base_arity_typeof(expr: &Expr) -> Option<(CallExpr, Expr)> {
    let Expr::CallExpr(call) = expr else {
        return None;
    };
    let (call, mut args) = matchers::plain_call(call.syntax(), "typeof", 1)?;
    Some((call, args.pop()?))
}

/// The unsafe fix replacing the whole comparison with an `isa` test.
///
/// The edit is built from the two operands' own source text, so whatever they
/// contain survives byte-for-byte; it is withheld when a comment sits anywhere
/// in the replaced span (the rewrite would drop it) or when `value` binds more
/// loosely than `isa` (see [`binds_at_least_as_tight_as_isa`]).
fn isa_rewrite(bin: &BinaryExpr, value: &Expr, ty: &Expr, negated: bool) -> Option<Fix> {
    if !binds_at_least_as_tight_as_isa(value) {
        return None;
    }
    let range = bin.syntax().text_range();
    // `#` opens both comment forms. A `#` inside a string literal costs a fix
    // that would have been fine, which is the safe direction to be wrong in.
    if bin.syntax().text().to_string().contains('#') {
        return None;
    }
    let value = value.syntax().text().to_string();
    let ty = ty.syntax().text().to_string();
    let content = if negated {
        format!("!({value} isa {ty})")
    } else {
        format!("{value} isa {ty}")
    };
    Some(Fix {
        description: "Rewrite as an `isa` test".to_string(),
        content,
        start: range.start().into(),
        end: range.end().into(),
        applicability: Applicability::Unsafe,
    })
}

/// Whether `expr` can be spliced in as `isa`'s left operand without
/// parentheses: it is delimited (a literal, a name, anything bracketed), a
/// postfix chain, or a prefix operator — all of which bind tighter than the
/// comparison tier `isa` sits in.
///
/// A `BINARY_EXPR` qualifies only for field access (`a.b`), the tightest infix
/// operator there is. Deciding it for the rest of the infix operators needs the
/// parser's precedence table, which is not exposed over the CST, so this rule
/// withholds the fix there rather than guessing — a missing fix on
/// `typeof(a + b) == T` costs nothing, a misbinding one costs correctness.
fn binds_at_least_as_tight_as_isa(expr: &Expr) -> bool {
    match expr {
        Expr::Literal(_)
        | Expr::StringLiteral(_)
        | Expr::CmdLiteral(_)
        | Expr::NonstandardIdentifier(_)
        | Expr::Name(_)
        | Expr::UnaryExpr(_)
        | Expr::ParenExpr(_)
        | Expr::TupleExpr(_)
        | Expr::VectExpr(_)
        | Expr::MatrixExpr(_)
        | Expr::Comprehension(_)
        | Expr::Braces(_)
        | Expr::CurlyExpr(_)
        | Expr::CallExpr(_)
        | Expr::IndexExpr(_)
        | Expr::DotCallExpr(_) => true,
        Expr::BinaryExpr(bin) => bin
            .op()
            .is_some_and(|op| op.syntax().kind() == SyntaxKind::DOT),
        _ => false,
    }
}
