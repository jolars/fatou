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

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::resolve::{Namespace, Resolution};

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
        // No resolution context, an unresolvable whole-module `using`, `eval`,
        // or an unfollowable `include`: all four leave the file unanswerable
        // (see `RuleContext::trusts_resolution`).
        if !ctx.trusts_resolution() {
            return;
        }
        let Some(resolver) = ctx.resolver() else {
            return;
        };

        let scan = ctx.file_scan();
        for ident in ctx.model.idents() {
            if ident.binding.is_some() {
                continue;
            }
            // Quoted code is data; a value read inside a macro call is exempt
            // (the macro may bind it), but the macro's own name is a real read.
            if scan.in_quote(ident.range) || (!ident.is_macro && scan.in_macro_call(ident.range)) {
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
                    ident.range,
                    format!("`{display}` is not defined"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{
        DefLocation, ExportedName, FunctionGroup, ModuleIndex, ModuleUsing, PackageIndex, Span,
        Visibility,
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
                usings: Vec::new(),
                imported_names: Vec::new(),
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
                usings: Vec::new(),
                imported_names: Vec::new(),
            },
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    /// A workspace package whose root module records whole-module `using` paths
    /// `usings` and module-level bound names `imported` — a sibling file's load
    /// surface, spliced into the module by `include`.
    fn workspace_with_loads(usings: &[&[&str]], imported: &[&str]) -> Arc<PackageIndex> {
        let mut pkg = (*workspace(&[])).clone();
        pkg.root.usings = usings
            .iter()
            .map(|components| ModuleUsing {
                leading_dots: 0,
                components: components.iter().map(|c| c.to_string()).collect(),
            })
            .collect();
        pkg.root.imported_names = imported.iter().map(|n| n.to_string()).collect();
        Arc::new(pkg)
    }

    /// `base` plus an extra package `name` exporting `exports`.
    fn base_plus(name: &str, exports: &[&str]) -> BTreeMap<String, Arc<PackageIndex>> {
        let mut lib = base(&[]);
        let extra = base(exports);
        let mut pkg = (*extra.get("Base").unwrap().clone()).clone();
        pkg.name = name.to_string();
        pkg.root.name = name.to_string();
        lib.insert(name.to_string(), Arc::new(pkg));
        lib
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
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages,
                workspace: ws.map(|pkg| (pkg, Vec::new())),
            }));
        let mut sink = Vec::new();
        UndefinedName.check_file(&ctx, &mut sink);
        sink.into_iter().map(|d| d.message.body).collect()
    }

    #[test]
    fn qualified_base_extension_resolves_via_self_export() {
        // `function Base.show(...)` reads `Base` as a module qualifier. The
        // harvest synthesizes Base's own name into its exports (Julia never
        // `export`s it), so the read resolves — a real-world `Base.show`
        // extension must not raise `undefined-name`.
        let lib = base(&["Base", "IO", "print", "show"]);
        let src = "function Base.show(io::IO, x)\n    print(io, x)\nend\n";
        assert_eq!(
            messages(src, &lib, Some(workspace(&[]))),
            Vec::<String>::new()
        );
    }

    #[test]
    fn qualified_base_extension_flags_without_self_export() {
        // Guard on the fix: strip Base's self-name and the same qualifier read
        // is unresolved — exactly the false positive the harvest self-export
        // removes.
        let lib = base(&["IO", "print", "show"]);
        let src = "function Base.show(io::IO, x)\n    print(io, x)\nend\n";
        let msgs = messages(src, &lib, Some(workspace(&[])));
        assert_eq!(msgs, vec!["`Base` is not defined".to_string()], "{msgs:?}");
    }

    #[test]
    fn workspace_package_self_name_resolves() {
        // A file spliced into the package's root module may qualify a call with
        // the package's own name (`MyPkg.helper()`); Julia binds a module's own
        // name inside it, so the qualifier must not raise `undefined-name`.
        // Regression: SLOPE.jl's `SLOPE.fit_slope_dense(...)` inside module SLOPE.
        let lib = base(&[]);
        let msgs = messages("f() = MyPkg.helper()\n", &lib, Some(workspace(&["helper"])));
        assert_eq!(msgs, Vec::<String>::new(), "{msgs:?}");
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
    fn sibling_using_export_resolves() {
        // SLOPE.jl regression: `cv.jl` reads `SparseMatrixCSC`, which a sibling
        // `models.jl` brings in with `using SparseArrays`. The module-wide
        // `using` resolves the read, so no false `undefined-name`.
        let lib = base_plus("SparseArrays", &["SparseMatrixCSC"]);
        let ws = workspace_with_loads(&[&["SparseArrays"]], &[]);
        assert_eq!(
            messages("f(::SparseMatrixCSC) = 1\n", &lib, Some(ws)),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn sibling_imported_name_resolves() {
        // A name a sibling file's `import Foo` binds is a module global here.
        let lib = base(&[]);
        let ws = workspace_with_loads(&[], &["Foo"]);
        assert_eq!(
            messages("g() = Foo.helper()\n", &lib, Some(ws)),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn unresolvable_sibling_using_bails_the_file() {
        // A sibling's `using` of an unharvested package could export anything,
        // so the whole file is skipped — even a genuine typo goes unreported.
        let lib = base(&[]);
        let ws = workspace_with_loads(&[&["Unharvested"]], &[]);
        assert_eq!(
            messages("f() = mystery()\n", &lib, Some(ws)),
            Vec::<String>::new(),
        );
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
        let ctx = RuleContext::new(None, &parsed.cst, &model);
        let mut sink = Vec::new();
        UndefinedName.check_file(&ctx, &mut sink);
        assert!(sink.is_empty());
    }
}
