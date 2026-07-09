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
