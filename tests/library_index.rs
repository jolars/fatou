//! Tests for the `LibraryIndex` salsa input: harvested package indexes round
//! trip through the incremental database, and per-package replacement leaves
//! the rest of the library untouched.

use std::sync::Arc;

use fatou::incremental::IncrementalDatabase;
use fatou::index::model::DefLocation;
use fatou::index::{ModuleIndex, PackageIndex, Span};

/// A minimal empty package index named `name`.
fn empty_package(name: &str) -> PackageIndex {
    PackageIndex {
        name: name.to_string(),
        root: ModuleIndex {
            name: name.to_string(),
            bare: false,
            loc: DefLocation {
                file: format!("src/{name}.jl").into(),
                range: Span { start: 0, end: 0 },
            },
            exports: Vec::new(),
            functions: Vec::new(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
        },
        members: Vec::new(),
        member_modules: Default::default(),
        diagnostics: Vec::new(),
    }
}

#[test]
fn package_index_round_trips() {
    let mut db = IncrementalDatabase::new();
    assert!(db.library_package("Foo").is_none(), "empty before any set");

    let foo = Arc::new(empty_package("Foo"));
    db.set_package_index("Foo", Arc::clone(&foo));

    let read = db.library_package("Foo").expect("Foo present after set");
    assert_eq!(read.name, "Foo");
    assert!(Arc::ptr_eq(&read, &foo), "the same Arc is handed back");
}

#[test]
fn per_package_replacement_keeps_the_rest() {
    let mut db = IncrementalDatabase::new();
    db.set_package_index("Foo", Arc::new(empty_package("Foo")));
    db.set_package_index("Bar", Arc::new(empty_package("Bar")));

    // Replace Foo; Bar must be untouched.
    let new_foo = Arc::new(empty_package("Foo"));
    db.set_package_index("Foo", Arc::clone(&new_foo));

    assert!(Arc::ptr_eq(&db.library_package("Foo").unwrap(), &new_foo));
    assert_eq!(db.library_package("Bar").unwrap().name, "Bar");
}

#[test]
fn set_library_packages_replaces_whole_map() {
    use std::collections::BTreeMap;

    let mut db = IncrementalDatabase::new();
    db.set_package_index("Old", Arc::new(empty_package("Old")));

    let mut map = BTreeMap::new();
    map.insert("New".to_string(), Arc::new(empty_package("New")));
    db.set_library_packages(map);

    assert!(db.library_package("Old").is_none(), "old set was replaced");
    assert_eq!(db.library_package("New").unwrap().name, "New");
}

#[test]
fn snapshot_reads_the_library() {
    let mut db = IncrementalDatabase::new();
    db.set_package_index("Foo", Arc::new(empty_package("Foo")));

    let snapshot = db.snapshot();
    assert_eq!(snapshot.library_package("Foo").unwrap().name, "Foo");
}

#[test]
fn source_roots_round_trip_through_set_library() {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    let mut db = IncrementalDatabase::new();
    assert!(db.package_root("Foo").is_none(), "no roots before any set");

    let mut packages = BTreeMap::new();
    packages.insert("Foo".to_string(), Arc::new(empty_package("Foo")));
    let mut roots = BTreeMap::new();
    roots.insert("Foo".to_string(), PathBuf::from("/depot/Foo/abcde"));
    db.set_library(packages, roots, None);

    assert_eq!(
        db.package_root("Foo"),
        Some(PathBuf::from("/depot/Foo/abcde"))
    );
    // The roots are visible through a read-only snapshot too.
    assert_eq!(
        db.snapshot().package_root("Foo"),
        Some(PathBuf::from("/depot/Foo/abcde"))
    );
    // A package with no registered root reads back `None`.
    assert!(db.package_root("Bar").is_none());
}

#[test]
fn set_library_packages_preserves_existing_roots() {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    let mut db = IncrementalDatabase::new();
    let mut packages = BTreeMap::new();
    packages.insert("Foo".to_string(), Arc::new(empty_package("Foo")));
    let mut roots = BTreeMap::new();
    roots.insert("Foo".to_string(), PathBuf::from("/depot/Foo"));
    db.set_library(packages, roots, None);

    // A packages-only update (the back-compat convenience) keeps the roots.
    let mut map = BTreeMap::new();
    map.insert("Foo".to_string(), Arc::new(empty_package("Foo")));
    db.set_library_packages(map);

    assert_eq!(db.package_root("Foo"), Some(PathBuf::from("/depot/Foo")));
}

#[test]
fn workspace_name_and_membership_round_trip() {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    let mut db = IncrementalDatabase::new();
    assert!(db.workspace_package().is_none());

    let mut packages = BTreeMap::new();
    packages.insert("MyPkg".to_string(), Arc::new(empty_package("MyPkg")));
    let mut roots = BTreeMap::new();
    roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
    db.set_library(packages, roots, Some("MyPkg".to_string()));

    assert_eq!(db.workspace_package().as_deref(), Some("MyPkg"));
    // A file under the package's `src/` is a member; one outside is not.
    assert!(
        db.workspace_module(Path::new("/work/MyPkg/src/bar.jl"))
            .is_some()
    );
    assert!(db.workspace_module(Path::new("/other/x.jl")).is_none());

    // A packages-only re-harvest (the on-save path) keeps the workspace name.
    let mut map = BTreeMap::new();
    map.insert("MyPkg".to_string(), Arc::new(empty_package("MyPkg")));
    db.set_library_packages(map);
    assert_eq!(db.workspace_package().as_deref(), Some("MyPkg"));
}

/// A throwaway temp file, removed on drop.
struct TempFile(std::path::PathBuf);

impl TempFile {
    fn new(name: &str, contents: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fatou_seed_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        TempFile(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[test]
fn seed_disk_file_reads_a_fresh_file_from_disk() {
    let file = TempFile::new("fresh.jl", "f() = 1\n");
    let mut db = IncrementalDatabase::new();

    let seeded = db.seed_disk_file(&file.0).expect("the file reads");
    assert_eq!(db.file_text(seeded), "f() = 1\n");
    // A second seed of the same path returns the same input (create-or-return).
    let again = db.seed_disk_file(&file.0).expect("still there");
    assert!(seeded == again, "the same input is reused");
}

#[test]
fn revert_file_to_disk_drops_an_unsaved_buffer() {
    // On close, a member file's discarded buffer must be replaced by on-disk
    // text so it stops contributing stale occurrences to the reverse index.
    let file = TempFile::new("m.jl", "greet() = 1\n");
    let mut db = IncrementalDatabase::new();
    let f = db.upsert_file(&file.0, "greet() = 999\n".to_string());
    assert_eq!(db.file_text(f), "greet() = 999\n");

    db.revert_file_to_disk(&file.0);
    assert_eq!(db.file_text(f), "greet() = 1\n", "reverted to on-disk text");
}

#[test]
fn revert_file_to_disk_ignores_untracked_and_missing() {
    let mut db = IncrementalDatabase::new();
    // An untracked path is a no-op (must not panic or create an input).
    db.revert_file_to_disk(std::path::Path::new("/no/such/file.jl"));
    assert!(
        db.lookup_file(std::path::Path::new("/no/such/file.jl"))
            .is_none()
    );
}

#[test]
fn seed_disk_file_never_clobbers_an_open_buffer() {
    // The load-bearing invariant: seeding must not overwrite an open, unsaved
    // buffer with stale disk text, or the editor loses unsaved work.
    let file = TempFile::new("open.jl", "on_disk() = 1\n");
    let mut db = IncrementalDatabase::new();

    // The editor opens the file with unsaved edits.
    let opened = db.upsert_file(&file.0, "in_buffer() = 2\n".to_string());
    // A re-harvest seeds the same member path.
    let seeded = db.seed_disk_file(&file.0).expect("the file reads");

    assert!(seeded == opened, "the open buffer's input is reused");
    assert_eq!(
        db.file_text(seeded),
        "in_buffer() = 2\n",
        "the buffer text wins over the disk read"
    );
}

#[test]
fn seed_disk_file_of_a_missing_file_is_none() {
    let mut db = IncrementalDatabase::new();
    assert!(
        db.seed_disk_file(std::path::Path::new("/no/such/file.jl"))
            .is_none()
    );
}

#[test]
fn workspace_reference_index_unions_across_member_files() {
    use std::collections::{BTreeMap, HashSet};
    use std::path::{Path, PathBuf};

    use fatou::index::FunctionGroup;
    use fatou::resolve::Namespace;

    // A workspace package MyPkg that defines a top-level `f`.
    let mut pkg = empty_package("MyPkg");
    pkg.root.functions.push(FunctionGroup {
        name: "f".to_string(),
        owner: None,
        methods: Vec::new(),
        doc: None,
    });

    let mut db = IncrementalDatabase::new();
    // Two member files: `a.jl` defines `f` (def + a recursive use), `b.jl`
    // only calls it (one free-read use).
    let a = db.upsert_file(
        Path::new("/work/MyPkg/src/a.jl"),
        "function f()\n    f()\nend\n".to_string(),
    );
    let b = db.upsert_file(Path::new("/work/MyPkg/src/b.jl"), "g() = f()\n".to_string());

    let mut packages = BTreeMap::new();
    packages.insert("MyPkg".to_string(), Arc::new(pkg));
    let mut roots = BTreeMap::new();
    roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
    db.set_library(packages, roots, Some("MyPkg".to_string()));
    db.set_workspace_files(vec![a, b]);

    let snap = db.snapshot();
    let index = snap.workspace_reference_index();
    let recs: Vec<_> = index
        .0
        .iter()
        .filter(|(k, _)| k.1 == Namespace::Value && k.2.as_str() == "f")
        .flat_map(|(_, v)| v.iter())
        .collect();

    // a.jl: definition + recursive call = 2; b.jl: one call = 1.
    assert_eq!(recs.len(), 3, "occurrences from both files are unioned");
    let paths: HashSet<_> = recs
        .iter()
        .filter_map(|(file, _)| snap.file_path_of(*file))
        .collect();
    assert_eq!(paths.len(), 2, "the symbol is referenced in both files");
    assert_eq!(
        recs.iter().filter(|(_, r)| r.is_def).count(),
        1,
        "exactly one definition site across the package"
    );
}

#[test]
fn nested_module_symbols_do_not_conflate_with_the_root() {
    // `MyPkg` defines `f` at the root and *also* `f` inside a nested `module
    // Sub`. Two member files: `a.jl` (host = root) and `sub.jl` (host = Sub).
    // The reverse index must key them by module path so the two `f`s stay apart.
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use fatou::index::model::{DefLocation, Span};
    use fatou::index::{FunctionGroup, ModuleIndex};
    use fatou::resolve::Namespace;
    use smol_str::SmolStr;

    let func = |name: &str| FunctionGroup {
        name: name.to_string(),
        owner: None,
        methods: Vec::new(),
        doc: None,
    };
    let mut pkg = empty_package("MyPkg");
    pkg.root.functions.push(func("f"));
    pkg.root.submodules.push(ModuleIndex {
        name: "Sub".to_string(),
        bare: false,
        loc: DefLocation {
            file: "src/MyPkg.jl".into(),
            range: Span { start: 0, end: 0 },
        },
        exports: Vec::new(),
        functions: vec![func("f")],
        types: Vec::new(),
        consts: Vec::new(),
        macros: Vec::new(),
        submodules: Vec::new(),
    });
    // `a.jl`'s top level is the root module; `sub.jl`'s is `Sub`.
    pkg.member_modules
        .insert(PathBuf::from("src/a.jl"), Vec::new());
    pkg.member_modules
        .insert(PathBuf::from("src/sub.jl"), vec!["Sub".to_string()]);

    let mut db = IncrementalDatabase::new();
    let a = db.upsert_file(
        Path::new("/work/MyPkg/src/a.jl"),
        "function f()\n    f()\nend\n".to_string(),
    );
    let sub = db.upsert_file(
        Path::new("/work/MyPkg/src/sub.jl"),
        "g() = f()\n".to_string(),
    );

    let mut packages = BTreeMap::new();
    packages.insert("MyPkg".to_string(), Arc::new(pkg));
    let mut roots = BTreeMap::new();
    roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
    db.set_library(packages, roots, Some("MyPkg".to_string()));
    db.set_workspace_files(vec![a, sub]);

    let snap = db.snapshot();
    let index = snap.workspace_reference_index();

    // Root `f`: only a.jl's definition and recursive call.
    let root_key = (Vec::new(), Namespace::Value, SmolStr::new("f"));
    let root_recs = index.0.get(&root_key).expect("root `f` bucket");
    assert_eq!(root_recs.len(), 2, "def plus recursive call in a.jl only");
    assert!(
        root_recs.iter().all(|(file, _)| *file == a),
        "no Sub occurrence leaked into the root `f`",
    );

    // `Sub.f`: only sub.jl's free-read call.
    let sub_key = (
        vec![SmolStr::new("Sub")],
        Namespace::Value,
        SmolStr::new("f"),
    );
    let sub_recs = index.0.get(&sub_key).expect("Sub `f` bucket");
    assert_eq!(sub_recs.len(), 1, "just the call in sub.jl");
    assert!(sub_recs.iter().all(|(file, _)| *file == sub));
}

#[test]
fn file_internal_nested_module_symbols_are_attributed_to_that_module() {
    // Shape A: the `module Sub` wrapper sits *inside* the included file, so the
    // file's host is the root but `Sub`'s symbols must be attributed to `Sub`
    // via the file-internal `module` nesting. Both a root `f` and a `Sub.f`
    // exist; each file hosts at the root, and the two `f`s must stay apart.
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use fatou::index::model::{DefLocation, Span};
    use fatou::index::{FunctionGroup, ModuleIndex};
    use fatou::resolve::Namespace;
    use smol_str::SmolStr;

    let func = |name: &str| FunctionGroup {
        name: name.to_string(),
        owner: None,
        methods: Vec::new(),
        doc: None,
    };
    let mut pkg = empty_package("MyPkg");
    pkg.root.functions.push(func("f"));
    pkg.root.submodules.push(ModuleIndex {
        name: "Sub".to_string(),
        bare: false,
        loc: DefLocation {
            file: "src/sub.jl".into(),
            range: Span { start: 0, end: 0 },
        },
        exports: Vec::new(),
        functions: vec![func("f")],
        types: Vec::new(),
        consts: Vec::new(),
        macros: Vec::new(),
        submodules: Vec::new(),
    });
    // Both files' top level hosts at the *root* module; `sub.jl` opens `Sub`
    // inline, so its `f` belongs to `Sub` through the model's scope nesting.
    pkg.member_modules
        .insert(PathBuf::from("src/root.jl"), Vec::new());
    pkg.member_modules
        .insert(PathBuf::from("src/sub.jl"), Vec::new());

    let mut db = IncrementalDatabase::new();
    let root = db.upsert_file(
        Path::new("/work/MyPkg/src/root.jl"),
        "f() = 2\nh() = f()\n".to_string(),
    );
    let sub = db.upsert_file(
        Path::new("/work/MyPkg/src/sub.jl"),
        "module Sub\nf() = 1\ng() = f()\nend\n".to_string(),
    );

    let mut packages = BTreeMap::new();
    packages.insert("MyPkg".to_string(), Arc::new(pkg));
    let mut roots = BTreeMap::new();
    roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
    db.set_library(packages, roots, Some("MyPkg".to_string()));
    db.set_workspace_files(vec![root, sub]);

    let snap = db.snapshot();
    let index = snap.workspace_reference_index();

    // Root `f`: def + call in root.jl only.
    let root_recs = index
        .0
        .get(&(Vec::new(), Namespace::Value, SmolStr::new("f")))
        .expect("root `f` bucket");
    assert_eq!(root_recs.len(), 2);
    assert!(root_recs.iter().all(|(file, _)| *file == root));

    // `Sub.f`: def + call in sub.jl — attributed to `Sub` through the inline
    // `module`, not missed (as it would be if resolved against the root) nor
    // merged with the root `f`.
    let sub_recs = index
        .0
        .get(&(
            vec![SmolStr::new("Sub")],
            Namespace::Value,
            SmolStr::new("f"),
        ))
        .expect("Sub `f` bucket");
    assert_eq!(sub_recs.len(), 2);
    assert!(sub_recs.iter().all(|(file, _)| *file == sub));
}

#[test]
fn resetting_workspace_files_drops_removed_members() {
    // Reconciliation on re-harvest: a file no longer in the member set stops
    // contributing to the reverse index (occurrences from a dropped file vanish).
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use fatou::index::FunctionGroup;
    use fatou::resolve::Namespace;

    let mut pkg = empty_package("MyPkg");
    pkg.root.functions.push(FunctionGroup {
        name: "f".to_string(),
        owner: None,
        methods: Vec::new(),
        doc: None,
    });

    let mut db = IncrementalDatabase::new();
    let a = db.upsert_file(Path::new("/work/MyPkg/src/a.jl"), "f() = 1\n".to_string());
    let b = db.upsert_file(Path::new("/work/MyPkg/src/b.jl"), "g() = f()\n".to_string());

    let mut packages = BTreeMap::new();
    packages.insert("MyPkg".to_string(), Arc::new(pkg));
    let mut roots = BTreeMap::new();
    roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
    db.set_library(packages, roots, Some("MyPkg".to_string()));

    let count_f = |db: &IncrementalDatabase| -> usize {
        let snap = db.snapshot();
        snap.workspace_reference_index()
            .0
            .iter()
            .filter(|(k, _)| k.1 == Namespace::Value && k.2.as_str() == "f")
            .map(|(_, v)| v.len())
            .sum()
    };

    // Both files in the set: the definition in a.jl plus the call in b.jl.
    db.set_workspace_files(vec![a, b]);
    assert_eq!(count_f(&db), 2);

    // A re-harvest that drops b.jl from the member set: only a.jl's def remains.
    db.set_workspace_files(vec![a]);
    assert_eq!(count_f(&db), 1, "the removed member no longer contributes");
}

/// Build an in-memory workspace package `MyPkg` rooted at `/work/MyPkg` from
/// `(relative src path, contents)` pairs, seed every file as a workspace member,
/// and return the database ready for `project_graph`.
#[cfg(test)]
fn seed_project(files: &[(&str, &str)]) -> IncrementalDatabase {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    let mut db = IncrementalDatabase::new();
    let seeded: Vec<_> = files
        .iter()
        .map(|(rel, text)| {
            let path = format!("/work/MyPkg/src/{rel}");
            db.upsert_file(Path::new(&path), (*text).to_string())
        })
        .collect();

    let mut packages = BTreeMap::new();
    packages.insert("MyPkg".to_string(), Arc::new(empty_package("MyPkg")));
    let mut roots = BTreeMap::new();
    roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
    db.set_library(packages, roots, Some("MyPkg".to_string()));
    db.set_workspace_files(seeded);
    db
}

#[cfg(test)]
fn src(rel: &str) -> std::path::PathBuf {
    fatou::incremental::normalize_path(std::path::Path::new(&format!("/work/MyPkg/src/{rel}")))
}

#[test]
fn project_graph_closure_hosts_and_diamond() {
    // MyPkg.jl wraps everything in `module MyPkg` (the absorbed root); `Sub`
    // nests; `shared.jl` is included from both `a.jl` (root) and `b.jl` (Sub),
    // a diamond that must appear once and never as a cycle.
    let db = seed_project(&[
        (
            "MyPkg.jl",
            "module MyPkg\ninclude(\"a.jl\")\nmodule Sub\ninclude(\"b.jl\")\nend\nend\n",
        ),
        ("a.jl", "include(\"shared.jl\")\n"),
        ("b.jl", "include(\"shared.jl\")\n"),
        ("shared.jl", "x = 1\n"),
    ]);
    let snap = db.snapshot();
    let g = snap.project_graph();

    // Depth-first, source order; `shared.jl` (a diamond) is walked once.
    assert_eq!(
        g.nodes,
        vec![src("MyPkg.jl"), src("a.jl"), src("shared.jl"), src("b.jl")]
    );

    // Root-module absorption: everything inside `module MyPkg` is at the root,
    // so `a.jl`/`shared.jl` host to [] and only `Sub` survives for `b.jl`.
    assert_eq!(g.host_modules[&src("MyPkg.jl")], Vec::<String>::new());
    assert_eq!(g.host_modules[&src("a.jl")], Vec::<String>::new());
    assert_eq!(g.host_modules[&src("shared.jl")], Vec::<String>::new());
    assert_eq!(g.host_modules[&src("b.jl")], vec!["Sub".to_string()]);

    // Both includers of `shared.jl` show up in its reverse adjacency.
    let mut includers = g.reverse[&src("shared.jl")].clone();
    includers.sort();
    assert_eq!(includers, vec![src("a.jl"), src("b.jl")]);
    assert_eq!(g.forward[&src("MyPkg.jl")], vec![src("a.jl"), src("b.jl")]);

    assert!(g.cycles.is_empty(), "a diamond is not a cycle");
    assert!(g.unresolved.is_empty());
}

#[test]
fn project_graph_reports_cycles_and_unresolved() {
    // `a.jl` -> `b.jl` -> `a.jl` is a true cycle; `missing.jl` does not exist.
    let db = seed_project(&[
        (
            "MyPkg.jl",
            "module MyPkg\ninclude(\"a.jl\")\ninclude(\"missing.jl\")\nend\n",
        ),
        ("a.jl", "include(\"b.jl\")\n"),
        ("b.jl", "include(\"a.jl\")\n"),
    ]);
    let snap = db.snapshot();
    let g = snap.project_graph();

    assert_eq!(g.cycles.len(), 1, "one back-edge closes the a<->b cycle");
    let cycle = &g.cycles[0];
    assert_eq!(cycle.from, src("b.jl"));
    assert_eq!(cycle.to, src("a.jl"));
    assert_eq!(cycle.raw, "a.jl");

    assert_eq!(g.unresolved.len(), 1, "missing.jl is unresolved");
    assert_eq!(g.unresolved[0].from, src("MyPkg.jl"));
    assert_eq!(g.unresolved[0].raw, "missing.jl");
}
