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
use crate::linter::rules::matchers;
use crate::linter::rules::test_matchers::is_direct_test_expression;
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
         `===` and `!=` to `!==`. Inside a real `Test.@test` assertion, it \
         instead prefers `isnothing(x)` or `!isnothing(x)` when that name is \
         confirmed to mean Base's predicate."
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

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
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
        let (Some(lhs), Some(rhs)) = (bin.lhs(), bin.rhs()) else {
            return;
        };
        let value = match (
            matchers::is_name(&lhs, "nothing"),
            matchers::is_name(&rhs, "nothing"),
        ) {
            (true, false) => &rhs,
            (false, true) => &lhs,
            (true, true) => &rhs,
            (false, false) => return,
        };

        let op_range = op.syntax().text_range();
        let mut diag = Diagnostic::new(
            self.id(),
            bin.syntax().text_range(),
            format!("comparison against `nothing` by value; use `{replacement}` or `isnothing`"),
        );
        if let Some(fix) = test_predicate_fix(ctx, &bin, value, replacement == "!==") {
            diag.fixes.push(fix);
        } else {
            diag.fixes.push(Fix {
                description: format!("Replace `{}` with `{replacement}`", op.text()),
                content: replacement.to_string(),
                start: op_range.start().into(),
                end: op_range.end().into(),
                applicability: Applicability::Safe,
            });
        }
        sink.push(diag);
    }
}

fn test_predicate_fix(
    ctx: &RuleContext<'_>,
    comparison: &BinaryExpr,
    value: &Expr,
    negated: bool,
) -> Option<Fix> {
    if !is_direct_test_expression(comparison.syntax(), ctx) {
        return None;
    }
    let range = comparison.syntax().text_range();
    if !can_write_base_isnothing(ctx, range.start()) {
        return None;
    }
    let outer = comparison.syntax().text_range();
    let inner = value.syntax().text_range();
    let text = comparison.syntax().text().to_string();
    let start = usize::from(inner.start() - outer.start());
    let end = usize::from(inner.end() - outer.start());
    if text[..start].contains('#') || text[end..].contains('#') {
        return None;
    }
    let value = value.syntax().text().to_string();
    Some(Fix {
        description: "Rewrite as an `isnothing` test".to_string(),
        content: if negated {
            format!("!isnothing({value})")
        } else {
            format!("isnothing({value})")
        },
        start: range.start().into(),
        end: range.end().into(),
        applicability: Applicability::Safe,
    })
}

/// Confirm that inserting the bare name `isnothing` cannot select a file-local
/// binding or an unknown whole-module export.
///
/// A visible `using Test` intentionally makes project-wide resolution
/// untrustworthy in the lightweight lint path when the stdlib index is absent.
/// Base's own export table is still known, so retain the ordinary resolver fast
/// path and use this stricter semantic fallback for that case.
fn can_write_base_isnothing(ctx: &RuleContext<'_>, at: rowan::TextSize) -> bool {
    if ctx.name_resolves_to_base("isnothing", at) {
        return true;
    }
    if ctx
        .model
        .names_in_scope_at(at)
        .into_iter()
        .any(|id| ctx.model.binding(id).name == "isnothing")
    {
        return false;
    }
    if ctx.model.module_loads().iter().any(|load| {
        load.kind == crate::semantic::LoadKind::Using
            && load.items.is_none()
            && (load.path.leading_dots != 0 || load.path.components.as_slice() != ["Test"])
    }) {
        return false;
    }
    ctx.base_export_module("isnothing").is_some()
}
