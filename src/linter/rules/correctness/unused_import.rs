//! `unused-import`: an explicitly imported name that is never used in the file.
//!
//! Covers the forms that bind a *specific* name: `import X`, `import X as Y`,
//! `using X: a`, and `import X: a` (each item, including its `as` alias). The
//! whole-module `using X` form is deliberately exempt: it attaches `X`'s
//! exports, which appear as bare free reads this file can't yet resolve (the
//! package index is a later phase), so `X` looking "unread" says nothing about
//! whether the import is used.
//!
//! A qualified use (`X.f()`) marks the module binding read, and a re-`export`
//! marks the imported name read, so neither is a false positive.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::{BindingKind, LoadKind};

pub struct UnusedImport;

impl Rule for UnusedImport {
    fn id(&self) -> &'static str {
        "unused-import"
    }

    fn description(&self) -> &'static str {
        "Flag an explicitly imported name that is never used: `import X`, \
         `import X as Y`, and the colon-item forms `using X: a` / `import X: a`. \
         The whole-module `using X` form is exempt, since it attaches exports \
         that resolve elsewhere. A qualified use (`X.f`) or a re-`export` counts \
         as a use."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`sortperm` is imported but never referenced:",
            source: "using Base: sortperm, sum\n\nprintln(sum([1, 2, 3]))\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for binding in ctx.model.bindings() {
            if binding.read || binding.kind != BindingKind::Import {
                continue;
            }
            if is_whole_module_using(ctx, binding.def_range) {
                continue;
            }
            sink.push(Diagnostic::new(
                self.id(),
                binding.def_range.start().into(),
                binding.def_range.end().into(),
                format!("`{}` is imported but never used", binding.name),
            ));
        }
    }
}

/// Whether `def` falls inside a whole-module `using X` clause (`using`, no
/// colon-item list) — the one import form whose bound name says nothing about
/// whether the import is actually used.
fn is_whole_module_using(ctx: &RuleContext<'_>, def: rowan::TextRange) -> bool {
    ctx.model.module_loads().iter().any(|load| {
        load.kind == LoadKind::Using && load.items.is_none() && load.range.contains_range(def)
    })
}
