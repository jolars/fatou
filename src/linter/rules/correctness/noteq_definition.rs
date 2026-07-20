//! `noteq-definition`: a method definition for `!=` (or its unicode alias
//! `≠`). In Julia, `!=` is not a generic function of its own — it is
//! `const != = !(==)` — so a new method should go on `==` instead; a `!=`
//! overload can leave the two operators inconsistent with each other.
//!
//! Flagged shapes, mirroring StaticLint.jl's `NotEqDef` check:
//!
//! - the long form: `function !=(a, b) ... end`, including the qualified
//!   `function Base.:(!=)(a, b)`;
//! - the short form: `!=(a, b) = ...`, `(!=)(a, b) = ...`, `Base.:!=(a, b) =
//!   ...`, through any `where` clauses and a return-type annotation;
//! - the infix short form: `a != b = ...` and `a ≠ b = ...` (legal Julia,
//!   equivalent to the prefix definition).
//!
//! Using `!=` is never flagged: a comparison, a bare call, or a `!=`
//! expression on the right-hand side of an assignment is a use, not a
//! definition. Definitions inside quotes and macro calls are still flagged —
//! `@eval !=(a, b) = ...` defines the method just the same. No fix is
//! offered: rewriting the definition onto `==` means negating the body, a
//! semantic rewrite.

use crate::ast::{AssignmentExpr, AstNode, AstToken, BinaryExpr, CallExpr, FunctionDef, Operator};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::signature::peel_signature;
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct NotEqDefinition;

/// Whether `op` spells `!=`, in either the ASCII or the unicode form.
fn is_noteq(op: &Operator) -> bool {
    match op.syntax().kind() {
        SyntaxKind::NOT_EQ => true,
        SyntaxKind::UNICODE_OP => op.text() == "\u{2260}",
        _ => false,
    }
}

/// The `!=` token naming the function a signature defines, if any: peels
/// `where` clauses and a return-type annotation, then reads the call core's
/// operator callee, or — for the infix short form `a != b = ...` — the
/// binary operator itself.
fn defined_noteq(signature: SyntaxNode) -> Option<Operator> {
    let (core, _, _) = peel_signature(signature);
    let op = match core? {
        core if core.kind() == SyntaxKind::CALL_EXPR => CallExpr::cast(core)?.callee_operator()?,
        core if core.kind() == SyntaxKind::BINARY_EXPR => BinaryExpr::cast(core)?.op()?,
        _ => return None,
    };
    is_noteq(&op).then_some(op)
}

impl Rule for NotEqDefinition {
    fn id(&self) -> &'static str {
        "noteq-definition"
    }

    fn description(&self) -> &'static str {
        "Flag a method definition for `!=` (or `\u{2260}`). Julia defines `!=` \
         as `const != = !(==)`, so it is not meant to be overloaded: define \
         `==` instead, and `!=` follows automatically."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "Defining `!=` where `==` should carry the method:",
            source: "!=(a::Grade, b::Grade) = a.score != b.score\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::FUNCTION_DEF, SyntaxKind::ASSIGNMENT_EXPR]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else { return };
        let signature = match node.kind() {
            SyntaxKind::FUNCTION_DEF => FunctionDef::cast(node.clone())
                .and_then(|def| def.signature())
                .and_then(|sig| sig.expr())
                .map(|expr| expr.syntax().clone()),
            // A short-form definition is a plain `=` whose left side is a
            // signature; `+=` and friends carry a different operator token.
            SyntaxKind::ASSIGNMENT_EXPR => {
                let assign = AssignmentExpr::cast(node.clone());
                assign
                    .as_ref()
                    .and_then(AssignmentExpr::op)
                    .filter(|op| op.syntax().kind() == SyntaxKind::EQ)
                    .and_then(|_| assign.as_ref().and_then(AssignmentExpr::lhs))
                    .map(|lhs| lhs.syntax().clone())
            }
            _ => return,
        };
        let Some(op) = signature.and_then(defined_noteq) else {
            return;
        };
        sink.push(Diagnostic::new(
            self.id(),
            op.syntax().text_range(),
            format!(
                "`{}` is defined as `!(==)` and should not be overloaded; define `==` instead",
                op.text()
            ),
        ));
    }
}
