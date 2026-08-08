//! `unresolved-import`: a `using`/`import` of a module the project cannot load.
//!
//! Julia resolves a bare `using Foo` against the *active project*: inside a
//! package's own source files that is the package's `Project.toml`, so `Foo`
//! must appear in its `[deps]` (a standard library is no exception — a package
//! that reaches for `Printf` has to declare it). A name that appears nowhere
//! raises `ArgumentError: Package Foo not found in current path` the moment the
//! module is loaded, which is why this is a correctness finding rather than a
//! style one.
//!
//! The rule answers "can this name be loaded?" from two independent sources,
//! and stays silent when *either* says yes:
//!
//! - the project's declared `[deps]`
//!   ([`ResolutionContext::declared_deps`](crate::linter::rules::ResolutionContext::declared_deps)),
//!   read straight from `Project.toml` — so a project that was never
//!   instantiated, or one whose Julia installation fatou could not locate,
//!   still resolves its own dependencies; and
//! - the harvested library, which knows what the located installation actually
//!   ships. This is what exempts the standard library when a package
//!   under-declares it, and it also exempts a transitive manifest dependency —
//!   a deliberate false *negative*, since the harvest cannot tell a direct
//!   dependency from an indirect one.
//!
//! Everything else is left alone by construction: a relative path (`using
//! .Sub`, `import ..Sibling`) names a module inside the package and never
//! reaches the loader's environment, an interpolated path (`using $M`) has no
//! name to check, and a load inside quoted code or a macro call is data the
//! macro may rewrite.
//!
//! The soundness gate is the declared dependency set itself, and the driver
//! attaches it only for a file the harvest placed inside a workspace package
//! (see `ProjectContext::resolution_for`). That keeps the rule off exactly the
//! files whose `using` clauses resolve against a *different* environment: a
//! loose script, and a `test/`, `docs/`, or `benchmark/` file, whose deps live
//! in their own project (or the package's `[extras]`).
//!
//! Off by default, like the other project-gated rules: the language server
//! enables it for workspace member files, where it carries the project context;
//! on the CLI it is opt-in via `--select`, which is also what makes `fatou lint`
//! harvest the enclosing project.

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};

pub struct UnresolvedImport;

/// Module names bound in every environment, whatever the project declares.
/// `Base` and `Core` are always in the harvested library too, but `Main` is a
/// runtime module no harvest produces.
const ALWAYS_LOADABLE: &[&str] = &["Base", "Core", "Main"];

impl Rule for UnresolvedImport {
    fn id(&self) -> &'static str {
        "unresolved-import"
    }

    fn default_enabled(&self) -> bool {
        // Meaningful only with project context: the rule needs the enclosing
        // package's `Project.toml` to know what may be loaded. The language
        // server turns it on for workspace member files; on the CLI `--select`
        // both enables it and triggers the project harvest.
        false
    }

    fn description(&self) -> &'static str {
        "Flag a `using`/`import` of a module the enclosing project cannot load: \
         a name that is neither in its `Project.toml` `[deps]` nor provided by \
         the harvested environment (the standard library included). Julia \
         resolves a bare `using Foo` against the active project, so an \
         undeclared name raises `ArgumentError: Package Foo not found in \
         current path` when the module loads. Relative paths (`using .Sub`), \
         interpolated paths, and loads inside quoted code or macro calls are \
         exempt, and a name the harvest resolved is always accepted — so a \
         transitive dependency goes unreported rather than risking a false \
         positive. Off by default: the rule needs the project context that only \
         a package source file carries, so the language server enables it for \
         workspace member files while the CLI leaves it opt-in."
    }

    fn examples(&self) -> &'static [Example] {
        &[Example {
            caption: "The project declares `LinearAlgebra` in `[deps]`, but not \
                      `Frobnicate`:",
            source: "using LinearAlgebra\nusing Frobnicate\n",
        }]
    }

    fn example_declared_deps(&self) -> Option<&'static [&'static str]> {
        Some(&["LinearAlgebra"])
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        // No declared dependency set: either the caller has no project context,
        // or this file is not a package source file and loads against another
        // environment entirely. Both leave the question unanswerable.
        let Some(resolution) = &ctx.resolution else {
            return;
        };
        let Some(deps) = &resolution.declared_deps else {
            return;
        };

        let scan = ctx.file_scan();
        for load in ctx.model.module_loads() {
            // A relative path names a module inside this package; the loader
            // never consults the environment for it.
            if load.path.leading_dots != 0 {
                continue;
            }
            // An interpolated path (`using $M`) leaves no leading component to
            // look up.
            let (Some(name), Some(range)) = (load.path.components.first(), load.root_range) else {
                continue;
            };
            // A macro receives the load unevaluated and may rewrite it into
            // something else entirely. (Quoted code never reaches the model as a
            // load at all; the scan covers it for the same reason.)
            if scan.in_skipped(load.range) {
                continue;
            }
            if ALWAYS_LOADABLE.contains(&name.as_str())
                || deps.contains(name.as_str())
                || resolution.packages.package(name).is_some()
            {
                continue;
            }
            sink.push(Diagnostic::new(
                self.id(),
                range,
                format!("`{name}` is not a dependency of this project"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::DeclaredDeps;
    use crate::index::model::{DefLocation, ExportedName, ModuleIndex, PackageIndex, Span};
    use crate::linter::rules::ResolutionContext;
    use crate::semantic::SemanticModel;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// A library holding one package per name in `names`, each exporting
    /// nothing — the shape a harvest of the located installation leaves behind.
    fn library(names: &[&str]) -> BTreeMap<String, Arc<PackageIndex>> {
        names
            .iter()
            .map(|name| {
                let pkg = PackageIndex {
                    name: (*name).to_string(),
                    root: ModuleIndex {
                        name: (*name).to_string(),
                        bare: false,
                        loc: DefLocation {
                            file: "src/x.jl".into(),
                            range: Span { start: 0, end: 0 },
                        },
                        exports: Vec::<ExportedName>::new(),
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
                ((*name).to_string(), Arc::new(pkg))
            })
            .collect()
    }

    /// Run the rule alone over `src`, against a library holding `packages` and
    /// a project declaring `deps` (`None` for a file the driver gave no project
    /// context). Returns the findings.
    fn findings(src: &str, packages: &[&str], deps: Option<&[&str]>) -> Vec<Diagnostic> {
        let lib = library(packages);
        let parsed = crate::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "fixture must parse clean");
        let model = SemanticModel::build(&parsed.cst);
        let declared: Option<Arc<DeclaredDeps>> =
            deps.map(|names| Arc::new(names.iter().map(|n| (*n).to_string()).collect()));
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages: &lib,
                workspace: None,
                declared_deps: declared,
            }));
        let mut sink = Vec::new();
        UnresolvedImport.check_file(&ctx, &mut sink);
        sink
    }

    /// The findings' messages, for the cases that only care about which names
    /// were reported.
    fn messages(src: &str, packages: &[&str], deps: Option<&[&str]>) -> Vec<String> {
        findings(src, packages, deps)
            .into_iter()
            .map(|d| d.message.body)
            .collect()
    }

    fn flagged(name: &str) -> String {
        format!("`{name}` is not a dependency of this project")
    }

    #[test]
    fn undeclared_package_is_flagged() {
        assert_eq!(
            messages("using Frobnicate\n", &["Base"], Some(&[])),
            vec![flagged("Frobnicate")],
        );
    }

    #[test]
    fn import_is_checked_like_using() {
        assert_eq!(
            messages("import Frobnicate\n", &["Base"], Some(&[])),
            vec![flagged("Frobnicate")],
        );
    }

    #[test]
    fn every_clause_of_a_comma_list_is_checked() {
        // `import A, B` is two loads; only the undeclared one is reported.
        assert_eq!(
            messages(
                "import DataFrames, Frobnicate\n",
                &[],
                Some(&["DataFrames"])
            ),
            vec![flagged("Frobnicate")],
        );
    }

    #[test]
    fn item_list_and_alias_forms_are_checked() {
        assert_eq!(
            messages("using Frobnicate: thing\n", &[], Some(&[])),
            vec![flagged("Frobnicate")],
        );
        assert_eq!(
            messages("import Frobnicate as F\n", &[], Some(&[])),
            vec![flagged("Frobnicate")],
        );
    }

    #[test]
    fn a_dotted_path_is_judged_by_its_root() {
        // Only the first component names a package the loader looks up; the
        // submodule below it is the package's own business.
        assert_eq!(
            messages("using Frobnicate.Sub\n", &[], Some(&[])),
            vec![flagged("Frobnicate")],
        );
        assert_eq!(
            messages("using DataFrames.Sub: x\n", &[], Some(&["DataFrames"])),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn the_span_covers_the_package_name_alone() {
        // The caret belongs on the name the loader fails to find, not on the
        // whole statement — which for the item-list form is much wider.
        let src = "using Frobnicate.Sub: thing\n";
        let found = findings(src, &[], Some(&[]));
        assert_eq!(found.len(), 1);
        assert_eq!(&src[found[0].range], "Frobnicate");
    }

    #[test]
    fn declared_dependency_is_silent() {
        assert_eq!(
            messages("using DataFrames\n", &["Base"], Some(&["DataFrames"])),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn a_harvested_package_is_silent_even_when_undeclared() {
        // The located installation ships the standard library whether or not
        // the project declares it, and the harvest is what knows that.
        assert_eq!(
            messages("using LinearAlgebra\n", &["LinearAlgebra"], Some(&[])),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn base_core_and_main_are_always_loadable() {
        // `Main` is a runtime module no harvest produces, so an empty library
        // must not make it a finding.
        assert_eq!(
            messages(
                "using Base.Threads\nimport Core\nusing Main.Helpers\n",
                &[],
                Some(&[]),
            ),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn relative_paths_are_not_dependencies() {
        // `using .Sub` names a module inside the package; the loader never
        // consults the environment for it.
        assert_eq!(
            messages(
                "using .Sub\nimport ..Sibling\nusing .Sub: helper\n",
                &[],
                Some(&[]),
            ),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn an_interpolated_path_has_no_name_to_check() {
        assert_eq!(
            messages("import $A\n", &[], Some(&[])),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn quoted_code_and_macro_calls_are_skipped() {
        // Quoted code is data, and a macro receives the load unevaluated.
        assert_eq!(
            messages("ex = :(using Frobnicate)\n", &[], Some(&[])),
            Vec::<String>::new(),
        );
        assert_eq!(
            messages("quote\n    using Frobnicate\nend\n", &[], Some(&[])),
            Vec::<String>::new(),
        );
        assert_eq!(
            messages("@testset using Frobnicate\n", &[], Some(&[])),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn without_a_declared_dependency_set_the_file_is_silent() {
        // The gate the driver applies: a script or a `test/` file loads against
        // another environment, so nothing here is answerable.
        assert_eq!(
            messages("using Frobnicate\n", &["Base"], None),
            Vec::<String>::new(),
        );
    }

    #[test]
    fn no_resolution_context_is_silent() {
        let parsed = crate::parser::parse("using Frobnicate\n");
        let model = SemanticModel::build(&parsed.cst);
        let ctx = RuleContext::new(None, &parsed.cst, &model);
        let mut sink = Vec::new();
        UnresolvedImport.check_file(&ctx, &mut sink);
        assert!(sink.is_empty());
    }
}
