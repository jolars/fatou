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
pub mod harvest;
pub mod model;
pub mod typeexpr;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::environment::Environment;

pub use base::{build_system_index, build_system_library};
pub use harvest::{harvest_entry, harvest_package, harvest_package_named};
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
    let mut lib = build_system_library(env.install.as_ref());
    for package in &env.packages {
        let Some(source) = &package.source else {
            continue;
        };
        let index = harvest_package_named(source, &package.name);
        lib.packages.insert(package.name.clone(), Arc::new(index));
        lib.roots.insert(package.name.clone(), source.clone());
    }
    // The package under development, indexed like a depot package so its
    // top-level symbols resolve across its own files. Registered last so it
    // wins the name slot over any same-named dependency.
    if let Some(dev) = env.dev_package() {
        lib.packages
            .insert(dev.name.clone(), Arc::new(harvest_workspace(&dev)));
        lib.roots.insert(dev.name.clone(), dev.root);
        lib.workspaces = vec![dev.name];
    }
    lib
}

/// Harvest just the package under development into a fresh [`PackageIndex`].
/// Split out so the language server can re-run it on save without re-resolving
/// the whole environment.
pub fn harvest_workspace(dev: &crate::environment::DevPackage) -> PackageIndex {
    harvest_package_named(&dev.root, &dev.name)
}
