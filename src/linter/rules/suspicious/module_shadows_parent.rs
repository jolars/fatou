//! `module-shadows-parent`: a nested `module` named the same as its direct
//! parent module. Legal Julia, but every module binds its own name inside
//! itself, so the child rebinds that self-reference: after `module A` inside
//! `module A`, the name `A` in the parent body refers to the *child*, and
//! qualified names like `A.x` silently resolve against the wrong module. The
//! usual cause is a file `include`d into the module it already defines.
//!
//! Only the direct parent counts (`SemanticModel::enclosing_module_path`'s
//! last component): `module A ... module B ... module A` is unusual but
//! unambiguous. Both `module` and `baremodule` produce the same shape and are
//! both checked. Quoted code and macro-call arguments (`@eval module A`) stay
//! silent — quoted code is data, and a macro may rewrite its argument into
//! anything (mirrors the `break-outside-loop` exemptions).
//!
//! No fix: renaming a module means updating every reference to it, a semantic
//! rewrite rather than a tight lossless edit.

use crate::ast::{AstNode, AstToken, ModuleDef};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct ModuleShadowsParent;

impl Rule for ModuleShadowsParent {
    fn id(&self) -> &'static str {
        "module-shadows-parent"
    }

    fn description(&self) -> &'static str {
        "Flag a nested `module` with the same name as its direct parent module. \
         A module binds its own name inside itself, so the child rebinds that \
         self-reference: `A` in the parent body then refers to the child, and \
         qualified names like `A.x` resolve against the wrong module. The usual \
         cause is a file `include`d into the module it already defines. No fix: \
         renaming a module means updating every reference to it."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "A submodule shadowing the module that contains it:",
            source: "module A\n\nmodule A\nend\n\nend\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::MODULE_DEF]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(module) = el.as_node().cloned().and_then(ModuleDef::cast) else {
            return;
        };
        let Some(ident) = module.name().and_then(|name| name.ident()) else {
            return;
        };

        // Quoted code is data, and a macro may rewrite its argument into
        // anything (`@eval module A` under `module A` is the standard way to
        // *build* such a module deliberately).
        let in_quote_or_macro = module.syntax().ancestors().skip(1).any(|a| {
            matches!(
                a.kind(),
                SyntaxKind::QUOTE_EXPR | SyntaxKind::QUOTE_SYM | SyntaxKind::MACRO_CALL
            )
        });
        if in_quote_or_macro {
            return;
        }

        // The module keyword sits *outside* the module's own scope (which
        // starts at the body block), so the path here is the enclosing chain;
        // its last component is the direct parent module.
        let scope = ctx.model.scope_at(module.syntax().text_range().start());
        let path = ctx.model.enclosing_module_path(scope);
        if path.last().map(|parent| parent.as_str()) != Some(ident.text()) {
            return;
        }

        sink.push(Diagnostic::new(
            self.id(),
            ident.syntax().text_range(),
            format!(
                "module `{}` has the same name as its parent module",
                ident.text()
            ),
        ));
    }
}
