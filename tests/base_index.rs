//! Filesystem-level tests for the Base/Core/stdlib system index: harvesting a
//! miniature Julia installation and the no-install fallback.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fatou::environment::{self, EnvContext};
use fatou::index::{PackageIndex, build_system_index};
use fatou::linter::Diagnostic;

/// A unique temp directory removed on drop (mirrors `tests/environment.rs`).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fatou-base-{}-{}", std::process::id(), n));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// A miniature `<prefix>/share/julia` tree: Base entering at `base/Base.jl` and
/// pulling `exports.jl`, Core at `base/boot.jl`, and one stdlib package `Foo`.
fn make_install(prefix: &Path) {
    let julia = prefix.join("share/julia");
    write(
        &julia.join("base/Base.jl"),
        "baremodule Base\ninclude(\"exports.jl\")\nmap(f, x) = x\nend\n",
    );
    write(&julia.join("base/exports.jl"), "export map, println\n");
    write(&julia.join("base/boot.jl"), "export Any, Int, Type\n");
    write(
        &julia.join("stdlib/v1.11/Foo/src/Foo.jl"),
        "module Foo\nexport foo\nfoo() = 1\nend\n",
    );
    write(&prefix.join("bin/julia"), "#!/bin/sh\n");
}

fn install_at(prefix: &Path) -> environment::JuliaInstall {
    let ctx = EnvContext {
        workspace_root: prefix.to_path_buf(),
        julia_project: None,
        julia_depot_path: None,
        home: None,
        julia_bindir: Some(prefix.join("bin").to_string_lossy().into_owned()),
        path: None,
    };
    environment::locate_install(&ctx, &[]).expect("install")
}

fn export_names(index: &fatou::index::PackageIndex) -> Vec<&str> {
    index.root.exports.iter().map(|e| e.name.as_str()).collect()
}

fn lint_call_arity(system: &BTreeMap<String, Arc<PackageIndex>>, source: &str) -> Vec<Diagnostic> {
    use fatou::config::LintConfig;
    use fatou::linter::rules::ResolutionContext;
    use fatou::linter::{ResolvedRules, lint_parsed};
    use fatou::parser::parse;
    use fatou::semantic::SemanticModel;

    let config = LintConfig {
        select: Some(vec!["call-arity".into()]),
        ..Default::default()
    };
    let (rules, warnings) = ResolvedRules::resolve(&config);
    assert!(warnings.is_empty());
    let parsed = parse(source);
    assert!(parsed.diagnostics.is_empty());
    let model = SemanticModel::build(&parsed.cst);
    lint_parsed(
        None,
        &parsed.cst,
        &model,
        &rules,
        Some(ResolutionContext {
            packages: system,
            workspace: None,
            declared_deps: None,
        }),
        &[],
    )
}

#[test]
fn harvests_base_core_and_stdlib() {
    let tmp = TempDir::new();
    let prefix = tmp.path().join("julia");
    make_install(&prefix);
    let install = install_at(&prefix);

    let system = build_system_index(Some(&install));

    let base = system.get("Base").expect("Base index");
    assert!(export_names(base).contains(&"map"));
    assert!(export_names(base).contains(&"println"));
    // The harvested method group is present too, not just the export name.
    assert!(base.root.functions.iter().any(|g| g.name == "map"));

    let core = system.get("Core").expect("Core index");
    assert!(export_names(core).contains(&"Int"));
    assert!(export_names(core).contains(&"Type"));

    let foo = system.get("Foo").expect("Foo stdlib index");
    assert!(export_names(foo).contains(&"foo"));
    assert!(foo.root.functions.iter().any(|g| g.name == "foo"));
}

#[test]
fn call_arity_accepts_generated_base_varargs() {
    let tmp = TempDir::new();
    let prefix = tmp.path().join("julia");
    make_install(&prefix);
    let base = prefix.join("share/julia/base");
    write(
        &base.join("Base.jl"),
        "baremodule Base\nexport view, min, max, sqrt, zeros\ninclude(\"operators.jl\")\nend\n",
    );
    write(
        &base.join("operators.jl"),
        r#"view(A::AbstractArray, I::Vararg{Any,N}) where {N} = A
min(x) = x
min(x, y) = x
max(x) = x
max(x, y) = y
sqrt(x) = x
for op in (:+, :*, :&, :|, :xor, :min, :max, :kron)
    @eval begin
        ($op)(a, b, c, xs...) = (@inline; afoldl($op, ($op)(($op)(a,b),c), xs...))
    end
end
"#,
    );
    let system = build_system_index(Some(&install_at(&prefix)));
    assert!(system.values().all(|pkg| pkg.diagnostics.is_empty()));
    let diagnostics = lint_call_arity(
        &system,
        "A = zeros(3, 3)\nview(A, 1:2, 1:2)\nview(A, :, 1)\nmax(1, 2, 3)\nmax(1, 2, 3, 4)\nmin(1, 2, 3)\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let diagnostics = lint_call_arity(
        &system,
        "max()\nmin()\nsqrt(1, 2)\nmax(1, 2, 3; unknown=true)\n",
    );
    // Generated groups may have unharvested overloads, so neither their arity
    // nor their keyword set is exhaustive. Ordinary functions still report.
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0].message.body.contains("`sqrt`"));
}

#[test]
fn call_arity_accepts_base_split_keywords() {
    let tmp = TempDir::new();
    let prefix = tmp.path().join("julia");
    make_install(&prefix);
    let base = prefix.join("share/julia/base");
    write(
        &base.join("Base.jl"),
        "baremodule Base\nexport split\ninclude(\"multidimensional.jl\")\ninclude(\"strings/strings.jl\")\ninclude(\"docs/Docs.jl\")\nend\n",
    );
    write(
        &base.join("multidimensional.jl"),
        "module IteratorsMD\nsplit(t, V::Val) = t\nend\n",
    );
    write(
        &base.join("strings/strings.jl"),
        "include(\"strings/util.jl\")\n",
    );
    write(
        &base.join("strings/util.jl"),
        r#"function split(str::T, splitter;
               limit::Integer=0, keepempty::Bool=true) where {T<:AbstractString}
    collect(eachsplit(str, splitter; limit, keepempty))
end
split(str::AbstractString; limit::Integer=0, keepempty::Bool=false) =
    split(str, isspace; limit, keepempty)
"#,
    );
    // Base's own includes use the bootstrap root, while a submodule's include
    // retains normal Julia file-relative resolution.
    write(
        &base.join("docs/Docs.jl"),
        "module Docs\ninclude(\"bindings.jl\")\nend\n",
    );
    write(&base.join("docs/bindings.jl"), "binding() = nothing\n");
    write(&base.join("bindings.jl"), "wrong_root() = nothing\n");

    let system = build_system_index(Some(&install_at(&prefix)));
    let diagnostics = lint_call_arity(
        &system,
        "split(\"a b\", ' '; keepempty=false)\nsplit(\"a=b\", '='; limit=2)\nsplit(\"a b\"; keepempty=false, limit=2)\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let base = &system["Base"];
    assert!(base.diagnostics.is_empty(), "{:?}", base.diagnostics);
    let split = base
        .root
        .functions
        .iter()
        .find(|g| g.name == "split")
        .unwrap();
    assert_eq!(split.methods.len(), 2);
    assert_eq!(split.methods[0].loc.file, Path::new("strings/util.jl"));
    let docs = base
        .root
        .submodules
        .iter()
        .find(|m| m.name == "Docs")
        .unwrap();
    assert_eq!(docs.functions[0].name, "binding");
    assert_eq!(docs.functions.len(), 1);

    let diagnostics = lint_call_arity(
        &system,
        "split(\"a b\", ' '; unknown=true)\nsplit(\"a b\", ' ', 2)\n",
    );
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
}

#[test]
fn call_arity_accepts_generated_base_search_methods() {
    let tmp = TempDir::new();
    let prefix = tmp.path().join("julia");
    make_install(&prefix);
    let base = prefix.join("share/julia/base");
    write(
        &base.join("Base.jl"),
        "baremodule Base\nexport searchsortedfirst, searchsortedlast, searchsorted\ninclude(\"sort.jl\")\nend\n",
    );
    write(
        &base.join("sort.jl"),
        r#"module Sort
searchsortedfirst(v, x, lo, hi, o) = lo
searchsortedlast(v, x, lo, hi, o) = hi
searchsorted(v, x, lo, hi, o) = lo:hi
for s in [:searchsortedfirst, :searchsortedlast, :searchsorted]
    @eval begin
        $s(v::AbstractVector, x, o::Ordering) = $s(v,x,firstindex(v),lastindex(v),o)
        $s(v::AbstractVector, x;
           lt=isless, by=identity, rev::Union{Bool,Nothing}=nothing, order::Ordering=Forward) =
            $s(v,x,ord(lt,by,rev,order))
    end
end
end
"#,
    );
    let system = build_system_index(Some(&install_at(&prefix)));
    assert!(system.values().all(|pkg| pkg.diagnostics.is_empty()));
    let diagnostics = lint_call_arity(
        &system,
        "searchsortedfirst([1, 3, 5], 3)\nsearchsortedlast([1, 3, 5], 3; rev=false)\nsearchsorted([1, 3, 5], 3; by=identity, lt=isless)\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    for group in &system["Base"].root.submodules[0].functions {
        assert_eq!(group.methods.len(), 3, "{}", group.name);
        assert!(group.methods[1..].iter().all(|method| method.is_generated));
        assert_eq!(group.methods[2].params.len(), 2);
        assert_eq!(group.methods[2].keyword_params.len(), 4);
    }
}

/// Manual end-to-end check against a real local Julia install. Ignored in CI;
/// run with `cargo test --test base_index -- --ignored`.
#[test]
#[ignore = "requires a real Julia installation on PATH"]
fn harvests_real_install() {
    let ctx = EnvContext::from_process(std::env::temp_dir());
    let install = environment::locate_install(&ctx, &[])
        .expect("a Julia installation locatable via JULIA_BINDIR, juliaup, or PATH");

    let system = build_system_index(Some(&install));
    let base = system.get("Base").expect("Base index");
    for name in ["println", "map", "push!"] {
        assert!(
            export_names(base).contains(&name),
            "Base should export {name}"
        );
    }
    let core = system.get("Core").expect("Core index");
    assert!(export_names(core).contains(&"Type"));
    // A couple of well-known stdlibs should have been harvested.
    assert!(system.contains_key("LinearAlgebra"));
    assert!(system.contains_key("Random"));

    let diagnostics = lint_call_arity(
        &system,
        "split(\"a b\", ' '; keepempty=false)\nsplit(\"a=b\", '='; limit=2)\nsearchsortedfirst([1, 3, 5], 3)\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn fallback_when_no_install() {
    let system = build_system_index(None);
    let base = system.get("Base").expect("Base index");
    let core = system.get("Core").expect("Core index");

    assert!(export_names(base).contains(&"println"));
    assert!(export_names(core).contains(&"Int"));
    // The fallback carries names only, no harvested method groups.
    assert!(base.root.functions.is_empty());
    // No stdlib packages without an installation.
    assert!(!system.contains_key("Foo"));
}
