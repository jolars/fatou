//! `undefined-name`: a free identifier that no resolution tier provides.
//!
//! A read that binds nowhere — not up the scope chain, not an explicit
//! import, not a workspace sibling, not a whole-module `using`'s export, not
//! Base/Core — raises `UndefVarError` the moment it runs. Resolution follows
//! the shared masking order in [`crate::resolve::Resolver`], so this rule
//! agrees with completion, hover, and go-to-definition about what a name
//! means.
//!
//! Julia's `include`-splicing and metaprogramming make "is this defined?"
//! undecidable for a file in isolation, so the rule buys soundness with
//! deliberate bail-outs, skipping the whole file when:
//!
//! - the caller provides no [`ResolutionContext`] (nothing to resolve
//!   against);
//! - a whole-module `using` does not resolve against the provided library
//!   (an unharvested package, or a relative `using .M`) — it may export
//!   anything;
//! - the file calls `eval`/`@eval` (definitions invisible to the model);
//! - the file `include`s anything while no workspace context is known, or
//!   `include`s a non-literal path even with one (the harvest cannot follow
//!   it).
//!
//! Within a checkable file, value reads inside macro calls are exempt (a
//! macro receives unevaluated expressions and may bind names itself), quoted
//! code (`:(…)`, `quote … end`) is exempt entirely, and the module-implicit
//! names `eval`, `include`, `new`, and `ccall` always resolve.
//!
//! Off by default: without project context a bare file may be an `include`d
//! fragment reading its host's globals. The language server enables the rule
//! for workspace member files, where the include graph pins the file's host
//! module and the harvested library answers the remaining tiers; on the CLI
//! (which resolves against the built-in Base/Core snapshot only) it is
//! opt-in via `--select`, sound for self-contained scripts.

use rowan::TextRange;

use crate::ast::{AstNode, AstToken, CallExpr, Expr, MacroCall};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::project::include_target;
use crate::resolve::{Namespace, PackageSource, Resolution, Resolver, module_at};
use crate::semantic::{LoadKind, SemanticModel};
use crate::syntax::{SyntaxKind, SyntaxNode};

pub struct UndefinedName;

/// Names every module defines implicitly (`eval`, `include`) or that are
/// magic in their position (`new` in inner constructors, `ccall`'s builtin).
/// None appear in export lists, so resolution alone would flag them.
const MODULE_IMPLICIT: &[&str] = &["eval", "include", "new", "ccall"];

impl Rule for UndefinedName {
    fn id(&self) -> &'static str {
        "undefined-name"
    }

    fn default_enabled(&self) -> bool {
        // Sound only with project context: a bare file may be an `include`d
        // fragment reading its host's globals. The language server turns the
        // rule on for workspace member files; CLI users opt in for
        // self-contained scripts.
        false
    }

    fn description(&self) -> &'static str {
        "Flag an identifier that no resolution tier provides: not a local or \
         a file binding, not a workspace sibling, not a whole-module \
         `using`'s export, and not a Base/Core name. Such a read raises \
         `UndefVarError` at runtime. The whole file is skipped when it \
         `eval`s, `include`s outside a known workspace, or `using`s a module \
         the library cannot resolve — in those cases any name may exist; \
         value reads inside macro calls and quoted code are likewise exempt. \
         Off by default: the rule needs project context to be sound, so the \
         language server enables it for workspace member files, while the CLI \
         (resolving against a built-in Base/Core snapshot) leaves it opt-in \
         for self-contained scripts."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "`raduis` is a typo; no tier resolves it:",
            source: "function area(radius)\n    return pi * raduis^2\nend\n",
        }]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let Some(resolution) = &ctx.resolution else {
            return;
        };
        if has_unresolvable_using(ctx.model, resolution.packages) {
            return;
        }
        let scan = FileScan::collect(ctx.root);
        if scan.calls_eval {
            return;
        }
        // With a workspace, literal includes are followed by the harvest, so
        // their definitions resolve via the workspace tier; a dynamic path
        // (or any include without a workspace) brings in unknowable names.
        if scan.dynamic_include || (resolution.workspace.is_none() && scan.literal_include) {
            return;
        }

        let resolver = Resolver::new(ctx.model, resolution.packages)
            .with_workspace(resolution.workspace.clone());
        for ident in ctx.model.idents() {
            if ident.binding.is_some() {
                continue;
            }
            if scan.in_skipped(ident.range, ident.is_macro) {
                continue;
            }
            let namespace = if ident.is_macro {
                Namespace::Macro
            } else {
                Namespace::Value
            };
            if !ident.is_macro
                && (MODULE_IMPLICIT.contains(&ident.name.as_str()) || ident.name == "_")
            {
                continue;
            }
            if resolver.resolve(&ident.name, ident.range.start(), namespace)
                == Resolution::Unresolved
            {
                let display = if ident.is_macro {
                    format!("@{}", ident.name)
                } else {
                    ident.name.to_string()
                };
                sink.push(Diagnostic::new(
                    self.id(),
                    ident.range.start().into(),
                    ident.range.end().into(),
                    format!("`{display}` is not defined"),
                ));
            }
        }
    }
}

/// Whether any whole-module `using` in the file fails to resolve against
/// `packages`: a relative or interpolated path, an unharvested package, or a
/// missing submodule. Such a `using` may export anything, so no free read in
/// the file can be called undefined. (Item lists — `using X: a` — bind their
/// names explicitly and don't gate the file.)
fn has_unresolvable_using(model: &SemanticModel, packages: &dyn PackageSource) -> bool {
    model.module_loads().iter().any(|load| {
        if load.kind != LoadKind::Using || load.items.is_some() {
            return false;
        }
        if load.path.leading_dots != 0 || load.path.components.is_empty() {
            return true;
        }
        let Some(pkg) = packages.package(&load.path.components[0]) else {
            return true;
        };
        module_at(&pkg.root, &load.path.components[1..]).is_none()
    })
}

/// One pass over the CST collecting everything the rule skips or bails on:
/// macro-call and quote extents, and the `eval`/`include` call shapes.
struct FileScan {
    /// `MACRO_CALL` extents. Value reads inside are exempt (the macro may
    /// bind them); the macro's own name is still checked.
    macro_calls: Vec<TextRange>,
    /// `QUOTE_EXPR` extents: quoted code is data, not reads.
    quotes: Vec<TextRange>,
    calls_eval: bool,
    literal_include: bool,
    dynamic_include: bool,
}

impl FileScan {
    fn collect(root: &SyntaxNode) -> Self {
        let mut scan = FileScan {
            macro_calls: Vec::new(),
            quotes: Vec::new(),
            calls_eval: false,
            literal_include: false,
            dynamic_include: false,
        };
        for node in root.descendants() {
            match node.kind() {
                SyntaxKind::MACRO_CALL => {
                    scan.macro_calls.push(node.text_range());
                    let name = MacroCall::cast(node)
                        .and_then(|call| call.name())
                        .and_then(|name| name.macro_token());
                    if name.is_some_and(|token| token.text() == "eval") {
                        scan.calls_eval = true;
                    }
                }
                SyntaxKind::QUOTE_EXPR => scan.quotes.push(node.text_range()),
                SyntaxKind::CALL_EXPR => {
                    let Some(call) = CallExpr::cast(node) else {
                        continue;
                    };
                    let Some(Expr::Name(callee)) = call.callee() else {
                        continue;
                    };
                    match callee.ident().map(|ident| ident.text().to_string()) {
                        Some(name) if name == "eval" => scan.calls_eval = true,
                        Some(name) if name == "include" => {
                            if include_target(&call).is_some() {
                                scan.literal_include = true;
                            } else {
                                scan.dynamic_include = true;
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        scan
    }

    /// Whether the read at `range` is exempt: inside quoted code, or a value
    /// read inside a macro call (the macro name itself stays checked).
    fn in_skipped(&self, range: TextRange, is_macro: bool) -> bool {
        let within = |extents: &[TextRange]| extents.iter().any(|e| e.contains_range(range));
        within(&self.quotes) || (!is_macro && within(&self.macro_calls))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{
        DefLocation, ExportedName, FunctionGroup, ModuleIndex, PackageIndex, Span, Visibility,
    };
    use crate::linter::rules::ResolutionContext;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn loc() -> DefLocation {
        DefLocation {
            file: "src/x.jl".into(),
            range: Span { start: 0, end: 0 },
        }
    }

    /// A library with a Base exporting `exports`, plus a workspace package
    /// `MyPkg` defining top-level functions `siblings` (unexported — the shape
    /// of a package's own globals).
    fn base(exports: &[&str]) -> BTreeMap<String, Arc<PackageIndex>> {
        let pkg = PackageIndex {
            name: "Base".to_string(),
            root: ModuleIndex {
                name: "Base".to_string(),
                bare: false,
                loc: loc(),
                exports: exports
                    .iter()
                    .map(|n| ExportedName {
                        name: n.to_string(),
                        visibility: Visibility::Exported,
                        loc: loc(),
                    })
                    .collect(),
                functions: Vec::new(),
                types: Vec::new(),
                consts: Vec::new(),
                macros: Vec::new(),
                submodules: Vec::new(),
            },
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        };
        BTreeMap::from([("Base".to_string(), Arc::new(pkg))])
    }

    fn workspace(siblings: &[&str]) -> Arc<PackageIndex> {
        Arc::new(PackageIndex {
            name: "MyPkg".to_string(),
            root: ModuleIndex {
                name: "MyPkg".to_string(),
                bare: false,
                loc: loc(),
                exports: Vec::new(),
                functions: siblings
                    .iter()
                    .map(|f| FunctionGroup {
                        name: f.to_string(),
                        owner: None,
                        methods: Vec::new(),
                        doc: None,
                    })
                    .collect(),
                types: Vec::new(),
                consts: Vec::new(),
                macros: Vec::new(),
                submodules: Vec::new(),
            },
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    /// Lint `src` with the rule alone, against `packages` and an optional
    /// workspace package (host module = the package root).
    fn messages(
        src: &str,
        packages: &BTreeMap<String, Arc<PackageIndex>>,
        ws: Option<Arc<PackageIndex>>,
    ) -> Vec<String> {
        let parsed = crate::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "fixture must parse clean");
        let model = SemanticModel::build(&parsed.cst);
        let ctx = RuleContext {
            path: None,
            root: &parsed.cst,
            model: &model,
            resolution: Some(ResolutionContext {
                packages,
                workspace: ws.map(|pkg| (pkg, Vec::new())),
            }),
        };
        let mut sink = Vec::new();
        UndefinedName.check_file(&ctx, &mut sink);
        sink.into_iter().map(|d| d.message).collect()
    }

    #[test]
    fn workspace_sibling_resolves() {
        // `helper` is defined in a sibling file of the package; with the
        // workspace tier it resolves, while `helprr` stays undefined.
        let lib = base(&[]);
        let msgs = messages(
            "f() = helper() + helprr()\n",
            &lib,
            Some(workspace(&["helper"])),
        );
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("helprr"));
    }

    #[test]
    fn without_workspace_a_sibling_read_would_flag() {
        // The same source with no workspace context flags both — which is
        // exactly why the rule is gated to member files by the server and
        // opt-in on the CLI.
        let lib = base(&[]);
        let msgs = messages("f() = helper() + helprr()\n", &lib, None);
        assert_eq!(msgs.len(), 2, "{msgs:?}");
    }

    #[test]
    fn literal_include_bails_only_without_a_workspace() {
        let lib = base(&[]);
        let src = "include(\"other.jl\")\nf() = mystery()\n";
        // No workspace: the include splices unknowable names — bail.
        assert_eq!(messages(src, &lib, None), Vec::<String>::new());
        // With a workspace, the harvest followed the include; `mystery` not
        // being in the package index is a real finding.
        let msgs = messages(src, &lib, Some(workspace(&["helper"])));
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("mystery"));
    }

    #[test]
    fn dynamic_include_bails_even_with_a_workspace() {
        let lib = base(&[]);
        let src = "include(joinpath(root, \"gen.jl\"))\nf() = mystery()\n";
        assert_eq!(
            messages(src, &lib, Some(workspace(&[]))),
            Vec::<String>::new()
        );
    }

    #[test]
    fn quoted_code_is_not_read() {
        let lib = base(&[]);
        let msgs = messages(
            "ex = :(alpha + beta)\nblock = quote\n    gamma(delta)\nend\n",
            &lib,
            Some(workspace(&[])),
        );
        assert_eq!(msgs, Vec::<String>::new());
    }

    #[test]
    fn no_resolution_context_is_silent() {
        let parsed = crate::parser::parse("f() = mystery()\n");
        let model = SemanticModel::build(&parsed.cst);
        let ctx = RuleContext {
            path: None,
            root: &parsed.cst,
            model: &model,
            resolution: None,
        };
        let mut sink = Vec::new();
        UndefinedName.check_file(&ctx, &mut sink);
        assert!(sink.is_empty());
    }
}
