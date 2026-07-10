//! Go-to-definition (`textDocument/definition`).
//!
//! The symbol under the cursor is classified exactly as hover classifies it —
//! qualified read, local occurrence, or free read — but instead of rendering
//! markdown, the definition site is returned as an LSP [`Location`]:
//!
//! - an **intra-file** binding points back into the current document at its
//!   `def_range`;
//! - a **library** symbol (Base/Core, a `using`'d export, or a `Foo.bar`
//!   qualified read) points into the depot source on disk — the package's
//!   harvested [`DefLocation`] is package-relative, so it is joined with the
//!   package's source root (known only to the live server) and the target file
//!   is read to turn the byte span into a line/column range.
//!
//! Multiple dispatch resolves to the group's first method for now; returning
//! *every* method of a function is a separate Phase 6 item.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Location, Position, Range, Uri};
use rowan::{TextRange, TextSize};

use crate::incremental::Analysis;
use crate::index::model::{DefLocation, Span};
use crate::index::{ModuleIndex, PackageIndex};
use crate::parser::parse;
use crate::resolve::{Namespace, PackageSource, Resolution, Resolver, resolve_submodule};
use crate::semantic::{LoadKind, SemanticModel};
use crate::text::{LineIndex, PositionEncoding};

use super::uri::from_path;

/// The definition of the symbol at `position` in `text`, re-parsing it. Pure and
/// unit-testable; `uri` is the requesting document (so an intra-file result
/// points back at it) and `packages` supplies the library plus its source roots.
pub fn compute_definition<P: PackageSource>(
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
    packages: &P,
) -> Option<Location> {
    let model = SemanticModel::build(&parse(text).cst);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    definition_for(&model, packages, uri, &line_index, offset, encoding)
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
) -> Option<Location> {
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached model is stale.
            return None;
        }
        let model = snapshot.semantic_model(file);
        // The inner `Option` is the definition (a cursor on nothing definable is
        // a legitimate `None`); the outer distinguishes that from a cache miss.
        Some(definition_for(
            model,
            snapshot,
            uri,
            &line_index,
            offset,
            encoding,
        ))
    }));
    match cached {
        Ok(Some(location)) => location,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_definition(uri, text, position, encoding, snapshot),
    }
}

/// Shared entry point for the fresh-parse and cached-model paths. Mirrors the
/// three shapes of [`hover_content`](super::hover). `line_index` indexes the
/// *current* document, for intra-file results.
fn definition_for<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    uri: &Uri,
    line_index: &LineIndex,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<Location> {
    // A qualified name (`Foo.bar`, `Base.@time`) carries its whole module path.
    if let Some(q) = model
        .qualified_reads()
        .iter()
        .find(|q| q.range.contains_inclusive(offset))
    {
        let (name, module_path) = q.path.split_last()?;
        let head = module_path.first()?;
        let pkg = packages.package(head)?;
        let rest: Vec<&str> = module_path[1..].iter().map(|s| s.as_str()).collect();
        let module = resolve_submodule(&pkg.root, &rest)?;
        return library_location(packages, &pkg, module, name, encoding);
    }
    // An ordinary identifier occurrence: local when it binds, else a free read.
    if let Some(ident) = model.ident_at(offset) {
        if let Some(bid) = ident.binding {
            return Some(self_location(
                uri,
                model.binding(bid).def_range,
                line_index,
                encoding,
            ));
        }
        let ns = if ident.is_macro {
            Namespace::Macro
        } else {
            Namespace::Value
        };
        return free_read_location(
            model,
            packages,
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
    // itself.
    if let Some(bid) = model.binding_at(offset) {
        return Some(self_location(
            uri,
            model.binding(bid).def_range,
            line_index,
            encoding,
        ));
    }
    None
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
/// order to its definition: a binding the occurrence walk missed is still local,
/// otherwise a Base/Core or `using`'d library symbol.
#[allow(clippy::too_many_arguments)]
fn free_read_location<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    uri: &Uri,
    name: &str,
    offset: TextSize,
    ns: Namespace,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Option<Location> {
    match Resolver::new(model, packages).resolve(name, offset, ns) {
        Resolution::Binding(bid) => Some(self_location(
            uri,
            model.binding(bid).def_range,
            line_index,
            encoding,
        )),
        Resolution::System { module, name } => {
            let pkg = packages.package(&module)?;
            library_location(packages, &pkg, &pkg.root, &name, encoding)
        }
        Resolution::Using { module, name } => {
            library_from_using(model, packages, &module, &name, encoding)
        }
        Resolution::Unresolved => None,
    }
}

/// Find the module a whole-module `using` brings in and locate `name` in it.
/// `module` is the clause's display name (its last component): a plain
/// `using LinearAlgebra` names the package directly; a `using A.B` needs the
/// clause walked from its package root. Mirrors hover's `library_from_using`.
fn library_from_using<P: PackageSource>(
    model: &SemanticModel,
    packages: &P,
    module: &str,
    name: &str,
    encoding: PositionEncoding,
) -> Option<Location> {
    if let Some(pkg) = packages.package(module)
        && let Some(location) = library_location(packages, &pkg, &pkg.root, name, encoding)
    {
        return Some(location);
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
        if let Some(m) = resolve_submodule(&pkg.root, &rest)
            && let Some(location) = library_location(packages, &pkg, m, name, encoding)
        {
            return Some(location);
        }
    }
    None
}

/// Turn a library symbol into a [`Location`] in a depot source file: find its
/// [`DefLocation`] in `module`, join the package-relative path with `pkg`'s
/// source root (known only to the live server), read the target file, and
/// convert the byte span to a line/column range. `None` when the root is unknown
/// (e.g. the baked-in fallback Base) or the file cannot be read.
fn library_location<P: PackageSource>(
    packages: &P,
    pkg: &PackageIndex,
    module: &ModuleIndex,
    name: &str,
    encoding: PositionEncoding,
) -> Option<Location> {
    let def = library_def_location(module, name)?;
    let root = packages.package_root(&pkg.name)?;
    let abs = root.join(&def.file);
    let text = std::fs::read_to_string(&abs).ok()?;
    let line_index = LineIndex::new(&text);
    Some(Location {
        uri: from_path(&abs)?,
        range: span_to_range(def.range, &line_index, encoding),
    })
}

/// Look `name` up among `module`'s defined symbols and return its definition
/// location. Mirrors the search order of hover's `render_library_symbol`
/// (macros for an `@` name, then functions, types, consts). A function group
/// resolves to its first method (multiple-dispatch "go to all methods" is a
/// later item).
fn library_def_location<'m>(module: &'m ModuleIndex, name: &str) -> Option<&'m DefLocation> {
    if name.starts_with('@') {
        return module
            .macros
            .iter()
            .find(|m| m.name == name)
            .map(|m| &m.loc);
    }
    if let Some(f) = module.functions.iter().find(|f| f.name == name) {
        return f.methods.first().map(|m| &m.loc);
    }
    if let Some(t) = module.types.iter().find(|t| t.name == name) {
        return Some(&t.loc);
    }
    if let Some(c) = module.consts.iter().find(|c| c.name == name) {
        return Some(&c.loc);
    }
    None
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
    }

    impl PackageSource for TestLib {
        fn package(&self, name: &str) -> Option<Arc<PackageIndex>> {
            self.packages.get(name).cloned()
        }
        fn package_root(&self, name: &str) -> Option<PathBuf> {
            self.roots.get(name).cloned()
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
    fn def_at(marked: &str, lib: &impl PackageSource) -> Option<Location> {
        let offset = marked.find('|').expect("a cursor marker");
        let src = marked.replacen('|', "", 1);
        let line_index = LineIndex::new(&src);
        let position = line_index.byte_to_position(offset, PositionEncoding::Utf16);
        compute_definition(&doc_uri(), &src, position, PositionEncoding::Utf16, lib)
    }

    #[test]
    fn local_variable_jumps_to_its_assignment() {
        let loc = def_at("function f()\n    x = 1\n    x|\nend", &TestLib::default()).unwrap();
        assert_eq!(loc.uri, doc_uri());
        // The `x` in `x = 1` on line 1, column 4.
        assert_eq!(loc.range.start, Position::new(1, 4));
        assert_eq!(loc.range.end, Position::new(1, 5));
    }

    #[test]
    fn call_jumps_to_the_function_definition() {
        let loc = def_at("greet(a) = a\ngreet|(1)", &TestLib::default()).unwrap();
        assert_eq!(loc.uri, doc_uri());
        // The `greet` in the definition on line 0, column 0.
        assert_eq!(loc.range.start, Position::new(0, 0));
        assert_eq!(loc.range.end, Position::new(0, 5));
    }

    #[test]
    fn parameter_use_jumps_to_the_parameter() {
        let loc = def_at("function f(abc)\n    abc|\nend", &TestLib::default()).unwrap();
        assert_eq!(loc.range.start, Position::new(0, 11));
        assert_eq!(loc.range.end, Position::new(0, 14));
    }

    #[test]
    fn unresolved_name_has_no_definition() {
        assert!(def_at("nope|()", &TestLib::default()).is_none());
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

        let loc = def_at("using Greetings\ngreet|(1)", &lib).unwrap();
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

        assert!(def_at("using Greetings\ngreet|(1)", &lib).is_none());
    }
}
