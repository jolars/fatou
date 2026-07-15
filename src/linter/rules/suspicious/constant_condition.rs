//! `constant-condition`: a boolean literal as an `if`/`elseif`/`while` test or
//! as an operand of the short-circuit `&&`/`||`. The branch (or the
//! short-circuit) is decided before the code runs, so the literal is usually
//! leftover debug scaffolding or a mistyped name. One rule where StaticLint
//! has three codes (ConstIfCondition, PointlessAND, PointlessOR).
//!
//! `while true` is exempt: Julia has no dedicated infinite-loop construct, so
//! `while true` + `break` is the idiom. `while false` (dead code) is still
//! flagged. Out of scope: the eager bitwise `&`/`|` and the broadcast
//! `.&&`/`.||` (their operands are values, not tests), a ternary test (no
//! `CONDITION` node), and a parenthesized literal operand (`x && (true)`) —
//! the condition side unwraps one paren layer via [`Condition::expr`], the
//! operand side only matches the bare literal. `false && expr` is
//! occasionally used to disable code deliberately; suppress those sites with
//! `# fatou-ignore constant-condition`.
//!
//! No fix: simplifying a constant condition means deleting a branch or an
//! operand — a structural rewrite, not a tight lossless edit.

use crate::ast::{AstNode, AstToken, BinaryExpr, Condition, Expr};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct ConstantCondition;

impl Rule for ConstantCondition {
    fn id(&self) -> &'static str {
        "constant-condition"
    }

    fn description(&self) -> &'static str {
        "Flag a `true`/`false` literal used as an `if`/`elseif`/`while` test or \
         as an operand of the short-circuit `&&`/`||`. The branch or \
         short-circuit is decided before the code runs, so the literal is \
         usually leftover debugging or a mistyped name. `while true` is exempt \
         as Julia's idiomatic infinite loop. No fix: removing the constant \
         means restructuring the branch."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "A literal `if` test always takes the branch:",
                source: "if true\n    println(\"always\")\nend\n",
            },
            Example {
                caption: "A literal operand decides `&&` at parse time:",
                source: "ok = false && check(x)\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CONDITION, SyntaxKind::BINARY_EXPR]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        if let Some(cond) = Condition::cast(node.clone()) {
            // The whole test is a bare boolean literal (one paren layer is
            // unwrapped). A literal buried in a larger test expression is the
            // `&&`/`||` arm's business.
            let Some(Expr::Literal(lit)) = cond.expr() else {
                return;
            };
            let Some(tok) = lit.bool_token() else { return };
            if tok.kind() == SyntaxKind::TRUE_KW
                && cond
                    .syntax()
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::WHILE_EXPR)
            {
                return;
            }
            sink.push(Diagnostic::new(
                self.id(),
                lit.syntax().text_range(),
                format!("this condition is always `{}`", tok.text()),
            ));
        } else if let Some(bin) = BinaryExpr::cast(node.clone()) {
            // Only the lazy forms: `&`/`|` are eager value operators, and the
            // broadcast `.&&`/`.||` map over collections.
            let Some(op) = bin
                .op()
                .filter(|op| matches!(op.syntax().kind(), SyntaxKind::AND_AND | SyntaxKind::OR_OR))
            else {
                return;
            };
            for operand in [bin.lhs(), bin.rhs()] {
                let Some(Expr::Literal(lit)) = operand else {
                    continue;
                };
                let Some(tok) = lit.bool_token() else {
                    continue;
                };
                sink.push(Diagnostic::new(
                    self.id(),
                    lit.syntax().text_range(),
                    format!("`{}` has a constant `{}` operand", op.text(), tok.text()),
                ));
            }
        }
    }
}
