//! End-to-end: the CLI lint pipeline resolves cross-file names when handed a
//! harvested project via [`ProjectContext::Harvested`], so `undefined-name`
//! stops flagging a name a sibling file's `using` brings in module-wide.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fatou::config::LintConfig;
use fatou::file_discovery::ExcludeFilter;
use fatou::index::{HarvestedLibrary, harvest_package_named};
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
