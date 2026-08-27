//! `invalid-type-declaration`: a `::` annotation whose type is a function.
//!
//! The right side of a `::` has to be a type. Julia checks it eagerly, so
//! `f(x::g)` where `g` is a generic function raises a `TypeError` the moment
//! the method is defined — not when it is called — and a value-position
//! `x::g` raises one as soon as it runs. The code cannot do what it says,
//! which makes this a correctness finding rather than a style one.
//!
//! Telling a function from a type is a resolution question, and only a
//! *closed* answer is trustworthy, so the rule is deliberately partial. It
//! fires on exactly two tiers:
//!
//! - a binding in this file the semantic model recorded as
//!   [`BindingKind::Function`] — a `function g … end` or a short-form
//!   `g(x) = …`;
//! - a top-level name of the enclosing workspace package whose index holds a
//!   function group and nothing else.
//!
//! Everything else stays silent. A Base/Core name is known only by the export
//! snapshot's names, which carry no kinds (`f(x::sin)` is a miss); an
//! `import`ed or `using`'d name belongs to a module whose harvest may be
//! absent; a plain variable or a `const` alias can perfectly well hold a type
//! (`const T = Int`), and the model calls neither a function.
//!
//! The one shape that would otherwise misfire is a *constructor*: an outer
//! `Foo(x::Int) = …` binds `Foo` as a function even though `Foo` is a type, so
//! any same-named type, `const`, or module — in this file or anywhere in the
//! workspace package — withholds the finding. The whole-file bail-outs are the
//! shared ones ([`RuleContext::trusts_resolution`]): `eval`, an unfollowable
//! `include`, an unresolvable `using`. A macro call or quoted code may be
//! reshaped before it runs, so annotations inside one are skipped.
//!
//! Off by default, like the other resolution-gated rules: without a project
//! context there is no resolver to ask.

use crate::ast::{AstNode, AstToken, Expr, TypeAnnotation};
use crate::index::model::ModuleIndex;
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::resolve::{Namespace, Resolution, module_at};
use crate::semantic::BindingKind;
use crate::syntax::{SyntaxElement, SyntaxKind};

pub struct InvalidTypeDeclaration;

impl Rule for InvalidTypeDeclaration {
    fn id(&self) -> &'static str {
        "invalid-type-declaration"
    }

    fn default_enabled(&self) -> bool {
        // Needs a resolution context to tell a function from a type at all;
        // the language server turns it on for workspace member files, and the
        // CLI leaves it to an explicit `--select`.
        false
    }

    fn description(&self) -> &'static str {
        "Flag a `::` type declaration whose declared type is a function. The \
         right side of a `::` must be a type, and Julia checks it eagerly: \
         `f(x::g)` for a generic function `g` raises a `TypeError` when the \
         method is defined, and a value-position `x::g` raises one as soon as \
         it runs. The rule is deliberately partial, since only a closed answer \
         about a name is trustworthy: it fires for a function defined in this \
         file and for a top-level function of the enclosing workspace package, \
         and stays silent for a Base/Core name (the export snapshot records no \
         kinds), an imported name, and any binding that could hold a type such \
         as a variable or a `const` alias. A same-named type, `const`, or \
         module anywhere in the file or the package withholds the finding, so \
         an outer constructor (`Foo(x::Int) = …`, which binds `Foo` as a \
         function too) is never flagged. The file is skipped entirely when it \
         `eval`s, `include`s outside a known workspace, or `using`s a module \
         the library cannot resolve, and annotations inside a macro call or \
         quoted code are exempt. Off by default: like the other \
         resolution-gated rules it needs project context, so the language \
         server enables it for workspace member files while the CLI leaves it \
         opt-in via `--select`."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`scale` is a function, so declaring `y::scale` is a `TypeError`:",
            source: "scale(x) = 2x\n\napply(y::scale) = y\n",
        }]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::TYPE_ANNOTATION]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        // No resolution context, an unresolvable whole-module `using`, `eval`,
        // or an unfollowable `include`: all four leave the file unanswerable
        // (see `RuleContext::trusts_resolution`).
        if !ctx.trusts_resolution() {
            return;
        }
        let Some(node) = el.as_node() else { return };
        let Some(annotation) = TypeAnnotation::cast(node.clone()) else {
            return;
        };
        // Only a bare name: a type application (`g{Int}`), a qualified name,
        // and a computed type all fall outside the closed world.
        let Some(Expr::Name(name)) = annotation.ty() else {
            return;
        };
        let Some(ident) = name.ident() else { return };
        let range = ident.syntax().text_range();

        // A macro may rewrite the signature it is handed, and quoted code is
        // data rather than a declaration.
        if ctx.file_scan().in_skipped(range) {
            return;
        }
        let Some(resolver) = ctx.resolver() else {
            return;
        };

        let text = ident.text();
        let is_function = match resolver.resolve(text, range.start(), Namespace::Value) {
            Resolution::Binding(id) => ctx.model.binding(id).kind == BindingKind::Function,
            Resolution::Workspace { module, name } => ctx
                .resolution
                .as_ref()
                .and_then(|resolution| resolution.workspace.as_ref())
                .and_then(|(pkg, _)| module_at(&pkg.root, &module))
                .is_some_and(|module| module.functions.iter().any(|g| g.name == name.as_str())),
            _ => false,
        };
        if !is_function || self.names_a_type_too(text, ctx) {
            return;
        }

        sink.push(Diagnostic::new(
            self.id(),
            range,
            format!(
                "`{text}` is a function, not a type: `::{text}` is not a valid type declaration"
            ),
        ));
    }
}

impl InvalidTypeDeclaration {
    /// Whether anything the closed world can see also gives `name` a
    /// *non-function* meaning: a type, a `const`, or a module, in this file or
    /// anywhere in the enclosing workspace package. The constructor guard —
    /// `Foo(x::Int) = …` binds `Foo` as a function while `Foo` is a type — and
    /// the general "the model saw two definitions of this name" bail-out.
    ///
    /// Scope-blind on purpose: a same-named type in a nested module is not the
    /// binding that resolved, but under-reporting there is the cheap mistake.
    fn names_a_type_too(&self, name: &str, ctx: &RuleContext<'_>) -> bool {
        let in_file = ctx.model.bindings().iter().any(|binding| {
            binding.name == name
                && matches!(
                    binding.kind,
                    BindingKind::Type
                        | BindingKind::Const
                        | BindingKind::Module
                        | BindingKind::Import
                )
        });
        if in_file {
            return true;
        }
        ctx.resolution
            .as_ref()
            .and_then(|resolution| resolution.workspace.as_ref())
            .is_some_and(|(pkg, _)| non_function_definition(&pkg.root, name))
    }
}

/// Whether `module` or any submodule defines `name` as something other than a
/// function: a type, a `const`, or a nested module.
fn non_function_definition(module: &ModuleIndex, name: &str) -> bool {
    module.types.iter().any(|t| t.name == name)
        || module.consts.iter().any(|c| c.name == name)
        || module.submodules.iter().any(|m| m.name == name)
        || module
            .submodules
            .iter()
            .any(|sub| non_function_definition(sub, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{
        DefLocation, FunctionGroup, Method, PackageIndex, Span, TypeDef, TypeKind,
    };
    use crate::linter::rules::ResolutionContext;
    use crate::semantic::SemanticModel;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn loc() -> DefLocation {
        DefLocation {
            file: "src/x.jl".into(),
            range: Span { start: 0, end: 0 },
        }
    }

    /// A workspace package `MyPkg` whose root defines `functions` and `types`.
    fn workspace(functions: &[&str], types: &[&str]) -> Arc<PackageIndex> {
        Arc::new(PackageIndex {
            name: "MyPkg".to_string(),
            root: ModuleIndex {
                name: "MyPkg".to_string(),
                bare: false,
                loc: loc(),
                doc: None,
                exports: Vec::new(),
                functions: functions
                    .iter()
                    .map(|f| FunctionGroup {
                        name: (*f).to_string(),
                        owner: None,
                        methods: vec![Method {
                            params: Vec::new(),
                            keyword_params: Vec::new(),
                            type_args: Vec::new(),
                            where_clauses: Vec::new(),
                            return_type: None,
                            has_body: true,
                            doc: None,
                            loc: loc(),
                        }],
                        doc: None,
                    })
                    .collect(),
                types: types
                    .iter()
                    .map(|t| TypeDef {
                        name: (*t).to_string(),
                        kind: TypeKind::Abstract,
                        type_params: Vec::new(),
                        supertype: None,
                        fields: Vec::new(),
                        doc: None,
                        loc: loc(),
                    })
                    .collect(),
                consts: Vec::new(),
                macros: Vec::new(),
                submodules: Vec::new(),
                usings: Vec::new(),
                imported_names: Vec::new(),
            },
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    /// Lint `src` with the rule alone, resolving against an empty library and
    /// the given workspace package.
    fn messages(src: &str, ws: Option<Arc<PackageIndex>>) -> Vec<String> {
        let parsed = crate::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "fixture must parse clean");
        let model = SemanticModel::build(&parsed.cst);
        let packages: BTreeMap<String, Arc<PackageIndex>> = BTreeMap::new();
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages: &packages,
                workspace: ws.map(|pkg| (pkg, Vec::new())),
                declared_deps: None,
            }));
        let mut sink = Vec::new();
        for el in parsed.cst.descendants_with_tokens() {
            if el.kind() == SyntaxKind::TYPE_ANNOTATION {
                InvalidTypeDeclaration.check(&el, &ctx, &mut sink);
            }
        }
        sink.into_iter().map(|d| d.message.body).collect()
    }

    #[test]
    fn a_workspace_function_in_an_annotation_is_flagged() {
        // `helper` is defined in a sibling file of the package, as a function.
        let msgs = messages("f(x::helper) = x\n", Some(workspace(&["helper"], &[])));
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("`helper`"), "{msgs:?}");
    }

    #[test]
    fn a_workspace_constructor_group_is_not_flagged() {
        // The package defines both a type `Helper` and its constructors, so
        // the function group is no evidence that the name is not a type.
        assert_eq!(
            messages(
                "f(x::Helper) = x\n",
                Some(workspace(&["Helper"], &["Helper"]))
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn no_resolution_context_is_silent() {
        let parsed = crate::parser::parse("g(x) = x\nf(y::g) = y\n");
        let model = SemanticModel::build(&parsed.cst);
        let ctx = RuleContext::new(None, &parsed.cst, &model);
        let mut sink = Vec::new();
        for el in parsed.cst.descendants_with_tokens() {
            if el.kind() == SyntaxKind::TYPE_ANNOTATION {
                InvalidTypeDeclaration.check(&el, &ctx, &mut sink);
            }
        }
        assert!(sink.is_empty());
    }
}
