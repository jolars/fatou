//! `unused-binding`: a local variable that is assigned but never read.
//!
//! Restricted to genuine locals ([`BindingKind::Local`] and
//! [`BindingKind::LetVar`]). Parameters, loop and comprehension variables,
//! `catch` variables, struct fields, type parameters, and every top-level
//! definition (functions, types, modules, globals, consts, imports) are exempt:
//! those are meaningful even when unread — API surface, structural names, or the
//! job of a different rule (`unused-import`). Names beginning with `_` follow
//! Julia's throwaway convention and are skipped.

use rowan::TextRange;

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::BindingKind;
use crate::syntax::{SyntaxKind, SyntaxNode};

pub struct UnusedBinding;

/// Consuming attribute-DSL macros whose `begin ... end` block binds each
/// `name = default` as an attribute the macro reads, not a dead local. Matched
/// on the macro's final name component, so `Makie.@recipe` counts. Scope-
/// transparent macros (`@testset`, `@inbounds`, `@views`) are deliberately
/// excluded: they run their body as written, so a dead local there is a
/// genuine finding. Extend this list as new attribute DSLs surface (it is
/// name-based, so an aliased macro would evade it — an accepted limitation).
const ATTRIBUTE_DSL_MACROS: &[&str] = &["gen_defaults!", "DocumentedAttributes", "recipe"];

/// Whether `range` sits inside the argument block of a consuming attribute-DSL
/// macro call (see [`ATTRIBUTE_DSL_MACROS`]).
fn in_attribute_dsl_macro(root: &SyntaxNode, range: TextRange) -> bool {
    let mut node = match root.covering_element(range) {
        rowan::NodeOrToken::Token(t) => t.parent(),
        rowan::NodeOrToken::Node(n) => Some(n),
    };
    while let Some(current) = node {
        if current.kind() == SyntaxKind::MACRO_CALL
            && macro_call_name(&current)
                .is_some_and(|name| ATTRIBUTE_DSL_MACROS.contains(&name.as_str()))
        {
            return true;
        }
        node = current.parent();
    }
    false
}

/// Whether `range` is an assignment target passed *directly* as a macro-call
/// argument, as opposed to nested inside a block the macro runs as written. A
/// macro receives its argument as data and can reference every name in it
/// (`@show a, b = expr` prints and returns each name), so such a target is not a
/// dead local. The walk stops at the first enclosing [`SyntaxKind::BLOCK`]: a
/// name inside a macro's `begin ... end` body (or any function/`do`/`let` body
/// spliced in unevaluated) is reached only after crossing that block, so it
/// stays a genuine local and is still flagged (matching the scope-transparent
/// `@testset` case). Only a target reached before any block — a direct argument
/// expression — is exempt.
fn is_direct_macro_argument(root: &SyntaxNode, range: TextRange) -> bool {
    let mut node = match root.covering_element(range) {
        rowan::NodeOrToken::Token(t) => t.parent(),
        rowan::NodeOrToken::Node(n) => Some(n),
    };
    while let Some(current) = node {
        match current.kind() {
            SyntaxKind::BLOCK => return false,
            SyntaxKind::MACRO_CALL => return true,
            _ => node = current.parent(),
        }
    }
    false
}

/// The final name component of a macro call's `@name`: `gen_defaults!` for
/// `@gen_defaults!`, `recipe` for `Makie.@recipe`.
fn macro_call_name(call: &SyntaxNode) -> Option<String> {
    let name = call
        .children()
        .find(|c| c.kind() == SyntaxKind::MACRO_NAME)?;
    name.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT)
        .last()
        .map(|t| t.text().to_string())
}

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
            // A consuming attribute DSL reads its block's bindings as
            // attributes, so they are not dead locals.
            if in_attribute_dsl_macro(ctx.root, binding.def_range) {
                continue;
            }
            // An assignment target passed directly as a macro argument
            // (`@show a, b = expr`) is data the macro reads, not a dead local.
            if is_direct_macro_argument(ctx.root, binding.def_range) {
                continue;
            }
            sink.push(Diagnostic::new(
                self.id(),
                binding.def_range,
                format!(
                    "local variable `{}` is assigned but never used",
                    binding.name
                ),
            ));
        }
    }
}
