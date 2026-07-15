//! `duplicate-argument`: the same parameter name declared twice in one
//! signature.
//!
//! Julia rejects this at definition time (`function argument name not unique`),
//! so it is always a bug. Driven by the semantic model, which records one
//! parameter binding per occurrence in the function's scope, so every signature
//! form — long, short (`f(x, x) = ...`), anonymous, and `do` — is covered
//! uniformly. Positional and keyword parameters share the one namespace
//! (`f(x; x)` is also a duplicate).

use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::BindingKind;

pub struct DuplicateArgument;

impl Rule for DuplicateArgument {
    fn id(&self) -> &'static str {
        "duplicate-argument"
    }

    /// Julia rejects the definition outright, so this is always an error.
    fn default_severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &'static str {
        "Flag the same parameter name declared more than once in a single \
         signature. Julia rejects such a definition outright, so it is always a \
         mistake. Positional and keyword parameters share one namespace."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`x` appears twice in the parameter list:",
            source: "function dist(x, y, x)\n    hypot(x, y)\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for scope in ctx.model.scopes() {
            let mut seen: Vec<&str> = Vec::new();
            for &id in &scope.bindings {
                let binding = ctx.model.binding(id);
                if !matches!(binding.kind, BindingKind::Param | BindingKind::KeywordParam) {
                    continue;
                }
                if seen.contains(&binding.name.as_str()) {
                    sink.push(Diagnostic::new(
                        self.id(),
                        binding.def_range,
                        format!("argument name `{}` is used more than once", binding.name),
                    ));
                } else {
                    seen.push(binding.name.as_str());
                }
            }
        }
    }
}
