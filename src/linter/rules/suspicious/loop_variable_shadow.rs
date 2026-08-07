//! `loop-variable-shadow`: a `for` loop whose index variable is already an
//! enclosing `for` loop's index, and an assignment to a loop variable inside
//! its own loop.
//!
//! Both shapes are legal Julia that reads as something it is not. A nested
//! `for i in ...` inside `for i in ...` binds a *fresh* variable, so the outer
//! index is unreachable for the rest of the inner loop — code below the inner
//! loop still sees the outer one, which is exactly the confusion the shape
//! invites. The usual cause is a copy-pasted inner loop whose index was never
//! renamed. Assigning to a loop variable is the mirror image: `for` rebinds
//! the variable from the iterator on every pass, so the assignment is
//! discarded at the next iteration and can never influence the iteration
//! itself.
//!
//! Both questions are pure scope questions the semantic model already answers,
//! so this is a whole-file pass over the bindings rather than a node-shape
//! rule: every [`BindingKind::ForVar`] in a [`ScopeKind::For`] scope is asked
//! whether an enclosing loop scope binds the same name, and whether any of its
//! occurrences writes it.
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
//! Scope of the assignment check: every write and read-write occurrence of a
//! loop variable, wherever it sits (a closure inside the body writes the
//! captured loop variable and is discarded just the same). Note that
//! `for x in xs; x = f(x); ...` — rebinding the element to a normalized form
//! for the rest of the iteration — is a working idiom that this check reports
//! too; it is reported because it is indistinguishable from `i += 1`, the
//! attempt to steer the iteration that the check exists for.
//!
//! No fix: renaming the inner index means rewriting every reference to it in
//! the loop, and dropping an assignment to a loop variable changes what the
//! body computes. Both are decisions, not tight lossless edits.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::{Access, Binding, BindingId, BindingKind, ScopeId, ScopeKind, SemanticModel};

pub struct LoopVariableShadow;

impl Rule for LoopVariableShadow {
    fn id(&self) -> &'static str {
        "loop-variable-shadow"
    }

    fn description(&self) -> &'static str {
        "Flag a `for` loop whose index variable is already an enclosing `for` \
         loop's index, and an assignment to a loop variable inside its own \
         loop. The nested `for` binds a fresh variable, so the outer index is \
         unreachable inside it — usually a copy-pasted inner loop whose index \
         was never renamed. An assignment to a loop variable is discarded at \
         the next iteration, since `for` rebinds the variable from the \
         iterator on every pass, so it can never steer the iteration. \
         Comprehension and generator clauses are left alone, as is reuse \
         across a function body, a closure, or a `do` block. No fix: renaming \
         an index or dropping an assignment changes what the body computes."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "A nested loop reusing the enclosing loop's index:",
                source: "for i in 1:3\n    for i in 1:2\n        println(i)\n    end\nend\n",
            },
            Example {
                caption: "An assignment the next iteration discards:",
                source: "for i in 1:10\n    if isodd(i)\n        i += 1\n    end\n    println(i)\nend\n",
            },
        ]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let model = ctx.model;
        for (index, binding) in model.bindings().iter().enumerate() {
            if !is_loop_variable(model, binding) {
                continue;
            }
            let id = BindingId(index as u32);

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

            for occurrence in model.occurrences(id) {
                if occurrence.is_def
                    || !matches!(occurrence.access, Access::Write | Access::ReadWrite)
                {
                    continue;
                }
                sink.push(Diagnostic::new(
                    self.id(),
                    occurrence.range,
                    format!(
                        "assignment to loop variable `{}` is discarded at the next iteration",
                        binding.name
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
