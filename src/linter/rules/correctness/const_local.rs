//! `const-local`: a `const` declaration on a local binding. The construct
//! parses clean but always fails at lowering with "syntax: unsupported `const`
//! declaration on local variable", so it is a guaranteed failure rather than a
//! style call. `const` is only meaningful in a *global* scope — the file top
//! level and each `module`/`baremodule` body.
//!
//! The check is an ancestor walk from the `CONST_STMT` (the same shape
//! `break-outside-loop` uses), classifying the innermost enclosing construct.
//! Boundary semantics were verified against Julia 1.12:
//!
//! - **Local** (a finding): `function`/`macro` definitions and their short
//!   form (`f(x) = ...`, whose right-hand side is a body just as much as a
//!   block is), `->` lambdas, do-block bodies, `let` bodies, `for`/`while`
//!   bodies, every part of a `try`, and comprehension/generator bodies. A
//!   function's default argument counts too: it is evaluated inside the
//!   function's own scope.
//! - **Global** (legal, stop): a `module`/`baremodule` body, or reaching the
//!   file root. `begin`/`if` open no scope, so a `const` inside one at top
//!   level is fine.
//! - **A `struct` body** (legal, stop): `const` there is a *field attribute*,
//!   legal on a mutable struct since Julia 1.8. (On an immutable struct it is
//!   a different error, about field attributes, which is not this rule's.) An
//!   inner constructor inside the body is still a function, and the walk meets
//!   it first.
//! - **Quoted code and macro calls** (silent, and this wins even past a local
//!   boundary, so the walk continues rather than reporting on the spot):
//!   quoted code is data and is never lowered where it is written, and a macro
//!   may rewrite what it is handed. The same exemptions `break-outside-loop`
//!   and `undefined-name` make.
//!
//! Four positions inside a scope-opening construct are deliberately *not*
//! findings, because they evaluate in the enclosing scope: a `for`
//! comprehension's iterator spec, a `while` condition, a `do`-call's call
//! part, and a `let`'s binding list. The last is conservative rather than
//! exact — a `let`'s *first* binding is enclosing, but the later ones see the
//! earlier ones and so are local. Missing those is preferable to guessing.
//!
//! A `const` carrying a `global` or `local` modifier is exempt: `global const`
//! is legal in a soft local scope and inside a function raises a *different*
//! error ("`global const` declaration not allowed inside function"), and
//! `local const` is rejected everywhere, top level included, with another
//! message again. Neither is the error this rule names.
//!
//! No fix is offered: dropping the `const` changes the declaration's meaning,
//! and hoisting it out of the scope changes where the name lives.

use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::correctness::const_decl;
use crate::linter::rules::{Example, Rule, RuleContext, matchers};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct ConstLocal;

/// Whether `node` opens a local scope for a `const` reached from `from`, the
/// child subtree the ancestor walk arrived through.
fn opens_local_scope(node: &SyntaxNode, from: &SyntaxNode) -> bool {
    match node.kind() {
        // Both the signature and the body are the function's own scope: a
        // default argument is evaluated there too.
        SyntaxKind::FUNCTION_DEF | SyntaxKind::MACRO_DEF | SyntaxKind::ARROW_EXPR => true,
        // Only the do *body* is the closure; the call part evaluates in the
        // enclosing scope.
        SyntaxKind::DO_EXPR => from.kind() == SyntaxKind::BLOCK,
        // Only the body; the binding list is left alone (see the module docs).
        SyntaxKind::LET_EXPR => from.kind() == SyntaxKind::BLOCK,
        // Only the body; the iterator spec and the condition evaluate in the
        // enclosing scope.
        SyntaxKind::FOR_EXPR | SyntaxKind::WHILE_EXPR => from.kind() == SyntaxKind::BLOCK,
        // Every part of a `try` is local: the body, `catch`, `else`, `finally`.
        SyntaxKind::TRY_EXPR => true,
        // Only the body; the iterator spec evaluates in the enclosing scope.
        SyntaxKind::COMPREHENSION
        | SyntaxKind::BRACES_COMPREHENSION
        | SyntaxKind::TYPED_COMPREHENSION
        | SyntaxKind::GENERATOR => from.kind() != SyntaxKind::FOR_BINDING,
        // A short-form definition's right-hand side is a function body.
        SyntaxKind::ASSIGNMENT_EXPR => matchers::is_short_form_def(node),
        _ => false,
    }
}

impl Rule for ConstLocal {
    fn id(&self) -> &'static str {
        "const-local"
    }

    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag a `const` declaration inside a local scope — a function or macro \
         body, a `let`, a `for`/`while` body, a `try`, a closure, or a \
         comprehension. `const` is only meaningful at global scope (the file \
         top level and each `module` body); anywhere else the code parses but \
         always fails at lowering with \"unsupported `const` declaration on \
         local variable\". A `const` field of a mutable struct is a different \
         construct and is left alone, as is a `const` inside quoted code or a \
         macro argument, which may never be lowered as written."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "`const` inside a function body:",
                source: "function scale(x)\n    const factor = 2\n    factor * x\nend\n",
            },
            Example {
                caption: "A `let` body is local too — the declaration belongs at top level:",
                source: "let\n    const limit = 10\n    limit\nend\n",
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
        if const_decl::scope_modifier(node).is_some() {
            return;
        }

        // Walk to the root rather than stopping at the first verdict: a quote
        // or macro call further out silences the finding even when a local
        // scope sits in between (`:(function f() const z = 1 end)`).
        let mut from = node.clone();
        let mut local = None;
        for ancestor in node.ancestors().skip(1) {
            if const_decl::is_unlowered_context(&ancestor) {
                return;
            }
            // Only the innermost verdict counts; the walk continues purely to
            // find an enclosing quote or macro call.
            if local.is_none() {
                if opens_local_scope(&ancestor, &from) {
                    local = Some(true);
                } else if matches!(
                    ancestor.kind(),
                    // A global scope, or a `struct` body where `const` is a
                    // field attribute rather than a declaration.
                    SyntaxKind::MODULE_DEF | SyntaxKind::STRUCT_DEF
                ) {
                    local = Some(false);
                }
            }
            from = ancestor;
        }

        if local == Some(true) {
            sink.push(Diagnostic::new(
                self.id(),
                node.text_range(),
                "`const` declaration on a local variable",
            ));
        }
    }
}
