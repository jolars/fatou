//! The Base/Core/stdlib system index: the implicitly-available surface that
//! nearly every free identifier in Julia code resolves against.
//!
//! When a [`JuliaInstall`] is located, [`build_system_index`] harvests Base
//! (`base/Base.jl`), Core (`base/boot.jl`), and every standard-library package
//! (`stdlib/vX.Y/<Name>`) with fatou's own parser, exactly like a depot
//! package. When no installation is found, it synthesizes minimal `Base`/`Core`
//! indexes from a baked-in export list (arity's `StaticBaseR` analog) so name
//! resolution still has a floor instead of flagging every builtin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::environment::JuliaInstall;

use super::HarvestedLibrary;
use super::cache::{CacheKey, IndexCache};
use super::harvest::{harvest_entry, harvest_package_named};
use super::model::{DefLocation, ExportedName, ModuleIndex, PackageIndex, Span, Visibility};

/// Base's harvest entry files. Julia 1.12 split the `module Base` opener and
/// `exports.jl`/`public.jl` into `Base_compiler.jl`, leaving the bulk API in
/// `Base.jl`; older releases keep everything in `Base.jl`. Harvest whichever
/// exist and merge, so both layouts yield a complete export surface.
const BASE_ENTRIES: &[&str] = &["Base_compiler.jl", "Base.jl"];

/// The baked-in fallback export lists, snapshots of a real Base/Core (one name
/// per line; blank lines and `#` comments ignored). Regenerate from a real Julia
/// install by dumping `sort(names(Base))` / `sort(names(Core))` (dropping gensym
/// names containing `#`) into these files.
const BASE_FALLBACK: &str = include_str!("fallback/base_exports.txt");
const CORE_FALLBACK: &str = include_str!("fallback/core_exports.txt");

/// Build the Base/Core/stdlib index for `install`, keyed by module name. Falls
/// back to the baked-in export list when `install` is `None`. Drops the source
/// roots; callers that need go-to-definition into system sources use
/// [`build_system_library`].
pub fn build_system_index(install: Option<&JuliaInstall>) -> BTreeMap<String, Arc<PackageIndex>> {
    build_system_library(install).packages
}

/// Build the Base/Core/stdlib index for `install` along with each package's
/// absolute source root (empty for the baked-in fallback, which has no on-disk
/// sources). Sequential and uncached; the language server uses
/// [`build_system_library_cached`] to harvest in parallel off the on-disk cache.
pub fn build_system_library(install: Option<&JuliaInstall>) -> HarvestedLibrary {
    match install {
        Some(install) => assemble(
            system_entries(install)
                .iter()
                .map(|work| harvest_system_one(work, install, None))
                .collect(),
        ),
        None => fallback_library(),
    }
}

/// [`build_system_library`], but harvesting each Base/Core/stdlib package in
/// parallel on `pool` and consulting `cache` first: a warm cache reloads each
/// index instead of re-parsing (Base and the stdlib are the largest harvest), a
/// miss harvests and stores. Keyed by the install version
/// ([`CacheKey::for_system`]), so a Julia upgrade retires the old entries.
pub fn build_system_library_cached(
    install: Option<&JuliaInstall>,
    cache: Option<&IndexCache>,
    pool: &rayon::ThreadPool,
) -> HarvestedLibrary {
    match install {
        Some(install) => {
            let work = system_entries(install);
            let results = pool.install(|| {
                work.par_iter()
                    .map(|item| harvest_system_one(item, install, cache))
                    .collect()
            });
            assemble(results)
        }
        None => fallback_library(),
    }
}

/// How one system package is harvested from the installation's plain sources.
enum SystemKind {
    /// Base, merged from every present [`BASE_ENTRIES`] file under `base/`.
    Base,
    /// Core, from `base/boot.jl`.
    Core,
    /// A stdlib package, through the ordinary `src/<Name>.jl` entry.
    Stdlib,
}

/// One system package to harvest: its name, source root, and how to enter it.
struct SystemWork {
    name: String,
    root: PathBuf,
    kind: SystemKind,
}

/// The full list of system packages to harvest for `install`: Base and Core
/// (rooted at `base/`, since their `DefLocation`s are relative to it), then every
/// installed stdlib package, each `using`-able regardless of the manifest.
fn system_entries(install: &JuliaInstall) -> Vec<SystemWork> {
    let mut work = vec![
        SystemWork {
            name: "Base".to_string(),
            root: install.base_dir.clone(),
            kind: SystemKind::Base,
        },
        SystemWork {
            name: "Core".to_string(),
            root: install.base_dir.clone(),
            kind: SystemKind::Core,
        },
    ];
    if let Ok(entries) = std::fs::read_dir(&install.stdlib_dir) {
        for entry in entries.filter_map(Result::ok) {
            let dir = entry.path();
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if dir.join("src").join(format!("{name}.jl")).is_file() {
                work.push(SystemWork {
                    name: name.to_string(),
                    root: dir,
                    kind: SystemKind::Stdlib,
                });
            }
        }
    }
    work
}

/// Harvest one system package, reading it from `cache` when present and storing a
/// fresh harvest back. Returns the name, index, and source root for [`assemble`].
fn harvest_system_one(
    work: &SystemWork,
    install: &JuliaInstall,
    cache: Option<&IndexCache>,
) -> (String, Arc<PackageIndex>, PathBuf) {
    let key = CacheKey::for_system(install);
    if let Some(index) = cache.and_then(|cache| cache.load(&work.name, &key)) {
        return (work.name.clone(), Arc::new(index), work.root.clone());
    }
    let index = match work.kind {
        SystemKind::Base => harvest_base(&work.root),
        SystemKind::Core => harvest_entry(&work.root, &work.root.join("boot.jl"), "Core"),
        SystemKind::Stdlib => harvest_package_named(&work.root, &work.name),
    };
    if let Some(cache) = cache {
        cache.store(&work.name, &key, &index);
    }
    (work.name.clone(), Arc::new(index), work.root.clone())
}

/// Fold harvested `(name, index, root)` triples into a [`HarvestedLibrary`].
fn assemble(results: Vec<(String, Arc<PackageIndex>, PathBuf)>) -> HarvestedLibrary {
    let mut packages = BTreeMap::new();
    let mut roots = BTreeMap::new();
    for (name, index, root) in results {
        packages.insert(name.clone(), index);
        roots.insert(name, root);
    }
    HarvestedLibrary {
        packages,
        roots,
        workspaces: Vec::new(),
        declared_deps: BTreeMap::new(),
        project_files: BTreeMap::new(),
        deps: BTreeMap::new(),
    }
}

/// The baked-in fallback library, used when no installation is located.
fn fallback_library() -> HarvestedLibrary {
    HarvestedLibrary {
        packages: fallback_system(),
        roots: BTreeMap::new(),
        workspaces: Vec::new(),
        declared_deps: BTreeMap::new(),
        project_files: BTreeMap::new(),
        deps: BTreeMap::new(),
    }
}

/// Harvest Base by merging every present [`BASE_ENTRIES`] file into one index.
fn harvest_base(base_dir: &Path) -> PackageIndex {
    let mut merged: Option<PackageIndex> = None;
    for entry in BASE_ENTRIES {
        let file = base_dir.join(entry);
        if !file.is_file() {
            continue;
        }
        let index = harvest_entry(base_dir, &file, "Base");
        match &mut merged {
            None => merged = Some(index),
            Some(acc) => merge_index(acc, index),
        }
    }
    // No entry present: harvest the canonical name for a proper missing-file
    // diagnostic rather than a silent empty index.
    merged.unwrap_or_else(|| harvest_entry(base_dir, &base_dir.join(BASE_ENTRIES[0]), "Base"))
}

/// Fold `other`'s root items and diagnostics into `acc`. Function methods merge
/// into the existing `(name, owner)` group; other kinds concatenate (duplicate
/// exports/types are harmless for name resolution).
fn merge_index(acc: &mut PackageIndex, other: PackageIndex) {
    let root = &mut acc.root;
    if root.doc.is_none() {
        root.doc = other.root.doc.clone();
    }
    root.exports.extend(other.root.exports);
    root.types.extend(other.root.types);
    root.consts.extend(other.root.consts);
    root.macros.extend(other.root.macros);
    root.submodules.extend(other.root.submodules);
    for group in other.root.functions {
        match root
            .functions
            .iter_mut()
            .find(|g| g.name == group.name && g.owner == group.owner)
        {
            Some(existing) => {
                if existing.doc.is_none() {
                    existing.doc = group.doc.clone();
                }
                existing.methods.extend(group.methods);
            }
            None => root.functions.push(group),
        }
    }
    acc.diagnostics.extend(other.diagnostics);
}

/// Synthesize `Base` and `Core` from the baked-in export lists.
fn fallback_system() -> BTreeMap<String, Arc<PackageIndex>> {
    let mut out = BTreeMap::new();
    out.insert(
        "Base".to_string(),
        Arc::new(synthetic_index("Base", BASE_FALLBACK)),
    );
    out.insert(
        "Core".to_string(),
        Arc::new(synthetic_index("Core", CORE_FALLBACK)),
    );
    out
}

/// A minimal [`PackageIndex`] whose root module exports exactly the names in
/// `list` (one per line; blanks and `#` comments skipped). Carries no
/// signatures or types — enough to answer "is this name exported by Base/Core?".
fn synthetic_index(name: &str, list: &str) -> PackageIndex {
    let loc = DefLocation {
        file: PathBuf::new(),
        range: Span { start: 0, end: 0 },
    };
    let exports = list
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|export| ExportedName {
            name: export.to_string(),
            visibility: Visibility::Exported,
            loc: loc.clone(),
        })
        .collect();

    PackageIndex {
        name: name.to_string(),
        root: ModuleIndex {
            name: name.to_string(),
            // Core is a `baremodule`; Base is a normal module.
            bare: name == "Core",
            loc,
            doc: None,
            exports,
            functions: Vec::new(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
            usings: Vec::new(),
            imported_names: Vec::new(),
        },
        members: Vec::new(),
        member_modules: Default::default(),
        diagnostics: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_populates_base_and_core_exports() {
        let system = build_system_index(None);
        let base = system.get("Base").expect("Base index");
        let core = system.get("Core").expect("Core index");

        let base_names: Vec<&str> = base.root.exports.iter().map(|e| e.name.as_str()).collect();
        assert!(
            base_names.contains(&"println"),
            "Base should export println"
        );
        assert!(base_names.contains(&"map"), "Base should export map");
        assert!(base_names.contains(&"push!"), "Base should export push!");

        let core_names: Vec<&str> = core.root.exports.iter().map(|e| e.name.as_str()).collect();
        assert!(core_names.contains(&"Int"), "Core should export Int");
        assert!(core_names.contains(&"Type"), "Core should export Type");

        assert!(core.root.bare, "Core is a baremodule");
        assert!(!base.root.bare, "Base is a normal module");
    }

    #[test]
    fn synthetic_index_skips_blanks_and_comments() {
        let index = synthetic_index("Base", "# a comment\n\nfoo\n  bar  \n");
        let names: Vec<&str> = index.root.exports.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["foo", "bar"]);
    }

    #[test]
    fn merging_method_groups_promotes_the_first_available_doc() {
        let parsed = crate::parser::parse("\"docs\"\nf(x) = x\n");
        let documented = super::super::harvest::harvest_tree(&parsed.cst);
        let mut first = synthetic_index("Base", "");
        first
            .root
            .functions
            .push(super::super::model::FunctionGroup {
                name: "f".to_string(),
                owner: None,
                methods: Vec::new(),
                doc: None,
            });
        let second = PackageIndex {
            name: "Base".to_string(),
            root: documented,
            members: Vec::new(),
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        };

        merge_index(&mut first, second);
        let group = first
            .root
            .functions
            .iter()
            .find(|group| group.name == "f")
            .unwrap();
        assert_eq!(group.doc.as_ref().unwrap().text, "docs");
        assert_eq!(group.methods[0].doc.as_ref().unwrap().text, "docs");
    }
}
