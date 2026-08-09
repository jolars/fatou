//! `duplicate-method`: two method definitions in one file whose dispatch
//! signatures are identical. Julia keeps one method per signature, so the later
//! definition silently replaces the earlier one and everything the earlier body
//! did is dead. Either the repeat is a copy-paste left behind by a rename, or
//! one of the two was meant to have a different signature.
//!
//! Identity is the *dispatch* signature, read off the same lowered
//! [`TypeExpr`](crate::index::typeexpr::TypeExpr) form the package index
//! harvests ([`harvest_tree`]), so the rule agrees with `call-arity` about what
//! a file defines. Two definitions collide when they share a module, a function
//! name (and its qualifying owner, so `Base.show` and a bare `show` are
//! separate), and a positional parameter list that lowers to the same sequence
//! of types and vararg flags, under the same `where` specs. What is
//! deliberately *not* part of that identity, because Julia does not dispatch on
//! it either:
//!
//! - **argument names** — `f(x::Int)` and `f(y::Int)` are one method;
//! - **keyword arguments** — they live on the lowered keyword sorter, not in
//!   the method table, so `f(x::Int; a = 1)` really is replaced by
//!   `f(x::Int; b = 2)`;
//! - **default values** — `f(x::Int = 0)` defines `f(::Int)` among others, so
//!   it collides with a plain `f(x::Int)`;
//! - **the declared return type** — `f(x::Int)::Int` and `f(x::Int)::Float64`
//!   are the same method with the second body winning.
//!
//! The type arguments a definition applies to its own name *are* part of it:
//! `MyStruct{Float64}()` is a method of `Type{MyStruct{Float64}}`, so it lives
//! beside `MyStruct()` rather than replacing it.
//!
//! `where` bounds, by contrast, *are* part of the identity: `f(x::T) where {T
//! <: Real}` and `f(x::T) where {T <: Integer}` are two methods. The comparison
//! is structural but not alpha-converting, so renaming a type variable
//! (`where T` versus `where S`) reads as a difference and is not flagged — a
//! miss, never a false positive.
//!
//! Being a correctness rule, everything unclear is withheld rather than
//! guessed:
//!
//! - only *module-level* definitions participate. The harvest walk never
//!   descends into a function body, so two closures named alike in two
//!   different functions are two locals, not a repeat;
//! - a definition inside a **macro call** is skipped. A macro receives the
//!   signature unevaluated and may rewrite it, so two identically written
//!   arguments are no evidence of two identical methods. This covers the
//!   `@static if` idiom from both sides — the branches of a conditional are not
//!   walked at all, so a plain `if VERSION ...` split is invisible too;
//! - a bodyless `function f end` declaration defines no method and is ignored.
//!
//! Only the second and later definitions are flagged; the first is the one
//! whose body is dead, but it is also the one the author most likely meant to
//! keep. No fix: which body survives, and whether the repeat was meant to carry
//! a different signature, is the author's call.

use rowan::TextRange;

use crate::index::harvest_tree;
use crate::index::model::{Method, ModuleIndex};
use crate::index::typeexpr::TypeExpr;
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::syntax::SyntaxKind;

pub struct DuplicateMethod;

/// The part of a harvested [`Method`] Julia dispatches on: the positional
/// parameters' lowered types (`None` for an unannotated argument, which is
/// `Any`) paired with their vararg flags, plus the `where` specs. Argument
/// names, defaults, keyword parameters, and the return type are all absent by
/// design — see the module docs.
#[derive(PartialEq, Eq)]
struct SignatureKey {
    type_args: Vec<TypeExpr>,
    params: Vec<(Option<TypeExpr>, bool)>,
    where_clauses: Vec<TypeExpr>,
}

impl SignatureKey {
    fn of(method: &Method) -> Self {
        Self {
            type_args: method.type_args.clone(),
            params: method
                .params
                .iter()
                .map(|param| (param.type_annotation.clone(), param.is_vararg))
                .collect(),
            where_clauses: method.where_clauses.clone(),
        }
    }
}

impl Rule for DuplicateMethod {
    fn id(&self) -> &'static str {
        "duplicate-method"
    }

    fn description(&self) -> &'static str {
        "Flag a method definition whose dispatch signature an earlier \
         definition in the same file already used. Julia holds one method per \
         signature, so the later definition silently replaces the earlier one \
         and the earlier body becomes dead. Signatures are compared on what \
         dispatch actually sees: the positional argument types (an unannotated \
         argument is `Any`), the `where` specs, and any type arguments the \
         definition applies to its own name, as a parameterized constructor \
         does. Argument names, default \
         values, keyword arguments, and the declared return type are not part \
         of a method's identity, so definitions differing only in those still \
         collide. Definitions with different `where` bounds are separate \
         methods and are not flagged, and neither are definitions inside a \
         macro call or a conditional branch, where the written signature is \
         not evidence of what gets defined."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "The second definition replaces the first, so `1` is unreachable:",
            source: "f(x::Int) = 1\nf(y::Int) = 2\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        // A macro may rewrite the signature it is handed, so a definition
        // beneath one is not evidence of the method it appears to define.
        let macro_calls: Vec<TextRange> = ctx
            .root
            .descendants()
            .filter(|node| node.kind() == SyntaxKind::MACRO_CALL)
            .map(|node| node.text_range())
            .collect();
        let index = harvest_tree(ctx.root);
        self.check_module(&index, &macro_calls, sink);
    }
}

impl DuplicateMethod {
    /// Report the repeats within one module, then recurse into its nested
    /// `module` blocks — each is its own namespace, so the same signature in
    /// two of them is two distinct methods.
    fn check_module(
        &self,
        module: &ModuleIndex,
        macro_calls: &[TextRange],
        sink: &mut Vec<Diagnostic>,
    ) {
        for group in &module.functions {
            let mut seen: Vec<SignatureKey> = Vec::new();
            for method in &group.methods {
                // `function f end` declares the function without adding a
                // method to it.
                if !method.has_body {
                    continue;
                }
                let range =
                    TextRange::new(method.loc.range.start.into(), method.loc.range.end.into());
                if macro_calls.iter().any(|call| call.contains_range(range)) {
                    continue;
                }
                let key = SignatureKey::of(method);
                if seen.contains(&key) {
                    sink.push(Diagnostic::new(
                        self.id(),
                        range,
                        format!(
                            "`{}` is already defined with this signature earlier in this \
                             file; the later definition replaces the earlier one",
                            display_name(group.owner.as_deref(), &group.name)
                        ),
                    ));
                } else {
                    seen.push(key);
                }
            }
        }
        for submodule in &module.submodules {
            self.check_module(submodule, macro_calls, sink);
        }
    }
}

/// The function as written: `show` for a bare definition, `Base.show` for a
/// qualified extension.
fn display_name(owner: Option<&[String]>, name: &str) -> String {
    match owner {
        Some(path) => format!("{}.{name}", path.join(".")),
        None => name.to_string(),
    }
}
