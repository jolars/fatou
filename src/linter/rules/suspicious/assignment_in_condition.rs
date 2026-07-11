//! `assignment-in-condition`: a bare `=` used as the test of an `if`/`elseif`/
//! `while`. Legal Julia (the condition takes the assigned value), but almost
//! always a `==` typo, so it is flagged with a safe `=` -> `==` fix.
//!
//! Only the bare `=` form is flagged: `==`, `===`, and the comparison operators
//! parse as their own nodes, never an `ASSIGNMENT_EXPR`, so there is no risk of
//! a false positive on a genuine comparison. A parenthesized condition
//! (`if (x = 1)`) is unwrapped first.

use crate::ast::{AstNode, AstToken, Condition, Expr};
use crate::linter::diagnostic::{Applicability, Diagnostic, Fix};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct AssignmentInCondition;

impl Rule for AssignmentInCondition {
    fn id(&self) -> &'static str {
        "assignment-in-condition"
    }

    fn description(&self) -> &'static str {
        "Flag a bare `=` assignment used as the test of an `if`/`elseif`/`while`. \
         It is valid Julia but almost always a typo for `==`, so it is reported \
         with a safe fix that rewrites `=` to `==`."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`=` where `==` was meant:",
            source: "if x = 5\n    println(x)\nend\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CONDITION]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        // The test is a bare-`=` assignment: `Condition::expr` unwraps a single
        // parenthesized layer (`if (x = 1)`); `==`/`===`/comparisons parse as
        // their own nodes, and augmented forms (`+=`) carry a non-`EQ` operator.
        let Some(condition) = el.as_node().cloned().and_then(Condition::cast) else {
            return;
        };
        let Some(Expr::AssignmentExpr(assign)) = condition.expr() else {
            return;
        };
        let Some(op) = assign
            .op()
            .filter(|op| op.syntax().kind() == SyntaxKind::EQ)
        else {
            return;
        };

        let range = op.syntax().text_range();
        let assign_range = assign.syntax().text_range();
        let mut diag = Diagnostic::new(
            self.id(),
            assign_range.start().into(),
            assign_range.end().into(),
            "assignment used as a condition; did you mean `==`?".to_string(),
        );
        diag.fixes.push(Fix {
            description: "Replace `=` with `==`".to_string(),
            content: "==".to_string(),
            start: range.start().into(),
            end: range.end().into(),
            applicability: Applicability::Safe,
        });
        sink.push(diag);
    }
}
