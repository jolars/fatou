//! `non-public-access`: reading `Foo.bar` where `Foo` neither `export`s `bar`
//! nor declares it `public`.
//!
//! Julia gives a module two ways to say "this name is API": `export`, which
//! also attaches the name to a `using`, and — since 1.11 — `public`, which
//! declares intent without attaching anything. Everything else is internal:
//! free to be renamed, re-typed, or deleted in a patch release. Reaching for it
//! through a qualified path is legal, silent, and the classic way to acquire a
//! dependency on something nobody promised to keep.
//!
//! This is the Julia analogue of arity's `internal-function` (`pkg:::fn`), and
//! a better-defined question here than in R: `public` makes "intended API" an
//! explicit declaration rather than a convention.
//!
//! The rule answers only what the harvested index can support, so three gates
//! keep it honest:
//!
//! - **The qualifier must really name a module.** The root is taken to be a
//!   module only when this file's own whole-module `using`/`import` binds it,
//!   when Base/Core provide it, or when no tier binds it at all and a package
//!   of that name exists. A local, a parameter, a global, or a name an item
//!   list bound (`import Foo: Bar`) is a value, and `x.field` is a field
//!   access, not a module access.
//! - **The module must declare a public API the harvest can read.** A module
//!   with no `export`/`public` statement has not opted into the distinction —
//!   its whole API is reached qualified by design, as `JSON3.read` is — and one
//!   that re-exports with `Reexport.jl` has drawn the line inside a macro call
//!   the harvest does not follow (`@reexport using ColorTypes` is why
//!   `Colors.alpha` is API). Neither is reportable.
//! - **The package under development is exempt.** A file of the workspace
//!   package may use its own internals; that is what internals are for.
//!
//! A Base/Core export is exempt through *any* module, since every module but a
//! `baremodule` implicitly does `using Base`: `Threads.ReentrantLock` reaches
//! Base's type, not an internal of `Threads`.
//!
//! What the rule claims is "not declared public", not "does not exist": a
//! member the harvest never saw is reported the same way, since a name no
//! module declares is not API either way. Bare names are `undefined-name`'s
//! question, not this rule's.
//!
//! Definition sites are not reads and stay out: `Base.show(io, x) = …` extends
//! a function (whether it *should* is `type-piracy`'s question) and
//! `Foo.bar = 1` writes rather than reads. So does anything inside a quote or a
//! macro call, the shared [`FileScan`](super::super::file_scan::FileScan)
//! exemption every resolution-dependent rule takes.
//!
//! The rule reports what a module *declared*, which is all a static reading can
//! know. A package whose documented interface is qualified and unexported —
//! `Tables.getcolumn`, `Parsers.xparse` — is reported all the same, because
//! nothing in its source separates that interface from its internals; `public`
//! (1.11) is exactly the statement that would. That, plus an `export` list a
//! module generates with `@eval`, is why the rule is off by default. There is
//! no fix: the caller cannot make someone else's name public.

use smol_str::SmolStr;

use crate::index::ModuleIndex;
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::matchers::in_signature_position;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::resolve::{Namespace, PackageSource, Resolution, resolve_submodule};
use crate::semantic::{BindingKind, QualifiedRead, SemanticModel};
use crate::syntax::{SyntaxKind, SyntaxNode};

pub struct NonPublicAccess;

/// The names every module binds without saying so: Julia gives each one its own
/// `eval` and `include`. Neither can appear in an export list, and both are API
/// by construction — `Base.eval(M, ex)` reaches exactly what it looks like.
const MODULE_IMPLICIT: &[&str] = &["eval", "include"];

impl Rule for NonPublicAccess {
    fn id(&self) -> &'static str {
        "non-public-access"
    }

    fn default_enabled(&self) -> bool {
        // Meaningful only against a harvested library: the rule has to read the
        // target module's `export`/`public` statements. The language server
        // turns it on for workspace member files; on the CLI `--select` both
        // enables it and triggers the project harvest.
        false
    }

    fn description(&self) -> &'static str {
        "Flag a qualified read of a name the target module neither `export`s \
         nor declares `public`. Those two statements are how a Julia module \
         says what its API is — `public` (1.11) declares intent without \
         attaching the name to a `using` — so everything else is internal and \
         free to change in a patch release. Only a qualifier that really names \
         a module is checked: a local, a parameter, a name an item list bound \
         (`import Foo: Bar`), and any plain field access are values, not \
         modules. Two kinds of module are left alone: one that declares no \
         public API at all, whose whole surface is reached qualified by design, \
         and one that re-exports with `Reexport.jl`, whose `@reexport using \
         Bar` is a macro call the index cannot follow. So is the package under \
         development, whose own internals its files may use. A Base/Core \
         export reaches through any module, since every module but a \
         `baremodule` implicitly does `using Base` — `Threads.ReentrantLock` \
         is Base's type — and so do the `eval` and `include` every module \
         binds for itself. Definition sites are not reads: `Base.show(io, x) = \
         …` extends a function and `Foo.bar = 1` writes one, and code inside a \
         quote or a macro call is exempt as everywhere else. Off by default: \
         the rule needs the harvested library that only project context \
         provides, and it reports what a module *declared*, so a package whose \
         documented interface is qualified and unexported is reported too. No \
         fix — the caller cannot make someone else's name public."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "`summarysize` is part of Base's public API; \
                          `unwrap_unionall` is not:",
                source: "Base.summarysize(x)\nBase.unwrap_unionall(T)\n",
            },
            Example {
                caption: "Macros are checked in their own namespace:",
                source: "Base.@_inline_meta\n",
            },
        ]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        // The shared soundness floor: without it, neither what the qualifier
        // names nor what the file binds is knowable.
        if !ctx.trusts_resolution() {
            return;
        }
        for read in ctx.model.qualified_reads() {
            if let Some(diagnostic) = self.finding(ctx, read) {
                sink.push(diagnostic);
            }
        }
    }
}

impl NonPublicAccess {
    /// The finding for one qualified read, if it names a member the target
    /// module declares neither `export`ed nor `public`.
    fn finding(&self, ctx: &RuleContext<'_>, read: &QualifiedRead) -> Option<Diagnostic> {
        let (member, qualifier) = read.path.split_last()?;
        if qualifier.is_empty() || MODULE_IMPLICIT.contains(&member.as_str()) {
            return None;
        }
        // Quoted code is data, and a macro may rewrite the value expressions it
        // received — but a qualified macro read *is* the macro call's own name,
        // and reaches the module exactly as written.
        let scan = ctx.file_scan();
        if scan.in_quote(read.range) || (!read.is_macro && scan.in_macro_call(read.range)) {
            return None;
        }
        // A definition or an assignment target is not a read.
        if is_definition_target(ctx.root, read) {
            return None;
        }

        let resolution = ctx.resolution.as_ref()?;
        let path = module_path(ctx, read, qualifier)?;
        let head = path.first()?;
        // A file of the package under development may use its own internals.
        if resolution
            .workspace
            .as_ref()
            .is_some_and(|(pkg, _)| pkg.name == head.as_str())
        {
            return None;
        }
        let package = resolution.packages.package(head)?;
        let rest: Vec<&str> = path[1..].iter().map(SmolStr::as_str).collect();
        let module = resolve_submodule(&package.root, &rest)?;

        // A module that declares nothing public has not drawn the line this
        // rule reports on, and one that re-exports through `Reexport.jl` has
        // drawn it somewhere the harvest cannot see.
        if !declares_public_api(module) || reexports_through_a_macro(module) {
            return None;
        }
        if module.exports.iter().any(|e| e.name == member.as_str()) {
            return None;
        }
        // Every module but a `baremodule` implicitly does `using Base`, so a
        // Base/Core export is reachable through it (`Threads.ReentrantLock`)
        // without being an internal of its own.
        if !module.bare && system_exports(resolution.packages, member) {
            return None;
        }

        let written = qualifier
            .iter()
            .map(SmolStr::as_str)
            .collect::<Vec<_>>()
            .join(".");
        Some(Diagnostic::new(
            self.id(),
            read.range,
            format!("`{written}` does not export `{member}` or declare it `public`"),
        ))
    }
}

/// The module path, relative to a library package's root, that `qualifier`
/// names — `["Base", "Threads"]` for `Threads` under `using Base.Threads` — or
/// `None` when the qualifier is not a module whose API this rule can read.
fn module_path(
    ctx: &RuleContext<'_>,
    read: &QualifiedRead,
    qualifier: &[SmolStr],
) -> Option<Vec<SmolStr>> {
    let root = &qualifier[0];
    let resolver = ctx.resolver()?;
    let mut path = match resolver.resolve(root, read.range.start(), Namespace::Value) {
        // A binding names a module only when a whole-module `using`/`import`
        // in this file made it one; everything else here is a value.
        Resolution::Binding(id) => {
            if ctx.model.binding(id).kind != BindingKind::Import {
                return None;
            }
            loaded_module_path(ctx.model, root)?
        }
        // A name the package under development provides — including its own
        // module name, the `MyPkg.internal()` self-access shape.
        Resolution::Workspace { .. } => return None,
        // Base/Core themselves, or a submodule they export (`Sys`, `Iterators`).
        Resolution::System { module, .. } if module != *root => vec![module, root.clone()],
        // A sibling file's import, a `using`'d name, or a name no tier binds:
        // the root can only mean the package of that name, if one exists.
        _ => vec![root.clone()],
    };
    path.extend(qualifier[1..].iter().cloned());
    Some(path)
}

/// The module path a whole-module `using`/`import` in this file binds to
/// `name`: `["Foo"]` for `F` under `import Foo as F`, `["Base", "Threads"]` for
/// `Threads` under `using Base.Threads`.
///
/// `None` when no such clause exists — an item list (`import Foo: Bar`) binds
/// names, not modules — or when the path is relative (`using .Sub`) or
/// interpolated, neither of which names a library package.
fn loaded_module_path(model: &SemanticModel, name: &SmolStr) -> Option<Vec<SmolStr>> {
    model.module_loads().iter().find_map(|load| {
        if load.items.is_some() || load.path.leading_dots != 0 {
            return None;
        }
        let bound = load
            .alias
            .as_ref()
            .or_else(|| load.path.components.last())?;
        (bound == name).then(|| load.path.components.clone())
    })
}

/// Whether `module` declares a public API at all: some `export`ed or `public`
/// name other than its own, which the harvest adds implicitly (Julia binds a
/// module's name inside itself without any statement saying so).
fn declares_public_api(module: &ModuleIndex) -> bool {
    module.exports.iter().any(|e| e.name != module.name)
}

/// Whether `module` extends its export surface through `Reexport.jl`, whose
/// `@reexport using Bar` is a macro call the harvest does not follow — so the
/// `export` statements it did see are only part of the story, and nothing about
/// the module is reportable. Loading the package at all is the signal; a module
/// that imports `Reexport` and never uses it costs only a false negative.
fn reexports_through_a_macro(module: &ModuleIndex) -> bool {
    let named_reexport = |name: &str| matches!(name, "Reexport" | "@reexport");
    module
        .imported_names
        .iter()
        .any(|name| named_reexport(name))
        || module
            .usings
            .iter()
            .any(|using| using.components.last().is_some_and(|c| named_reexport(c)))
}

/// Whether Base or Core `export`s (or declares `public`) `member`. Every module
/// but a `baremodule` implicitly does `using Base`, so such a name is reachable
/// through *any* module without being that module's own internal.
fn system_exports(packages: &dyn PackageSource, member: &str) -> bool {
    ["Base", "Core"].iter().any(|name| {
        packages
            .package(name)
            .is_some_and(|pkg| pkg.root.exports.iter().any(|e| e.name == member))
    })
}

/// Whether the chain `read` covers is a definition target rather than a read:
/// the callee of a definition's signature (`Base.show(io, x) = …`,
/// `function Base.show(io, x)`) or the left-hand side of an assignment
/// (`Foo.bar = 1`).
fn is_definition_target(root: &SyntaxNode, read: &QualifiedRead) -> bool {
    let Some(node) = root.covering_element(read.range).into_node() else {
        return false;
    };
    let Some(parent) = node.parent() else {
        return false;
    };
    let is_first = parent.children().next().is_some_and(|first| first == node);
    match parent.kind() {
        SyntaxKind::ASSIGNMENT_EXPR => is_first,
        SyntaxKind::CALL_EXPR => is_first && in_signature_position(&parent),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::harvest_tree;
    use crate::index::model::{ModuleIndex, PackageIndex};
    use crate::linter::rules::ResolutionContext;
    use crate::semantic::SemanticModel;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    type Library = BTreeMap<String, Arc<PackageIndex>>;

    /// A package named `name` harvested from `src`, exactly as the real harvest
    /// reads a package's own sources — `export`/`public` statements included.
    fn pkg(name: &str, src: &str) -> Arc<PackageIndex> {
        let parsed = crate::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "fixture must parse clean");
        let mut root = ModuleIndex {
            name: name.to_string(),
            ..harvest_tree(&parsed.cst)
        };
        // The real harvest binds each module's own name; the tree harvest of a
        // bare source does not, so mirror it here.
        root.add_self_name_exports();
        Arc::new(PackageIndex {
            name: name.to_string(),
            root,
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        })
    }

    /// A library holding each `(name, source)` package, plus a Base that
    /// exports one name (so `declares_public_api` holds for it).
    fn library(packages: &[(&str, &str)]) -> Library {
        let mut lib = Library::from([
            ("Base".to_string(), pkg("Base", "export length\n")),
            ("Core".to_string(), pkg("Core", "")),
        ]);
        for (name, src) in packages {
            lib.insert((*name).to_string(), pkg(name, src));
        }
        lib
    }

    /// The package the fixtures reach into: `make` is exported, `peek` is
    /// declared `public`, and `tweak` is neither.
    const FOO: &str = "export make\npublic peek\nmake() = 1\npeek() = 2\ntweak() = 3\n";

    /// Run the rule alone over `src` against `lib`, optionally as a source file
    /// of the workspace package `workspace`.
    fn findings_in(src: &str, lib: &Library, workspace: Option<Arc<PackageIndex>>) -> Vec<String> {
        let parsed = crate::parser::parse(src);
        assert!(parsed.diagnostics.is_empty(), "fixture must parse clean");
        let model = SemanticModel::build(&parsed.cst);
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages: lib,
                workspace: workspace.map(|pkg| (pkg, Vec::new())),
                declared_deps: None,
            }));
        let mut sink = Vec::new();
        NonPublicAccess.check_file(&ctx, &mut sink);
        sink.into_iter().map(|d| d.message.body).collect()
    }

    /// The findings for `src` against a library holding `Foo` (plus any extra
    /// packages), linted as a loose file.
    fn findings(src: &str, extra: &[(&str, &str)]) -> Vec<String> {
        let mut packages = vec![("Foo", FOO)];
        packages.extend_from_slice(extra);
        findings_in(src, &library(&packages), None)
    }

    fn count(src: &str) -> usize {
        findings(src, &[]).len()
    }

    fn flagged(qualifier: &str, member: &str) -> String {
        format!("`{qualifier}` does not export `{member}` or declare it `public`")
    }

    #[test]
    fn an_internal_member_is_flagged() {
        assert_eq!(
            findings("using Foo\nFoo.tweak()\n", &[]),
            vec![flagged("Foo", "tweak")]
        );
    }

    #[test]
    fn exported_and_public_members_are_silent() {
        // `export` and `public` both declare API; only the third call is one.
        assert_eq!(
            findings("using Foo\nFoo.make()\nFoo.peek()\nFoo.tweak()\n", &[]),
            vec![flagged("Foo", "tweak")]
        );
    }

    #[test]
    fn a_member_read_as_a_value_is_flagged_too() {
        // Not just calls: any read of the qualified name reaches the internal.
        assert_eq!(
            findings("using Foo\nf = Foo.tweak\n", &[]),
            vec![flagged("Foo", "tweak")]
        );
    }

    #[test]
    fn a_module_that_declares_nothing_public_is_left_alone() {
        // A package whose whole API is qualified by design (`JSON3.read`) never
        // drew the line this rule reports on.
        let quiet = [("Quiet", "read(x) = 1\nwrite(x) = 2\n")];
        assert_eq!(
            findings("using Quiet\nQuiet.read(x)\n", &quiet),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_module_that_re_exports_through_a_macro_is_left_alone() {
        // `@reexport using ColorTypes` is a macro call the harvest does not
        // follow, so the `export` statements it saw are only part of the story.
        let re = [(
            "Re",
            "using Reexport\n@reexport using Other\nexport own\nown() = 1\n",
        )];
        assert_eq!(
            findings("using Re\nRe.alpha(x)\n", &re),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_base_name_reached_through_another_module_is_exempt() {
        // Every module but a `baremodule` implicitly does `using Base`, so
        // `Foo.length` is Base's function, not an internal of `Foo`.
        assert_eq!(count("using Foo\nFoo.length(x)\n"), 0);
    }

    #[test]
    fn the_names_every_module_binds_implicitly_are_exempt() {
        // Julia gives each module its own `eval` and `include`; no export list
        // can name them.
        assert_eq!(count("using Foo\nFoo.eval(ex)\n"), 0);
        assert_eq!(count("using Foo\nFoo.include(\"x.jl\")\n"), 0);
    }

    #[test]
    fn macros_are_checked_in_their_own_namespace() {
        let pkgs = [(
            "Mac",
            "export @shout\nmacro shout(x)\n    x\nend\nmacro whisper(x)\n    x\nend\n",
        )];
        assert_eq!(
            findings("using Mac\nMac.@shout 1\nMac.@whisper 1\n", &pkgs),
            vec![flagged("Mac", "@whisper")]
        );
    }

    #[test]
    fn a_submodule_path_resolves() {
        let pkgs = [(
            "Nest",
            "export Sub\nmodule Sub\nexport ok\nok() = 1\nhidden() = 2\nend\n",
        )];
        assert_eq!(
            findings("using Nest\nNest.Sub.ok()\nNest.Sub.hidden()\n", &pkgs),
            vec![flagged("Nest.Sub", "hidden")]
        );
    }

    #[test]
    fn an_alias_and_a_dotted_load_resolve_to_the_module() {
        assert_eq!(
            findings("import Foo as F\nF.tweak()\n", &[]),
            vec![flagged("F", "tweak")]
        );
        let pkgs = [(
            "Outer",
            "module Inner\nexport ok\nok() = 1\nsecret() = 2\nend\n",
        )];
        assert_eq!(
            findings("using Outer.Inner\nInner.secret()\n", &pkgs),
            vec![flagged("Inner", "secret")]
        );
    }

    #[test]
    fn base_and_core_resolve_without_a_load() {
        // The implicit tier: `Base` needs no `using` to name the module.
        assert_eq!(
            findings_in("Base.internal_thing()\n", &library(&[]), None),
            vec![flagged("Base", "internal_thing")]
        );
        assert_eq!(
            findings_in("Base.length(x)\n", &library(&[]), None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_definition_or_an_assignment_target_is_not_a_read() {
        // Extending a function is `type-piracy`'s question, and a write is not
        // a read.
        assert_eq!(count("using Foo\nFoo.tweak(x) = 1\n"), 0);
        assert_eq!(count("using Foo\nfunction Foo.tweak(x)\n    1\nend\n"), 0);
        assert_eq!(count("using Foo\nFoo.tweak = 1\n"), 0);
        // The body of such a definition is still ordinary code.
        assert_eq!(
            findings("using Foo\nFoo.make(x) = Foo.tweak(x)\n", &[]),
            vec![flagged("Foo", "tweak")]
        );
    }

    #[test]
    fn a_field_access_on_a_value_is_not_a_module_access() {
        // A local, a global, and a parameter are values whatever they are named.
        assert_eq!(count("Foo = load()\nFoo.tweak\n"), 0);
        assert_eq!(count("g(Foo) = Foo.tweak\n"), 0);
        assert_eq!(
            count("function g()\n    Foo = load()\n    Foo.tweak\nend\n"),
            0
        );
    }

    #[test]
    fn an_item_list_binds_names_not_modules() {
        // `import Foo: Bar` binds the *member* `Bar`; `Bar.x` is a field access
        // even if a package happens to be named `Bar`.
        let pkgs = [("Bar", "export ok\nok() = 1\nhidden() = 2\n")];
        assert_eq!(
            findings("import Foo: Bar\nBar.hidden\n", &pkgs),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_relative_load_names_a_module_inside_the_package() {
        // `using .Foo` is the package's own submodule, not the library package
        // of that name.
        assert_eq!(count("using .Foo\nFoo.tweak()\n"), 0);
    }

    #[test]
    fn an_unharvested_module_is_left_alone() {
        assert_eq!(count("import Mystery\nMystery.thing()\n"), 0);
    }

    #[test]
    fn the_package_under_development_may_use_its_own_internals() {
        let ws = pkg("Foo", FOO);
        assert_eq!(
            findings_in("Foo.tweak()\n", &library(&[("Foo", FOO)]), Some(ws)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn quoted_code_and_macro_calls_are_skipped() {
        assert_eq!(count("using Foo\nex = :(Foo.tweak())\n"), 0);
        assert_eq!(count("using Foo\nquote\n    Foo.tweak()\nend\n"), 0);
        assert_eq!(count("using Foo\n@assert Foo.tweak()\n"), 0);
    }

    #[test]
    fn eval_and_an_unfollowable_include_bail_the_file() {
        // The shared soundness floor: either can splice in a `Foo` of its own.
        assert_eq!(count("using Foo\neval(ex)\nFoo.tweak()\n"), 0);
        assert_eq!(count("using Foo\ninclude(\"other.jl\")\nFoo.tweak()\n"), 0);
    }

    #[test]
    fn the_span_covers_the_whole_qualified_chain() {
        let src = "using Foo\nx = Foo.tweak(1)\n";
        let parsed = crate::parser::parse(src);
        let model = SemanticModel::build(&parsed.cst);
        let lib = library(&[("Foo", FOO)]);
        let ctx =
            RuleContext::new(None, &parsed.cst, &model).with_resolution(Some(ResolutionContext {
                packages: &lib,
                workspace: None,
                declared_deps: None,
            }));
        let mut sink = Vec::new();
        NonPublicAccess.check_file(&ctx, &mut sink);
        assert_eq!(sink.len(), 1);
        assert_eq!(&src[sink[0].range], "Foo.tweak");
    }

    #[test]
    fn no_resolution_context_is_silent() {
        let parsed = crate::parser::parse("using Foo\nFoo.tweak()\n");
        let model = SemanticModel::build(&parsed.cst);
        let ctx = RuleContext::new(None, &parsed.cst, &model);
        let mut sink = Vec::new();
        NonPublicAccess.check_file(&ctx, &mut sink);
        assert!(sink.is_empty());
    }
}
