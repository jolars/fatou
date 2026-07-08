//! `assignment-in-condition`: a bare `=` used as the test of an `if`/`elseif`/
//! `while`. Legal Julia (the condition takes the assigned value), but almost
//! always a `==` typo, so it is flagged with a safe `=` -> `==` fix.
//!
//! Only the bare `=` form is flagged: `==`, `===`, and the comparison operators
//! parse as their own nodes, never an `ASSIGNMENT_EXPR`, so there is no risk of
//! a false positive on a genuine comparison. A parenthesized condition
//! (`if (x = 1)`) is unwrapped first.

use crate::linter::diagnostic::{Applicability, Diagnostic, Fix, Severity};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

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
        let Some(condition) = el.as_node() else {
            return;
        };
        let Some(assign) = condition_assignment(condition) else {
            return;
        };
        let Some(eq) = assign
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| t.kind() == SyntaxKind::EQ)
        else {
            return;
        };

        let range = eq.text_range();
        sink.push(Diagnostic {
            path: None,
            start: assign.text_range().start().into(),
            end: assign.text_range().end().into(),
            rule: self.id().to_string(),
            severity: Severity::Warning,
            message: "assignment used as a condition; did you mean `==`?".to_string(),
            fixes: vec![Fix {
                description: "Replace `=` with `==`".to_string(),
                content: "==".to_string(),
                start: range.start().into(),
                end: range.end().into(),
                applicability: Applicability::Safe,
            }],
            suppressed: false,
        });
    }
}

/// The bare-`=` `ASSIGNMENT_EXPR` directly forming `condition`, unwrapping a
/// single parenthesized layer (`if (x = 1)`). `None` for any other condition.
fn condition_assignment(condition: &SyntaxNode) -> Option<SyntaxNode> {
    let mut node = condition.children().next()?;
    if node.kind() == SyntaxKind::PAREN_EXPR {
        node = node.children().next()?;
    }
    (node.kind() == SyntaxKind::ASSIGNMENT_EXPR).then_some(node)
}
