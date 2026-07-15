//! `break-outside-loop`: a `break` or `continue` with no enclosing `for` or
//! `while`. The construct parses clean but always errors at lowering ("break
//! or continue outside loop"), so this is a guaranteed runtime failure, not a
//! style call.
//!
//! The check is an ancestor walk from the `break`/`continue` node, with three
//! kinds of stops (boundary semantics verified against Julia 1.12 lowering):
//!
//! - **Loop** (`for`/`while`): legal, no finding. The whole loop node counts,
//!   including the iterator spec and the `while` condition — both sit inside
//!   the loop's break scope (`for i in (break; 1:3)` is legal Julia).
//! - **Function boundary** (`function`/`macro` definitions, `->` lambdas,
//!   do-block bodies, comprehension and generator bodies): `break` cannot
//!   reach a loop outside the closure, so it is an error even when a loop
//!   encloses the boundary — flag immediately. Two positions are exempt
//!   because they evaluate in the *enclosing* scope and the walk continues
//!   through them: a do-call's call part (`foreach((break; xs)) do x ... end`)
//!   and a comprehension's iterator spec (`[x for x in (break; xs)]`).
//! - **Quoted code and macro calls**: stop silently. Quoted code is data, and
//!   a macro may rewrite its arguments into anything (mirrors the
//!   `undefined-name` exemptions). A do-block attached to a macro call is
//!   likewise left alone.
//!
//! Reaching the file root without a stop is a finding. No fix is offered:
//! deleting the statement changes behavior, and there is no loop to attach it
//! to.

use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct BreakOutsideLoop;

/// Whether `node` is a function boundary that `break`/`continue` can never
/// escape, given `from`, the child subtree the ancestor walk arrived from.
fn is_function_boundary(node: &SyntaxNode, from: &SyntaxNode) -> bool {
    match node.kind() {
        SyntaxKind::FUNCTION_DEF | SyntaxKind::MACRO_DEF | SyntaxKind::ARROW_EXPR => true,
        // Only the do *body* is the closure; the call part evaluates in the
        // enclosing scope. A macro-call do stays silent like any macro call.
        SyntaxKind::DO_EXPR => {
            from.kind() == SyntaxKind::BLOCK
                && !node.children().any(|c| c.kind() == SyntaxKind::MACRO_CALL)
        }
        // Only the comprehension/generator *body* is the closure; the
        // iterator spec (`FOR_BINDING`) evaluates in the enclosing scope.
        SyntaxKind::COMPREHENSION
        | SyntaxKind::BRACES_COMPREHENSION
        | SyntaxKind::TYPED_COMPREHENSION
        | SyntaxKind::GENERATOR => from.kind() != SyntaxKind::FOR_BINDING,
        _ => false,
    }
}

impl Rule for BreakOutsideLoop {
    fn id(&self) -> &'static str {
        "break-outside-loop"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag a `break` or `continue` with no enclosing `for` or `while` loop. \
         The code parses but always fails at lowering with \"break or continue \
         outside loop\" — including inside a closure, do-block, or \
         comprehension body defined within a loop, since `break` cannot cross \
         a function boundary."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "`break` with no loop in sight:",
                source: "function process(x)\n    if x < 0\n        break\n    end\n    x\nend\n",
            },
            Example {
                caption: "A do-block body is an anonymous function, so the outer loop is out of reach:",
                source: "for i in 1:3\n    foreach(1:2) do x\n        continue\n    end\nend\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::BREAK_EXPR, SyntaxKind::CONTINUE_EXPR]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else {
            return;
        };

        let mut from = node.clone();
        for ancestor in node.ancestors().skip(1) {
            match ancestor.kind() {
                SyntaxKind::FOR_EXPR | SyntaxKind::WHILE_EXPR => return,
                SyntaxKind::QUOTE_EXPR | SyntaxKind::QUOTE_SYM | SyntaxKind::MACRO_CALL => {
                    return;
                }
                _ if is_function_boundary(&ancestor, &from) => break,
                _ => {}
            }
            from = ancestor;
        }

        let keyword = if node.kind() == SyntaxKind::BREAK_EXPR {
            "break"
        } else {
            "continue"
        };
        sink.push(Diagnostic::new(
            self.id(),
            node.text_range(),
            format!("`{keyword}` outside of a `for` or `while` loop"),
        ));
    }
}
