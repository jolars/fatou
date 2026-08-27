//! `test-bare-expression`: an opt-in check for `@test` assertions whose
//! argument has no syntactic comparison, Boolean connective, or predicate
//! call.
//!
//! Julia is dynamic, so a bare variable or indexed value may certainly contain
//! a `Bool`. The check is therefore deliberately conservative about calls and
//! deliberately default-off: it is a test-suite policy that asks authors to
//! state the asserted relationship rather than a correctness proof.

use crate::ast::{AstNode, AstToken, BinaryExpr, Expr, MacroCall};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::test_matchers::test_invocation;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct TestBareExpression;

impl Rule for TestBareExpression {
    fn id(&self) -> &'static str {
        "test-bare-expression"
    }

    fn description(&self) -> &'static str {
        "Flag an `@test` argument that has no syntactic comparison, Boolean \
         connective, or predicate call—for example, `@test ready`. Calls and \
         nested macro calls are treated conservatively as potentially Boolean. \
         The rule only matches the real `Test.@test` macro after a visible, \
         file-local Test load. It is disabled by default because Julia's \
         dynamic types make bare Boolean values valid, and it offers no fix."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A bare value hides what relationship the test asserts:",
            source: "using Test\n@test result\n",
        }]
    }

    fn default_enabled(&self) -> bool {
        false
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::MACRO_CALL]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(call) = el.as_node().cloned().and_then(MacroCall::cast) else {
            return;
        };
        let Some(invocation) = test_invocation(&call, ctx) else {
            return;
        };
        if boolean_shaped(&invocation.expression) {
            return;
        }
        sink.push(Diagnostic::new(
            self.id(),
            invocation.expression.syntax().text_range(),
            "this assertion has no comparison or predicate".to_string(),
        ));
    }
}

fn boolean_shaped(expr: &Expr) -> bool {
    match expr {
        Expr::CallExpr(_) | Expr::DotCallExpr(_) | Expr::MacroCall(_) => true,
        Expr::ParenExpr(paren) => paren.expr().is_some_and(|inner| boolean_shaped(&inner)),
        Expr::UnaryExpr(unary) => unary
            .op()
            .is_some_and(|op| op.syntax().kind() == SyntaxKind::BANG),
        Expr::BinaryExpr(binary) => binary_is_boolean_shaped(binary),
        Expr::Other(node) => node.kind() == SyntaxKind::COMPARISON_EXPR,
        _ => false,
    }
}

fn binary_is_boolean_shaped(binary: &BinaryExpr) -> bool {
    if binary.is_comparison() {
        return true;
    }
    binary.op().is_some_and(|op| {
        matches!(
            op.syntax().kind(),
            SyntaxKind::EQ_EQ
                | SyntaxKind::NOT_EQ
                | SyntaxKind::EQ_EQ_EQ
                | SyntaxKind::NOT_EQ_EQ
                | SyntaxKind::LT
                | SyntaxKind::LE
                | SyntaxKind::GT
                | SyntaxKind::GE
                | SyntaxKind::SUBTYPE
                | SyntaxKind::SUPERTYPE
                | SyntaxKind::DOT_EQ_EQ
                | SyntaxKind::DOT_NOT_EQ
                | SyntaxKind::DOT_EQ_EQ_EQ
                | SyntaxKind::DOT_NOT_EQ_EQ
                | SyntaxKind::DOT_LT
                | SyntaxKind::DOT_LE
                | SyntaxKind::DOT_GT
                | SyntaxKind::DOT_GE
                | SyntaxKind::DOT_SUBTYPE
                | SyntaxKind::DOT_SUPERTYPE
                | SyntaxKind::AND_AND
                | SyntaxKind::OR_OR
        )
    })
}
