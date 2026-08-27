//! `unresolved-docstring-reference`: a provably missing explicit `@ref` target.
//!
//! Documenter destinations can also name headings, other pages, extensions, or
//! dynamically generated bindings. The rule therefore considers only explicit
//! targets whose link label is code, exempts local Markdown anchors, and uses
//! the linter's shared resolution soundness floor. Unknown packages and target
//! shapes are unprovable and stay silent.

use crate::ast::{AstNode as _, Expr, Root};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::parser;
use crate::resolve::{Namespace, PackageSource, Resolution, Resolver};
use crate::semantic::SemanticModel;

pub struct UnresolvedDocstringReference;

impl Rule for UnresolvedDocstringReference {
    fn id(&self) -> &'static str {
        "unresolved-docstring-reference"
    }

    fn description(&self) -> &'static str {
        "Flag an explicit, code-labeled Documenter `@ref` target when project \
         resolution can prove that no such Julia symbol exists. Local Markdown \
         anchors are accepted, and inferred references, prose-labeled links, \
         unknown packages, dynamic definitions, unresolved `using`s, opaque \
         docstrings, and unsupported target shapes stay silent rather than \
         risk a false positive."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "The explicit target retains the same typo as its code label:",
            source: "\"\"\"\nSee [`Base.raduis`](@ref Base.raduis).\n\"\"\"\narea(radius) = pi * radius^2\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        if !ctx.trusts_resolution() {
            return;
        }
        let scan = ctx.documentation_scan();
        let Some(resolver) = ctx.resolver() else {
            return;
        };
        let project_member = ctx
            .resolution
            .as_ref()
            .is_some_and(|resolution| resolution.workspace.is_some());
        for reference in &scan.references {
            if scan.anchors.contains(&reference.target)
                || !target_is_missing(resolver, &reference.target, reference.at, project_member)
            {
                continue;
            }
            sink.push(Diagnostic::new(
                self.id(),
                reference.range,
                format!(
                    "documentation reference `{}` does not resolve",
                    reference.target
                ),
            ));
        }
    }
}

fn target_is_missing<P: PackageSource + ?Sized>(
    resolver: &Resolver<'_, P>,
    target: &str,
    at: rowan::TextSize,
    project_member: bool,
) -> bool {
    let parsed = parser::parse(target);
    if !parsed.diagnostics.is_empty() {
        return false;
    }
    let Some(root) = Root::cast(parsed.cst.clone()) else {
        return false;
    };
    let mut items = root.items();
    let Some(item) = items.next() else {
        return false;
    };
    if items.next().is_some()
        || !matches!(
            item,
            Expr::Name(_) | Expr::BinaryExpr(_) | Expr::CallExpr(_) | Expr::MacroCall(_)
        )
    {
        return false;
    }

    let reference = SemanticModel::build(&parsed.cst);
    if let Some(qualified) = reference.qualified_reads().first() {
        let namespace = if qualified.is_macro {
            Namespace::Macro
        } else {
            Namespace::Value
        };
        return resolver.qualified_name_exists(&qualified.path, namespace) == Some(false);
    }
    // A bare source file may itself be included into an unseen host module;
    // only a harvested workspace proves that its apparent free names are truly
    // absent from the project. Fully qualified known-package targets above do
    // not have that ambiguity.
    if !project_member {
        return false;
    }
    let Some(ident) = reference.idents().first() else {
        return false;
    };
    let namespace = if ident.is_macro {
        Namespace::Macro
    } else {
        Namespace::Value
    };
    matches!(
        resolver.resolve(&ident.name, at, namespace),
        Resolution::Unresolved
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::harvest_tree;
    use crate::index::model::{ModuleIndex, PackageIndex};
    use crate::linter::rules::ResolutionContext;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn package(name: &str, source: &str) -> Arc<PackageIndex> {
        let parsed = parser::parse(source);
        assert!(parsed.diagnostics.is_empty());
        Arc::new(PackageIndex {
            name: name.to_string(),
            root: ModuleIndex {
                name: name.to_string(),
                ..harvest_tree(&parsed.cst)
            },
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    #[test]
    fn a_workspace_proves_a_bare_target_missing() {
        let source = concat!(
            "\"\"\"See [`raduis`](@ref raduis).\"\"\"\n",
            "area(radius) = radius\n",
        );
        let parsed = parser::parse(source);
        let model = SemanticModel::build(&parsed.cst);
        let workspace = package("MyPkg", source);
        let packages = BTreeMap::from([("MyPkg".to_string(), workspace.clone())]);
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages: &packages,
                workspace: Some((workspace.clone(), Vec::new())),
                declared_deps: None,
            }));
        let resolver =
            Resolver::new(&model, &packages).with_workspace(Some((workspace.clone(), Vec::new())));

        assert!(target_is_missing(&resolver, "raduis", 0.into(), true));
        assert!(!target_is_missing(&resolver, "area", 0.into(), true));
        assert!(!target_is_missing(&resolver, "raduis", 0.into(), false));

        let mut diagnostics = Vec::new();
        UnresolvedDocstringReference.check_file(&ctx, &mut diagnostics);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.body.contains("raduis"));
    }

    #[test]
    fn a_workspace_sibling_satisfies_a_bare_target() {
        let source = concat!(
            "\"\"\"See [`sibling`](@ref sibling).\"\"\"\n",
            "area(radius) = radius\n",
        );
        let parsed = parser::parse(source);
        let model = SemanticModel::build(&parsed.cst);
        let workspace = package("MyPkg", "sibling() = 1\narea(radius) = radius\n");
        let packages = BTreeMap::from([("MyPkg".to_string(), workspace.clone())]);
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages: &packages,
                workspace: Some((workspace, Vec::new())),
                declared_deps: None,
            }));

        let mut diagnostics = Vec::new();
        UnresolvedDocstringReference.check_file(&ctx, &mut diagnostics);
        assert!(diagnostics.is_empty());
    }
}
