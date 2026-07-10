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
