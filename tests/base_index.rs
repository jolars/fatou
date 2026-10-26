//! Filesystem-level tests for the Base/Core/stdlib system index: harvesting a
//! miniature Julia installation and the no-install fallback.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fatou::environment::{self, EnvContext};
use fatou::index::build_system_index;

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
    use fatou::config::LintConfig;
    use fatou::linter::rules::ResolutionContext;
    use fatou::linter::{ResolvedRules, lint_parsed};
    use fatou::parser::parse;
    use fatou::semantic::SemanticModel;

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
    let config = LintConfig {
        select: Some(vec!["call-arity".into()]),
        ..Default::default()
    };
    let (rules, warnings) = ResolvedRules::resolve(&config);
    assert!(warnings.is_empty());
    let lint = |source: &str| {
        let parsed = parse(source);
        assert!(parsed.diagnostics.is_empty());
        let model = SemanticModel::build(&parsed.cst);
        lint_parsed(
            None,
            &parsed.cst,
            &model,
            &rules,
            Some(ResolutionContext {
                packages: &system,
                workspace: None,
                declared_deps: None,
            }),
            &[],
        )
    };
    let diagnostics = lint(
        "A = zeros(3, 3)\nview(A, 1:2, 1:2)\nview(A, :, 1)\nmax(1, 2, 3)\nmax(1, 2, 3, 4)\nmin(1, 2, 3)\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let diagnostics = lint("max()\nmin()\nsqrt(1, 2)\nmax(1, 2, 3; unknown=true)\n");
    // Generated groups may have unharvested overloads, so neither their arity
    // nor their keyword set is exhaustive. Ordinary functions still report.
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(diagnostics[0].message.body.contains("`sqrt`"));
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
