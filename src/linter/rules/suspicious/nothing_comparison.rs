//! `nothing-comparison`: `x == nothing` / `x != nothing` compares against
//! `nothing` by value (dispatching `Base.==` / `Base.!=`) instead of by
//! identity. `nothing` is the sole instance of the singleton `Nothing`, so an
//! identity test (`===` / `!==`, or `isnothing`) is what is meant: it is
//! faster, cannot be overloaded into surprising behavior, and matches the
//! idiom the Julia style guide recommends.
//!
//! Only the bare two-operand form is flagged: a lone comparison parses as a
//! `BINARY_EXPR`, whereas a chain (`a < b == nothing`) folds into a
//! `COMPARISON_EXPR`, and the already-correct `===` / `!==` carry their own
//! operator kinds. `nothing` is matched on either side by its identifier text;
//! it is a `Core` constant that is practically never shadowed, so a name-based
//! match is sound. The capitalized `Nothing` *type* is a different identifier
//! and is left alone. The fix rewrites `==` -> `===` and `!=` -> `!==`, a safe
//! edit that touches only the operator token.

use crate::ast::{AstNode, AstToken, BinaryExpr, Expr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct NothingComparison;

impl Rule for NothingComparison {
    fn id(&self) -> &'static str {
        "nothing-comparison"
    }

    fn description(&self) -> &'static str {
        "Flag `x == nothing` / `x != nothing`, which compares against `nothing` \
         by value. `nothing` is the singleton instance of `Nothing`, so an \
         identity test (`===` / `!==`, or `isnothing`) is meant: it is faster and \
         cannot be overloaded. The rule reports a safe fix rewriting `==` to \
         `===` and `!=` to `!==`."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "Comparing against `nothing` by value:",
            source: "if x == nothing\n    1\nend\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::BINARY_EXPR]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(bin) = el.as_node().cloned().and_then(BinaryExpr::cast) else {
            return;
        };
        // Only `==` / `!=`; `===` / `!==` (and the dotted/word forms) carry
        // their own operator kinds and are already correct.
        let Some(op) = bin.op() else { return };
        let replacement = match op.syntax().kind() {
            SyntaxKind::EQ_EQ => "===",
            SyntaxKind::NOT_EQ => "!==",
            _ => return,
        };

        // Match `nothing` on either operand by identifier text. It is a `Core`
        // constant that is practically never shadowed, so this is sound; the
        // capitalized `Nothing` type is a distinct identifier.
        let is_nothing = |operand: Option<Expr>| {
            matches!(operand, Some(Expr::Name(name))
                if name.ident().is_some_and(|id| id.text() == "nothing"))
        };
        if !is_nothing(bin.lhs()) && !is_nothing(bin.rhs()) {
            return;
        }

        let op_range = op.syntax().text_range();
        let mut diag = Diagnostic::new(
            self.id(),
            bin.syntax().text_range(),
            format!("comparison against `nothing` by value; use `{replacement}` or `isnothing`"),
        );
        diag.fixes.push(Fix {
            description: format!("Replace `{}` with `{replacement}`", op.text()),
            content: replacement.to_string(),
            start: op_range.start().into(),
            end: op_range.end().into(),
            applicability: Applicability::Safe,
        });
        sink.push(diag);
    }
}
