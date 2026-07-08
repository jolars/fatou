//! `unused-binding`: a local variable that is assigned but never read.
//!
//! Restricted to genuine locals ([`BindingKind::Local`] and
//! [`BindingKind::LetVar`]). Parameters, loop and comprehension variables,
//! `catch` variables, struct fields, type parameters, and every top-level
//! definition (functions, types, modules, globals, consts, imports) are exempt:
//! those are meaningful even when unread — API surface, structural names, or the
//! job of a different rule (`unused-import`). Names beginning with `_` follow
//! Julia's throwaway convention and are skipped.

use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::BindingKind;

pub struct UnusedBinding;

impl Rule for UnusedBinding {
    fn id(&self) -> &'static str {
        "unused-binding"
    }

    fn description(&self) -> &'static str {
        "Flag a local variable that is assigned but never read in the same \
         scope. Parameters, loop and `catch` variables, struct fields, type \
         parameters, and top-level definitions are exempt, since those are \
         meaningful even when unread. Names beginning with `_` are skipped, \
         following Julia's throwaway convention."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`tmp` is assigned inside `f` but never used:",
            source: "function f(x)\n    tmp = x + 1\n    return x\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for binding in ctx.model.bindings() {
            if binding.read {
                continue;
            }
            if !matches!(binding.kind, BindingKind::Local | BindingKind::LetVar) {
                continue;
            }
            if binding.name.starts_with('_') {
                continue;
            }
            sink.push(Diagnostic {
                path: None,
                start: binding.def_range.start().into(),
                end: binding.def_range.end().into(),
                rule: self.id().to_string(),
                severity: Severity::Warning,
                message: format!(
                    "local variable `{}` is assigned but never used",
                    binding.name
                ),
                fixes: Vec::new(),
                suppressed: false,
            });
        }
    }
}
