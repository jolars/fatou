//! Filesystem-level tests for Julia environment resolution: discovery
//! precedence and on-disk source resolution over throwaway directory trees.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fatou::environment::{self, EnvContext, EnvSource, PackageKind};

/// Lay out a minimal `<prefix>/share/julia` tree with a `bin/julia` stub, a
/// Base entry, and one stdlib package, enough for the install locator.
fn make_fake_install(prefix: &Path, stdlib_version: &str) {
    let julia = prefix.join("share/julia");
    write(&julia.join("base/Base.jl"), "baremodule Base\nend\n");
    write(&julia.join("base/boot.jl"), "export Any\n");
    write(
        &julia.join(format!("stdlib/v{stdlib_version}/Foo/src/Foo.jl")),
        "module Foo\nend\n",
    );
    // The PATH locator looks for `julia.exe` on Windows.
    let exe = if cfg!(windows) { "julia.exe" } else { "julia" };
    write(&prefix.join("bin").join(exe), "#!/bin/sh\n");
}

fn bare_ctx(workspace_root: PathBuf) -> EnvContext {
    EnvContext {
        workspace_root,
        julia_project: None,
        julia_depot_path: None,
        home: None,
        julia_bindir: None,
        path: None,
    }
}

/// A unique temp directory removed on drop. Avoids a `tempfile` dev-dependency.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fatou-env-{}-{}", std::process::id(), n));
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

const ABSTRACT_TREES_MANIFEST: &str = r#"
julia_version = "1.11.7"
manifest_format = "2.0"

[[deps.AbstractTrees]]
git-tree-sha1 = "2d9c9a55f9c93e8887ad391fbae72f8ef55e1177"
uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c"
version = "0.4.5"
"#;

/// Lay out `<depot>/packages/AbstractTrees/Ftf8W/src/` so the registered
/// package resolves to a real directory.
fn make_abstract_trees_depot(depot: &Path) {
    fs::create_dir_all(depot.join("packages/AbstractTrees/Ftf8W/src")).unwrap();
}

#[test]
fn resolves_project_manifest_and_source() {
    let tmp = TempDir::new();
    let ws = tmp.path().join("ws");
    let depot = tmp.path().join("depot");
    write(
        &ws.join("Project.toml"),
        "[deps]\nAbstractTrees = \"1520ce14-60c1-5f80-bbc7-55ef81b5835c\"\n",
    );
    write(&ws.join("Manifest.toml"), ABSTRACT_TREES_MANIFEST);
    make_abstract_trees_depot(&depot);

    let ctx = EnvContext {
        workspace_root: ws.clone(),
        julia_project: None,
        julia_depot_path: Some(depot.to_string_lossy().into_owned()),
        home: None,
        julia_bindir: None,
        path: None,
    };
    let env = environment::resolve(&ctx).unwrap().expect("environment");

    assert_eq!(env.source, EnvSource::WorkspaceWalkUp);
    assert_eq!(env.project_file, ws.join("Project.toml"));
    assert_eq!(env.manifest_file, Some(ws.join("Manifest.toml")));
    assert!(env.direct_deps.contains_key("AbstractTrees"));

    assert_eq!(env.packages.len(), 1);
    let pkg = &env.packages[0];
    assert_eq!(pkg.name, "AbstractTrees");
    assert_eq!(pkg.kind, PackageKind::Registered);
    assert_eq!(pkg.version.as_deref(), Some("0.4.5"));
    assert_eq!(pkg.source, Some(depot.join("packages/AbstractTrees/Ftf8W")));
}

#[test]
fn source_is_none_when_slug_missing_from_depot() {
    let tmp = TempDir::new();
    let ws = tmp.path().join("ws");
    write(&ws.join("Project.toml"), "");
    write(&ws.join("Manifest.toml"), ABSTRACT_TREES_MANIFEST);
    // Empty depot: the slug directory does not exist.
    let depot = tmp.path().join("depot");
    fs::create_dir_all(&depot).unwrap();

    let ctx = EnvContext {
        workspace_root: ws,
        julia_project: None,
        julia_depot_path: Some(depot.to_string_lossy().into_owned()),
        home: None,
        julia_bindir: None,
        path: None,
    };
    let env = environment::resolve(&ctx).unwrap().unwrap();
    assert_eq!(env.packages[0].source, None);
}

#[test]
fn julia_project_beats_workspace_walk_up() {
    let tmp = TempDir::new();
    let ws = tmp.path().join("ws");
    let other = tmp.path().join("other");
    write(&ws.join("Project.toml"), "name = \"Workspace\"\n");
    write(&other.join("Project.toml"), "name = \"Explicit\"\n");

    let ctx = EnvContext {
        workspace_root: ws,
        julia_project: Some(other.to_string_lossy().into_owned()),
        julia_depot_path: None,
        home: None,
        julia_bindir: None,
        path: None,
    };
    let env = environment::resolve(&ctx).unwrap().unwrap();
    assert_eq!(env.source, EnvSource::JuliaProject);
    assert_eq!(env.name.as_deref(), Some("Explicit"));
}

#[test]
fn julia_project_dot_walks_up() {
    let tmp = TempDir::new();
    let root = tmp.path().join("proj");
    let nested = root.join("a/b/c");
    fs::create_dir_all(&nested).unwrap();
    write(&root.join("JuliaProject.toml"), "name = \"Root\"\n");

    let ctx = EnvContext {
        workspace_root: nested,
        julia_project: Some("@.".to_string()),
        julia_depot_path: None,
        home: None,
        julia_bindir: None,
        path: None,
    };
    let env = environment::resolve(&ctx).unwrap().unwrap();
    assert_eq!(env.source, EnvSource::JuliaProject);
    assert_eq!(env.project_file, root.join("JuliaProject.toml"));
}

#[test]
fn julia_project_prefers_julia_prefixed_names() {
    let tmp = TempDir::new();
    let ws = tmp.path().join("ws");
    write(&ws.join("Project.toml"), "name = \"Plain\"\n");
    write(&ws.join("JuliaProject.toml"), "name = \"Preferred\"\n");

    let ctx = EnvContext {
        workspace_root: ws,
        julia_project: None,
        julia_depot_path: None,
        home: None,
        julia_bindir: None,
        path: None,
    };
    let env = environment::resolve(&ctx).unwrap().unwrap();
    assert_eq!(env.name.as_deref(), Some("Preferred"));
}

#[test]
fn falls_back_to_newest_default_env() {
    let tmp = TempDir::new();
    let home = tmp.path().join("home");
    let envs = home.join(".julia/environments");
    write(&envs.join("v1.7/Project.toml"), "name = \"Old\"\n");
    write(&envs.join("v1.11/Project.toml"), "name = \"New\"\n");

    let ctx = EnvContext {
        // Workspace with no project anywhere reachable within the temp tree.
        workspace_root: tmp.path().join("empty/ws"),
        julia_project: None,
        julia_depot_path: None,
        home: Some(home.clone()),
        julia_bindir: None,
        path: None,
    };
    fs::create_dir_all(tmp.path().join("empty/ws")).unwrap();
    let env = environment::resolve(&ctx).unwrap().unwrap();
    assert_eq!(env.source, EnvSource::DefaultEnv);
    assert_eq!(env.project_file, envs.join("v1.11/Project.toml"));
}

#[test]
fn dev_package_detected_for_named_project_with_entry() {
    let tmp = TempDir::new();
    let ws = tmp.path().join("MyPkg");
    write(&ws.join("Project.toml"), "name = \"MyPkg\"\n");
    write(&ws.join("src/MyPkg.jl"), "module MyPkg\nend\n");

    let ctx = EnvContext {
        workspace_root: ws.clone(),
        julia_project: None,
        julia_depot_path: None,
        home: None,
        julia_bindir: None,
        path: None,
    };
    let env = environment::resolve(&ctx).unwrap().unwrap();
    let dev = env.dev_package().expect("a package under development");
    assert_eq!(dev.name, "MyPkg");
    assert_eq!(dev.root, ws);
}

#[test]
fn dev_package_absent_without_entry_file() {
    // A named project with no matching `src/<Name>.jl` is a plain environment,
    // not a package under development.
    let tmp = TempDir::new();
    let ws = tmp.path().join("proj");
    write(&ws.join("Project.toml"), "name = \"Proj\"\n");

    let ctx = EnvContext {
        workspace_root: ws,
        julia_project: None,
        julia_depot_path: None,
        home: None,
        julia_bindir: None,
        path: None,
    };
    let env = environment::resolve(&ctx).unwrap().unwrap();
    assert!(env.dev_package().is_none());
}

#[test]
fn dev_package_absent_for_default_env() {
    // Even if the newest default env carries a name, a bare shared environment
    // is never a package under development.
    let tmp = TempDir::new();
    let home = tmp.path().join("home");
    let envs = home.join(".julia/environments");
    write(&envs.join("v1.11/Project.toml"), "name = \"Shared\"\n");
    write(&envs.join("v1.11/src/Shared.jl"), "module Shared\nend\n");

    let ctx = EnvContext {
        workspace_root: tmp.path().join("empty/ws"),
        julia_project: None,
        julia_depot_path: None,
        home: Some(home),
        julia_bindir: None,
        path: None,
    };
    fs::create_dir_all(tmp.path().join("empty/ws")).unwrap();
    let env = environment::resolve(&ctx).unwrap().unwrap();
    assert_eq!(env.source, EnvSource::DefaultEnv);
    assert!(env.dev_package().is_none());
}

/// Manual end-to-end check against the developer's real depot. Ignored in CI
/// (depends on `~/.julia`); run with `cargo test --test environment -- --ignored`.
#[test]
#[ignore = "requires a populated ~/.julia"]
fn resolves_against_real_depot() {
    let tmp = TempDir::new();
    let ws = tmp.path().join("empty");
    fs::create_dir_all(&ws).unwrap();
    let ctx = EnvContext {
        // No project reachable from the workspace, forcing the default env.
        workspace_root: ws,
        julia_project: None,
        julia_depot_path: std::env::var("JULIA_DEPOT_PATH").ok(),
        home: std::env::var_os("HOME").map(PathBuf::from),
        julia_bindir: None,
        path: None,
    };
    let env = environment::resolve(&ctx)
        .unwrap()
        .expect("a default environment");
    assert_eq!(env.source, EnvSource::DefaultEnv);

    let resolved: Vec<_> = env
        .packages
        .iter()
        .filter(|p| p.kind == PackageKind::Registered && p.source.is_some())
        .collect();
    assert!(
        !resolved.is_empty(),
        "expected at least one registered package to resolve to a real slug dir"
    );
    for pkg in resolved {
        assert!(pkg.source.as_deref().unwrap().is_dir());
    }
}

#[test]
fn locate_install_via_bindir_override() {
    let tmp = TempDir::new();
    let prefix = tmp.path().join("julia");
    make_fake_install(&prefix, "1.11");

    let mut ctx = bare_ctx(tmp.path().to_path_buf());
    ctx.julia_bindir = Some(prefix.join("bin").to_string_lossy().into_owned());

    let install = environment::locate_install(&ctx, &[]).expect("install");
    assert_eq!(install.share, prefix.join("share/julia"));
    assert_eq!(install.base_dir, prefix.join("share/julia/base"));
    assert_eq!(install.stdlib_dir, prefix.join("share/julia/stdlib/v1.11"));
    assert_eq!(install.version, "1.11");
    assert_eq!(install.prefix, prefix);
}

#[test]
fn locate_install_via_path() {
    let tmp = TempDir::new();
    let prefix = tmp.path().join("julia");
    make_fake_install(&prefix, "1.10");

    let mut ctx = bare_ctx(tmp.path().to_path_buf());
    ctx.path = Some(prefix.join("bin").to_string_lossy().into_owned());

    let install = environment::locate_install(&ctx, &[]).expect("install");
    assert_eq!(install.version, "1.10");
    // `install_from_path` canonicalizes the resolved binary, so compare against a
    // canonicalized prefix (on macOS `/var` is a symlink to `/private/var`).
    let prefix = fs::canonicalize(&prefix).unwrap();
    assert_eq!(install.base_dir, prefix.join("share/julia/base"));
}

#[test]
fn locate_install_via_juliaup() {
    let tmp = TempDir::new();
    let depot = tmp.path().join("depot");
    let juliaup = depot.join("juliaup");
    make_fake_install(&juliaup.join("julia-1.11.2"), "1.11");
    write(
        &juliaup.join("juliaup.json"),
        r#"{
            "Default": "release",
            "InstalledChannels": { "release": { "Version": "1.11.2+0.x64.linux.gnu" } },
            "InstalledVersions": { "1.11.2+0.x64.linux.gnu": { "Path": "julia-1.11.2" } }
        }"#,
    );

    let ctx = bare_ctx(tmp.path().to_path_buf());
    let install = environment::locate_install(&ctx, std::slice::from_ref(&depot)).expect("install");
    assert_eq!(install.share, juliaup.join("julia-1.11.2/share/julia"));
    assert_eq!(install.version, "1.11");
}

#[test]
fn locate_install_picks_newest_stdlib_version() {
    let tmp = TempDir::new();
    let prefix = tmp.path().join("julia");
    make_fake_install(&prefix, "1.9");
    // A second, higher stdlib version directory should win.
    write(
        &prefix.join("share/julia/stdlib/v1.11/Bar/src/Bar.jl"),
        "module Bar\nend\n",
    );

    let mut ctx = bare_ctx(tmp.path().to_path_buf());
    ctx.julia_bindir = Some(prefix.join("bin").to_string_lossy().into_owned());
    let install = environment::locate_install(&ctx, &[]).expect("install");
    assert_eq!(install.version, "1.11");
}

#[test]
fn locate_install_none_without_base() {
    let tmp = TempDir::new();
    // A share dir with no base/Base.jl is not a usable installation.
    let prefix = tmp.path().join("julia");
    write(&prefix.join("share/julia/stdlib/v1.11/Foo/src/Foo.jl"), "");
    let mut ctx = bare_ctx(tmp.path().to_path_buf());
    ctx.julia_bindir = Some(prefix.join("bin").to_string_lossy().into_owned());
    assert!(environment::locate_install(&ctx, &[]).is_none());
}

#[test]
fn resolve_fills_stdlib_sources_from_install() {
    let tmp = TempDir::new();
    let ws = tmp.path().join("ws");
    let prefix = tmp.path().join("julia");
    make_fake_install(&prefix, "1.11");
    write(&ws.join("Project.toml"), "");
    write(
        &ws.join("Manifest.toml"),
        r#"
manifest_format = "2.0"

[[deps.Foo]]
uuid = "00000000-0000-0000-0000-000000000010"
"#,
    );

    let mut ctx = bare_ctx(ws.clone());
    ctx.julia_bindir = Some(prefix.join("bin").to_string_lossy().into_owned());
    let env = environment::resolve(&ctx).unwrap().expect("environment");

    let foo = env.packages.iter().find(|p| p.name == "Foo").unwrap();
    assert_eq!(foo.kind, PackageKind::Stdlib);
    assert_eq!(
        foo.source,
        Some(prefix.join("share/julia/stdlib/v1.11/Foo"))
    );
    assert!(env.install.is_some());
}

#[test]
fn returns_none_when_no_project_found() {
    let tmp = TempDir::new();
    let ws = tmp.path().join("empty");
    fs::create_dir_all(&ws).unwrap();
    let ctx = EnvContext {
        workspace_root: ws,
        julia_project: None,
        julia_depot_path: None,
        home: Some(tmp.path().join("nonexistent-home")),
        julia_bindir: None,
        path: None,
    };
    assert!(environment::resolve(&ctx).unwrap().is_none());
}
