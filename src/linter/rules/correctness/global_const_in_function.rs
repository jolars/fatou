//! `global-const-in-function`: a `global const` declaration inside a function.
//! The construct parses clean but always fails at lowering with "syntax:
//! `global const` declaration not allowed inside function", so it is a
//! guaranteed failure rather than a style call. Julia does allow `global const`
//! in a *soft* local scope — a `let`, a `for`/`while` body, a `try`, a bare
//! `begin`/`if` — which is exactly why `const-local` leaves the modified form
//! alone and this rule takes it instead.
//!
//! The check is the ancestor walk `const-local` uses, but classifying the
//! innermost enclosing *function* rather than the innermost enclosing local
//! scope. Boundary semantics were verified against Julia 1.12:
//!
//! - **A function** (a finding): `function`/`macro` definitions and their short
//!   form (`f(x) = ...`), `->` lambdas, do-block bodies, and
//!   comprehension/generator bodies, which lower to closures. A function's
//!   default argument counts too: it is evaluated inside the function's own
//!   scope. Soft scopes nested inside a function do not rescue the declaration
//!   — `function f(); let; global const x = 1; end; end` still fails.
//! - **Not a function** (legal, no finding): a `let`, `for`, `while`, `try`,
//!   `begin`, or `if`; a `module`/`baremodule` body; a `struct` body; and the
//!   file root.
//! - **Quoted code and macro calls** (silent, and this wins even past a
//!   function boundary, so the walk continues rather than reporting on the
//!   spot): quoted code is data and is never lowered where it is written, and a
//!   macro may rewrite what it is handed. The same exemptions `const-local` and
//!   `break-outside-loop` make.
//!
//! The positions inside a closure-opening construct that evaluate in the
//! *enclosing* scope are not findings, matching `const-local`: a comprehension
//! or generator's iterator spec, and a `do`-call's call part. (A comprehension's
//! `if` filter is inside the closure, and is a finding.)
//!
//! Both modifier orders are flagged — `global const x = 1` and `const global
//! x = 1` mean the same thing — and the span covers the modifier keyword too,
//! since the pair is what Julia rejects.
//!
//! No fix is offered: dropping `global` changes the declaration's meaning (and
//! yields `const-local`'s error), and hoisting it out of the function changes
//! where the value is computed.

use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::correctness::const_decl;
use crate::linter::rules::{Example, Rule, RuleContext, matchers};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct GlobalConstInFunction;

/// Whether `node` is a function for a declaration reached from `from`, the child
/// subtree the ancestor walk arrived through.
fn opens_function(node: &SyntaxNode, from: &SyntaxNode) -> bool {
    match node.kind() {
        // Both the signature and the body are the function's own scope: a
        // default argument is evaluated there too.
        SyntaxKind::FUNCTION_DEF | SyntaxKind::MACRO_DEF | SyntaxKind::ARROW_EXPR => true,
        // Only the do *body* is the closure; the call part evaluates in the
        // enclosing scope.
        SyntaxKind::DO_EXPR => from.kind() == SyntaxKind::BLOCK,
        // A comprehension lowers to a closure, but its iterator spec evaluates
        // in the enclosing scope.
        SyntaxKind::COMPREHENSION
        | SyntaxKind::BRACES_COMPREHENSION
        | SyntaxKind::TYPED_COMPREHENSION
        | SyntaxKind::GENERATOR => from.kind() != SyntaxKind::FOR_BINDING,
        // A short-form definition's right-hand side is a function body.
        SyntaxKind::ASSIGNMENT_EXPR => matchers::is_short_form_def(node),
        _ => false,
    }
}

impl Rule for GlobalConstInFunction {
    fn id(&self) -> &'static str {
        "global-const-in-function"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag a `global const` declaration inside a function — a `function` or \
         `macro` body, a short-form definition, a closure, a do block, or a \
         comprehension. Julia allows `global const` at global scope and in a \
         soft local scope such as a `let`, a loop body, or a `try`, but inside \
         a function the code parses and then always fails at lowering with \
         \"`global const` declaration not allowed inside function\". A soft \
         scope nested inside a function does not help. Both spellings of the \
         modifier are flagged, and a declaration inside quoted code or a macro \
         argument is left alone, since it may never be lowered as written. An \
         unmodified `const` in a local scope is `const-local`'s finding."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "`global const` inside a function body:",
                source: "function setup()\n    global const LIMIT = 10\nend\n",
            },
            Example {
                caption: "A nested soft scope does not rescue it — the enclosing function still \
                          owns the declaration:",
                source: "function setup()\n    for i in 1:3\n        global const LIMIT = i\n    \
                         end\nend\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CONST_STMT]
    }

    fn check(&self, el: &SyntaxElement, _ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(node) = el.as_node() else {
            return;
        };
        let Some(modifier) = const_decl::scope_modifier(node) else {
            return;
        };
        if modifier.kind != SyntaxKind::GLOBAL_STMT {
            return;
        }

        // Walk to the root rather than stopping at the first function: a quote
        // or macro call further out silences the finding even when a function
        // sits in between (`:(function f() global const x = 1 end)`).
        let mut from = modifier.outer.clone();
        let mut in_function = false;
        for ancestor in modifier.outer.ancestors().skip(1) {
            if const_decl::is_unlowered_context(&ancestor) {
                return;
            }
            // Only the innermost verdict counts; the walk continues purely to
            // find an enclosing quote or macro call.
            in_function = in_function || opens_function(&ancestor, &from);
            from = ancestor;
        }

        if in_function {
            sink.push(Diagnostic::new(
                self.id(),
                modifier.outer.text_range(),
                "`global const` declaration inside a function",
            ));
        }
    }
}
