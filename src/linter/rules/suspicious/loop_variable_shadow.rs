//! `loop-variable-shadow`: a `for` loop whose index variable is already an
//! enclosing `for` loop's index.
//!
//! A nested `for i in ...` inside `for i in ...` binds a *fresh* variable, so
//! the outer index is unreachable for the rest of the inner loop — code below the inner
//! loop still sees the outer one, which is exactly the confusion the shape
//! invites. The usual cause is a copy-pasted inner loop whose index was never
//! renamed.
//!
//! This is a pure scope question the semantic model already answers,
//! so this is a whole-file pass over the bindings rather than a node-shape
//! rule: every [`BindingKind::ForVar`] in a [`ScopeKind::For`] scope is asked
//! whether an enclosing loop scope binds the same name.
//!
//! Scope of the shadow check:
//!
//! - Both loops must be statement `for`s. A comprehension or generator clause
//!   ([`ScopeKind::Comprehension`]) is left alone in both positions: its
//!   variable is scoped to the comprehension, and reusing a short name there
//!   (`[i for i in 1:2]` inside a `for i`) is idiomatic rather than a mistake.
//! - The walk up the scope chain stops at a function body and at a global
//!   scope. A nested definition, a closure, and a `do` block are separate
//!   units of code whose textual nesting inside the loop is incidental;
//!   reusing `i` there is not the copy-paste bug. Every other intervening
//!   scope (`while`, `try`/`catch`/`finally`, `let`, another `for`) is
//!   straight-line control flow inside the outer loop, so the walk passes
//!   through it.
//! - Only an enclosing *loop* variable counts. Shadowing a plain local or a
//!   parameter is what `for` always does, and flagging it would fire on
//!   ordinary code.
//! - The clause list of a single `for` chains loop scopes, so `for i in a, i
//!   in b` is reported by the same walk — the second clause shadows the first.
//!
//! Assignments to a loop variable are allowed: normalization and other updates
//! can intentionally change the value used later in the same iteration.
//! Whether an assigned value is discarded unread requires liveness analysis,
//! not a scope-only check.
//!
//! No fix: renaming the inner index means rewriting every reference to it in
//! the loop, and the intended binding is unknown.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::{Binding, BindingId, BindingKind, ScopeId, ScopeKind, SemanticModel};

pub struct LoopVariableShadow;

impl Rule for LoopVariableShadow {
    fn id(&self) -> &'static str {
        "loop-variable-shadow"
    }

    fn description(&self) -> &'static str {
        "Report an inner `for` loop that reuses an enclosing loop's index name. The inner loop \
         creates a new binding and hides the outer index, often because a copied loop was not \
         renamed.\n\n\
         Assignments to loop variables are allowed. Comprehensions, generators, and reuse across \
         function boundaries are not checked."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A nested loop reusing the enclosing loop's index:",
            source: "for i in 1:3\n    for i in 1:2\n        println(i)\n    end\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let model = ctx.model;
        for binding in model.bindings() {
            if !is_loop_variable(model, binding) {
                continue;
            }

            if let Some(outer) = enclosing_loop_variable(model, binding) {
                sink.push(Diagnostic::new(
                    self.id(),
                    binding.def_range,
                    format!(
                        "loop variable `{}` shadows the enclosing loop's variable",
                        model.binding(outer).name
                    ),
                ));
            }
        }
    }
}

/// Whether `binding` is a statement `for`'s index variable. A comprehension or
/// generator clause binds a [`BindingKind::ForVar`] too, but into a
/// [`ScopeKind::Comprehension`] scope, which this rule leaves alone.
fn is_loop_variable(model: &SemanticModel, binding: &Binding) -> bool {
    binding.kind == BindingKind::ForVar && model.scope(binding.scope).kind == ScopeKind::For
}

/// The same-named index of an enclosing `for` loop, if the scope chain reaches
/// one before leaving the loop's own unit of code (see the module docs).
fn enclosing_loop_variable(model: &SemanticModel, binding: &Binding) -> Option<BindingId> {
    let mut cursor = model.scope(binding.scope).parent;
    while let Some(id) = cursor {
        let scope = model.scope(id);
        if scope.kind.is_global() || scope.kind == ScopeKind::Function {
            return None;
        }
        if scope.kind == ScopeKind::For
            && let Some(outer) = same_named_loop_variable(model, id, binding)
        {
            return Some(outer);
        }
        cursor = scope.parent;
    }
    None
}

/// The index variable `scope` binds under `binding`'s name, if any.
fn same_named_loop_variable(
    model: &SemanticModel,
    scope: ScopeId,
    binding: &Binding,
) -> Option<BindingId> {
    model
        .scope(scope)
        .bindings
        .iter()
        .copied()
        .find(|&candidate| {
            let outer = model.binding(candidate);
            outer.kind == BindingKind::ForVar && outer.name == binding.name
        })
}
