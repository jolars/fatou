//! End-to-end: the CLI lint pipeline resolves cross-file names when handed a
//! harvested project via [`ProjectContext::Harvested`], so `undefined-name`
//! stops flagging a name a sibling file's `using` brings in module-wide — and
//! the project tier it derives per file, which decides whether
//! `unresolved-import` may speak at all.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fatou::config::LintConfig;
use fatou::file_discovery::ExcludeFilter;
use fatou::index::{HarvestedLibrary, build_system_index, harvest_package_named};
use fatou::linter::{ProjectContext, check_paths_with_config};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fatou-lint-ws-{}-{}", std::process::id(), n));
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

/// A workspace package `M` whose `a.jl` does `using SparseArrays` and whose
/// `b_contents` file reads a name from it, plus a harvested `SparseArrays`
/// dependency exporting `SparseMatrixCSC`.
fn project(b_contents: &str) -> (TempDir, TempDir, HarvestedLibrary) {
    let dep = TempDir::new();
    write(
        &dep.path().join("src/SparseArrays.jl"),
        "module SparseArrays\nexport SparseMatrixCSC\nstruct SparseMatrixCSC end\nend\n",
    );
    let dep_index = harvest_package_named(dep.path(), "SparseArrays");

    let pkg = TempDir::new();
    write(
        &pkg.path().join("src/M.jl"),
        "module M\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write(&pkg.path().join("src/a.jl"), "using SparseArrays\n");
    write(&pkg.path().join("src/b.jl"), b_contents);
    let pkg_index = harvest_package_named(pkg.path(), "M");

    let library = HarvestedLibrary {
        packages: BTreeMap::from([
            ("M".to_string(), Arc::new(pkg_index)),
            ("SparseArrays".to_string(), Arc::new(dep_index)),
        ]),
        roots: BTreeMap::from([("M".to_string(), pkg.path().to_path_buf())]),
        workspaces: vec!["M".to_string()],
        declared_deps: Default::default(),
        project_files: Default::default(),
        deps: Default::default(),
    };
    (dep, pkg, library)
}

/// `undefined-name` findings for `src/b.jl`, linted as a member of the project.
fn undefined_in_b(pkg: &TempDir, library: &HarvestedLibrary) -> Vec<String> {
    let config = LintConfig {
        select: Some(vec!["undefined-name".to_string()]),
        ..Default::default()
    };
    let result = check_paths_with_config(
        &[pkg.path().join("src/b.jl")],
        &config,
        &ExcludeFilter::none(),
        None,
        ProjectContext::Harvested(library),
    )
    .expect("lint succeeds");
    result
        .reports
        .into_iter()
        .flat_map(|r| r.diagnostics)
        .filter(|d| d.rule == "undefined-name")
        .map(|d| d.message.body)
        .collect()
}

#[test]
fn cli_resolves_a_sibling_files_using_export() {
    // `b.jl` reads `SparseMatrixCSC`, which sibling `a.jl` brings in with
    // `using SparseArrays`. Under the harvested project it resolves.
    let (_dep, pkg, library) = project("f(::SparseMatrixCSC) = 1\n");
    assert_eq!(undefined_in_b(&pkg, &library), Vec::<String>::new());
}

#[test]
fn cli_still_flags_a_genuine_typo() {
    // A name no sibling load provides is still reported.
    let (_dep, pkg, library) = project("f(::SparseMatrixCSX) = 1\n");
    let findings = undefined_in_b(&pkg, &library);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("SparseMatrixCSX"), "{findings:?}");
}

#[test]
fn generated_imported_constructors_remain_types_in_sibling_files() {
    let dep = TempDir::new();
    write(
        &dep.path().join("src/SparseArrays.jl"),
        "module SparseArrays\nexport SparseMatrixCSC\nstruct SparseMatrixCSC end\n\
         module Types\nexport NestedType\nstruct NestedType end\nend\nend\n",
    );
    let dep_index = Arc::new(harvest_package_named(dep.path(), "SparseArrays"));
    for (import, name) in [
        ("import Base.Int", "Int"),
        ("import Base: Int", "Int"),
        ("import Base: Int as ImportedInt", "ImportedInt"),
        ("", "Int"),
        ("using Base", "BigInt"),
        ("using SparseArrays", "SparseMatrixCSC"),
        ("using SparseArrays.Types", "NestedType"),
    ] {
        let pkg = TempDir::new();
        write(
            &pkg.path().join("src/M.jl"),
            "module M\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
        );
        write(
            &pkg.path().join("src/a.jl"),
            &format!(
                "{import}\nstruct MyType end\nhelper(x) = x\n\
                 for T in (:{name}, :helper)\n    @eval ($T)(x::MyType) = 1\nend\n"
            ),
        );
        write(
            &pkg.path().join("src/b.jl"),
            &format!("g(x::{name}) = x\nh(x::helper) = x\n"),
        );
        let index = harvest_package_named(pkg.path(), "M");
        assert!(index.diagnostics.is_empty(), "{:?}", index.diagnostics);
        let constructor = index
            .root
            .functions
            .iter()
            .find(|group| group.name == name)
            .expect("generated constructor is harvested");
        assert!(constructor.methods[0].is_generated);
        let mut packages = build_system_index(None);
        packages.insert("SparseArrays".into(), Arc::clone(&dep_index));
        packages.insert("M".into(), Arc::new(index));
        let library = HarvestedLibrary {
            packages,
            roots: BTreeMap::from([("M".into(), pkg.path().to_path_buf())]),
            workspaces: vec!["M".into()],
            ..Default::default()
        };
        let config = LintConfig {
            select: Some(vec!["invalid-type-declaration".into()]),
            ..Default::default()
        };
        let result = check_paths_with_config(
            &[pkg.path().join("src/b.jl")],
            &config,
            &ExcludeFilter::none(),
            None,
            ProjectContext::Harvested(&library),
        )
        .expect("lint succeeds");
        let diagnostics = &result.reports[0].diagnostics;
        assert_eq!(diagnostics.len(), 1, "{import}: {diagnostics:?}");
        assert_eq!(diagnostics[0].rule, "invalid-type-declaration");
        assert!(diagnostics[0].message.body.contains("`helper`"));
    }
}

#[test]
fn generated_only_groups_do_not_establish_non_type_identity() {
    let pkg = TempDir::new();
    write(
        &pkg.path().join("src/M.jl"),
        "module M\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write(
        &pkg.path().join("src/a.jl"),
        "begin\nstruct WrappedType end\nend\nhelper(x) = x\n\
         for op in (:WrappedType, :helper, :generated_only)\n\
             @eval ($op)(x::Int) = x\nend\n",
    );
    write(
        &pkg.path().join("src/b.jl"),
        "f(x::WrappedType) = x\ng(x::helper) = x\nh(x::generated_only) = x\n",
    );
    let index = harvest_package_named(pkg.path(), "M");
    assert!(index.diagnostics.is_empty(), "{:?}", index.diagnostics);
    let constructor = index
        .root
        .functions
        .iter()
        .find(|group| group.name == "WrappedType")
        .expect("generated constructor is harvested");
    assert!(constructor.methods.iter().all(|method| method.is_generated));
    let library = HarvestedLibrary {
        packages: BTreeMap::from([("M".into(), Arc::new(index))]),
        roots: BTreeMap::from([("M".into(), pkg.path().to_path_buf())]),
        workspaces: vec!["M".into()],
        ..Default::default()
    };
    let config = LintConfig {
        select: Some(vec!["invalid-type-declaration".into()]),
        ..Default::default()
    };
    let result = check_paths_with_config(
        &[pkg.path().join("src/b.jl")],
        &config,
        &ExcludeFilter::none(),
        None,
        ProjectContext::Harvested(&library),
    )
    .expect("lint succeeds");
    let diagnostics = &result.reports[0].diagnostics;
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].rule, "invalid-type-declaration");
    assert!(diagnostics[0].message.body.contains("`helper`"));
}

/// A workspace package `M` declaring `SparseArrays` in its `Project.toml`
/// `[deps]`, with a `src/b.jl` member and a `test/runtests.jl` non-member.
fn project_with_deps() -> (TempDir, HarvestedLibrary) {
    let pkg = TempDir::new();
    write(
        &pkg.path().join("src/M.jl"),
        "module M\ninclude(\"b.jl\")\nend\n",
    );
    write(
        &pkg.path().join("src/b.jl"),
        "using SparseArrays\nusing Frobnicate\n",
    );
    write(
        &pkg.path().join("test/runtests.jl"),
        "using Test\nusing Frobnicate\n",
    );
    let pkg_index = harvest_package_named(pkg.path(), "M");

    let library = HarvestedLibrary {
        packages: BTreeMap::from([("M".to_string(), Arc::new(pkg_index))]),
        roots: BTreeMap::from([("M".to_string(), pkg.path().to_path_buf())]),
        workspaces: vec!["M".to_string()],
        declared_deps: BTreeMap::from([(
            "M".to_string(),
            Arc::new(["SparseArrays".to_string()].into_iter().collect()),
        )]),
        project_files: Default::default(),
        deps: Default::default(),
    };
    (pkg, library)
}

/// `unresolved-import` findings for `file`, linted against `library`.
fn unresolved_imports_in(file: PathBuf, library: &HarvestedLibrary) -> Vec<String> {
    let config = LintConfig {
        select: Some(vec!["unresolved-import".to_string()]),
        ..Default::default()
    };
    let result = check_paths_with_config(
        &[file],
        &config,
        &ExcludeFilter::none(),
        None,
        ProjectContext::Harvested(library),
    )
    .expect("lint succeeds");
    result
        .reports
        .into_iter()
        .flat_map(|r| r.diagnostics)
        .filter(|d| d.rule == "unresolved-import")
        .map(|d| d.message.body)
        .collect()
}

#[test]
fn cli_flags_an_undeclared_import_in_a_package_source_file() {
    // `SparseArrays` is declared, `Frobnicate` is not — and the declared set
    // answers even though nothing was harvested for either.
    let (pkg, library) = project_with_deps();
    let findings = unresolved_imports_in(pkg.path().join("src/b.jl"), &library);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].contains("Frobnicate"), "{findings:?}");
}

#[test]
fn cli_leaves_a_test_file_alone() {
    // `test/runtests.jl` is no workspace member: its `using` clauses resolve
    // against the test environment, so the package's `[deps]` say nothing about
    // them and the driver attaches no declared set.
    let (pkg, library) = project_with_deps();
    assert_eq!(
        unresolved_imports_in(pkg.path().join("test/runtests.jl"), &library),
        Vec::<String>::new(),
    );
}

#[test]
fn package_unused_imports_follow_includes_without_script_entry_points() {
    let (_dep, pkg, mut library) = project("f(x) = sin(x)\n");
    write(&pkg.path().join("src/a.jl"), "using Base: sin, cos\n");
    library
        .packages
        .insert("M".into(), Arc::new(harvest_package_named(pkg.path(), "M")));
    let config = LintConfig {
        select: Some(vec!["unused-import".into()]),
        ..Default::default()
    };
    let lint = || {
        check_paths_with_config(
            &[pkg.path().join("src/a.jl")],
            &config,
            &ExcludeFilter::none(),
            None,
            ProjectContext::Harvested(&library),
        )
        .unwrap()
    };
    let findings = lint();
    assert_eq!(findings.total_findings, 1, "{findings:?}");
    assert!(
        findings.reports[0].diagnostics[0]
            .message
            .body
            .contains("`cos`")
    );

    // Usage comes from current sources, even if the harvested index is stale.
    write(&pkg.path().join("src/b.jl"), "f(x) = cos(x)\n");
    let findings = lint();
    assert_eq!(findings.total_findings, 1, "{findings:?}");
    assert!(
        findings.reports[0].diagnostics[0]
            .message
            .body
            .contains("`sin`")
    );
    write(&pkg.path().join("src/b.jl"), "include(path)\n");
    assert_eq!(lint().total_findings, 0);

    // A stale membership index cannot turn a newly included source into an
    // isolated file and invent an unused-import finding.
    write(
        &pkg.path().join("src/M.jl"),
        "module M\ninclude(\"new.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write(&pkg.path().join("src/new.jl"), "using Base: sin, tan\n");
    write(&pkg.path().join("src/b.jl"), "f(x) = sin(x)\n");
    let findings = check_paths_with_config(
        &[pkg.path().join("src/new.jl")],
        &config,
        &ExcludeFilter::none(),
        None,
        ProjectContext::Harvested(&library),
    )
    .unwrap();
    assert_eq!(findings.total_findings, 1, "{findings:?}");
    assert!(
        findings.reports[0].diagnostics[0]
            .message
            .body
            .contains("`tan`")
    );
}
