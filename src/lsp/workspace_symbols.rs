//! Workspace symbols (`workspace/symbol`): a fuzzy search over every top-level
//! definition of the package under development. Where document symbols
//! ([`symbols`](super::symbols)) outline a single file's CST, this reads off the
//! harvested [`PackageIndex`](crate::index::PackageIndex) of the workspace
//! package — the module tree the
//! index harvester built by following `include` chains — so a query reaches
//! definitions in files the editor never opened.
//!
//! Each function, type, const, macro, and (sub)module carries a package-relative
//! [`DefLocation`]; it is turned into an on-disk [`Location`] exactly as
//! go-to-definition does ([`definition`](super::definition)): the location's file
//! is joined with the package's source root (known only to the live server), the
//! file read, and the byte span converted to a line/column range. The match is a
//! case-insensitive subsequence, the convention `Ctrl-T`-style symbol pickers
//! expect; an empty query returns everything.

use std::collections::HashMap;
use std::path::PathBuf;

use lsp_types::{Location, OneOf, Range, SymbolKind, WorkspaceSymbol};

use crate::incremental::Analysis;
use crate::index::model::Span;
use crate::index::{DefLocation, ModuleIndex, TypeKind};
use crate::resolve::PackageSource;
use crate::text::{LineIndex, PositionEncoding};

use super::uri::from_path;

/// The workspace symbols matching `query`, drawn from the package under
/// development named `workspace` (one folder's package; the caller merges
/// across folders). Pure and unit-testable; `packages` supplies the harvested
/// index plus its source roots (mirroring [`compute_definition`]'s shape).
/// Yields nothing when the package's index or source root is unknown, or
/// nothing matches.
///
/// [`compute_definition`]: super::definition::compute_definition
pub fn compute_workspace_symbols<P: PackageSource>(
    query: &str,
    packages: &P,
    workspace: &str,
    encoding: PositionEncoding,
) -> Vec<WorkspaceSymbol> {
    let name = workspace;
    let (Some(pkg), Some(root)) = (packages.package(name), packages.package_root(name)) else {
        return Vec::new();
    };
    // Phase 1: walk the module tree, keeping the (name, kind, container,
    // location) of every definition whose name matches the query.
    let mut matches = Vec::new();
    let root_symbol = Candidate {
        name: pkg.root.name.clone(),
        kind: SymbolKind::MODULE,
        container: None,
        loc: &pkg.root.loc,
    };
    consider(root_symbol, query, &mut matches);
    walk_module(&pkg.root, query, &mut matches);

    // Phase 2: materialize each match's location off disk. Files are read once
    // and shared across the symbols they contain (a single file usually holds
    // many); an unreadable file drops its symbols rather than faking a range.
    let mut texts: HashMap<PathBuf, Option<String>> = HashMap::new();
    matches
        .into_iter()
        .filter_map(|m| {
            let abs = root.join(&m.file);
            let text = texts
                .entry(abs.clone())
                .or_insert_with(|| std::fs::read_to_string(&abs).ok())
                .as_deref()?;
            let line_index = LineIndex::new(text);
            Some(WorkspaceSymbol {
                name: m.name,
                kind: m.kind,
                tags: None,
                container_name: m.container,
                location: OneOf::Left(Location {
                    uri: from_path(&abs)?,
                    range: span_to_range(m.span, &line_index, encoding),
                }),
                data: None,
            })
        })
        .collect()
}

/// Compute workspace symbols off the snapshot's library index. Unlike the
/// per-document features there is no live-buffer or cached-parse gate: the result
/// is a projection of the harvested [`PackageIndex`](crate::index::PackageIndex)
/// in the `LibraryIndex` salsa
/// input, independent of any open document. The [`Analysis`] snapshot is itself
/// the [`PackageSource`].
pub(crate) fn workspace_symbols_via_db(
    snapshot: &Analysis,
    query: &str,
    encoding: PositionEncoding,
) -> Vec<WorkspaceSymbol> {
    snapshot
        .workspace_packages()
        .iter()
        .flat_map(|name| compute_workspace_symbols(query, snapshot, name, encoding))
        .collect()
}

/// A definition that matched the query, before its location is read off disk.
/// Borrows the `DefLocation` (`file` + `range`) from the harvested index.
struct Candidate<'a> {
    name: String,
    kind: SymbolKind,
    container: Option<String>,
    loc: &'a DefLocation,
}

/// Emit `candidate` when its name is a subsequence of `query`.
fn consider(candidate: Candidate<'_>, query: &str, out: &mut Vec<RawMatch>) {
    if subsequence_match(query, &candidate.name) {
        out.push(RawMatch {
            name: candidate.name,
            kind: candidate.kind,
            container: candidate.container,
            file: candidate.loc.file.clone(),
            span: candidate.loc.range,
        });
    }
}

/// A matched definition with its location detached from the borrowed index, so
/// the phase-2 file reads own no borrow of the
/// [`PackageIndex`](crate::index::PackageIndex).
struct RawMatch {
    name: String,
    kind: SymbolKind,
    container: Option<String>,
    file: PathBuf,
    span: Span,
}

/// Collect `module`'s members (functions, types, consts, macros), then each
/// submodule (as a `MODULE` symbol) and recurse. The container of every member
/// is the enclosing module's name, matching how a picker qualifies results.
fn walk_module(module: &ModuleIndex, query: &str, out: &mut Vec<RawMatch>) {
    let container = Some(module.name.clone());
    for f in &module.functions {
        // A function group shares one name across its methods; point at the
        // first method's definition (multiple-dispatch "list all" is a later
        // item, as in go-to-definition).
        if let Some(method) = f.methods.first() {
            consider(
                Candidate {
                    name: f.name.clone(),
                    kind: SymbolKind::FUNCTION,
                    container: container.clone(),
                    loc: &method.loc,
                },
                query,
                out,
            );
        }
    }
    for t in &module.types {
        let kind = match t.kind {
            TypeKind::Struct { .. } | TypeKind::Primitive { .. } => SymbolKind::STRUCT,
            TypeKind::Abstract => SymbolKind::INTERFACE,
        };
        consider(
            Candidate {
                name: t.name.clone(),
                kind,
                container: container.clone(),
                loc: &t.loc,
            },
            query,
            out,
        );
    }
    for c in &module.consts {
        consider(
            Candidate {
                name: c.name.clone(),
                kind: SymbolKind::CONSTANT,
                container: container.clone(),
                loc: &c.loc,
            },
            query,
            out,
        );
    }
    for m in &module.macros {
        // The macro name keeps its `@` sigil (as harvested); a query typed with
        // or without the `@` still subsequence-matches.
        consider(
            Candidate {
                name: m.name.clone(),
                kind: SymbolKind::FUNCTION,
                container: container.clone(),
                loc: &m.loc,
            },
            query,
            out,
        );
    }
    for sub in &module.submodules {
        consider(
            Candidate {
                name: sub.name.clone(),
                kind: SymbolKind::MODULE,
                container: container.clone(),
                loc: &sub.loc,
            },
            query,
            out,
        );
        walk_module(sub, query, out);
    }
}

/// Whether `query` is a case-insensitive subsequence of `name` (its characters
/// appear in order, not necessarily contiguously). An empty query matches every
/// name — the usual "show all" behavior of a symbol picker.
fn subsequence_match(query: &str, name: &str) -> bool {
    let mut needle = query.chars().flat_map(char::to_lowercase).peekable();
    if needle.peek().is_none() {
        return true;
    }
    for hay in name.chars().flat_map(char::to_lowercase) {
        if needle.peek() == Some(&hay) {
            needle.next();
        }
    }
    needle.peek().is_none()
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use lsp_types::Position;

    use crate::index::{PackageIndex, harvest_package_named};

    use super::super::uri::to_path;

    /// A library with package indexes plus their source roots and the workspace
    /// package name, so the workspace-symbol path can join a package-relative
    /// [`DefLocation`] with an on-disk root. Mirrors `definition.rs`'s `TestLib`.
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

    /// A unique temp directory removed on drop (mirrors `definition.rs`).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("fatou-ws-{}-{}", std::process::id(), n));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Harvest an on-disk package `Name` whose `src/Name.jl` is `entry`, plus any
    /// extra `(relative_path, contents)` files, and wrap it in a [`TestLib`] with
    /// the workspace name returned alongside.
    fn harvest(files: &[(&str, &str)], name: &str) -> (TempDir, TestLib) {
        let tmp = TempDir::new();
        for (rel, contents) in files {
            let path = tmp.path.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
        }
        let pkg = harvest_package_named(&tmp.path, name);
        let mut lib = TestLib::default();
        lib.packages.insert(name.to_string(), Arc::new(pkg));
        lib.roots.insert(name.to_string(), tmp.path.clone());
        (tmp, lib)
    }

    /// The matched symbol names for `query`, sorted for order-independent asserts.
    fn names(query: &str, lib: &TestLib, workspace: &str) -> Vec<String> {
        let mut got: Vec<String> =
            compute_workspace_symbols(query, lib, workspace, PositionEncoding::Utf16)
                .into_iter()
                .map(|s| s.name)
                .collect();
        got.sort();
        got
    }

    #[test]
    fn subsequence_matches_case_insensitively_and_out_of_order() {
        assert!(subsequence_match("", "anything"));
        assert!(subsequence_match("fb", "FooBar"));
        assert!(subsequence_match("FOOBAR", "foobar"));
        assert!(subsequence_match("foo", "foo"));
        assert!(!subsequence_match("bf", "FooBar")); // wrong order
        assert!(!subsequence_match("xyz", "FooBar"));
        assert!(!subsequence_match("foobarbaz", "foobar")); // needle longer
    }

    #[test]
    fn empty_query_returns_every_top_level_symbol() {
        let (_tmp, lib) = harvest(
            &[(
                "src/MyPkg.jl",
                "module MyPkg\n\
                 foo(x) = x\n\
                 bar(x) = x\n\
                 struct Point end\n\
                 abstract type Shape end\n\
                 const K = 1\n\
                 macro m(x) end\n\
                 end\n",
            )],
            "MyPkg",
        );
        // The module itself plus its six members.
        assert_eq!(
            names("", &lib, "MyPkg"),
            vec!["@m", "K", "MyPkg", "Point", "Shape", "bar", "foo"]
        );
    }

    #[test]
    fn subsequence_query_filters() {
        let (_tmp, lib) = harvest(
            &[(
                "src/MyPkg.jl",
                "module MyPkg\nfoobar(x) = x\nfizz(x) = x\nother(x) = x\nend\n",
            )],
            "MyPkg",
        );
        assert_eq!(names("fb", &lib, "MyPkg"), vec!["foobar"]);
        assert_eq!(names("f", &lib, "MyPkg"), vec!["fizz", "foobar"]);
    }

    #[test]
    fn unknown_workspace_returns_empty() {
        let (_tmp, lib) = harvest(
            &[("src/MyPkg.jl", "module MyPkg\nfoo(x) = x\nend\n")],
            "MyPkg",
        );
        assert!(
            compute_workspace_symbols("foo", &lib, "Missing", PositionEncoding::Utf16).is_empty()
        );
    }

    #[test]
    fn submodule_members_are_included_with_container() {
        let (_tmp, lib) = harvest(
            &[(
                "src/MyPkg.jl",
                "module MyPkg\nmodule Inner\ninner_fn(x) = x\nend\nend\n",
            )],
            "MyPkg",
        );
        let syms = compute_workspace_symbols("inner_fn", &lib, "MyPkg", PositionEncoding::Utf16);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "inner_fn");
        assert_eq!(syms[0].container_name.as_deref(), Some("Inner"));
    }

    #[test]
    fn macro_matches_by_bare_name_and_resolves_location() {
        let (tmp, lib) = harvest(
            &[("src/MyPkg.jl", "module MyPkg\nmacro timed(x) end\nend\n")],
            "MyPkg",
        );
        // Typed without the sigil, `timed` is still a subsequence of `@timed`.
        let syms = compute_workspace_symbols("timed", &lib, "MyPkg", PositionEncoding::Utf16);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "@timed");
        let OneOf::Left(loc) = &syms[0].location else {
            panic!("expected a resolved location");
        };
        assert_eq!(
            to_path(&loc.uri),
            Some(tmp.path.join("src").join("MyPkg.jl"))
        );
        // `timed` on line 1, after `macro ` (columns 6..11).
        assert_eq!(loc.range.start, Position::new(1, 6));
        assert_eq!(loc.range.end, Position::new(1, 11));
    }

    #[test]
    fn location_points_across_files() {
        // `bar` lives in an included sibling file, not the entry file.
        let (tmp, lib) = harvest(
            &[
                ("src/MyPkg.jl", "module MyPkg\ninclude(\"bar.jl\")\nend\n"),
                ("src/bar.jl", "bar(x) = x\n"),
            ],
            "MyPkg",
        );
        let syms = compute_workspace_symbols("bar", &lib, "MyPkg", PositionEncoding::Utf16);
        assert_eq!(syms.len(), 1);
        let OneOf::Left(loc) = &syms[0].location else {
            panic!("expected a resolved location");
        };
        assert_eq!(to_path(&loc.uri), Some(tmp.path.join("src").join("bar.jl")));
        assert_eq!(loc.range.start, Position::new(0, 0));
        assert_eq!(loc.range.end, Position::new(0, 3));
    }

    #[test]
    fn unknown_root_yields_nothing() {
        // A package index with no registered source root cannot materialize
        // locations, so it yields no symbols rather than bogus paths.
        let tmp = TempDir::new();
        let entry = tmp.path.join("src").join("MyPkg.jl");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(&entry, "module MyPkg\nfoo(x) = x\nend\n").unwrap();
        let pkg = harvest_package_named(&tmp.path, "MyPkg");
        let mut lib = TestLib::default();
        lib.packages.insert("MyPkg".to_string(), Arc::new(pkg));
        // No `roots` entry.
        assert!(
            compute_workspace_symbols("foo", &lib, "MyPkg", PositionEncoding::Utf16).is_empty()
        );
    }
}
