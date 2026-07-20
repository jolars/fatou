//! `unused-type-parameter`: a `where` type parameter never used in the
//! signature or body.
//!
//! Driven by the semantic model, which binds every `where` clause parameter
//! (bare `where T`, braced `where {T, S}`, bounded `where {T<:Number}`, and
//! chained `where T where S`) into the method's function scope with read
//! tracking. A parameter no annotation, bound, return type, or body
//! expression ever reads is dead — usually a leftover from a refactor or a
//! sign that a `::T` annotation was forgotten — and can be deleted from the
//! `where` clause.
//!
//! Struct type parameters (`struct Unit{T} end`) bind in the struct scope and
//! are deliberately out of scope: phantom type parameters are idiomatic Julia.
//! All-underscore names (`_`, `__`, ...) follow the throwaway convention and
//! are skipped, mirroring `unused-argument`. No fix is shipped: deleting a
//! parameter means restructuring the `where` clause itself (dropping a sole
//! `where T` layer, or splicing one name out of a braced group), which is not
//! a tight textual edit by construction.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::{BindingKind, ScopeKind};

pub struct UnusedTypeParameter;

impl Rule for UnusedTypeParameter {
    fn id(&self) -> &'static str {
        "unused-type-parameter"
    }

    fn description(&self) -> &'static str {
        "Flag a `where` clause type parameter that is never used in the \
         signature or body, covering bare (`where T`), braced \
         (`where {T, S}`), bounded (`where {T<:Number}`), and chained \
         (`where T where S`) clauses. An unread parameter is usually a \
         refactoring leftover or a forgotten `::T` annotation. Struct type \
         parameters are exempt (phantom parameters like `struct Unit{T} end` \
         are idiomatic), as are all-underscore names."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`T` is bound but never referenced:",
            source: "function f(x) where {T}\n    x + 1\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for binding in ctx.model.bindings() {
            if binding.read || binding.kind != BindingKind::TypeParam {
                continue;
            }
            // `where` parameters bind in the method's function scope; struct
            // `{T}` parameters bind in the struct scope and stay exempt.
            if ctx.model.scope(binding.scope).kind != ScopeKind::Function {
                continue;
            }
            if binding.name.chars().all(|c| c == '_') {
                continue;
            }
            sink.push(Diagnostic::new(
                self.id(),
                binding.def_range,
                format!("type parameter `{}` is never used", binding.name),
            ));
        }
    }
}
