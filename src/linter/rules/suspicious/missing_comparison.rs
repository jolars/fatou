//! `missing-comparison`: `x == missing` / `x != missing` compares against
//! `missing` by value, which can only ever produce `missing`.
//!
//! `missing` propagates through `==`: `1 == missing` is `missing`, and so is
//! `missing == missing`. The comparison therefore carries no information
//! whatever — it is `missing` regardless of `x` — and using it where a `Bool`
//! is required raises `TypeError: non-boolean (Missing) used in boolean
//! context`. What is meant is `ismissing(x)` (or the identity test `===` /
//! `!==`), which answers the question with a real `Bool`.
//!
//! The shape rules mirror [`super::NothingComparison`]: only the bare
//! two-operand form is flagged, since a lone comparison parses as a
//! `BINARY_EXPR` whereas a chain (`a < b == missing`) folds into a
//! `COMPARISON_EXPR`, and the already-correct `===` / `!==` carry their own
//! operator kinds. The broadcast `.==` / `.!=` are likewise distinct kinds and
//! are left alone: elementwise comparison over a container is a different
//! question from the scalar identity test. `missing` is matched on either side
//! by identifier text — it is a `Core` constant that is practically never
//! shadowed — and the capitalized `Missing` *type* is a different identifier.
//!
//! **The fix is `Unsafe`, unlike `nothing-comparison`'s.** Both rewrite the
//! operator in place (`==` -> `===`, `!=` -> `!==`), but the two differ in what
//! that does to the value. `x == nothing` dispatches `Base.==`, which for the
//! `Nothing` singleton agrees with `===` for any sane type, so that rewrite
//! preserves behavior. Here the original expression evaluates to `missing` and
//! the rewrite makes it a `Bool` — that change *is* the point (it is what
//! unbreaks the boolean context), but it is a behavior change, so it waits for
//! `--unsafe-fixes`. Code that deliberately relies on `missing` propagating out
//! of the comparison would be altered by it.

use crate::ast::{AstNode, AstToken, BinaryExpr, Expr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::matchers;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct MissingComparison;

impl Rule for MissingComparison {
    fn id(&self) -> &'static str {
        "missing-comparison"
    }

    fn description(&self) -> &'static str {
        "Flag `x == missing` / `x != missing`. `missing` propagates through \
         `==`, so the comparison is always `missing` no matter what `x` is, and \
         using it as a condition raises a `TypeError`. Use `ismissing` (or the \
         identity test `===` / `!==`) instead. The rule reports an unsafe fix \
         rewriting `==` to `===` and `!=` to `!==`: the rewrite turns a \
         `missing` result into a `Bool`, which is the intent but is still a \
         change in behavior."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "Comparing against `missing` by value:",
            source: "if x == missing\n    1\nend\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::BINARY_EXPR]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(bin) = el.as_node().cloned().and_then(BinaryExpr::cast) else {
            return;
        };
        // Only `==` / `!=`; `===` / `!==` and the broadcast `.==` / `.!=` carry
        // their own operator kinds and are out of scope.
        let Some(op) = bin.op() else { return };
        let replacement = match op.syntax().kind() {
            SyntaxKind::EQ_EQ => "===",
            SyntaxKind::NOT_EQ => "!==",
            _ => return,
        };

        // Match `missing` on either operand by identifier text. It is a `Core`
        // constant that is practically never shadowed, so this is sound; the
        // capitalized `Missing` type is a distinct identifier.
        let is_missing =
            |operand: Option<Expr>| operand.is_some_and(|expr| matchers::is_name(&expr, "missing"));
        if !is_missing(bin.lhs()) && !is_missing(bin.rhs()) {
            return;
        }

        let op_range = op.syntax().text_range();
        let mut diag = Diagnostic::new(
            self.id(),
            bin.syntax().text_range(),
            format!(
                "comparison against `missing` by value is always `missing`; \
                 use `ismissing` or `{replacement}`"
            ),
        );
        diag.fixes.push(Fix {
            description: format!("Replace `{}` with `{replacement}`", op.text()),
            content: replacement.to_string(),
            start: op_range.start().into(),
            end: op_range.end().into(),
            applicability: Applicability::Unsafe,
        });
        sink.push(diag);
    }
}
