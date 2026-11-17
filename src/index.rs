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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::environment::{Environment, PackageMeta};

pub use base::{build_system_index, build_system_library, build_system_library_cached};
pub use cache::{CacheKey, IndexCache};
pub use harvest::{
    harvest_entry, harvest_entry_with_options, harvest_package, harvest_package_named,
    harvest_package_with_options, harvest_tree,
};
pub use model::{
    ConstDef, DefLocation, Docstring, ExportedName, Field, FunctionGroup, HarvestDiagnostic,
    MacroDef, Method, ModuleIndex, ModuleUsing, PackageIndex, Param, Span, TypeDef, TypeKind,
    Visibility,
};
pub use typeexpr::TypeExpr;

/// A harvested library keyed by package name, paired with each package's
/// absolute source root. The roots let go-to-definition join a package-relative
/// [`DefLocation`] with the on-disk directory to reach the real source file;
/// they live here rather than in the serializable [`PackageIndex`] model, which
/// is deliberately depot-independent.
#[derive(Debug, Clone, Default)]
pub struct HarvestedLibrary {
    /// Runtime loading contexts and edition-specific indexes. Paths stay out of
    /// the serializable, depot-independent `PackageIndex` model.
    pub syntax: LibrarySyntax,
    pub packages: BTreeMap<String, Arc<PackageIndex>>,
    pub roots: BTreeMap<String, PathBuf>,
    /// The packages under development, one per workspace folder that is a
    /// package project: their names, sorted, each keying an entry in
    /// `packages`/`roots`. Unlike the read-only depot packages, a workspace
    /// package's files are edited live, so its symbols also resolve as the
    /// enclosing module's globals (see [`Resolver`](crate::resolve::Resolver))
    /// and it is re-harvested on save.
    pub workspaces: Vec<String>,
    /// Each workspace package's declared direct dependencies — the names in its
    /// project's `Project.toml` `[deps]` — keyed by the package's name (an entry
    /// of [`workspaces`](Self::workspaces)).
    ///
    /// This is what a package's own `using`/`import` clauses may name, and it is
    /// deliberately *not* derived from the harvest: a project that has never
    /// been instantiated (no `Manifest.toml`), or one whose Julia installation
    /// fatou could not locate, still declares its dependencies here. The
    /// linter's `unresolved-import` reads it; a folder that is not a package
    /// project contributes no entry.
    ///
    /// The CLI's copy of what the language server derives from the project file
    /// itself ([`project_declared_deps`](crate::incremental::project_declared_deps)):
    /// `fatou lint` has no salsa database, and no editor buffer to be stale
    /// against.
    pub declared_deps: BTreeMap<String, Arc<DeclaredDeps>>,
    /// Each workspace package's project file, keyed the same way as
    /// [`declared_deps`](Self::declared_deps). The language server tracks these
    /// paths as salsa inputs so an *unsaved* `[deps]` edit reaches
    /// `unresolved-import` without waiting for a save and a re-resolve.
    pub project_files: BTreeMap<String, PathBuf>,
    /// Every manifest-pinned package's version and kind, keyed by name.
    ///
    /// Kept beside [`roots`](Self::roots) rather than folded into it because
    /// the two have different key sets on purpose: `roots` names what was
    /// *harvested*, this names what the manifest *pinned*. A standard library
    /// with no located Julia install, or a registered package whose depot slug
    /// is missing, has an entry here and none there — and "installed, version
    /// 0.4.5, source not found" is exactly what a reader of that project file
    /// wants told.
    pub deps: BTreeMap<String, PackageMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSyntax {
    pub root: PathBuf,
    pub source: crate::parse_context::EditionSource,
    pub options: crate::parser::ParseOptions,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibrarySyntax {
    pub files: BTreeMap<PathBuf, Vec<crate::parse_context::SourceContext>>,
    pub unindexed_options: Vec<(
        crate::parse_context::SourceContext,
        crate::parser::ParseOptions,
    )>,
    /// Extension entries and checkouts with no slot in the name-keyed index.
    pub unindexed_files: BTreeMap<PathBuf, Vec<crate::parse_context::SourceContext>>,
    /// Loading roots and membership survive a failed manifest resolve only
    /// to prevent edits under the wrong grammar.
    pub failed_manifest_files: BTreeMap<PathBuf, Vec<crate::parse_context::SourceContext>>,
    pub packages: BTreeMap<String, PackageSyntax>,
    pub variants: BTreeMap<(String, crate::parser::ParseOptions), Arc<PackageIndex>>,
    /// Project and manifest paths both identify an environment. Name masking
    /// keeps the library's existing first-environment precedence.
    pub environments: BTreeMap<PathBuf, BTreeMap<String, crate::parse_context::EditionSource>>,
}

/// The module names a package's own source files may `using`/`import`: its
/// project's `Project.toml` `[deps]`. Stdlib and Base/Core names are *not*
/// folded in — a consumer answers those from the harvested library, which knows
/// what the located installation actually ships.
pub type DeclaredDeps = std::collections::BTreeSet<String>;

impl PackageSyntax {
    pub(crate) fn entry(&self, name: &str) -> PathBuf {
        crate::incremental::normalize_path(&self.root.join(match name {
            "Base" => "Base.jl".to_string(),
            "Core" => "boot.jl".to_string(),
            _ => format!("src/{name}.jl"),
        }))
    }
}

impl LibrarySyntax {
    pub(crate) fn retain_failed_manifest_memberships(
        &mut self,
        previous: &Self,
        failed: &std::collections::BTreeSet<PathBuf>,
    ) {
        self.retain_failed_manifest_files(&previous.files, failed);
        self.retain_failed_manifest_files(&previous.failed_manifest_files, failed);
    }

    pub(crate) fn retain_failed_manifest_files(
        &mut self,
        files: &BTreeMap<PathBuf, Vec<crate::parse_context::SourceContext>>,
        failed: &std::collections::BTreeSet<PathBuf>,
    ) {
        if failed.is_empty() {
            return;
        }
        for (path, contexts) in files {
            for context in contexts {
                if matches!(&context.source, crate::parse_context::EditionSource::Manifest { path, .. } if failed.contains(path))
                {
                    let retained = self.failed_manifest_files.entry(path.clone()).or_default();
                    if !retained.contains(context) {
                        retained.push(context.clone());
                    }
                }
            }
        }
    }

    /// All cached variants participate in membership discovery. Selection at
    /// query time filters each context to its currently active variant, so a
    /// metadata edit can switch to a cached edition without another harvest.
    pub(crate) fn rebuild_memberships(&mut self) {
        let mut files = self.unindexed_files.clone();
        for (name, package) in &self.packages {
            let mut sources = vec![package.source.clone()];
            for source in self
                .environments
                .values()
                .filter_map(|packages| packages.get(name))
            {
                if !sources.contains(source) {
                    sources.push(source.clone());
                }
            }
            for ((variant_name, _), index) in &self.variants {
                if variant_name != name {
                    continue;
                }
                for member in &index.members {
                    let contexts = files
                        .entry(crate::incremental::normalize_path(
                            &package.root.join(member),
                        ))
                        .or_default();
                    for source in &sources {
                        let context = crate::parse_context::SourceContext {
                            entry: package.entry(name),
                            source: source.clone(),
                        };
                        if !contexts.contains(&context) {
                            contexts.push(context);
                        }
                    }
                }
            }
        }
        self.files = files;
    }
}

/// Only the effective environment and enclosing workspace affect a lint view.
/// Standalone files in the same environment can share one view.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LibraryViewKey {
    environment: Option<PathBuf>,
    workspace: Option<String>,
}

impl HarvestedLibrary {
    /// A disk-backed CLI view of the selected environment. The LSP derives the
    /// same view from tracked metadata in `library_package_for_context`.
    pub fn in_context(
        &self,
        context: &crate::parse_context::SourceContext,
        override_edition: Option<crate::parser::SyntaxEdition>,
    ) -> Self {
        let buffers = BTreeMap::new();
        let mut editions = crate::parse_context::DiskEditionSelections::new(&buffers);
        self.lint_view(
            &self.lint_view_key(context),
            override_edition,
            &mut editions,
        )
    }

    pub(crate) fn lint_view_key(
        &self,
        context: &crate::parse_context::SourceContext,
    ) -> LibraryViewKey {
        LibraryViewKey {
            environment: context
                .source
                .path()
                .filter(|path| self.syntax.environments.contains_key(*path))
                .map(PathBuf::from),
            workspace: self
                .workspaces
                .iter()
                .find(|name| {
                    self.roots.get(*name).is_some_and(|root| {
                        crate::incremental::normalize_path(
                            &root.join("src").join(format!("{name}.jl")),
                        ) == context.entry
                    })
                })
                .cloned(),
        }
    }

    pub(crate) fn lint_view(
        &self,
        key: &LibraryViewKey,
        override_edition: Option<crate::parser::SyntaxEdition>,
        editions: &mut crate::parse_context::DiskEditionSelections<'_>,
    ) -> Self {
        let mut view = self.clone();
        if self.syntax.packages.is_empty() {
            return view;
        }
        for (name, package) in &self.syntax.packages {
            let source = key
                .environment
                .as_deref()
                .and_then(|path| self.syntax.environments.get(path))
                .and_then(|packages| packages.get(name))
                .unwrap_or(&package.source);
            let source_override = override_edition.filter(|_| key.workspace.as_ref() == Some(name));
            let overridden = source_override.map(crate::parse_context::EditionSelection::known);
            let selected = overridden
                .as_ref()
                .unwrap_or_else(|| editions.select(source));
            match selected
                .certain
                .then(|| self.syntax.variants.get(&(name.clone(), selected.options)))
                .flatten()
            {
                Some(index) => {
                    view.packages.insert(name.clone(), Arc::clone(index));
                }
                None => {
                    view.packages.remove(name);
                }
            }
        }
        view.workspaces
            .retain(|name| key.workspace.as_ref() == Some(name));
        view
    }

    /// The workspace package and host module path for `path`, when it is a
    /// source file of one of the dev packages (else `None`). The CLI counterpart
    /// of the language server's
    /// [`workspace_member`](crate::incremental::Analysis::workspace_member): the
    /// longest matching `src/` prefix picks the package, and the host module
    /// comes from the harvester's own
    /// [`member_modules`](crate::index::PackageIndex::member_modules) record
    /// (held in lockstep with the salsa include graph by a parity test), so no
    /// graph rebuild is needed. Lets `fatou lint` resolve cross-file names
    /// exactly as the server does.
    pub fn workspace_member(
        &self,
        path: &std::path::Path,
    ) -> Option<(Arc<PackageIndex>, crate::resolve::ModulePath)> {
        use crate::incremental::normalize_path;
        let path = normalize_path(path);
        // Longest matching `src/` prefix wins, so a nested package folder claims
        // its own files (mirrors `incremental::workspace_package_for`).
        let mut best: Option<(usize, &String)> = None;
        for name in &self.workspaces {
            let Some(root) = self.roots.get(name) else {
                continue;
            };
            let src = normalize_path(&root.join("src"));
            let in_closure = self.packages.get(name).is_some_and(|package| {
                package
                    .members
                    .iter()
                    .any(|member| normalize_path(&root.join(member)) == path)
            });
            if !in_closure {
                continue;
            }
            let depth = if path.starts_with(&src) {
                src.components().count()
            } else {
                0
            };
            if best.is_none_or(|(d, _)| depth > d) {
                best = Some((depth, name));
            }
        }
        let (_, name) = best?;
        let pkg = self.packages.get(name)?;
        let root = normalize_path(self.roots.get(name)?);
        // The file's package-relative path keys its host module in the harvest.
        // A file outside the include closure has no entry: not a member.
        let rel = pkg
            .members
            .iter()
            .find(|member| normalize_path(&root.join(member)) == path)?;
        let host = pkg.member_modules.get(rel)?;
        Some((
            Arc::clone(pkg),
            host.iter().map(smol_str::SmolStr::new).collect(),
        ))
    }
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

/// Each workspace package's declared direct dependencies (its project's
/// `Project.toml` `[deps]`), keyed by package name. Deduped by name with the
/// first folder winning, exactly like [`dev_packages`], so the map and the
/// workspace list agree on which environment owns a name.
fn declared_deps(envs: &[Environment]) -> BTreeMap<String, Arc<DeclaredDeps>> {
    let mut out: BTreeMap<String, Arc<DeclaredDeps>> = BTreeMap::new();
    for env in envs {
        let Some(dev) = env.dev_package() else {
            continue;
        };
        out.entry(dev.name)
            .or_insert_with(|| Arc::new(env.declared_deps.clone()));
    }
    out
}

/// Every manifest-pinned package's version and kind across `envs`, first
/// environment winning a name — the same claim rule the harvest itself uses,
/// so the two agree on which environment a name came from.
///
/// Unfiltered by whether the package's source was found: an unlocatable
/// package is still pinned, and saying so is the point.
fn dep_meta(envs: &[Environment]) -> BTreeMap<String, PackageMeta> {
    let mut out: BTreeMap<String, PackageMeta> = BTreeMap::new();
    for env in envs {
        for package in &env.packages {
            out.entry(package.name.clone())
                .or_insert_with(|| package.meta());
        }
    }
    out
}

/// Each workspace package's project file, deduped by name exactly like
/// [`declared_deps`] so the two maps name the same environment for a name.
fn project_files(envs: &[Environment]) -> BTreeMap<String, PathBuf> {
    let mut out: BTreeMap<String, PathBuf> = BTreeMap::new();
    for env in envs {
        let Some(dev) = env.dev_package() else {
            continue;
        };
        out.entry(dev.name)
            .or_insert_with(|| env.project_file.clone());
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
            if package.kind == crate::environment::PackageKind::Stdlib {
                continue; // Already harvested from the installation's own project.
            }
            let index = harvest_package_with_options(
                source,
                &package.name,
                crate::parser::ParseOptions {
                    syntax_edition: package.syntax_edition,
                },
            );
            lib.packages.insert(package.name.clone(), Arc::new(index));
            lib.roots.insert(package.name.clone(), source.clone());
        }
    }
    // The packages under development, indexed like depot packages so their
    // top-level symbols resolve across their own files.
    let projects = project_files(envs);
    for dev in dev_packages(envs) {
        lib.packages.insert(
            dev.name.clone(),
            Arc::new(harvest_workspace_for_project(
                &dev,
                projects.get(&dev.name).map(PathBuf::as_path),
            )),
        );
        lib.roots.insert(dev.name.clone(), dev.root);
        lib.workspaces.push(dev.name);
    }
    lib.workspaces.sort();
    lib.declared_deps = declared_deps(envs);
    lib.project_files = projects;
    lib.deps = dep_meta(envs);
    install_syntax(envs, &mut lib);
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
            if package.kind != crate::environment::PackageKind::Stdlib {
                work.push((&package.name, source, package));
            }
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

    let projects = project_files(envs);
    for dev in dev_packages(envs) {
        lib.packages.insert(
            dev.name.clone(),
            Arc::new(harvest_workspace_for_project(
                &dev,
                projects.get(&dev.name).map(PathBuf::as_path),
            )),
        );
        lib.roots.insert(dev.name.clone(), dev.root);
        lib.workspaces.push(dev.name);
    }
    lib.workspaces.sort();
    lib.declared_deps = declared_deps(envs);
    lib.project_files = projects;
    lib.deps = dep_meta(envs);
    install_syntax(envs, &mut lib);
    lib
}

/// Track syntax for every resolved pin. Only the checkout selected by name
/// contributes symbols to the library.
fn install_syntax(envs: &[Environment], lib: &mut HarvestedLibrary) {
    use crate::incremental::normalize_path;
    use crate::parse_context::{
        ContextCatalog, EditionSource, SourceContext, SourceRole, dependency_edition_source,
        workspace_package_entries,
    };
    use crate::parser::ParseOptions;

    // A manifest pin or workspace package can replace a bundled package.
    // An edition match alone cannot make the previous root's index reusable.
    lib.syntax.packages.retain(|name, package| {
        lib.roots
            .get(name)
            .is_some_and(|root| normalize_path(root) == normalize_path(&package.root))
    });
    lib.syntax
        .variants
        .retain(|(name, _), _| lib.syntax.packages.contains_key(name));
    let mut unindexed_roots = Vec::new();
    let buffers = BTreeMap::new();
    for env in envs {
        let project = normalize_path(&env.project_file);
        let manifest = env.manifest_file.as_deref().map(normalize_path);
        let mut sources = BTreeMap::new();
        for package in &env.packages {
            let Some(root) = &package.source else {
                continue;
            };
            let indexed = lib
                .roots
                .get(&package.name)
                .map(|root| normalize_path(root))
                == Some(normalize_path(root));
            let source = dependency_edition_source(package, root, manifest.as_deref());
            unindexed_roots.extend(package.extension_entries(root).map(|entry| SourceContext {
                entry,
                source: source.clone(),
            }));
            if !indexed {
                unindexed_roots.push(SourceContext {
                    entry: root.join("src").join(format!("{}.jl", package.name)),
                    source,
                });
                continue;
            }
            if package.kind == crate::environment::PackageKind::Stdlib {
                if let Some(installed) = lib.syntax.packages.get(&package.name) {
                    sources.insert(package.name.clone(), installed.source.clone());
                }
                continue;
            }
            let options = ParseOptions {
                syntax_edition: package.syntax_edition,
            };
            sources.insert(package.name.clone(), source.clone());
            if !lib.syntax.packages.contains_key(&package.name) {
                lib.syntax.variants.insert(
                    (package.name.clone(), options),
                    if lib.workspaces.contains(&package.name) {
                        Arc::new(harvest_package_with_options(root, &package.name, options))
                    } else {
                        Arc::clone(&lib.packages[&package.name])
                    },
                );
                lib.syntax.packages.insert(
                    package.name.clone(),
                    PackageSyntax {
                        root: normalize_path(root),
                        source: source.clone(),
                        options,
                    },
                );
            }
            lib.syntax
                .variants
                .entry((package.name.clone(), options))
                .or_insert_with(|| {
                    Arc::new(harvest_package_with_options(root, &package.name, options))
                });
        }
        if let Some(dev) = env.dev_package() {
            let indexed = lib.roots.get(&dev.name).map(|root| normalize_path(root))
                == Some(normalize_path(&dev.root));
            unindexed_roots.extend(
                workspace_package_entries(&project, &buffers)
                    .into_iter()
                    .skip(usize::from(indexed)),
            );
            if indexed {
                let source = EditionSource::Project {
                    path: project.clone(),
                    role: SourceRole::Package,
                };
                let selection = std::fs::read_to_string(&project)
                    .ok()
                    .as_deref()
                    .map(|text| source.select(Some(text)))
                    .unwrap_or_else(|| source.select(None));
                let index = Arc::clone(&lib.packages[&dev.name]);
                lib.syntax.packages.insert(
                    dev.name.clone(),
                    PackageSyntax {
                        root: normalize_path(&dev.root),
                        source: source.clone(),
                        options: selection.options,
                    },
                );
                lib.syntax
                    .variants
                    .insert((dev.name.clone(), selection.options), Arc::clone(&index));
                sources.insert(dev.name, source);
            }
        }
        lib.syntax.environments.insert(project, sources.clone());
        if let Some(manifest) = manifest {
            lib.syntax.environments.insert(manifest, sources);
        }
    }
    let catalog = ContextCatalog::load(&unindexed_roots, &buffers, None);
    lib.syntax.unindexed_options = catalog
        .files
        .iter()
        .flat_map(|(path, contexts)| {
            contexts
                .iter()
                .filter(|(context, _)| context.entry == *path)
                .map(|(context, selection)| (context.clone(), selection.options))
        })
        .collect();
    lib.syntax.unindexed_files = catalog
        .files
        .into_iter()
        .map(|(path, contexts)| {
            (
                path,
                contexts.into_iter().map(|(context, _)| context).collect(),
            )
        })
        .collect();
    lib.syntax.rebuild_memberships();
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
    let index = harvest_package_with_options(
        source,
        name,
        crate::parser::ParseOptions {
            syntax_edition: package.syntax_edition,
        },
    );
    if let (Some(cache), Some(key)) = (cache, key.as_ref()) {
        cache.store(name, key, &index);
    }
    Arc::new(index)
}

/// Harvest just the package under development into a fresh [`PackageIndex`].
/// Split out so the language server can re-run it on save without re-resolving
/// the whole environment.
pub fn harvest_workspace(dev: &crate::environment::DevPackage) -> PackageIndex {
    let project = crate::environment::project_file_in(&dev.root);
    harvest_workspace_for_project(dev, project.as_deref())
}

fn harvest_workspace_for_project(
    dev: &crate::environment::DevPackage,
    project: Option<&Path>,
) -> PackageIndex {
    use crate::parse_context::{EditionSource, SourceRole};

    let options = project.map_or_else(
        || {
            crate::parse_context::EditionSelection::known(crate::parser::SyntaxEdition::OLDEST)
                .options
        },
        |path| {
            let source = EditionSource::Project {
                path: path.to_path_buf(),
                role: SourceRole::Package,
            };
            source
                .select(std::fs::read_to_string(path).ok().as_deref())
                .options
        },
    );
    harvest_package_with_options(&dev.root, &dev.name, options)
}
