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
    db.set_library(packages, roots);

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
    db.set_library(packages, roots);

    // A packages-only update (the back-compat convenience) keeps the roots.
    let mut map = BTreeMap::new();
    map.insert("Foo".to_string(), Arc::new(empty_package("Foo")));
    db.set_library_packages(map);

    assert_eq!(db.package_root("Foo"), Some(PathBuf::from("/depot/Foo")));
}
