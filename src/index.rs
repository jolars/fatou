//! The package index: a structured, serializable view of a Julia package's
//! public API, harvested from its source with fatou's own parser.
//!
//! [`harvest_package`] parses a package's `src/` entry file, follows static
//! `include()` chains to assemble the module tree, and extracts exported and
//! `public` names, function signatures grouped by name (multiple dispatch),
//! struct/abstract/primitive types with supertypes, consts, macros, and
//! docstrings — each as a [`model`] value stamped with a source location. The
//! result feeds the [`LibraryIndex`](crate::incremental::LibraryIndex) salsa
//! input, which later completion, hover, and go-to-definition read.

pub mod base;
pub mod cache;
pub mod harvest;
pub mod model;
pub mod typeexpr;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::environment::Environment;

pub use base::{build_system_index, build_system_library, build_system_library_cached};
pub use cache::{CacheKey, IndexCache};
pub use harvest::{harvest_entry, harvest_package, harvest_package_named, harvest_tree};
pub use model::{
    ConstDef, DefLocation, Docstring, ExportedName, Field, FunctionGroup, HarvestDiagnostic,
    MacroDef, Method, ModuleIndex, PackageIndex, Param, Span, TypeDef, TypeKind, Visibility,
};
pub use typeexpr::TypeExpr;

/// A harvested library keyed by package name, paired with each package's
/// absolute source root. The roots let go-to-definition join a package-relative
/// [`DefLocation`] with the on-disk directory to reach the real source file;
/// they live here rather than in the serializable [`PackageIndex`] model, which
/// is deliberately depot-independent.
#[derive(Debug, Clone, Default)]
pub struct HarvestedLibrary {
    pub packages: BTreeMap<String, Arc<PackageIndex>>,
    pub roots: BTreeMap<String, PathBuf>,
    /// The packages under development, one per workspace folder that is a
    /// package project: their names, sorted, each keying an entry in
    /// `packages`/`roots`. Unlike the read-only depot packages, a workspace
    /// package's files are edited live, so its symbols also resolve as the
    /// enclosing module's globals (see [`Resolver`](crate::resolve::Resolver))
    /// and it is re-harvested on save.
    pub workspaces: Vec<String>,
}

/// Harvest a whole resolved environment: Base/Core/stdlib from its located
/// installation (or the baked-in fallback), plus every manifest package with a
/// resolved source root. Best-effort — a package whose source is unknown or
/// unreadable is skipped (its own harvest records any diagnostics).
pub fn harvest_library(env: &Environment) -> HarvestedLibrary {
    harvest_libraries(std::slice::from_ref(env))
}

/// The packages under development across `envs` (one workspace folder each):
/// each environment's [`Environment::dev_package`], deduped by name with the
/// first (folder-order) winning. The loser folder's files simply stay
/// non-members — the library map is name-keyed, so two same-named live
/// packages cannot coexist.
pub fn dev_packages(envs: &[Environment]) -> Vec<crate::environment::DevPackage> {
    let mut out: Vec<crate::environment::DevPackage> = Vec::new();
    for env in envs {
        let Some(dev) = env.dev_package() else {
            continue;
        };
        if out.iter().any(|d| d.name == dev.name) {
            continue; // First folder wins the name slot.
        }
        out.push(dev);
    }
    out
}

/// Harvest several resolved environments (one per workspace folder) into one
/// merged library. The system index is harvested once, from the first
/// environment with a located installation; depot packages merge by name with
/// the first environment winning a conflict (the map is name-keyed, so two
/// pinned versions of one package cannot coexist — a documented limitation);
/// each folder's dev package is registered last, winning the name slot over
/// any same-named dependency, exactly as in the single-environment harvest.
pub fn harvest_libraries(envs: &[Environment]) -> HarvestedLibrary {
    let install = envs.iter().find_map(|env| env.install.as_ref());
    let mut lib = build_system_library(install);
    // Names claimed by an earlier environment's manifest: a later environment
    // pinning the same package (possibly at another version) is skipped. Within
    // one environment a manifest package still overwrites a same-named system
    // entry, as in the single-environment harvest.
    let mut claimed = std::collections::BTreeSet::new();
    for env in envs {
        for package in &env.packages {
            let Some(source) = &package.source else {
                continue;
            };
            if !claimed.insert(package.name.clone()) {
                continue; // First environment wins the name slot.
            }
            let index = harvest_package_named(source, &package.name);
            lib.packages.insert(package.name.clone(), Arc::new(index));
            lib.roots.insert(package.name.clone(), source.clone());
        }
    }
    // The packages under development, indexed like depot packages so their
    // top-level symbols resolve across their own files.
    for dev in dev_packages(envs) {
        lib.packages
            .insert(dev.name.clone(), Arc::new(harvest_workspace(&dev)));
        lib.roots.insert(dev.name.clone(), dev.root);
        lib.workspaces.push(dev.name);
    }
    lib.workspaces.sort();
    lib
}

/// [`harvest_libraries`], harvested in parallel on `pool` and reading each
/// package from the on-disk `cache` first. The system library goes through
/// [`build_system_library_cached`]; registered packages are deduped by name
/// exactly as the sequential path, then harvested concurrently — each a cache
/// hit (reload) or miss (harvest and store), keyed by `git-tree-sha1`
/// ([`CacheKey::for_package`]). Dev packages are never cached (edited live) and
/// are harvested as before. The name-keyed [`BTreeMap`] makes the result
/// independent of completion order, so it equals the sequential harvest.
///
/// `cache` is `None` when no cache directory could be resolved: harvesting is
/// then still parallel, just uncached.
pub fn harvest_libraries_parallel(
    envs: &[Environment],
    cache: Option<&IndexCache>,
    pool: &rayon::ThreadPool,
) -> HarvestedLibrary {
    use rayon::prelude::*;

    let install = envs.iter().find_map(|env| env.install.as_ref());
    let mut lib = build_system_library_cached(install, cache, pool);

    // Registered packages with a resolved source, deduped by name with the first
    // environment winning — the same claim rule as `harvest_libraries`.
    let mut claimed = std::collections::BTreeSet::new();
    let mut work: Vec<(&str, &std::path::Path, &crate::environment::Package)> = Vec::new();
    for env in envs {
        for package in &env.packages {
            let Some(source) = &package.source else {
                continue;
            };
            if !claimed.insert(package.name.clone()) {
                continue; // First environment wins the name slot.
            }
            work.push((&package.name, source, package));
        }
    }

    // A registered package harvested here overwrites any same-named system entry,
    // exactly as the sequential path inserts after the system harvest.
    let harvested: Vec<(String, Arc<PackageIndex>, PathBuf)> = pool.install(|| {
        work.par_iter()
            .map(|&(name, source, package)| {
                let index = harvest_package_cached(name, source, package, cache);
                (name.to_string(), index, source.to_path_buf())
            })
            .collect()
    });
    for (name, index, root) in harvested {
        lib.packages.insert(name.clone(), index);
        lib.roots.insert(name, root);
    }

    for dev in dev_packages(envs) {
        lib.packages
            .insert(dev.name.clone(), Arc::new(harvest_workspace(&dev)));
        lib.roots.insert(dev.name.clone(), dev.root);
        lib.workspaces.push(dev.name);
    }
    lib.workspaces.sort();
    lib
}

/// Harvest one registered package, reading it from `cache` when present (keyed by
/// its content identity) and storing a fresh harvest back. A package with no
/// cache key ([`CacheKey::for_package`] is `None`) is always harvested live.
fn harvest_package_cached(
    name: &str,
    source: &std::path::Path,
    package: &crate::environment::Package,
    cache: Option<&IndexCache>,
) -> Arc<PackageIndex> {
    let key = CacheKey::for_package(package);
    if let (Some(cache), Some(key)) = (cache, key.as_ref())
        && let Some(index) = cache.load(name, key)
    {
        return Arc::new(index);
    }
    let index = harvest_package_named(source, name);
    if let (Some(cache), Some(key)) = (cache, key.as_ref()) {
        cache.store(name, key, &index);
    }
    Arc::new(index)
}

/// Harvest just the package under development into a fresh [`PackageIndex`].
/// Split out so the language server can re-run it on save without re-resolving
/// the whole environment.
pub fn harvest_workspace(dev: &crate::environment::DevPackage) -> PackageIndex {
    harvest_package_named(&dev.root, &dev.name)
}
