//! Filesystem-level tests for the package harvester: `include` chains, nested
//! modules, multiple-dispatch grouping, docstrings, and best-effort recovery
//! over throwaway `src/` trees.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fatou::index::{HarvestDiagnostic, ModuleIndex, harvest_entry, harvest_package_named};

/// A unique temp directory removed on drop. Avoids a `tempfile`
/// dev-dependency (mirrors `tests/environment.rs`).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fatou-harvest-{}-{}", std::process::id(), n));
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

/// Names of the function groups in a module.
fn fn_names(module: &ModuleIndex) -> Vec<&str> {
    module.functions.iter().map(|g| g.name.as_str()).collect()
}

#[test]
fn single_file_package() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\nexport f\nf(x) = x + 1\nend\n",
    );
    let index = harvest_package_named(tmp.path(), "Pkg");
    assert_eq!(index.name, "Pkg");
    assert_eq!(fn_names(&index.root), ["f"]);
    assert_eq!(index.root.exports.len(), 1);
    assert!(index.diagnostics.is_empty(), "{:?}", index.diagnostics);
}

/// The language server's save-time re-harvest dedup compares successive
/// `PackageIndex` values and only writes the db on a change. That relies on
/// harvesting being deterministic: the same source must harvest identically
/// (so a no-op save elides the write), while a changed API must differ (so a
/// real edit still republishes).
#[test]
fn harvest_is_deterministic_for_dedup() {
    let tmp = TempDir::new();
    let entry = tmp.path().join("src/Pkg.jl");
    write(&entry, "module Pkg\nexport f\nf(x) = x + 1\nend\n");

    let first = harvest_package_named(tmp.path(), "Pkg");
    let again = harvest_package_named(tmp.path(), "Pkg");
    assert_eq!(first, again, "unchanged source must harvest identically");

    // A new exported binding changes the public API: dedup must not suppress it.
    write(
        &entry,
        "module Pkg\nexport f, g\nf(x) = x + 1\ng(x) = x - 1\nend\n",
    );
    let changed = harvest_package_named(tmp.path(), "Pkg");
    assert_ne!(first, changed, "an API change must harvest differently");
}

#[test]
fn harvest_entry_enters_non_src_layout() {
    // Julia's Base enters at `base/Base.jl`, not `src/<name>.jl`. `harvest_entry`
    // takes the entry file explicitly and keeps locations relative to the root.
    let tmp = TempDir::new();
    let base = tmp.path().join("base");
    write(
        &base.join("Base.jl"),
        "baremodule Base\ninclude(\"exports.jl\")\nf(x) = x\nend\n",
    );
    write(&base.join("exports.jl"), "export f\n");

    let index = harvest_entry(&base, &base.join("Base.jl"), "Base");
    assert_eq!(index.name, "Base");
    assert_eq!(fn_names(&index.root), ["f"]);
    assert_eq!(index.root.exports.len(), 1);
    assert_eq!(index.root.exports[0].name, "f");
    // The exported name's location is relative to the root (`exports.jl`).
    assert_eq!(index.root.exports[0].loc.file, PathBuf::from("exports.jl"));
    assert!(index.diagnostics.is_empty(), "{:?}", index.diagnostics);
}

#[test]
fn include_chain_splices_into_top_module() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write(&tmp.path().join("src/a.jl"), "a() = 1\n");
    write(&tmp.path().join("src/b.jl"), "b() = 2\n");
    let index = harvest_package_named(tmp.path(), "Pkg");
    let mut names = fn_names(&index.root);
    names.sort_unstable();
    assert_eq!(
        names,
        ["a", "b"],
        "both included files land in the top module"
    );
    assert!(index.diagnostics.is_empty(), "{:?}", index.diagnostics);
    // The member set is the include closure, package-relative, entry first.
    let members: Vec<&Path> = index.members.iter().map(PathBuf::as_path).collect();
    assert_eq!(
        members,
        [
            Path::new("src/Pkg.jl"),
            Path::new("src/a.jl"),
            Path::new("src/b.jl"),
        ],
    );
}

#[test]
fn include_inside_submodule_lands_in_submodule() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\nmodule Sub\ninclude(\"sub.jl\")\nend\nend\n",
    );
    write(&tmp.path().join("src/sub.jl"), "inner() = 1\n");
    let index = harvest_package_named(tmp.path(), "Pkg");
    assert!(index.root.functions.is_empty(), "nothing at top level");
    assert_eq!(index.root.submodules.len(), 1);
    assert_eq!(fn_names(&index.root.submodules[0]), ["inner"]);
    // The included file's top level belongs to the nested `Sub` module, while
    // the entry file itself belongs to the root (empty path).
    assert_eq!(
        index.member_modules.get(Path::new("src/sub.jl")),
        Some(&vec!["Sub".to_string()]),
    );
    assert_eq!(
        index.member_modules.get(Path::new("src/Pkg.jl")),
        Some(&Vec::new()),
    );
}

#[test]
fn nested_include_directory() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\ninclude(\"sub/mod.jl\")\nend\n",
    );
    // A relative include inside a subdirectory resolves against that file's dir.
    write(&tmp.path().join("src/sub/mod.jl"), "include(\"impl.jl\")\n");
    write(&tmp.path().join("src/sub/impl.jl"), "deep() = 1\n");
    let index = harvest_package_named(tmp.path(), "Pkg");
    assert_eq!(fn_names(&index.root), ["deep"]);
}

#[test]
fn dispatch_across_files_groups_by_name() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\ninclude(\"int.jl\")\ninclude(\"float.jl\")\nend\n",
    );
    write(&tmp.path().join("src/int.jl"), "f(x::Int) = 1\n");
    write(&tmp.path().join("src/float.jl"), "f(x::Float64) = 2\n");
    let index = harvest_package_named(tmp.path(), "Pkg");
    assert_eq!(fn_names(&index.root), ["f"], "one group across two files");
    assert_eq!(index.root.functions[0].methods.len(), 2);
}

#[test]
fn missing_include_is_reported_not_fatal() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\ninclude(\"gone.jl\")\ng() = 1\nend\n",
    );
    let index = harvest_package_named(tmp.path(), "Pkg");
    assert_eq!(
        fn_names(&index.root),
        ["g"],
        "harvest continues past the miss"
    );
    assert!(
        index.diagnostics.iter().any(
            |d| matches!(d, HarvestDiagnostic::UnresolvedInclude { raw, .. } if raw == "gone.jl")
        ),
        "{:?}",
        index.diagnostics
    );
}

#[test]
fn include_cycle_walks_each_file_once() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\ninclude(\"a.jl\")\nend\n",
    );
    write(&tmp.path().join("src/a.jl"), "a() = 1\ninclude(\"b.jl\")\n");
    write(&tmp.path().join("src/b.jl"), "b() = 2\ninclude(\"a.jl\")\n");
    let index = harvest_package_named(tmp.path(), "Pkg");
    let mut names = fn_names(&index.root);
    names.sort_unstable();
    assert_eq!(names, ["a", "b"], "each defined once despite the cycle");
    assert!(
        index
            .diagnostics
            .iter()
            .any(|d| matches!(d, HarvestDiagnostic::IncludeCycle { .. })),
        "the back-edge is reported: {:?}",
        index.diagnostics
    );
}

#[test]
fn duplicate_include_walks_once() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\ninclude(\"a.jl\")\ninclude(\"a.jl\")\nend\n",
    );
    write(&tmp.path().join("src/a.jl"), "a() = 1\n");
    let index = harvest_package_named(tmp.path(), "Pkg");
    assert_eq!(
        index.root.functions[0].methods.len(),
        1,
        "the second include is skipped, not a second method"
    );
    // The duplicated include records the file once, not twice.
    assert_eq!(
        index.members,
        [PathBuf::from("src/Pkg.jl"), PathBuf::from("src/a.jl")],
    );
}

#[test]
fn parse_error_file_is_best_effort() {
    let tmp = TempDir::new();
    // A syntactically broken statement, then a clean definition after it.
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\nfunction broken(\ngood() = 1\nend\n",
    );
    let index = harvest_package_named(tmp.path(), "Pkg");
    assert!(
        index
            .diagnostics
            .iter()
            .any(|d| matches!(d, HarvestDiagnostic::ParseError { .. })),
        "{:?}",
        index.diagnostics
    );
}

#[test]
fn missing_entry_file_yields_empty_index() {
    let tmp = TempDir::new();
    // No src/Pkg.jl written.
    let index = harvest_package_named(tmp.path(), "Pkg");
    assert!(index.root.functions.is_empty());
    assert!(matches!(
        index.diagnostics.as_slice(),
        [HarvestDiagnostic::EntryFileMissing { .. }]
    ));
}

#[test]
fn types_supertypes_and_docstrings() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        concat!(
            "module Pkg\n",
            "\"An animal.\"\n",
            "abstract type Animal end\n",
            "struct Dog <: Animal\n    name::String\nend\n",
            "end\n",
        ),
    );
    let index = harvest_package_named(tmp.path(), "Pkg");
    let animal = index
        .root
        .types
        .iter()
        .find(|t| t.name == "Animal")
        .unwrap();
    assert_eq!(animal.doc.as_ref().unwrap().text, "An animal.");
    let dog = index.root.types.iter().find(|t| t.name == "Dog").unwrap();
    assert!(dog.supertype.is_some(), "Dog <: Animal supertype recorded");
    assert_eq!(dog.fields.len(), 1);
}

#[test]
fn locations_are_relative_to_package_root() {
    let tmp = TempDir::new();
    write(
        &tmp.path().join("src/Pkg.jl"),
        "module Pkg\ninclude(\"a.jl\")\nend\n",
    );
    write(&tmp.path().join("src/a.jl"), "a() = 1\n");
    let index = harvest_package_named(tmp.path(), "Pkg");
    let loc = &index.root.functions[0].methods[0].loc;
    assert_eq!(
        loc.file,
        Path::new("src/a.jl"),
        "the location is depot-independent"
    );
}
