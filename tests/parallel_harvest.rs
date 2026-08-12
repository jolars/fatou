//! The parallel, cache-aware library harvest (`harvest_libraries_parallel`)
//! agrees with the sequential `harvest_libraries`, and its on-disk cache is
//! written on a miss and read on the next harvest instead of re-parsing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fatou::environment::{EnvSource, Environment, Package, PackageKind, Uuid};
use fatou::index::{IndexCache, harvest_libraries, harvest_libraries_parallel};

fn nil_uuid() -> Uuid {
    "00000000-0000-0000-0000-000000000000".parse().unwrap()
}

/// Write a minimal Julia package `name` with a `src/<name>.jl` entry into a
/// fresh directory under `parent`, returning its root.
fn make_package(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join(format!("{name}.jl")),
        format!(
            "module {name}\n\
             export greet\n\
             greet(x) = x + 1\n\
             struct Widget end\n\
             const ANSWER = 42\n\
             end\n"
        ),
    )
    .unwrap();
    root
}

/// A registered manifest package rooted at `source` with content id `sha`.
fn registered(name: &str, sha: &str, source: PathBuf) -> Package {
    Package {
        name: name.to_string(),
        uuid: nil_uuid(),
        version: Some("1.0.0".to_string()),
        tree_sha1: Some(sha.to_string()),
        deps: Vec::new(),
        kind: PackageKind::Registered,
        source: Some(source),
    }
}

/// An environment with no located install (so the system library is the baked-in
/// fallback, identical on both harvest paths) carrying `packages`.
fn env_with(packages: Vec<Package>) -> Environment {
    Environment {
        project_file: PathBuf::from("Project.toml"),
        project_dir: PathBuf::from("."),
        manifest_file: None,
        name: None,
        uuid: None,
        direct_deps: BTreeMap::new(),
        declared_deps: Default::default(),
        packages,
        depots: Vec::new(),
        source: EnvSource::DefaultEnv,
        install: None,
    }
}

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
}

#[test]
fn parallel_harvest_equals_sequential() {
    let tmp = tempfile::tempdir().unwrap();
    let packages = vec![
        registered("Foo", "sha-foo", make_package(tmp.path(), "Foo")),
        registered("Bar", "sha-bar", make_package(tmp.path(), "Bar")),
        registered("Baz", "sha-baz", make_package(tmp.path(), "Baz")),
    ];
    let env = env_with(packages);

    let sequential = harvest_libraries(std::slice::from_ref(&env));
    let parallel = harvest_libraries_parallel(&[env], None, &pool(4));

    assert_eq!(sequential.packages, parallel.packages);
    assert_eq!(sequential.roots, parallel.roots);
    assert_eq!(sequential.workspaces, parallel.workspaces);
}

#[test]
fn cold_harvest_populates_cache_then_warm_harvest_reads_it() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let cache = IndexCache::open_at(cache_dir.path());

    let root = make_package(tmp.path(), "Foo");
    let env = env_with(vec![registered("Foo", "sha-foo", root.clone())]);

    // Cold: harvest from source and populate the cache.
    let cold = harvest_libraries_parallel(std::slice::from_ref(&env), Some(&cache), &pool(2));
    let cold_foo = cold.packages.get("Foo").expect("Foo harvested").clone();
    assert!(
        cache.load("Foo", "sha-foo").is_some(),
        "the cold harvest should have written Foo to the cache"
    );

    // Remove the source so a re-parse would yield a different (missing-entry)
    // index: only the cache can still return the original.
    std::fs::remove_dir_all(&root).unwrap();

    let warm = harvest_libraries_parallel(&[env], Some(&cache), &pool(2));
    let warm_foo = warm.packages.get("Foo").expect("Foo from cache");
    assert_eq!(
        &cold_foo, warm_foo,
        "the warm harvest should reload the cached index, not re-parse the deleted source"
    );
}

#[test]
fn uncacheable_packages_are_never_written() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let cache = IndexCache::open_at(cache_dir.path());

    // A registered package without a git-tree-sha1 has no stable content key.
    let root = make_package(tmp.path(), "Foo");
    let mut package = registered("Foo", "unused", root);
    package.tree_sha1 = None;
    let env = env_with(vec![package]);

    let harvested = harvest_libraries_parallel(&[env], Some(&cache), &pool(2));
    assert!(
        harvested.packages.contains_key("Foo"),
        "still harvested live"
    );
    assert!(
        cache.load("Foo", "unused").is_none(),
        "a package with no content key must not be cached"
    );
}
