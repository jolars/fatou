//! Salsa-backed incremental layer: file text → parse tree.
//!
//! The CST is cached as a `rowan::GreenNode` (Arc-backed, `Send + Sync`) rather
//! than a `SyntaxNode` (which holds non-`Send` cursor state and is neither `Eq`
//! nor `salsa::Update`). Callers materialize a fresh cursor via
//! [`parsed_tree_root`] — a cheap atomic clone.
//!
//! This honors Tenet 2 (incremental parsing is first-class): a text edit
//! invalidates only [`parsed_document`] and its dependents. The token/block
//! reparse *splicing* that makes a single-keystroke edit cheaper than a full
//! parse is deferred (see `TODO.md`); today every edit triggers a full parse,
//! which is still correct.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use salsa::{Durability, Setter};

use crate::index::PackageIndex;
use crate::parser::{ParseDiagnostic, parse};
use crate::project::{self, IncludeEdge};
use crate::resolve::{Candidate, Namespace, PackageSource, Resolution, Resolver};
use crate::semantic::SemanticModel;
use crate::syntax::SyntaxNode;

use rowan::TextSize;

/// An opaque, process-local file identity, allocated once when a file is first
/// seen and never reused. The stable handle the rest of the system keys on
/// without a path leaking in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

#[salsa::input]
pub struct SourceFile {
    /// This file's opaque identity. Set once, never mutated.
    pub id: FileId,
    /// The path this file was tracked under, or `None` for an in-memory
    /// document. Set once at creation and never mutated.
    #[returns(ref)]
    pub path: Option<PathBuf>,
    #[returns(ref)]
    pub text: String,
}

/// The harvested library index: every resolved package's [`PackageIndex`]
/// keyed by name. Wrapped so it carries an opaque whole-value [`salsa::Update`]
/// (the model types stay salsa-free), and so the map is cheap to swap — each
/// value is an [`Arc`], so replacing one package clones only pointers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryPackages(pub BTreeMap<String, Arc<PackageIndex>>);

// SAFETY: `maybe_update` overwrites the whole value when it differs (by `Eq`)
// and reports the change, leaving no dangling references — the standard opaque
// leaf pattern. This is why the [`PackageIndex`] model needs no `Update` impl.
unsafe impl salsa::Update for LibraryPackages {
    unsafe fn maybe_update(old_pointer: *mut Self, new: Self) -> bool {
        let old = unsafe { &mut *old_pointer };
        if *old == new {
            false
        } else {
            *old = new;
            true
        }
    }
}

/// The whole harvested library, as a single HIGH-durability salsa input. One
/// input holds every package (rather than one input per package): the library
/// set changes only on a manifest change or a re-harvest, which HIGH durability
/// encodes, and there are no per-package incremental consumers yet. Per-package
/// replacement stays cheap because [`LibraryPackages`] holds `Arc`s.
#[salsa::input(singleton)]
pub struct LibraryIndex {
    #[returns(ref)]
    pub packages: LibraryPackages,
}

/// The cached parse of a file. The `GreenNode` is not `Eq`/`salsa::Update`, so
/// [`parsed_document`] is `no_eq, unsafe(non_update_types)`: salsa never
/// compares parse outputs and relies purely on input (text) change detection to
/// invalidate. Sound because the tree is a pure function of the text.
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    pub green: rowan::GreenNode,
    pub diagnostics: Vec<ParseDiagnostic>,
}

#[salsa::db]
pub trait IncrementalDb: salsa::Database {}

/// Parse `file`'s text into a cached green tree plus diagnostics.
#[salsa::tracked(returns(ref), no_eq, unsafe(non_update_types))]
pub fn parsed_document(db: &dyn IncrementalDb, file: SourceFile) -> ParsedDocument {
    let text = file.text(db);
    let parsed = parse(text.as_str());
    ParsedDocument {
        green: parsed.cst.green().into_owned(),
        diagnostics: parsed.diagnostics,
    }
}

/// The parse diagnostics for `file` (empty when it parses cleanly).
pub fn parse_diagnostics(db: &dyn IncrementalDb, file: SourceFile) -> &[ParseDiagnostic] {
    &parsed_document(db, file).diagnostics
}

/// Materialize the cached parse for `file` as a fresh `SyntaxNode` cursor.
pub fn parsed_tree_root(db: &dyn IncrementalDb, file: SourceFile) -> SyntaxNode {
    SyntaxNode::new_root(parsed_document(db, file).green.clone())
}

/// The per-file semantic model (scope tree, bindings, reads), built from the
/// cached parse. Unlike [`parsed_document`] this query keeps structural `Eq`:
/// when an edit leaves the model unchanged (the model carries text ranges, so
/// this means same-shape edits), salsa backdates it and dependents are not
/// re-run. The robust invalidation barrier for position-shifting edits is the
/// range-free firewall projections (`file_exports`, `file_free_reads`; see
/// `TODO.md` Phase 2), which layer on top of this query.
#[salsa::tracked(returns(ref), unsafe(non_update_types))]
pub fn semantic_model(db: &dyn IncrementalDb, file: SourceFile) -> SemanticModel {
    SemanticModel::build(&parsed_tree_root(db, file))
}

// The firewall queries: range-free projections of [`semantic_model`] (or, for
// [`include_edges`], of the parse tree). Each re-executes on any edit but
// returns an `Eq` value unchanged by a body edit or a mere position shift, so
// salsa backdates it and the project-level memos built on top are not rebuilt.
// See [`crate::project`]. Together they are the barrier the `semantic_model`
// doc above forward-refers to.

/// The file's top-level definitions ([`crate::project::file_exports`]): what
/// another file that `include`s this one sees. Editing a function body changes
/// [`semantic_model`] but leaves this `BTreeSet` equal, so salsa backdates.
#[salsa::tracked(returns(ref))]
pub fn file_exports(db: &dyn IncrementalDb, file: SourceFile) -> BTreeSet<String> {
    project::file_exports(semantic_model(db, file))
}

/// The names the file reads but binds nowhere in it
/// ([`crate::project::file_free_reads`]). The mirror firewall to
/// [`file_exports`].
#[salsa::tracked(returns(ref))]
pub fn file_free_reads(db: &dyn IncrementalDb, file: SourceFile) -> BTreeSet<String> {
    project::file_free_reads(semantic_model(db, file))
}

/// The module-qualified names the file references, each a full dotted path
/// ([`crate::project::file_qualified_reads`]).
#[salsa::tracked(returns(ref))]
pub fn file_qualified_reads(db: &dyn IncrementalDb, file: SourceFile) -> BTreeSet<String> {
    project::file_qualified_reads(semantic_model(db, file))
}

/// The file's static `include("literal")` edges, in source order, resolved
/// against the file's own directory ([`crate::project::include_edges`]). The
/// path is an input field set once, so this re-runs only on a text edit and
/// backdates when the edges are unchanged.
#[salsa::tracked(returns(ref))]
pub fn include_edges(db: &dyn IncrementalDb, file: SourceFile) -> Vec<IncludeEdge> {
    let root = parsed_tree_root(db, file);
    let base_dir = file.path(db).as_deref().and_then(Path::parent);
    project::include_edges(&root, base_dir)
}

/// Lexically normalize `path` for use as a deduplication key: absolutize it
/// (without touching the filesystem) and collapse `.`/`..` segments. Purely
/// textual, so it is stable for not-yet-saved buffers and never blocks on I/O.
pub fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The normalized-path → input index plus the [`FileId`] allocator, so reaching
/// the same file by an equivalent path spelling reuses one input (and its cached
/// queries). In-memory files get a [`FileId`] but no entry here.
#[derive(Default)]
struct FileSourceMap {
    by_path: HashMap<PathBuf, SourceFile>,
    next_id: u32,
}

impl FileSourceMap {
    fn alloc_id(&mut self) -> FileId {
        let id = FileId(self.next_id);
        self.next_id += 1;
        id
    }
}

#[salsa::db]
pub struct IncrementalDatabase {
    storage: salsa::Storage<Self>,
    source_map: Arc<Mutex<FileSourceMap>>,
}

impl Default for IncrementalDatabase {
    fn default() -> Self {
        Self {
            storage: salsa::Storage::new(None),
            source_map: Arc::new(Mutex::new(FileSourceMap::default())),
        }
    }
}

/// Cloning yields a second handle onto the *same* salsa storage (a cheap
/// `Arc`-bump of the shared state, plus the shared path→input map). This is how
/// the language server runs read-only queries off the analysis thread: the
/// owner mints a short-lived clone (see [`snapshot`](IncrementalDatabase::snapshot)),
/// hands it to a worker, and the clone is dropped promptly. Salsa is
/// single-writer — a clone outstanding when the owner performs a write blocks
/// that write until the clone drops (and trips `salsa::Cancelled` in any read
/// still in flight), so clones must never be held across a write or parked
/// long-term.
impl Clone for IncrementalDatabase {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            source_map: Arc::clone(&self.source_map),
        }
    }
}

impl std::fmt::Debug for IncrementalDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IncrementalDatabase")
            .finish_non_exhaustive()
    }
}

#[salsa::db]
impl salsa::Database for IncrementalDatabase {}

#[salsa::db]
impl IncrementalDb for IncrementalDatabase {}

impl IncrementalDatabase {
    pub fn new() -> Self {
        Self::default()
    }

    /// Track an in-memory document with no on-disk path. Gets a fresh
    /// [`FileId`] and a `None` path, so it never aliases another file.
    pub fn add_file(&self, text: impl Into<String>) -> SourceFile {
        let id = self
            .source_map
            .lock()
            .expect("file source map mutex poisoned")
            .alloc_id();
        SourceFile::new(self, id, None, text.into())
    }

    /// Track (or reuse) the file at `path`, replacing its text. Equivalent path
    /// spellings map to the same input.
    pub fn upsert_file(&mut self, path: &Path, text: String) -> SourceFile {
        let key = normalize_path(path);
        let existing = self
            .source_map
            .lock()
            .expect("file source map mutex poisoned")
            .by_path
            .get(&key)
            .copied();
        match existing {
            Some(file) => {
                if file.text(self) != &text {
                    file.set_text(self).to(text);
                }
                file
            }
            None => {
                let id = self
                    .source_map
                    .lock()
                    .expect("file source map mutex poisoned")
                    .alloc_id();
                let file = SourceFile::new(self, id, Some(key.clone()), text);
                self.source_map
                    .lock()
                    .expect("file source map mutex poisoned")
                    .by_path
                    .insert(key, file);
                file
            }
        }
    }

    /// Look up the input tracked for `path`, if any.
    pub fn lookup_file(&self, path: &Path) -> Option<SourceFile> {
        let key = normalize_path(path);
        self.source_map
            .lock()
            .expect("file source map mutex poisoned")
            .by_path
            .get(&key)
            .copied()
    }

    pub fn set_file_text(&mut self, file: SourceFile, text: impl Into<String>) {
        file.set_text(self).to(text.into());
    }

    /// The text currently tracked for `file`.
    pub fn file_text(&self, file: SourceFile) -> &str {
        file.text(self).as_str()
    }

    pub fn parsed_tree(&self, file: SourceFile) -> SyntaxNode {
        parsed_tree_root(self, file)
    }

    /// Replace the whole harvested library index, at HIGH durability. Creates
    /// the singleton input on first call. Re-analyze open files after a swap:
    /// dependents of the index (once they exist) will have been invalidated.
    pub fn set_library_packages(&mut self, packages: BTreeMap<String, Arc<PackageIndex>>) {
        let value = LibraryPackages(packages);
        match LibraryIndex::try_get(self) {
            Some(index) => {
                index
                    .set_packages(self)
                    .with_durability(Durability::HIGH)
                    .to(value);
            }
            None => {
                // Creating the singleton input registers it in storage; the
                // handle is refetched via `try_get` on later calls.
                let _ = LibraryIndex::builder(value)
                    .durability(Durability::HIGH)
                    .new(self);
            }
        }
    }

    /// Insert or replace a single package's index, keeping the rest. Cheap: the
    /// map's other entries are `Arc` pointer clones.
    pub fn set_package_index(&mut self, name: impl Into<String>, index: Arc<PackageIndex>) {
        let mut packages = LibraryIndex::try_get(self)
            .map(|lib| lib.packages(self).0.clone())
            .unwrap_or_default();
        packages.insert(name.into(), index);
        self.set_library_packages(packages);
    }

    /// The harvested index for `name`, if the library has been populated and
    /// contains it.
    pub fn library_package(&self, name: &str) -> Option<Arc<PackageIndex>> {
        LibraryIndex::try_get(self)?
            .packages(self)
            .0
            .get(name)
            .cloned()
    }

    /// Mint a read-only [`Analysis`] snapshot: a short-lived db clone wrapped so
    /// callers can only *read*. Drop it promptly — an outstanding clone blocks
    /// the next write (salsa is single-writer; see the [`Clone`] impl).
    pub fn snapshot(&self) -> Analysis {
        Analysis(self.clone())
    }
}

/// A read-only handle onto the incremental database, à la rust-analyzer's
/// `Analysis` (vs. its writer `AnalysisHost`). Wraps a short-lived clone of the
/// analysis thread's [`IncrementalDatabase`] and exposes *only* read queries,
/// so a read job cannot call `upsert_file` or salsa setters — the single-writer
/// invariant is encoded in the type system rather than left to convention.
pub struct Analysis(IncrementalDatabase);

impl Analysis {
    /// The `SourceFile` input currently tracked for `path`, if any.
    pub fn lookup_file(&self, path: &Path) -> Option<SourceFile> {
        self.0.lookup_file(path)
    }

    /// The text currently tracked for `file`.
    pub fn file_text(&self, file: SourceFile) -> &str {
        self.0.file_text(file)
    }

    /// Parse diagnostics for `file` (empty when it parses cleanly).
    pub fn parse_diagnostics(&self, file: SourceFile) -> &[ParseDiagnostic] {
        parse_diagnostics(&self.0, file)
    }

    /// A fresh `SyntaxNode` over the cached parse tree.
    pub fn parsed_tree(&self, file: SourceFile) -> SyntaxNode {
        self.0.parsed_tree(file)
    }

    /// The cached semantic model for `file`.
    pub fn semantic_model(&self, file: SourceFile) -> &SemanticModel {
        semantic_model(&self.0, file)
    }

    /// The file's top-level definitions (the [`file_exports`] firewall query).
    pub fn file_exports(&self, file: SourceFile) -> &BTreeSet<String> {
        file_exports(&self.0, file)
    }

    /// The names the file reads but binds nowhere (the [`file_free_reads`]
    /// firewall query).
    pub fn file_free_reads(&self, file: SourceFile) -> &BTreeSet<String> {
        file_free_reads(&self.0, file)
    }

    /// The module-qualified names the file references (the
    /// [`file_qualified_reads`] firewall query).
    pub fn file_qualified_reads(&self, file: SourceFile) -> &BTreeSet<String> {
        file_qualified_reads(&self.0, file)
    }

    /// The file's static `include` edges (the [`include_edges`] firewall query).
    pub fn include_edges(&self, file: SourceFile) -> &[IncludeEdge] {
        include_edges(&self.0, file)
    }

    /// The harvested index for package `name`, if present.
    pub fn library_package(&self, name: &str) -> Option<Arc<PackageIndex>> {
        self.0.library_package(name)
    }

    /// Resolve `name` read at `offset` in `file` through the shared masking
    /// order (locals/imports, then `using`'d exports, then Base/Core). `name` is
    /// bare even for a macro; pick the namespace with `namespace`.
    pub fn resolve_name(
        &self,
        file: SourceFile,
        name: &str,
        offset: TextSize,
        namespace: Namespace,
    ) -> Resolution {
        Resolver::new(self.semantic_model(file), self).resolve(name, offset, namespace)
    }

    /// Every name visible at `offset` in `file`, in the shared masking order
    /// with shadowed names dropped. For completion.
    pub fn visible_names(
        &self,
        file: SourceFile,
        offset: TextSize,
        namespace: Namespace,
    ) -> Vec<Candidate> {
        Resolver::new(self.semantic_model(file), self).visible(offset, namespace)
    }
}

/// The library seen through a read-only snapshot, so a [`Resolver`] can run off
/// the analysis thread.
impl PackageSource for Analysis {
    fn package(&self, name: &str) -> Option<Arc<PackageIndex>> {
        self.library_package(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_reparses_on_edit() {
        let mut db = IncrementalDatabase::new();
        let file = db.add_file("x = 1\n");
        assert_eq!(parsed_tree_root(&db, file).to_string(), "x = 1\n");

        db.set_file_text(file, "x = 2 + 3\n");
        let root = parsed_tree_root(&db, file);
        assert_eq!(root.to_string(), "x = 2 + 3\n");
        assert!(parse_diagnostics(&db, file).is_empty());
    }

    #[test]
    fn upsert_dedups_by_normalized_path() {
        let mut db = IncrementalDatabase::new();
        let a = db.upsert_file(Path::new("/tmp/a.jl"), "x = 1\n".into());
        let b = db.upsert_file(Path::new("/tmp/./a.jl"), "x = 2\n".into());
        assert!(a == b, "equivalent path spellings should reuse one input");
        assert_eq!(parsed_tree_root(&db, a).to_string(), "x = 2\n");
    }
}
