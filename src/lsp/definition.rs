//! Go-to-definition (`textDocument/definition`).
//!
//! The symbol under the cursor is classified exactly as hover classifies it —
//! qualified read, local occurrence, or free read — but instead of rendering
//! markdown, the definition site is returned as an LSP [`Location`]:
//!
//! - an **intra-file** binding points back into the current document at its
//!   `def_range`;
//! - a **workspace sibling** (a top-level symbol of the enclosing package under
//!   development, defined in another of its files) resolves through the shared
//!   masking order's workspace tier and jumps into that file, reusing the
//!   library-location path below;
//! - a **library** symbol (Base/Core, a `using`'d export, or a `Foo.bar`
//!   qualified read) points into the depot source on disk — the package's
//!   harvested [`DefLocation`] is package-relative, so it is joined with the
//!   package's source root (known only to the live server) and the target file
//!   is read to turn the byte span into a line/column range.
//!
//! Multiple dispatch is navigation-aware: a function resolves to *every* one of
//! its method definition sites (the client shows a picker when there is more
//! than one). Same-file methods come from the binding's `Write` occurrences,
//! workspace methods from the reverse-occurrence index across member files, and
//! library methods from the harvested [`FunctionGroup`](crate::index::FunctionGroup).
//! A qualified read (and a bare read reaching the implicit Base/Core tier)
//! additionally surfaces workspace methods *extending* the library function
//! (`Base.show(io, x) = ...`, harvested with `owner`), unioned with the target
//! package's own sites.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::{Location, Position, Range, Uri};
use rowan::{TextRange, TextSize};
use smol_str::SmolStr;

use crate::incremental::Analysis;
use crate::index::model::{DefLocation, Span};
use crate::index::{ModuleIndex, PackageIndex};
use crate::parser::parse;
use crate::resolve::{
    ModulePath, Namespace, OccurrenceKey, PackageSource, Resolution, Resolver, module_at,
    resolve_submodule,
};
use crate::semantic::{Access, BindingId, BindingKind, LoadKind, QualifiedRead, SemanticModel};
use crate::text::{LineIndex, PositionEncoding};

use super::cross_file;
use super::uri::from_path;

/// The definition sites of the symbol at `position` in `text`, re-parsing it.
/// Pure and unit-testable; `uri` is the requesting document (so an intra-file
/// result points back at it) and `packages` supplies the library plus its
/// source roots. Empty when the cursor is on nothing definable; more than one
/// entry when a function has several methods.
pub fn compute_definition<P: PackageSource>(
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
    packages: &P,
) -> Vec<Location> {
    let model = SemanticModel::build(&parse(text).cst);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let workspace = super::uri::to_path(uri).and_then(|p| packages.workspace_member(&p));
    definition_for(
        &model,
        packages,
        workspace,
        uri,
        &line_index,
        offset,
        encoding,
    )
}

/// Compute the definition off the snapshot's cached parse when the db's tracked
/// buffer for `path` still matches `text`; otherwise re-parse. A write racing the
/// read trips `salsa::Cancelled`, which also falls back to a fresh parse. Mirrors
/// [`hover_via_db`](super::hover::hover_via_db).
pub(crate) fn definition_via_db(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Vec<Location> {
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached model is stale.
            return None;
        }
        let model = snapshot.semantic_model(file);
        // A workspace top-level function is answered from the reverse-occurrence
        // index across every member file, so all of its methods are found even
        // when they live in different files (mirrors `references_via_db`).
        if let Some(symbol) = cross_file::workspace_symbol_at(snapshot, path, model, offset)
            && is_workspace_function(snapshot, path, &symbol)
        {
            let locations = cross_file_definitions(snapshot, &symbol, encoding);
            // A non-empty cross-file result wins; an empty one (member set not
            // seeded yet) falls through to the intra-file answer.
            if !locations.is_empty() {
                return Some(locations);
            }
        }
        let workspace = snapshot.workspace_member(path);
        // The inner `Vec` is the definition sites (a cursor on nothing definable
        // is legitimately empty); the outer `Option` distinguishes a cache miss.
        Some(definition_for(
            model,
            snapshot,
            workspace,
            uri,
            &line_index,
            offset,
            encoding,
        ))
    }));
    match cached {
        Ok(Some(locations)) => locations,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_definition(uri, text, position, encoding, snapshot),
    }
}

/// Whether the workspace symbol `key` names a plain (unqualified) function of
/// its module, so the multi-method cross-file path applies. Other namespaces
/// and symbol kinds (types, consts, macros) have a single definition site and
/// stay on the ordinary resolution path.
fn is_workspace_function(snapshot: &Analysis, path: &Path, key: &OccurrenceKey) -> bool {
    if key.namespace != Namespace::Value {
        return false;
    }
    let Some((pkg, _)) = snapshot.workspace_member(path) else {
        return false;
    };
    let Some(module) = module_at(&pkg.root, &key.module) else {
        return false;
    };
    module
        .functions
        .iter()
        .any(|f| f.name == key.name && f.owner.is_none())
}

/// Every definition site of the workspace symbol across the package's member
/// files: the first method of each defining file is its `is_def` occurrence,
/// later methods in the same file are `Write` occurrences on the shared
/// binding. (A pathological top-level `global f; f = 2` rebinding would
/// surface here too — erroneous Julia, accepted.)
fn cross_file_definitions(
    snapshot: &Analysis,
    symbol: &OccurrenceKey,
    encoding: PositionEncoding,
) -> Vec<Location> {
    cross_file::gather_sites(snapshot, symbol, encoding)
        .into_iter()
        .filter(|site| site.is_def || site.access == Access::Write)
        .map(|site| Location {
            uri: site.uri,
            range: site.range,
        })
        .collect()
}

/// Shared entry point for the fresh-parse and cached-model paths. Mirrors the
/// three shapes of [`hover_content`](super::hover). `line_index` indexes the
/// *current* document, for intra-file results.
#[allow(clippy::too_many_arguments)]
fn definition_for<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    workspace: Option<(Arc<PackageIndex>, ModulePath)>,
    uri: &Uri,
    line_index: &LineIndex,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Vec<Location> {
    // A qualified name (`Foo.bar`, `Base.@time`) carries its whole module path.
    if let Some(q) = model
        .qualified_reads()
        .iter()
        .find(|q| q.range.contains_inclusive(offset))
    {
        return qualified_locations(q, packages, workspace.as_ref(), encoding);
    }
    // An ordinary identifier occurrence: local when it binds, else a free read.
    if let Some(ident) = model.ident_at(offset) {
        if let Some(bid) = ident.binding {
            return binding_locations(model, uri, bid, line_index, encoding);
        }
        let ns = if ident.is_macro {
            Namespace::Macro
        } else {
            Namespace::Value
        };
        return free_read_locations(
            model,
            packages,
            workspace,
            uri,
            &ident.name,
            offset,
            ns,
            line_index,
            encoding,
        );
    }
    // A definition site (the cursor sits on a name in its own definition) is not
    // an occurrence, so it is found through the binding arena; it points at
    // itself (and at its sibling methods, for a function).
    if let Some(bid) = model.binding_at(offset) {
        return binding_locations(model, uri, bid, line_index, encoding);
    }
    Vec::new()
}

/// Resolve a qualified read (`Foo.bar`) into every definition site of the
/// symbol: the named package's own sites, unioned with workspace methods
/// extending it (`Foo.bar(x) = ...` harvested with `owner`). The library walk
/// is best-effort, so an unindexed target package still surfaces the
/// workspace extensions.
fn qualified_locations<P: PackageSource>(
    q: &QualifiedRead,
    packages: &P,
    workspace: Option<&(Arc<PackageIndex>, ModulePath)>,
    encoding: PositionEncoding,
) -> Vec<Location> {
    let Some((name, module_path)) = q.path.split_last() else {
        return Vec::new();
    };
    let mut sites = workspace_extension_sites(packages, workspace, module_path, name);
    if let Some(head) = module_path.first()
        && let Some(pkg) = packages.package(head)
    {
        let rest: Vec<&str> = module_path[1..].iter().map(|s| s.as_str()).collect();
        if let Some(module) = resolve_submodule(&pkg.root, &rest) {
            sites.extend(library_def_sites(packages, &pkg, module, name));
        }
    }
    site_locations(sites, encoding)
}

/// The on-disk definition sites of workspace methods extending
/// `module_path.name` (`Base.show(io, x) = ...` harvested with
/// `owner == module_path`), across the workspace package's whole module tree.
/// Empty without a workspace or a known source root.
fn workspace_extension_sites<P: PackageSource>(
    packages: &P,
    workspace: Option<&(Arc<PackageIndex>, ModulePath)>,
    module_path: &[SmolStr],
    name: &str,
) -> Vec<(PathBuf, Span)> {
    let Some((pkg, _)) = workspace else {
        return Vec::new();
    };
    let Some(root) = packages.package_root(&pkg.name) else {
        return Vec::new();
    };
    let owner: Vec<&str> = module_path.iter().map(|s| s.as_str()).collect();
    pkg.root
        .extension_groups(&owner, name)
        .into_iter()
        .flat_map(|g| &g.methods)
        .map(|m| (root.join(&m.loc.file), m.loc.range))
        .collect()
}

/// Every definition site of `bid` in the current document. A plain binding is
/// its `def_range`; a function (or macro) binding additionally owns one `Write`
/// occurrence per later method definition, so those are its remaining methods.
/// Non-function bindings never collect writes — `x = 1; x = 2` keeps a single
/// definition site.
fn binding_locations(
    model: &SemanticModel,
    uri: &Uri,
    bid: BindingId,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Vec<Location> {
    let binding = model.binding(bid);
    let mut ranges = vec![binding.def_range];
    if matches!(binding.kind, BindingKind::Function | BindingKind::Macro) {
        ranges.extend(
            model
                .idents()
                .iter()
                .filter(|i| i.binding == Some(bid) && i.access == Access::Write)
                .map(|i| i.range),
        );
    }
    ranges.sort_by_key(|r| r.start());
    ranges.dedup();
    ranges
        .into_iter()
        .map(|range| self_location(uri, range, line_index, encoding))
        .collect()
}

/// A [`Location`] in the current document `uri` covering `range`.
fn self_location(
    uri: &Uri,
    range: TextRange,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Location {
    Location {
        uri: uri.clone(),
        range: to_range(range, line_index, encoding),
    }
}

/// Resolve a free (non-local, non-qualified) read through the shared masking
/// order to its definition sites: a binding the occurrence walk missed is still
/// local, otherwise a Base/Core or `using`'d library symbol.
#[allow(clippy::too_many_arguments)]
fn free_read_locations<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    workspace: Option<(Arc<PackageIndex>, ModulePath)>,
    uri: &Uri,
    name: &str,
    offset: TextSize,
    ns: Namespace,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Vec<Location> {
    match Resolver::new(model, packages)
        .with_workspace(workspace.clone())
        .resolve(name, offset, ns)
    {
        Resolution::Binding(bid) => binding_locations(model, uri, bid, line_index, encoding),
        // A same-module sibling: look the name up in the file's host module and
        // jump into the sibling file on disk (the depot path). Harvest merges
        // the include closure into one group, so every method is found even
        // when the methods span member files.
        Resolution::Workspace { module, name } => {
            let Some((pkg, _)) = workspace else {
                return Vec::new();
            };
            let Some(host) = module_at(&pkg.root, &module) else {
                return Vec::new();
            };
            library_locations(packages, &pkg, host, &name, encoding)
        }
        // A bare read resolving through the implicit Base/Core tier *is* the
        // library function, so workspace methods extending it are among its
        // definition sites too (the Workspace tier only sees the file's host
        // module, so an extension in a sibling module lands here).
        Resolution::System { module, name } => {
            let mut sites = workspace_extension_sites(
                packages,
                workspace.as_ref(),
                std::slice::from_ref(&module),
                &name,
            );
            if let Some(pkg) = packages.package(&module) {
                sites.extend(library_def_sites(packages, &pkg, &pkg.root, &name));
            }
            site_locations(sites, encoding)
        }
        Resolution::Using { module, name } => {
            library_from_using(model, packages, &module, &name, encoding)
        }
        // A sibling file's module-level import: navigation to the `import` site
        // is deferred, so no definition location.
        Resolution::WorkspaceImport { .. } | Resolution::Unresolved => Vec::new(),
    }
}

/// Find the module a whole-module `using` brings in and locate `name`'s
/// definition sites in it. `module` is the clause's display name (its last
/// component): a plain `using LinearAlgebra` names the package directly; a
/// `using A.B` needs the clause walked from its package root. Mirrors hover's
/// `library_from_using`.
fn using_def_sites<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    module: &str,
    name: &str,
) -> Vec<(PathBuf, Span)> {
    if let Some(pkg) = packages.package(module) {
        let sites = library_def_sites(packages, &pkg, &pkg.root, name);
        if !sites.is_empty() {
            return sites;
        }
    }
    for load in model.module_loads() {
        if load.kind != LoadKind::Using || load.items.is_some() {
            continue;
        }
        let comps = &load.path.components;
        if comps.last().map(|c| c.as_str()) != Some(module) {
            continue;
        }
        let Some(first) = comps.first() else { continue };
        let Some(pkg) = packages.package(first.as_str()) else {
            continue;
        };
        let rest: Vec<&str> = comps[1..].iter().map(|c| c.as_str()).collect();
        if let Some(m) = resolve_submodule(&pkg.root, &rest) {
            let sites = library_def_sites(packages, &pkg, m, name);
            if !sites.is_empty() {
                return sites;
            }
        }
    }
    Vec::new()
}

/// [`using_def_sites`]' single representative site (its first method). Shared
/// with call hierarchy, which wants one item per symbol.
pub(crate) fn using_def_site<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    module: &str,
    name: &str,
) -> Option<(PathBuf, Span)> {
    using_def_sites(model, packages, module, name)
        .into_iter()
        .next()
}

/// [`using_def_sites`] materialized into [`Location`]s, for go-to-definition.
fn library_from_using<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    module: &str,
    name: &str,
    encoding: PositionEncoding,
) -> Vec<Location> {
    site_locations(using_def_sites(model, packages, module, name), encoding)
}

/// The on-disk definition sites of a library symbol: its [`DefLocation`]s in
/// `module`, with the package-relative paths joined onto `pkg`'s source root
/// (known only to the live server). Empty when the symbol is not defined in
/// `module` or the root is unknown (e.g. the baked-in fallback Base). A
/// function contributes one site per method.
fn library_def_sites<P: PackageSource>(
    packages: &P,
    pkg: &PackageIndex,
    module: &ModuleIndex,
    name: &str,
) -> Vec<(PathBuf, Span)> {
    let defs = library_def_locations(module, name);
    if defs.is_empty() {
        return Vec::new();
    }
    let Some(root) = packages.package_root(&pkg.name) else {
        return Vec::new();
    };
    defs.into_iter()
        .map(|def| (root.join(&def.file), def.range))
        .collect()
}

/// [`library_def_sites`]' single representative site (a function's first
/// method). Shared with call and type hierarchy, which want one item per
/// symbol and re-derive the definition's shape from the file.
pub(crate) fn library_def_site<P: PackageSource>(
    packages: &P,
    pkg: &PackageIndex,
    module: &ModuleIndex,
    name: &str,
) -> Option<(PathBuf, Span)> {
    library_def_sites(packages, pkg, module, name)
        .into_iter()
        .next()
}

/// Turn a library symbol into [`Location`]s in depot source files: find its
/// definition sites via [`library_def_sites`] and materialize them. Empty when
/// the sites are unknown or no target file can be read.
fn library_locations<P: PackageSource>(
    packages: &P,
    pkg: &PackageIndex,
    module: &ModuleIndex,
    name: &str,
    encoding: PositionEncoding,
) -> Vec<Location> {
    site_locations(library_def_sites(packages, pkg, module, name), encoding)
}

/// Materialize on-disk `(path, span)` sites into [`Location`]s, reading each
/// distinct file once (methods of one group can span the include closure).
/// Unreadable files are skipped; the result is ordered by path, then offset.
fn site_locations(mut sites: Vec<(PathBuf, Span)>, encoding: PositionEncoding) -> Vec<Location> {
    sites.sort_by(|a, b| (&a.0, a.1.start).cmp(&(&b.0, b.1.start)));
    sites.dedup();
    let mut out = Vec::new();
    for chunk in sites.chunk_by(|a, b| a.0 == b.0) {
        let abs = &chunk[0].0;
        let Ok(text) = std::fs::read_to_string(abs) else {
            continue;
        };
        let Some(uri) = from_path(abs) else {
            continue;
        };
        let line_index = LineIndex::new(&text);
        for (_, span) in chunk {
            out.push(Location {
                uri: uri.clone(),
                range: span_to_range(*span, &line_index, encoding),
            });
        }
    }
    out
}

/// Look `name` up among `module`'s defined symbols and return its definition
/// locations. Mirrors the search order of hover's `render_library_symbol`
/// (macros for an `@` name, then functions, types, consts). A function group
/// contributes every method; the other kinds have a single site.
fn library_def_locations<'m>(module: &'m ModuleIndex, name: &str) -> Vec<&'m DefLocation> {
    if name.starts_with('@') {
        return module
            .macros
            .iter()
            .find(|m| m.name == name)
            .map(|m| &m.loc)
            .into_iter()
            .collect();
    }
    // Prefer the bare group: a module can also hold a qualified-extension
    // group of the same name (`Base.show` next to its own `show`), whose
    // methods belong to the *owner's* function. The name-only fallback keeps
    // a module that only extends navigable — the Workspace tier resolves
    // bare reads by name alone, and there those methods are the answer.
    if let Some(f) = module
        .functions
        .iter()
        .find(|f| f.name == name && f.owner.is_none())
        .or_else(|| module.functions.iter().find(|f| f.name == name))
    {
        return f.methods.iter().map(|m| &m.loc).collect();
    }
    if let Some(t) = module.types.iter().find(|t| t.name == name) {
        return vec![&t.loc];
    }
    if let Some(c) = module.consts.iter().find(|c| c.name == name) {
        return vec![&c.loc];
    }
    Vec::new()
}

fn to_range(range: TextRange, line_index: &LineIndex, encoding: PositionEncoding) -> Range {
    Range {
        start: line_index.byte_to_position(range.start().into(), encoding),
        end: line_index.byte_to_position(range.end().into(), encoding),
    }
}

fn span_to_range(span: Span, line_index: &LineIndex, encoding: PositionEncoding) -> Range {
    Range {
        start: line_index.byte_to_position(span.start as usize, encoding),
        end: line_index.byte_to_position(span.end as usize, encoding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::index::harvest_package_named;

    use super::super::uri::to_path;

    /// A library with both package indexes and source roots, so the go-to-def
    /// path can join a package-relative [`DefLocation`] with an on-disk root.
    #[derive(Default)]
    struct TestLib {
        packages: BTreeMap<String, Arc<PackageIndex>>,
        roots: BTreeMap<String, PathBuf>,
        /// The workspace package, returned for any path (tests pass member
        /// paths); mirrors the live server's [`Analysis::workspace_module`].
        workspace: Option<(Arc<PackageIndex>, ModulePath)>,
    }

    impl PackageSource for TestLib {
        fn package(&self, name: &str) -> Option<Arc<PackageIndex>> {
            self.packages.get(name).cloned()
        }
        fn package_root(&self, name: &str) -> Option<PathBuf> {
            self.roots.get(name).cloned()
        }
        fn workspace_member(&self, _path: &Path) -> Option<(Arc<PackageIndex>, ModulePath)> {
            self.workspace.clone()
        }
    }

    /// A unique temp directory removed on drop (mirrors `tests/harvest.rs`,
    /// avoiding a `tempfile` dev-dependency).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("fatou-def-{}-{}", std::process::id(), n));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn doc_uri() -> Uri {
        Uri::from_str("file:///work/s.jl").unwrap()
    }

    /// Go-to-definition at the position marked by `|` in `src` (the marker is
    /// stripped before parsing), for the current document `s.jl`.
    fn def_at(marked: &str, lib: &impl PackageSource) -> Vec<Location> {
        let offset = marked.find('|').expect("a cursor marker");
        let src = marked.replacen('|', "", 1);
        let line_index = LineIndex::new(&src);
        let position = line_index.byte_to_position(offset, PositionEncoding::Utf16);
        compute_definition(&doc_uri(), &src, position, PositionEncoding::Utf16, lib)
    }

    /// [`def_at`] for symbols with exactly one definition site.
    fn single_def_at(marked: &str, lib: &impl PackageSource) -> Option<Location> {
        let mut locations = def_at(marked, lib);
        match locations.len() {
            0 => None,
            1 => Some(locations.remove(0)),
            n => panic!("expected at most one definition site, got {n}"),
        }
    }

    #[test]
    fn local_variable_jumps_to_its_assignment() {
        let loc =
            single_def_at("function f()\n    x = 1\n    x|\nend", &TestLib::default()).unwrap();
        assert_eq!(loc.uri, doc_uri());
        // The `x` in `x = 1` on line 1, column 4.
        assert_eq!(loc.range.start, Position::new(1, 4));
        assert_eq!(loc.range.end, Position::new(1, 5));
    }

    #[test]
    fn call_jumps_to_the_function_definition() {
        let loc = single_def_at("greet(a) = a\ngreet|(1)", &TestLib::default()).unwrap();
        assert_eq!(loc.uri, doc_uri());
        // The `greet` in the definition on line 0, column 0.
        assert_eq!(loc.range.start, Position::new(0, 0));
        assert_eq!(loc.range.end, Position::new(0, 5));
    }

    #[test]
    fn parameter_use_jumps_to_the_parameter() {
        let loc = single_def_at("function f(abc)\n    abc|\nend", &TestLib::default()).unwrap();
        assert_eq!(loc.range.start, Position::new(0, 11));
        assert_eq!(loc.range.end, Position::new(0, 14));
    }

    #[test]
    fn unresolved_name_has_no_definition() {
        assert!(def_at("nope|()", &TestLib::default()).is_empty());
    }

    #[test]
    fn using_export_jumps_into_the_depot_source() {
        // Harvest a real on-disk package so its `DefLocation` is genuine, then
        // point go-to-def at a `using`'d export.
        let tmp = TempDir::new();
        let entry = tmp.path.join("src").join("Greetings.jl");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(
            &entry,
            "module Greetings\nexport greet\ngreet(name) = name\nend\n",
        )
        .unwrap();

        let pkg = harvest_package_named(&tmp.path, "Greetings");
        let mut lib = TestLib::default();
        lib.packages.insert("Greetings".to_string(), Arc::new(pkg));
        lib.roots.insert("Greetings".to_string(), tmp.path.clone());

        let loc = single_def_at("using Greetings\ngreet|(1)", &lib).unwrap();
        assert_eq!(to_path(&loc.uri), Some(entry.clone()));
        // The `greet` definition on line 2, column 0 of the depot source.
        assert_eq!(loc.range.start, Position::new(2, 0));
        assert_eq!(loc.range.end, Position::new(2, 5));
    }

    #[test]
    fn library_without_a_known_root_has_no_definition() {
        // Same package, but no source root registered: the relative location
        // cannot be materialized, so go-to-def yields nothing rather than a
        // bogus path.
        let tmp = TempDir::new();
        let entry = tmp.path.join("src").join("Greetings.jl");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(
            &entry,
            "module Greetings\nexport greet\ngreet(x) = x\nend\n",
        )
        .unwrap();
        let pkg = harvest_package_named(&tmp.path, "Greetings");
        let mut lib = TestLib::default();
        lib.packages.insert("Greetings".to_string(), Arc::new(pkg));

        assert!(def_at("using Greetings\ngreet|(1)", &lib).is_empty());
    }

    #[test]
    fn workspace_sibling_jumps_into_the_other_file() {
        // A package under development: `MyPkg.jl` includes `bar.jl`, which
        // defines `bar`. A free read of `bar` in a member file resolves through
        // the workspace tier and jumps into `bar.jl` on disk.
        let tmp = TempDir::new();
        let src = tmp.path.join("src");
        fs::create_dir_all(&src).unwrap();
        let bar = src.join("bar.jl");
        fs::write(
            src.join("MyPkg.jl"),
            "module MyPkg\ninclude(\"bar.jl\")\nend\n",
        )
        .unwrap();
        fs::write(&bar, "bar(x) = x\n").unwrap();

        let pkg = Arc::new(harvest_package_named(&tmp.path, "MyPkg"));
        let mut lib = TestLib::default();
        lib.packages.insert("MyPkg".to_string(), Arc::clone(&pkg));
        lib.roots.insert("MyPkg".to_string(), tmp.path.clone());
        lib.workspace = Some((pkg, Vec::new()));

        let loc = single_def_at("bar|(1)", &lib).unwrap();
        assert_eq!(to_path(&loc.uri), Some(bar));
        // The `bar` definition on line 0, columns 0..3 of the sibling file.
        assert_eq!(loc.range.start, Position::new(0, 0));
        assert_eq!(loc.range.end, Position::new(0, 3));
    }

    #[test]
    fn workspace_tier_is_off_without_membership() {
        // The same package, but no workspace module registered (the file is not
        // a member): `bar` stays unresolved.
        let tmp = TempDir::new();
        let src = tmp.path.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("MyPkg.jl"),
            "module MyPkg\ninclude(\"bar.jl\")\nend\n",
        )
        .unwrap();
        fs::write(src.join("bar.jl"), "bar(x) = x\n").unwrap();

        let pkg = Arc::new(harvest_package_named(&tmp.path, "MyPkg"));
        let mut lib = TestLib::default();
        lib.packages.insert("MyPkg".to_string(), pkg);
        lib.roots.insert("MyPkg".to_string(), tmp.path.clone());
        // `lib.workspace` left `None`.

        assert!(def_at("bar|(1)", &lib).is_empty());
    }

    #[test]
    fn call_returns_every_local_method() {
        let locs = def_at(
            "f(x::Int) = 1\nf(x::String) = \"s\"\nf|(1)",
            &TestLib::default(),
        );
        assert_eq!(locs.len(), 2, "{locs:?}");
        // Source order: the first method on line 0, the second on line 1.
        assert_eq!(locs[0].range.start, Position::new(0, 0));
        assert_eq!(locs[1].range.start, Position::new(1, 0));
    }

    #[test]
    fn long_and_short_form_methods_are_both_found() {
        let locs = def_at(
            "function f(x)\n    x\nend\nf(x, y) = x\nf|(1)",
            &TestLib::default(),
        );
        assert_eq!(locs.len(), 2, "{locs:?}");
        assert_eq!(locs[0].range.start, Position::new(0, 9));
        assert_eq!(locs[1].range.start, Position::new(3, 0));
    }

    #[test]
    fn cursor_on_the_first_method_returns_all_methods() {
        // The first method's name is the binding's own definition site (the
        // `binding_at` branch).
        let locs = def_at("f|(x::Int) = 1\nf(x::String) = \"s\"", &TestLib::default());
        assert_eq!(locs.len(), 2, "{locs:?}");
    }

    #[test]
    fn cursor_on_a_later_method_returns_all_methods() {
        // A later method's name is a `Write` occurrence on the shared binding
        // (the ident branch).
        let locs = def_at("f(x::Int) = 1\nf|(x::String) = \"s\"", &TestLib::default());
        assert_eq!(locs.len(), 2, "{locs:?}");
        assert_eq!(locs[0].range.start, Position::new(0, 0));
        assert_eq!(locs[1].range.start, Position::new(1, 0));
    }

    #[test]
    fn variable_reassignment_keeps_a_single_definition() {
        // Only function (and macro) bindings collect their writes; a rebound
        // variable still jumps to its first assignment alone.
        let loc = single_def_at(
            "function g()\n    x = 1\n    x = 2\n    x|\nend",
            &TestLib::default(),
        )
        .unwrap();
        assert_eq!(loc.range.start, Position::new(1, 4));
    }

    #[test]
    fn nested_local_function_methods_are_all_found() {
        // A local function inside a body never reaches the workspace index;
        // its methods come from the binding tier alone.
        let locs = def_at(
            "function outer()\n    g(x) = 1\n    g(x, y) = 2\n    g|(1)\nend",
            &TestLib::default(),
        );
        assert_eq!(locs.len(), 2, "{locs:?}");
        assert_eq!(locs[0].range.start, Position::new(1, 4));
        assert_eq!(locs[1].range.start, Position::new(2, 4));
    }

    #[test]
    fn using_export_returns_all_library_methods() {
        let tmp = TempDir::new();
        let entry = tmp.path.join("src").join("Greetings.jl");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(
            &entry,
            "module Greetings\nexport greet\ngreet(name) = name\ngreet(a, b) = a\nend\n",
        )
        .unwrap();

        let pkg = harvest_package_named(&tmp.path, "Greetings");
        let mut lib = TestLib::default();
        lib.packages.insert("Greetings".to_string(), Arc::new(pkg));
        lib.roots.insert("Greetings".to_string(), tmp.path.clone());

        let locs = def_at("using Greetings\ngreet|(1)", &lib);
        assert_eq!(locs.len(), 2, "{locs:?}");
        assert!(locs.iter().all(|l| to_path(&l.uri) == Some(entry.clone())));
        // Ordered by offset within the depot source.
        assert_eq!(locs[0].range.start, Position::new(2, 0));
        assert_eq!(locs[1].range.start, Position::new(3, 0));
    }

    #[test]
    fn workspace_sibling_methods_span_member_files() {
        // The two methods of `bar` live in different included files; harvest
        // merges the include closure into one group, so both are found.
        let tmp = TempDir::new();
        let src = tmp.path.join("src");
        fs::create_dir_all(&src).unwrap();
        let bar = src.join("bar.jl");
        let baz = src.join("baz.jl");
        fs::write(
            src.join("MyPkg.jl"),
            "module MyPkg\ninclude(\"bar.jl\")\ninclude(\"baz.jl\")\nend\n",
        )
        .unwrap();
        fs::write(&bar, "bar(x) = x\n").unwrap();
        fs::write(&baz, "bar(x, y) = x\n").unwrap();

        let pkg = Arc::new(harvest_package_named(&tmp.path, "MyPkg"));
        let mut lib = TestLib::default();
        lib.packages.insert("MyPkg".to_string(), Arc::clone(&pkg));
        lib.roots.insert("MyPkg".to_string(), tmp.path.clone());
        lib.workspace = Some((pkg, Vec::new()));

        let locs = def_at("bar|(1)", &lib);
        assert_eq!(locs.len(), 2, "{locs:?}");
        // Ordered by path: bar.jl before baz.jl.
        assert_eq!(to_path(&locs[0].uri), Some(bar));
        assert_eq!(to_path(&locs[1].uri), Some(baz));
    }

    /// A fake `Base` depot exporting `show`, plus a workspace package `MyPkg`
    /// with `mypkg_src` as its entry file, both registered with source roots
    /// and `MyPkg` set as the workspace.
    struct ExtensionFixture {
        lib: TestLib,
        base_entry: PathBuf,
        mypkg_entry: PathBuf,
        _dirs: (TempDir, TempDir),
    }

    fn extension_fixture(mypkg_src: &str) -> ExtensionFixture {
        let base_tmp = TempDir::new();
        let base_entry = base_tmp.path.join("src").join("Base.jl");
        fs::create_dir_all(base_entry.parent().unwrap()).unwrap();
        fs::write(
            &base_entry,
            "module Base\nexport show\nshow(io) = nothing\nend\n",
        )
        .unwrap();
        let ws_tmp = TempDir::new();
        let mypkg_entry = ws_tmp.path.join("src").join("MyPkg.jl");
        fs::create_dir_all(mypkg_entry.parent().unwrap()).unwrap();
        fs::write(&mypkg_entry, mypkg_src).unwrap();

        let base = harvest_package_named(&base_tmp.path, "Base");
        let pkg = Arc::new(harvest_package_named(&ws_tmp.path, "MyPkg"));

        let mut lib = TestLib::default();
        lib.packages.insert("Base".to_string(), Arc::new(base));
        lib.roots.insert("Base".to_string(), base_tmp.path.clone());
        lib.packages.insert("MyPkg".to_string(), Arc::clone(&pkg));
        lib.roots.insert("MyPkg".to_string(), ws_tmp.path.clone());
        lib.workspace = Some((pkg, Vec::new()));
        ExtensionFixture {
            lib,
            base_entry,
            mypkg_entry,
            _dirs: (base_tmp, ws_tmp),
        }
    }

    #[test]
    fn qualified_extension_surfaces_workspace_methods() {
        // `Base.show` unions Base's own method with the workspace extension.
        let fx = extension_fixture("module MyPkg\nBase.show(io, x) = 1\nend\n");
        let locs = def_at("Base.show|(io, 2)", &fx.lib);
        assert_eq!(locs.len(), 2, "{locs:?}");
        assert!(
            locs.iter()
                .any(|l| to_path(&l.uri) == Some(fx.base_entry.clone())),
            "{locs:?}"
        );
        assert!(
            locs.iter()
                .any(|l| to_path(&l.uri) == Some(fx.mypkg_entry.clone())),
            "{locs:?}"
        );
    }

    #[test]
    fn extension_definition_callee_navigates() {
        // The callee of an extension definition is a whole-chain qualified
        // read, so the cursor on it takes the same unioned path.
        let fx = extension_fixture("module MyPkg\nBase.show(io, x) = 1\nend\n");
        let locs = def_at("Base.sho|w(io, x) = 1", &fx.lib);
        assert_eq!(locs.len(), 2, "{locs:?}");
    }

    #[test]
    fn extension_in_submodule_surfaces() {
        // The whole module tree is walked: an extension inside a nested
        // `module` still adds a method to `Base.show`.
        let fx = extension_fixture("module MyPkg\nmodule Inner\nBase.show(io, x) = 1\nend\nend\n");
        let locs = def_at("Base.show|(io, 2)", &fx.lib);
        assert_eq!(locs.len(), 2, "{locs:?}");
        assert!(
            locs.iter()
                .any(|l| to_path(&l.uri) == Some(fx.mypkg_entry.clone())),
            "{locs:?}"
        );
    }

    #[test]
    fn extension_without_target_package_surfaces() {
        // No `Base` index loaded: the library walk is best-effort, so the
        // workspace extension still comes back alone.
        let mut fx = extension_fixture("module MyPkg\nBase.show(io, x) = 1\nend\n");
        fx.lib.packages.remove("Base");
        fx.lib.roots.remove("Base");
        let loc = single_def_at("Base.show|(io, 2)", &fx.lib).unwrap();
        assert_eq!(to_path(&loc.uri), Some(fx.mypkg_entry.clone()));
    }

    #[test]
    fn qualified_read_prefers_the_bare_group() {
        // A depot module holding both an extension group and its own bare
        // group of the same name: `Foo.g` means the bare one, even when the
        // extension is harvested first.
        let tmp = TempDir::new();
        let entry = tmp.path.join("src").join("Foo.jl");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(&entry, "module Foo\nBase.g(x) = 1\ng(x) = 2\nend\n").unwrap();
        let pkg = harvest_package_named(&tmp.path, "Foo");
        let mut lib = TestLib::default();
        lib.packages.insert("Foo".to_string(), Arc::new(pkg));
        lib.roots.insert("Foo".to_string(), tmp.path.clone());

        let loc = single_def_at("Foo.g|(1)", &lib).unwrap();
        assert_eq!(to_path(&loc.uri), Some(entry));
        assert_eq!(loc.range.start, Position::new(2, 0));
    }

    #[test]
    fn bare_base_name_includes_workspace_extension() {
        // A bare `show` resolving through the implicit Base tier is
        // `Base.show`, so the workspace extension is among its sites. The
        // extension lives in a submodule so the workspace tier (which only
        // sees the file's host module) cannot answer first.
        let fx = extension_fixture("module MyPkg\nmodule Inner\nBase.show(io, x) = 1\nend\nend\n");
        let locs = def_at("show|(1)", &fx.lib);
        assert_eq!(locs.len(), 2, "{locs:?}");
        assert!(
            locs.iter()
                .any(|l| to_path(&l.uri) == Some(fx.base_entry.clone())),
            "{locs:?}"
        );
        assert!(
            locs.iter()
                .any(|l| to_path(&l.uri) == Some(fx.mypkg_entry.clone())),
            "{locs:?}"
        );
    }

    /// Cross-file definitions over a hand-built workspace package: a request
    /// from a calling file reaches every method across the member files
    /// through the reverse-occurrence index.
    #[test]
    fn cross_file_definitions_span_member_files() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};
        use crate::text::PositionEncoding::Utf16;

        let a_text = "greet(a) = a\ngreet(a, b) = a\n";
        let b_text = "callit() = greet(2)\n";
        let (db, _) = workspace_db(&["greet"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();

        let a_uri = crate::lsp::uri::from_path(&member_path("a.jl")).unwrap();
        let b_path = member_path("b.jl");
        let b_uri = crate::lsp::uri::from_path(&b_path).unwrap();

        // Cursor on the `greet` call in b.jl: both methods in a.jl come back.
        let locs = definition_via_db(
            &snapshot,
            &b_uri,
            &b_path,
            b_text,
            Position::new(0, 11),
            Utf16,
        );
        assert_eq!(locs.len(), 2, "{locs:?}");
        assert!(locs.iter().all(|l| l.uri == a_uri));
        assert!(locs.iter().any(|l| l.range.start == Position::new(0, 0)));
        assert!(locs.iter().any(|l| l.range.start == Position::new(1, 0)));

        // Cursor on the first method's own definition: same answer.
        let a_path = member_path("a.jl");
        let locs = definition_via_db(
            &snapshot,
            &a_uri,
            &a_path,
            a_text,
            Position::new(0, 0),
            Utf16,
        );
        assert_eq!(locs.len(), 2, "{locs:?}");
    }

    /// The cross-file path is gated to functions: a rebound top-level variable
    /// in a member file keeps a single definition site.
    #[test]
    fn cross_file_gate_skips_non_function_symbols() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};
        use crate::text::PositionEncoding::Utf16;

        let a_text = "x = 1\nx = 2\nx\n";
        let (db, _) = workspace_db(&["greet"], &[("a.jl", a_text)]);
        let snapshot = db.snapshot();
        let a_path = member_path("a.jl");
        let a_uri = crate::lsp::uri::from_path(&a_path).unwrap();

        let locs = definition_via_db(
            &snapshot,
            &a_uri,
            &a_path,
            a_text,
            Position::new(2, 0),
            Utf16,
        );
        assert_eq!(locs.len(), 1, "{locs:?}");
        assert_eq!(locs[0].range.start, Position::new(0, 0));
    }
}
