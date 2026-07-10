//! The one name-resolution/masking order every consumer shares.
//!
//! Completion, hover, and the future undefined-name lint must all agree on how
//! a bare identifier resolves. Julia layers four tiers, innermost wins:
//!
//! 1. **local scopes** — a binding reachable up the scope chain (a local,
//!    parameter, loop/`let` variable, or a file/module global);
//! 2. **explicit imports** — `import X`, `import X: a`, `using X: a`, which are
//!    themselves file bindings (kind [`BindingKind::Import`]), so tiers 1 and 2
//!    are together exactly "does the name bind in this file?" — already answered
//!    by the [`SemanticModel`];
//! 3. **`using`'d exports** — the `export`ed names of a whole-module `using X`,
//!    tried in source order (first `using` that exports the name wins);
//! 4. **Base/Core implicit** — the names every (non-bare) module gets for free.
//!
//! [`Resolver::resolve`] walks the tiers and returns the first hit;
//! [`Resolver::visible`] enumerates every visible name in the same order with
//! shadowed names dropped, for completion. Both read one [`SemanticModel`] and a
//! [`PackageSource`] (the harvested library), so the order lives in one place.
//!
//! Macros resolve in a parallel namespace ([`Namespace::Macro`]): `@time` never
//! resolves to a value `time`, matching the model's split.
//!
//! Deferred: relative (`using ..A`) and interpolated `using`s do not resolve
//! against the library (their target module is unknown here), and `baremodule`
//! bodies are still granted the implicit Base/Core tier — the model does not yet
//! distinguish `baremodule` from `module`.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::Arc;

use rowan::TextSize;
use smol_str::SmolStr;

use crate::index::{ModuleIndex, PackageIndex, Visibility};
use crate::semantic::{Access, Binding, BindingId, BindingKind, LoadKind, ScopeId, SemanticModel};

/// The namespace a name is resolved in. Julia keeps macros separate: `@time`
/// and a value `time` never resolve to one another. `Ord` so it can key the
/// reverse-occurrence index's per-name buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Namespace {
    Value,
    Macro,
}

/// Where a name resolved, in the shared masking order. [`Resolution::Binding`]
/// covers tiers 1 and 2 (inspect the binding's [`BindingKind`] to tell a local
/// from an import); the library tiers name the module the symbol came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A binding in this file: a local, parameter, global, or explicit import.
    Binding(BindingId),
    /// A top-level symbol of the enclosing workspace package's module, defined
    /// in one of its *other* files (tier 2 — the same-module globals an
    /// `include` splices in). `name` is bare for a value, `@`-prefixed for a
    /// macro; the consumer looks it up in the workspace package's module.
    Workspace { name: SmolStr },
    /// An `export`ed name brought in by a whole-module `using` (tier 3).
    Using { module: SmolStr, name: SmolStr },
    /// An implicitly available Base/Core name (tier 4).
    System { module: SmolStr, name: SmolStr },
    /// No tier provides the name.
    Unresolved,
}

/// One recorded occurrence of a workspace top-level symbol: its byte range, the
/// definition-vs-use flag, and how the site accesses the binding. The unit the
/// reverse-occurrence index unions across files (cross-file references/rename).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccurrenceRec {
    pub range: rowan::TextRange,
    pub is_def: bool,
    pub access: Access,
}

/// One name visible at a position, tagged with the tier that provides it. The
/// completion counterpart of [`Resolution`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The name as it would be typed (macros keep their `@`).
    pub name: SmolStr,
    pub source: Source,
}

/// The tier a [`Candidate`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Binding(BindingId),
    /// A same-module sibling top-level symbol of the workspace package.
    Workspace {
        module: SmolStr,
    },
    Using {
        module: SmolStr,
    },
    System {
        module: SmolStr,
    },
}

/// The harvested library, seen as "give me the [`PackageIndex`] named `name`".
/// Implemented for the plain harvest map and for the language server's
/// read-only [`Analysis`](crate::incremental::Analysis) snapshot.
pub trait PackageSource {
    fn package(&self, name: &str) -> Option<Arc<PackageIndex>>;

    /// The absolute source root of package `name` (the directory its
    /// `DefLocation` paths are relative to), if known. Only the live server,
    /// which has located the depot on disk, can answer this; the plain harvest
    /// map has no roots, so the default is `None`. Go-to-definition uses it to
    /// turn a package-relative definition location into a real file path.
    fn package_root(&self, _name: &str) -> Option<std::path::PathBuf> {
        None
    }

    /// The workspace package's harvested index when `path` is one of its source
    /// files — the package whose same-module globals a file's free reads resolve
    /// against (resolution tier 2). Only the live server knows the workspace, so
    /// the default is `None`. See
    /// [`Analysis::workspace_module`](crate::incremental::Analysis::workspace_module).
    fn workspace_module(&self, _path: &std::path::Path) -> Option<Arc<PackageIndex>> {
        None
    }
}

impl PackageSource for BTreeMap<String, Arc<PackageIndex>> {
    fn package(&self, name: &str) -> Option<Arc<PackageIndex>> {
        self.get(name).cloned()
    }
}

/// Resolves names against one file's [`SemanticModel`] and a [`PackageSource`],
/// following the shared masking order. Cheap to construct; holds only borrows.
pub struct Resolver<'a, P: PackageSource> {
    model: &'a SemanticModel,
    packages: &'a P,
    /// The enclosing workspace package's index, when the file being resolved is
    /// one of its source files. Its root module's top-level symbols resolve as
    /// tier-2 same-module globals. `None` for a non-member file or a
    /// non-package workspace.
    workspace: Option<Arc<PackageIndex>>,
}

impl<'a, P: PackageSource> Resolver<'a, P> {
    pub fn new(model: &'a SemanticModel, packages: &'a P) -> Self {
        Resolver {
            model,
            packages,
            workspace: None,
        }
    }

    /// Set the enclosing workspace package (tier 2). Chain onto [`new`](Self::new)
    /// at a call site that knows the file's path, via
    /// [`PackageSource::workspace_module`].
    pub fn with_workspace(mut self, workspace: Option<Arc<PackageIndex>>) -> Self {
        self.workspace = workspace;
        self
    }

    /// Resolve `name` (bare, without `@` even in [`Namespace::Macro`]) as read
    /// at `offset`, walking the four tiers and returning the first hit.
    pub fn resolve(&self, name: &str, offset: TextSize, namespace: Namespace) -> Resolution {
        let wanted = wanted_name(name, namespace);

        // Tiers 1 + 2: a binding in this file.
        if let Some(binding) = self.file_binding(&wanted, offset, namespace) {
            return Resolution::Binding(binding);
        }
        // Tier 2 (cross-file): a same-module sibling top-level symbol of the
        // workspace package. Ranks above `using`/Base — an `include`-spliced
        // module global masks them, just as a same-file global would.
        if let Some(workspace) = &self.workspace
            && module_defines(&workspace.root, &wanted, namespace)
        {
            return Resolution::Workspace { name: wanted };
        }
        // Tier 3: a whole-module `using`'s exports, in source order.
        if let Some((module, name)) = self.using_export(&wanted, offset) {
            return Resolution::Using { module, name };
        }
        // Tier 4: Base/Core implicit.
        if let Some((module, name)) = self.system_export(&wanted) {
            return Resolution::System { module, name };
        }
        Resolution::Unresolved
    }

    /// Every name visible at `offset`, in the shared masking order with shadowed
    /// names dropped: file bindings innermost-first, then `using`'d exports in
    /// source order, then Base/Core. For completion.
    pub fn visible(&self, offset: TextSize, namespace: Namespace) -> Vec<Candidate> {
        let mut seen: HashSet<SmolStr> = HashSet::new();
        let mut out: Vec<Candidate> = Vec::new();

        // Tiers 1 + 2: file bindings up the scope chain (stops at the first
        // global scope, like reads do).
        let mut cursor = Some(self.model.scope_at(offset));
        while let Some(id) = cursor {
            let scope = self.model.scope(id);
            for &b in scope.bindings.iter().rev() {
                if let Some(name) = namespaced_binding_name(self.model.binding(b), namespace)
                    && seen.insert(name.clone())
                {
                    out.push(Candidate {
                        name,
                        source: Source::Binding(b),
                    });
                }
            }
            cursor = if scope.kind.is_global() {
                None
            } else {
                scope.parent
            };
        }

        // Tier 2 (cross-file): same-module sibling top-level symbols of the
        // workspace package, masked by anything bound in this file already.
        if let Some(workspace) = &self.workspace {
            for name in defined_names(&workspace.root, namespace) {
                if seen.insert(name.clone()) {
                    out.push(Candidate {
                        name,
                        source: Source::Workspace {
                            module: SmolStr::new(&workspace.name),
                        },
                    });
                }
            }
        }

        // Tier 3: whole-module `using`'d exports, in source order.
        let at = self.model.scope_at(offset);
        for load in self.model.module_loads() {
            let Some(module) = self.using_module(load, at) else {
                continue;
            };
            let display = load.path.components.last().unwrap().clone();
            for name in exported_names(&module, namespace) {
                if seen.insert(name.clone()) {
                    out.push(Candidate {
                        name,
                        source: Source::Using {
                            module: display.clone(),
                        },
                    });
                }
            }
        }

        // Tier 4: Base/Core implicit.
        for module in ["Base", "Core"] {
            let Some(pkg) = self.packages.package(module) else {
                continue;
            };
            for name in exported_names(&pkg.root, namespace) {
                if seen.insert(name.clone()) {
                    out.push(Candidate {
                        name,
                        source: Source::System {
                            module: SmolStr::new(module),
                        },
                    });
                }
            }
        }

        out
    }

    /// Every occurrence, in this file, of a top-level symbol of the enclosing
    /// workspace package, keyed by `(namespace, bare-or-@ name)`. Two sources
    /// unify under one name: this file's own module-global bindings (a file that
    /// *defines* the symbol, contributing its definition and intra-file uses) and
    /// its free reads that resolve to the workspace tier (a file that only *uses*
    /// it). Keying by name — not [`BindingId`], which is per-file — is what lets
    /// the aggregate stitch a defining file's occurrences together with a calling
    /// file's, including multi-file multiple dispatch (`function f end` plus
    /// methods of `f` across files).
    ///
    /// Empty when the file is not a workspace member. Qualified reads (`Pkg.foo`)
    /// are deferred: the model records only the whole chain's range, not the
    /// `foo` sub-span a precise rename needs.
    pub fn workspace_occurrences(&self) -> BTreeMap<(Namespace, SmolStr), Vec<OccurrenceRec>> {
        let mut out: BTreeMap<(Namespace, SmolStr), Vec<OccurrenceRec>> = BTreeMap::new();
        let Some(workspace) = &self.workspace else {
            return out;
        };

        // Defining files: occurrences of each module-global binding whose name
        // the package actually defines at top level. The `module_defines` gate
        // matches exactly what the workspace tier of `resolve` fires on, so a
        // plain file-scope global (`x = 1`) or an import the harvester skips is
        // left out, keeping this in lockstep with go-to-definition.
        for (i, binding) in self.model.bindings().iter().enumerate() {
            if !self.model.scope(binding.scope).kind.is_global() {
                continue;
            }
            for ns in [Namespace::Value, Namespace::Macro] {
                let Some(name) = namespaced_binding_name(binding, ns) else {
                    continue;
                };
                if !module_defines(&workspace.root, &name, ns) {
                    continue;
                }
                let recs = out.entry((ns, name)).or_default();
                for occ in self.model.occurrences(BindingId(i as u32)) {
                    recs.push(OccurrenceRec {
                        range: occ.range,
                        is_def: occ.is_def,
                        access: occ.access,
                    });
                }
            }
        }

        // Using files: free reads that resolve to the workspace tier.
        for ident in self.model.idents() {
            if ident.binding.is_some() {
                continue;
            }
            let ns = if ident.is_macro {
                Namespace::Macro
            } else {
                Namespace::Value
            };
            if let Resolution::Workspace { name } =
                self.resolve(&ident.name, ident.range.start(), ns)
            {
                out.entry((ns, name)).or_default().push(OccurrenceRec {
                    range: ident.range,
                    is_def: false,
                    access: ident.access,
                });
            }
        }

        // Source order within each name, deduped by range for a stable, unique
        // result (defensive: the two sources never overlap in practice).
        for recs in out.values_mut() {
            recs.sort_by_key(|r| (r.range.start(), r.range.end()));
            recs.dedup_by_key(|r| (r.range.start(), r.range.end()));
        }
        out
    }

    /// The workspace top-level symbol the cursor at `offset` names, if any — a
    /// package global defined in this file, or a free read resolving to the
    /// workspace tier. `None` for a local, a library symbol, or a non-member
    /// file. The classification cross-file references and rename escalate on;
    /// it mirrors go-to-definition's cursor classification exactly.
    pub fn workspace_symbol_at(&self, offset: TextSize) -> Option<(Namespace, SmolStr)> {
        let workspace = self.workspace.as_ref()?;
        // An identifier occurrence: local when it binds, else a free read.
        if let Some(ident) = self.model.ident_at(offset) {
            if let Some(bid) = ident.binding {
                return self.binding_workspace_symbol(bid, workspace);
            }
            let ns = if ident.is_macro {
                Namespace::Macro
            } else {
                Namespace::Value
            };
            return match self.resolve(&ident.name, offset, ns) {
                Resolution::Workspace { name } => Some((ns, name)),
                _ => None,
            };
        }
        // A definition site (the cursor sits on a name in its own definition).
        if let Some(bid) = self.model.binding_at(offset) {
            return self.binding_workspace_symbol(bid, workspace);
        }
        None
    }

    /// `(namespace, name)` when `bid` is a module-global the workspace package
    /// defines at top level — i.e. a package symbol other files can reference,
    /// not a plain local or a file-scope global the harvester skips. The gate
    /// matches `resolve`'s workspace tier, keeping references/rename in lockstep.
    fn binding_workspace_symbol(
        &self,
        bid: BindingId,
        workspace: &Arc<PackageIndex>,
    ) -> Option<(Namespace, SmolStr)> {
        let binding = self.model.binding(bid);
        if !self.model.scope(binding.scope).kind.is_global() {
            return None;
        }
        for ns in [Namespace::Value, Namespace::Macro] {
            if let Some(name) = namespaced_binding_name(binding, ns)
                && module_defines(&workspace.root, &name, ns)
            {
                return Some((ns, name));
            }
        }
        None
    }

    /// Tiers 1 + 2: the binding `wanted` reads to, resolving up the scope chain
    /// and stopping after the first global scope (module bodies do not see
    /// enclosing globals). Mirrors the builder's `resolve_read`.
    fn file_binding(
        &self,
        wanted: &SmolStr,
        offset: TextSize,
        namespace: Namespace,
    ) -> Option<BindingId> {
        let mut cursor = Some(self.model.scope_at(offset));
        while let Some(id) = cursor {
            let scope = self.model.scope(id);
            let hit = scope.bindings.iter().rev().copied().find(|&b| {
                namespaced_binding_name(self.model.binding(b), namespace).as_ref() == Some(wanted)
            });
            if hit.is_some() {
                return hit;
            }
            cursor = if scope.kind.is_global() {
                None
            } else {
                scope.parent
            };
        }
        None
    }

    /// Tier 3: the first whole-module `using` visible at `offset` that exports
    /// `wanted`, as `(reporting module, exported name)`.
    fn using_export(&self, wanted: &SmolStr, offset: TextSize) -> Option<(SmolStr, SmolStr)> {
        let at = self.model.scope_at(offset);
        for load in self.model.module_loads() {
            let Some(module) = self.using_module(load, at) else {
                continue;
            };
            if module_exports(&module, wanted) {
                let display = load.path.components.last().unwrap().clone();
                return Some((display, wanted.clone()));
            }
        }
        None
    }

    /// Tier 4: `wanted` if Base or Core exports it (Base first).
    fn system_export(&self, wanted: &SmolStr) -> Option<(SmolStr, SmolStr)> {
        for module in ["Base", "Core"] {
            let pkg = self.packages.package(module)?;
            if module_exports(&pkg.root, wanted) {
                return Some((SmolStr::new(module), wanted.clone()));
            }
        }
        None
    }

    /// The [`ModuleIndex`] a whole-module `using` clause brings into scope at
    /// `at`, or `None` when the clause is not a resolvable whole-module `using`
    /// (an `import`, an item list, a relative/interpolated path, or a module the
    /// library has not harvested). The returned module owns its data via the
    /// package `Arc`, cloned once here.
    fn using_module(
        &self,
        load: &crate::semantic::ModuleLoad,
        at: ScopeId,
    ) -> Option<Arc<ModuleIndexHandle>> {
        if load.kind != LoadKind::Using || load.items.is_some() {
            return None;
        }
        if load.path.leading_dots != 0 || load.path.components.is_empty() {
            return None;
        }
        if !self.scope_visible(load.scope, at) {
            return None;
        }
        let pkg = self.packages.package(&load.path.components[0])?;
        // Confirm the sub-path exists before committing to the handle.
        resolve_module_path(&pkg.root, &load.path.components[1..])?;
        Some(Arc::new(ModuleIndexHandle {
            pkg,
            rest: load.path.components[1..].to_vec(),
        }))
    }

    /// Whether a statement in `decl_scope` is visible from `at`: `decl_scope` is
    /// reachable up the scope chain without crossing out of the first enclosing
    /// global scope — the same reach a read has.
    fn scope_visible(&self, decl_scope: ScopeId, at: ScopeId) -> bool {
        let mut cursor = Some(at);
        while let Some(id) = cursor {
            if id == decl_scope {
                return true;
            }
            let scope = self.model.scope(id);
            if scope.kind.is_global() {
                return false;
            }
            cursor = scope.parent;
        }
        false
    }
}

/// A resolved whole-module `using` target that keeps its package `Arc` alive so
/// the borrowed [`ModuleIndex`] outlives one loop iteration. [`Deref`] to the
/// module it points at.
struct ModuleIndexHandle {
    pkg: Arc<PackageIndex>,
    rest: Vec<SmolStr>,
}

impl std::ops::Deref for ModuleIndexHandle {
    type Target = ModuleIndex;
    fn deref(&self) -> &ModuleIndex {
        // Verified resolvable in `using_module`, and the package Arc is
        // immutable, so the walk repeats deterministically.
        resolve_module_path(&self.pkg.root, &self.rest).expect("sub-path verified in using_module")
    }
}

/// Walk `root`'s submodules along `path` by name (`["B", "C"]` from `A`'s root
/// reaches `A.B.C`); an empty `path` is `root` itself. The by-name counterpart
/// of [`resolve_module_path`], for member completion resolving a dotted
/// receiver (`A.B.`) against the library.
pub fn resolve_submodule<'m>(root: &'m ModuleIndex, path: &[&str]) -> Option<&'m ModuleIndex> {
    let mut current = root;
    for name in path {
        current = current.submodules.iter().find(|m| m.name == *name)?;
    }
    Some(current)
}

/// Walk `root`'s submodules along `rest` (`using A.B.C` → from `A`'s root, walk
/// `B` then `C`); an empty `rest` is `root` itself.
fn resolve_module_path<'m>(root: &'m ModuleIndex, rest: &[SmolStr]) -> Option<&'m ModuleIndex> {
    let mut current = root;
    for component in rest {
        current = current
            .submodules
            .iter()
            .find(|m| m.name == component.as_str())?;
    }
    Some(current)
}

/// Whether `module` `export`s `wanted` (not merely `public`s it — only exported
/// names are brought in by `using` or the implicit Base/Core tier).
fn module_exports(module: &ModuleIndex, wanted: &SmolStr) -> bool {
    module
        .exports
        .iter()
        .any(|e| e.visibility == Visibility::Exported && e.name == wanted.as_str())
}

/// Whether `module` *defines* `wanted` at top level (a function, type, or const
/// in [`Namespace::Value`]; a macro in [`Namespace::Macro`]). Unlike
/// [`module_exports`], visibility is irrelevant: within a module every top-level
/// binding is a global its own files see, exported or not. Names are stored bare
/// for values and `@`-prefixed for macros, matching `wanted`.
fn module_defines(module: &ModuleIndex, wanted: &SmolStr, namespace: Namespace) -> bool {
    match namespace {
        Namespace::Value => {
            module.functions.iter().any(|f| f.name == wanted.as_str())
                || module.types.iter().any(|t| t.name == wanted.as_str())
                || module.consts.iter().any(|c| c.name == wanted.as_str())
        }
        Namespace::Macro => module.macros.iter().any(|m| m.name == wanted.as_str()),
    }
}

/// Every name `module` defines at top level in `namespace`, as it would be typed
/// (macros keep `@`), in definition order. The completion counterpart of
/// [`module_defines`].
fn defined_names(module: &ModuleIndex, namespace: Namespace) -> Vec<SmolStr> {
    match namespace {
        Namespace::Value => module
            .functions
            .iter()
            .map(|f| f.name.as_str())
            .chain(module.types.iter().map(|t| t.name.as_str()))
            .chain(module.consts.iter().map(|c| c.name.as_str()))
            .map(SmolStr::new)
            .collect(),
        Namespace::Macro => module
            .macros
            .iter()
            .map(|m| SmolStr::new(&m.name))
            .collect(),
    }
}

/// The `export`ed names of `module` in the given namespace, as they would be
/// typed (macros keep `@`), in source order.
fn exported_names(module: &ModuleIndex, namespace: Namespace) -> Vec<SmolStr> {
    module
        .exports
        .iter()
        .filter(|e| e.visibility == Visibility::Exported && in_namespace(&e.name, namespace))
        .map(|e| SmolStr::new(&e.name))
        .collect()
}

/// The name to look up for `name` read in `namespace`: `@`-prefixed for macros
/// (the sigil the index and imported-macro bindings carry), bare for values.
fn wanted_name(name: &str, namespace: Namespace) -> SmolStr {
    match namespace {
        Namespace::Value => SmolStr::new(name),
        Namespace::Macro => SmolStr::new(format!("@{name}")),
    }
}

/// Whether a stored name (`println`, `@time`) belongs to `namespace`.
fn in_namespace(name: &str, namespace: Namespace) -> bool {
    match namespace {
        Namespace::Value => !name.starts_with('@'),
        Namespace::Macro => name.starts_with('@'),
    }
}

/// The name of `binding` in `namespace`, normalized to how it is typed (macros
/// `@`-prefixed), or `None` if the binding does not live in that namespace.
///
/// The model stores macro-definition bindings bare (`m`) but imported-macro
/// bindings with the sigil (`@foo`); this reconciles both to `@name`, matching
/// the builder's `resolve_macro_read`.
fn namespaced_binding_name(binding: &Binding, namespace: Namespace) -> Option<SmolStr> {
    match namespace {
        Namespace::Value => match binding.kind {
            BindingKind::Macro => None,
            BindingKind::Import if binding.name.starts_with('@') => None,
            _ => Some(binding.name.clone()),
        },
        Namespace::Macro => match binding.kind {
            BindingKind::Macro => Some(SmolStr::new(format!("@{}", binding.name))),
            BindingKind::Import if binding.name.starts_with('@') => Some(binding.name.clone()),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::model::{DefLocation, ExportedName, Span};

    fn model_of(src: &str) -> SemanticModel {
        SemanticModel::build(&crate::parser::parse(src).cst)
    }

    fn loc() -> DefLocation {
        DefLocation {
            file: "src/x.jl".into(),
            range: Span { start: 0, end: 0 },
        }
    }

    /// A package whose root module exports `exports` (each `@name` is a macro).
    fn package(name: &str, exports: &[&str]) -> Arc<PackageIndex> {
        module_package(name, exports, Vec::new())
    }

    fn module_package(
        name: &str,
        exports: &[&str],
        submodules: Vec<ModuleIndex>,
    ) -> Arc<PackageIndex> {
        Arc::new(PackageIndex {
            name: name.to_string(),
            root: ModuleIndex {
                name: name.to_string(),
                bare: false,
                loc: loc(),
                exports: exports
                    .iter()
                    .map(|n| ExportedName {
                        name: n.to_string(),
                        visibility: Visibility::Exported,
                        loc: loc(),
                    })
                    .collect(),
                functions: Vec::new(),
                types: Vec::new(),
                consts: Vec::new(),
                macros: Vec::new(),
                submodules,
            },
            members: Vec::new(),
            diagnostics: Vec::new(),
        })
    }

    fn submodule(name: &str, exports: &[&str]) -> ModuleIndex {
        ModuleIndex {
            name: name.to_string(),
            bare: false,
            loc: loc(),
            exports: exports
                .iter()
                .map(|n| ExportedName {
                    name: n.to_string(),
                    visibility: Visibility::Exported,
                    loc: loc(),
                })
                .collect(),
            functions: Vec::new(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
        }
    }

    fn library(packages: &[Arc<PackageIndex>]) -> BTreeMap<String, Arc<PackageIndex>> {
        packages
            .iter()
            .map(|p| (p.name.clone(), Arc::clone(p)))
            .collect()
    }

    /// The offset just past the last occurrence of `needle` in `src`.
    fn after(src: &str, needle: &str) -> TextSize {
        TextSize::from((src.rfind(needle).unwrap() + needle.len()) as u32)
    }

    fn resolve(src: &str, name: &str, lib: &BTreeMap<String, Arc<PackageIndex>>) -> Resolution {
        let model = model_of(src);
        let offset = after(src, name);
        Resolver::new(&model, lib).resolve(name, offset, Namespace::Value)
    }

    #[test]
    fn local_binding_wins_over_everything() {
        let lib = library(&[package("Base", &["x"]), package("A", &["x"])]);
        let src = "using A\nfunction f()\n    x = 1\n    x\nend";
        let model = model_of(src);
        let offset = after(src, "    x");
        match Resolver::new(&model, &lib).resolve("x", offset, Namespace::Value) {
            Resolution::Binding(b) => {
                assert_eq!(model.binding(b).kind, BindingKind::Local);
            }
            other => panic!("expected the local binding, got {other:?}"),
        }
    }

    #[test]
    fn explicit_import_wins_over_using_and_base() {
        let lib = library(&[package("Base", &["f"]), package("A", &["f"])]);
        let src = "using A\nimport B: f\nf()";
        match resolve(src, "f", &lib) {
            Resolution::Binding(b) => {
                let model = model_of(src);
                assert_eq!(model.binding(b).kind, BindingKind::Import);
            }
            other => panic!("expected the explicit import, got {other:?}"),
        }
    }

    #[test]
    fn using_export_resolves_when_not_bound() {
        let lib = library(&[package("A", &["greet"])]);
        let src = "using A\ngreet()";
        assert_eq!(
            resolve(src, "greet", &lib),
            Resolution::Using {
                module: SmolStr::new("A"),
                name: SmolStr::new("greet"),
            }
        );
    }

    #[test]
    fn using_export_masks_base() {
        let lib = library(&[package("Base", &["map"]), package("A", &["map"])]);
        let src = "using A\nmap()";
        assert_eq!(
            resolve(src, "map", &lib),
            Resolution::Using {
                module: SmolStr::new("A"),
                name: SmolStr::new("map"),
            }
        );
    }

    #[test]
    fn earliest_using_wins_in_source_order() {
        let lib = library(&[package("A", &["dup"]), package("B", &["dup"])]);
        let src = "using B\nusing A\ndup()";
        // `using B` is written first, so it wins.
        assert_eq!(
            resolve(src, "dup", &lib),
            Resolution::Using {
                module: SmolStr::new("B"),
                name: SmolStr::new("dup"),
            }
        );
    }

    #[test]
    fn base_implicit_resolves_without_a_using() {
        let lib = library(&[package("Base", &["println"])]);
        let src = "println()";
        assert_eq!(
            resolve(src, "println", &lib),
            Resolution::System {
                module: SmolStr::new("Base"),
                name: SmolStr::new("println"),
            }
        );
    }

    #[test]
    fn unknown_name_is_unresolved() {
        let lib = library(&[package("Base", &["println"])]);
        assert_eq!(resolve("nope()", "nope", &lib), Resolution::Unresolved);
    }

    #[test]
    fn item_using_does_not_bring_whole_module_exports() {
        // `using A: only` binds `only`; a sibling export `other` stays free.
        let lib = library(&[package("A", &["only", "other"])]);
        let src = "using A: only\nother()";
        assert_eq!(resolve(src, "other", &lib), Resolution::Unresolved);
    }

    #[test]
    fn import_does_not_bring_exports() {
        let lib = library(&[package("A", &["thing"])]);
        let src = "import A\nthing()";
        assert_eq!(resolve(src, "thing", &lib), Resolution::Unresolved);
    }

    #[test]
    fn using_submodule_resolves_its_exports() {
        let lib = library(&[module_package("A", &[], vec![submodule("B", &["inner"])])]);
        let src = "using A.B\ninner()";
        assert_eq!(
            resolve(src, "inner", &lib),
            Resolution::Using {
                module: SmolStr::new("B"),
                name: SmolStr::new("inner"),
            }
        );
    }

    #[test]
    fn using_in_module_does_not_leak_to_file_scope() {
        let lib = library(&[package("A", &["helper"])]);
        let src = "module M\nusing A\nend\nhelper()";
        assert_eq!(resolve(src, "helper", &lib), Resolution::Unresolved);
    }

    #[test]
    fn file_using_does_not_reach_into_a_module_body() {
        let lib = library(&[package("A", &["helper"])]);
        let src = "using A\nmodule M\nhelper()\nend";
        let model = model_of(src);
        let offset = after(src, "helper");
        assert_eq!(
            Resolver::new(&model, &lib).resolve("helper", offset, Namespace::Value),
            Resolution::Unresolved,
            "a top-level `using` does not apply inside a nested module"
        );
    }

    #[test]
    fn relative_using_is_not_resolved_against_the_library() {
        let lib = library(&[package("A", &["thing"])]);
        let src = "using .A\nthing()";
        assert_eq!(resolve(src, "thing", &lib), Resolution::Unresolved);
    }

    #[test]
    fn macro_resolves_in_the_macro_namespace() {
        let lib = library(&[package("Base", &["@time"])]);
        let src = "@time f()";
        let model = model_of(src);
        let offset = after(src, "@time");
        assert_eq!(
            Resolver::new(&model, &lib).resolve("time", offset, Namespace::Macro),
            Resolution::System {
                module: SmolStr::new("Base"),
                name: SmolStr::new("@time"),
            }
        );
    }

    #[test]
    fn value_and_macro_namespaces_do_not_cross() {
        let lib = library(&[package("Base", &["@time", "time"])]);
        let src = "time\n@time f()";
        let model = model_of(src);
        // The value `time` must not resolve via the macro export and vice versa.
        let value_off = after(src, "time\n");
        assert!(matches!(
            Resolver::new(&model, &lib).resolve("time", value_off, Namespace::Value),
            Resolution::System { name, .. } if name == "time"
        ));
    }

    #[test]
    fn imported_macro_binding_wins_for_macro_reads() {
        let lib = library(&[package("Base", &["@time"])]);
        let src = "using X: @time\n@time f()";
        let model = model_of(src);
        let offset = after(src, "@time f");
        match Resolver::new(&model, &lib).resolve("time", offset, Namespace::Macro) {
            Resolution::Binding(b) => assert_eq!(model.binding(b).kind, BindingKind::Import),
            other => panic!("expected the imported macro binding, got {other:?}"),
        }
    }

    // --- workspace tier (same-module cross-file siblings) ------------------

    /// A package whose root module defines value symbols `functions` and macro
    /// symbols `macros` (each `@name`), none of them exported — the shape of a
    /// package's own top-level globals a sibling file sees.
    fn workspace_pkg(name: &str, functions: &[&str], macros: &[&str]) -> Arc<PackageIndex> {
        use crate::index::model::{FunctionGroup, MacroDef};
        let mut pkg = (*package(name, &[])).clone();
        pkg.root.functions = functions
            .iter()
            .map(|f| FunctionGroup {
                name: f.to_string(),
                owner: None,
                methods: Vec::new(),
                doc: None,
            })
            .collect();
        pkg.root.macros = macros
            .iter()
            .map(|m| MacroDef {
                name: m.to_string(),
                params: Vec::new(),
                doc: None,
                loc: loc(),
            })
            .collect();
        Arc::new(pkg)
    }

    fn resolve_ws(
        src: &str,
        name: &str,
        lib: &BTreeMap<String, Arc<PackageIndex>>,
        workspace: Option<Arc<PackageIndex>>,
    ) -> Resolution {
        let model = model_of(src);
        let offset = after(src, name);
        Resolver::new(&model, lib)
            .with_workspace(workspace)
            .resolve(name, offset, Namespace::Value)
    }

    #[test]
    fn workspace_sibling_resolves_when_free() {
        // `bar` is defined in a sibling file (harvested into the workspace
        // package), not in this file: it resolves as a workspace symbol.
        let ws = workspace_pkg("MyPkg", &["bar"], &[]);
        let lib = library(&[package("Base", &[])]);
        assert_eq!(
            resolve_ws("bar()", "bar", &lib, Some(ws)),
            Resolution::Workspace {
                name: SmolStr::new("bar")
            }
        );
    }

    #[test]
    fn workspace_sibling_masks_using_and_base() {
        // A same-module global outranks both a `using`'d export and Base.
        let ws = workspace_pkg("MyPkg", &["dup"], &[]);
        let lib = library(&[package("Base", &["dup"]), package("A", &["dup"])]);
        assert_eq!(
            resolve_ws("using A\ndup()", "dup", &lib, Some(ws)),
            Resolution::Workspace {
                name: SmolStr::new("dup")
            }
        );
    }

    #[test]
    fn local_binding_still_wins_over_workspace() {
        // A shadowing local must not be captured by the workspace tier.
        let ws = workspace_pkg("MyPkg", &["x"], &[]);
        let lib = library(&[package("Base", &[])]);
        let src = "function f()\n    x = 1\n    x\nend";
        let model = model_of(src);
        let offset = after(src, "    x");
        match Resolver::new(&model, &lib)
            .with_workspace(Some(ws))
            .resolve("x", offset, Namespace::Value)
        {
            Resolution::Binding(b) => assert_eq!(model.binding(b).kind, BindingKind::Local),
            other => panic!("expected the local binding, got {other:?}"),
        }
    }

    #[test]
    fn workspace_macro_resolves_in_macro_namespace() {
        let ws = workspace_pkg("MyPkg", &[], &["@sib"]);
        let lib = library(&[package("Base", &[])]);
        let src = "@sib f()";
        let model = model_of(src);
        let offset = after(src, "@sib");
        assert_eq!(
            Resolver::new(&model, &lib)
                .with_workspace(Some(ws))
                .resolve("sib", offset, Namespace::Macro),
            Resolution::Workspace {
                name: SmolStr::new("@sib")
            }
        );
    }

    #[test]
    fn workspace_names_appear_in_completion_between_locals_and_using() {
        let ws = workspace_pkg("MyPkg", &["sibling"], &[]);
        let lib = library(&[package("Base", &["println"]), package("A", &["greet"])]);
        let src = "using A\nfunction f(a)\n    b = 1\n    \nend";
        let model = model_of(src);
        let offset = after(src, "b = 1\n    ");
        let names: Vec<String> = Resolver::new(&model, &lib)
            .with_workspace(Some(ws))
            .visible(offset, Namespace::Value)
            .into_iter()
            .map(|c| c.name.to_string())
            .collect();
        assert!(names.contains(&"sibling".to_string()), "{names:?}");
        // Ordering: local `b` < workspace `sibling` < using `greet` < Base.
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("b") < pos("sibling"));
        assert!(pos("sibling") < pos("greet"));
        assert!(pos("greet") < pos("println"));
    }

    // --- completion enumeration --------------------------------------------

    fn visible_names(
        src: &str,
        needle: &str,
        lib: &BTreeMap<String, Arc<PackageIndex>>,
    ) -> Vec<String> {
        let model = model_of(src);
        let offset = after(src, needle);
        Resolver::new(&model, lib)
            .visible(offset, Namespace::Value)
            .into_iter()
            .map(|c| c.name.to_string())
            .collect()
    }

    #[test]
    fn visible_lists_all_tiers_in_masking_order() {
        let lib = library(&[package("Base", &["println"]), package("A", &["greet"])]);
        let src = "using A\nfunction f(a)\n    b = 1\n    \nend";
        let names = visible_names(src, "b = 1\n    ", &lib);
        for expected in ["a", "b", "f", "greet", "println"] {
            assert!(
                names.contains(&expected.to_string()),
                "missing {expected} in {names:?}"
            );
        }
        // Local scope names precede library names.
        assert!(names.iter().position(|n| n == "b") < names.iter().position(|n| n == "greet"));
        assert!(
            names.iter().position(|n| n == "greet") < names.iter().position(|n| n == "println")
        );
    }

    #[test]
    fn visible_drops_shadowed_names() {
        // A local `map` masks Base's `map`: `map` appears once, as the local.
        let lib = library(&[package("Base", &["map"])]);
        let src = "function f()\n    map = 1\n    \nend";
        let names = visible_names(src, "map = 1\n    ", &lib);
        assert_eq!(names.iter().filter(|n| *n == "map").count(), 1);
    }

    /// A flattened occurrence bucket: `(start, end, is_def)` per site, keyed by
    /// namespace and name.
    type OccMap = BTreeMap<(Namespace, SmolStr), Vec<(u32, u32, bool)>>;

    /// The occurrences keyed by `(namespace, name)` for `src`, resolved as a
    /// member of `workspace` against `lib`.
    fn workspace_occ(
        src: &str,
        workspace: &Arc<PackageIndex>,
        lib: &BTreeMap<String, Arc<PackageIndex>>,
    ) -> OccMap {
        let model = model_of(src);
        Resolver::new(&model, lib)
            .with_workspace(Some(Arc::clone(workspace)))
            .workspace_occurrences()
            .into_iter()
            .map(|(k, recs)| {
                let simple = recs
                    .iter()
                    .map(|r| (r.range.start().into(), r.range.end().into(), r.is_def))
                    .collect();
                (k, simple)
            })
            .collect()
    }

    #[test]
    fn defining_file_reports_its_module_global() {
        // The file defines `f`; the def site plus the intra-file use are recorded.
        let ws = workspace_pkg("MyPkg", &["f"], &[]);
        let src = "function f()\n    f()\nend\n";
        let occ = workspace_occ(src, &ws, &library(&[]));
        let recs = occ
            .get(&(Namespace::Value, SmolStr::new("f")))
            .expect("f is a workspace symbol");
        // The definition (is_def) and the recursive call.
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().any(|r| r.2), "the definition site is present");
        assert!(recs.iter().any(|r| !r.2), "the intra-file use is present");
    }

    #[test]
    fn using_file_reports_free_reads_of_a_workspace_symbol() {
        // This file does not define `f`, only calls it: a free read resolving to
        // the workspace tier, recorded as a non-def use.
        let ws = workspace_pkg("MyPkg", &["f"], &[]);
        let src = "g() = f() + f()\n";
        let occ = workspace_occ(src, &ws, &library(&[]));
        let recs = occ
            .get(&(Namespace::Value, SmolStr::new("f")))
            .expect("f resolves to the workspace");
        assert_eq!(recs.len(), 2, "both calls to f");
        assert!(recs.iter().all(|r| !r.2), "uses, not definitions");
    }

    #[test]
    fn a_shadowing_local_is_not_a_workspace_occurrence() {
        // A local `f` masks the workspace symbol: its uses bind locally and are
        // not reported as references to the package-level `f`.
        let ws = workspace_pkg("MyPkg", &["f"], &[]);
        let src = "function g()\n    f = 1\n    f + f\nend\n";
        let occ = workspace_occ(src, &ws, &library(&[]));
        assert!(
            !occ.contains_key(&(Namespace::Value, SmolStr::new("f"))),
            "the local f shadows the workspace symbol"
        );
    }

    #[test]
    fn non_member_file_reports_nothing() {
        // With no workspace set, nothing is a workspace occurrence.
        let model = model_of("f() = f()\n");
        let lib = library(&[]);
        let occ = Resolver::new(&model, &lib).workspace_occurrences();
        assert!(occ.is_empty());
    }

    #[test]
    fn macro_occurrences_use_the_macro_namespace() {
        let ws = workspace_pkg("MyPkg", &[], &["@m"]);
        // Defining file: `macro m() end` binds `m` in the macro namespace.
        let occ = workspace_occ("macro m()\nend\n", &ws, &library(&[]));
        assert!(occ.contains_key(&(Namespace::Macro, SmolStr::new("@m"))));
        // Using file: an `@m` free read resolves to the workspace macro.
        let occ2 = workspace_occ("f() = @m\n", &ws, &library(&[]));
        assert!(occ2.contains_key(&(Namespace::Macro, SmolStr::new("@m"))));
    }

    #[test]
    fn a_plain_global_is_not_a_workspace_occurrence() {
        // `x = 1` is a file-scope global, but the package does not define `x` at
        // top level (the harvester skips plain globals), so it stays intra-file.
        let ws = workspace_pkg("MyPkg", &["f"], &[]);
        let src = "x = 1\nf() = x\n";
        let occ = workspace_occ(src, &ws, &library(&[]));
        assert!(!occ.contains_key(&(Namespace::Value, SmolStr::new("x"))));
        assert!(occ.contains_key(&(Namespace::Value, SmolStr::new("f"))));
    }
}
