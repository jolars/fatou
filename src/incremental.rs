//! Salsa-backed incremental layer: file text → parse tree.
//!
//! The CST is cached as a `rowan::GreenNode` (Arc-backed, `Send + Sync`) rather
//! than a `SyntaxNode` (which holds non-`Send` cursor state and is not `Eq`).
//! Callers materialize a fresh cursor via
//! [`parsed_tree_root`] — a cheap atomic clone.
//!
//! This honors Tenet 2 (incremental parsing is first-class): a text edit
//! invalidates only [`parsed_document`] and its dependents, and the query then
//! splices the previous tree rather than reparsing the file. It works off a
//! side-channel that lives beside salsa rather than inside it: [`PrevParse`],
//! the tree to splice against, plus the chain of [`Edit`]s the language server
//! staged since that tree was built. Both are hints — every route through
//! [`parsed_document`] returns exactly what `parse_with_options(text, options)` would (Tenet 4), so
//! a cold, stale, or evicted cache costs a full parse and nothing else.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use salsa::{Durability, Setter};

use crate::environment::PackageMeta;
use crate::index::{DeclaredDeps, PackageIndex};
use crate::parse_context::{EditionSelection, SourceContext};
use crate::parser::{Edit, ParseDiagnostic, ReparseTier, diff_edit};
use crate::parser::{
    ParseOptions, ReparseBase, parse_with_options, reparse_edits_with_options, reparse_with_options,
};
use crate::project::{self, IncludeEdge};
use crate::resolve::{
    Candidate, ModulePath, Namespace, OccurrenceKey, OccurrenceRec, PackageSource, Resolution,
    Resolver,
};
use crate::semantic::{FileControlFlow, SemanticModel};
use crate::syntax::SyntaxNode;

use rowan::TextSize;
use smol_str::SmolStr;

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
    /// The document text as a shared immutable handle: setting it moves an
    /// `Arc` in, and [`PrevParse`] and the live buffer share the same
    /// allocation, so text changes hands without being copied.
    #[returns(ref)]
    pub text: Arc<str>,
}

/// The harvested library index: every resolved package's [`PackageIndex`]
/// keyed by name. Wrapped so the map stays an opaque whole-value leaf (the
/// model types stay salsa-free; salsa compares by `Eq` and swaps the whole
/// value), and so the swap is cheap — each value is an [`Arc`], so replacing
/// one package clones only pointers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryPackages(pub BTreeMap<String, Arc<PackageIndex>>);

/// Each harvested package's absolute source root (the directory its
/// [`DefLocation::file`](crate::index::model::DefLocation) paths are relative
/// to), keyed by name. Kept beside [`LibraryPackages`] rather than in the
/// serializable [`PackageIndex`] model, which is deliberately depot-independent:
/// only the live server, which knows where the depot is on disk, fills this, and
/// go-to-definition joins a root with a relative path to reach the real file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryRoots(pub BTreeMap<String, PathBuf>);

/// Every manifest-pinned package's version and kind, keyed by name — what a
/// hover over a `[deps]` entry reports. Wrapped for the same reason as
/// [`LibraryPackages`], and keyed differently from [`LibraryRoots`] on purpose:
/// see [`HarvestedLibrary::deps`](crate::index::HarvestedLibrary::deps).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryDeps(pub BTreeMap<String, PackageMeta>);

/// Each workspace package's project file, keyed by package name, as the
/// ordinary [`SourceFile`] input holding its text. Its own input rather than a
/// [`LibraryIndex`] field: it comes from the project files, not the harvest, so
/// a `Project.toml` edit moves it without a re-harvest and a re-harvest leaves
/// it alone. Absent until the environment resolves, which leaves
/// `unresolved-import` silent.
///
/// HIGH durability covers the *mapping* only — which file belongs to which
/// package changes on a re-resolve. The file's text stays a LOW-durability
/// `SourceFile` an editor may rewrite per keystroke, and
/// [`project_declared_deps`] derives the dependency set from it. Thus `[deps]`
/// follows the unsaved buffer without a project-file edit invalidating `.jl`
/// parses.
#[salsa::input(singleton)]
pub struct ProjectFiles {
    #[returns(ref)]
    pub files: BTreeMap<String, SourceFile>,
}

/// The whole harvested library, as a single HIGH-durability salsa input. One
/// input holds every package (rather than one input per package): the library
/// set changes only on a manifest change or a re-harvest, which HIGH durability
/// encodes, and there are no per-package incremental consumers yet. Per-package
/// replacement stays cheap because [`LibraryPackages`] holds `Arc`s. The
/// package-name -> source-root map rides alongside so go-to-definition can turn
/// a package-relative `DefLocation` into an on-disk path.
#[salsa::input(singleton)]
pub struct LibraryIndex {
    #[returns(ref)]
    pub packages: LibraryPackages,
    #[returns(ref)]
    pub roots: LibraryRoots,
    /// The names of the packages under development (one per workspace folder
    /// that is a package project), sorted, each keying an entry in
    /// [`packages`](LibraryIndex::packages)/[`roots`](LibraryIndex::roots).
    /// Empty when no folder is a package project. Distinguishes the live,
    /// editable, re-harvested-on-save packages from the read-only depot set.
    #[returns(ref)]
    pub workspaces: Vec<String>,
    /// What the manifest pinned, as opposed to what the harvest found. Written
    /// by [`IncrementalDatabase::set_library_deps`], separately from the other
    /// three — see there for why.
    #[returns(ref)]
    pub deps: LibraryDeps,
}

/// The workspace package's member files as salsa input handles, so the
/// demand-only reverse-occurrence index ([`workspace_reference_index`],
/// cross-file references/rename) can fan out over exactly the package's source
/// set. Rebuilt on each (re-)harvest from [`PackageIndex::members`]. A separate
/// input from [`LibraryIndex`] because it changes at a different cadence (its
/// members are LOW-durability editable buffers, not the HIGH-durability library).
#[salsa::input(singleton)]
pub struct WorkspaceFiles {
    #[returns(ref)]
    pub files: Vec<SourceFile>,
    /// Files with a known loading root but an uncertain edition. File moves
    /// must still check their include sites, without seeding symbol resolution.
    #[returns(ref)]
    rename_only: Vec<SourceFile>,
}

/// Availability and membership loaded by the script background worker. Text
/// stays in ordinary SourceFile inputs so unsaved edits use the same queries.
#[salsa::input(singleton)]
pub struct ScriptFiles {
    #[returns(ref)]
    pub files: BTreeMap<PathBuf, Option<SourceFile>>,
    #[returns(ref)]
    pub entries: BTreeSet<PathBuf>,
    pub pending: bool,
}

#[salsa::tracked(returns(ref))]
pub fn script_file_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> Arc<project::scripts::ScriptFile> {
    Arc::new(project::scripts::ScriptFile::project(
        &parsed_tree_root_with_options(db, file, options),
        semantic_model_with_options(db, file, options),
        file.path(db).as_deref().expect("script files have paths"),
        parse_diagnostics_with_options(db, file, options).is_empty(),
    ))
}

pub fn script_file(db: &dyn IncrementalDb, file: SourceFile) -> &Arc<project::scripts::ScriptFile> {
    script_file_with_options(db, file, primary_parse_options(db, file))
}

#[salsa::tracked(returns(ref))]
pub fn script_program_with_options(
    db: &dyn IncrementalDb,
    entry: SourceFile,
    options: ParseOptions,
) -> Arc<project::scripts::ScriptProgram> {
    let inputs = ScriptFiles::try_get(db);
    Arc::new(
        project::scripts::ScriptProgram::build_with_availability(
            entry
                .path(db)
                .as_deref()
                .expect("script entries have paths"),
            |path| {
                use project::scripts::ScriptFileState;
                match inputs.and_then(|input| input.files(db).get(path)) {
                    Some(Some(file)) => ScriptFileState::Available(Arc::clone(
                        script_file_with_options(db, *file, options),
                    )),
                    Some(None) => ScriptFileState::Failed,
                    None => ScriptFileState::Pending,
                }
            },
        )
        .with_parse_options(options),
    )
}

pub fn script_program(
    db: &dyn IncrementalDb,
    entry: SourceFile,
) -> &Arc<project::scripts::ScriptProgram> {
    script_program_with_options(db, entry, primary_parse_options(db, entry))
}

/// Current package include closures, including module uses from live buffers.
/// Range-free file projections keep literal and position edits from rebuilding
/// this result, while changes to global reads refresh dependent diagnostics.
#[salsa::tracked(returns(ref))]
fn workspace_import_programs(
    db: &dyn IncrementalDb,
    files: WorkspaceFiles,
    library: LibraryIndex,
) -> Vec<Arc<project::scripts::ScriptProgram>> {
    let by_path: BTreeMap<_, _> = files
        .files(db)
        .iter()
        .filter_map(|&file| Some((normalize_path(file.path(db).as_deref()?), file)))
        .collect();
    library
        .workspaces(db)
        .iter()
        .filter_map(|name| {
            let entry = library
                .roots(db)
                .0
                .get(name)?
                .join("src")
                .join(format!("{name}.jl"));
            let options = primary_parse_options(db, *by_path.get(&entry)?);
            Some(Arc::new(
                project::scripts::ScriptProgram::build(&entry, |path| {
                    Some(Arc::clone(script_file_with_options(
                        db,
                        *by_path.get(path)?,
                        options,
                    )))
                })
                .with_parse_options(options),
            ))
        })
        .collect()
}

/// One source can be loaded in several editions while retaining one text input.
pub fn parse_diagnostics_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> &[ParseDiagnostic] {
    &parsed_document_with_options(db, file, options).diagnostics
}
pub fn parsed_tree_root_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> SyntaxNode {
    SyntaxNode::new_root(
        parsed_document_with_options(db, file, options)
            .green
            .clone(),
    )
}

#[salsa::input(singleton)]
pub struct LibrarySyntaxInput {
    #[returns(ref)]
    pub data: Arc<crate::index::LibrarySyntax>,
}

#[salsa::tracked(returns(ref))]
fn manifest_editions(
    db: &dyn IncrementalDb,
    file: SourceFile,
) -> Arc<crate::parse_context::ManifestEditions> {
    let path = file.path(db).as_deref().expect("a manifest has a path");
    Arc::new(crate::parse_context::ManifestEditions::decode(
        path,
        file.text(db),
    ))
}

fn select_edition_source(
    db: &dyn IncrementalDb,
    source: &crate::parse_context::EditionSource,
) -> EditionSelection {
    let file = source.path().and_then(|path| {
        let input = SyntaxContexts::try_get(db)?;
        if input.unavailable(db).contains(path) {
            return None;
        }
        input.metadata(db).get(path).copied()
    });
    if let Some(file) = file
        && let crate::parse_context::EditionSource::Manifest { path, name, uuid } = source
    {
        return manifest_editions(db, file).select(path, name, *uuid);
    }
    source.select(file.map(|file| file.text(db).as_ref()))
}

fn library_package_for_context(
    db: &dyn IncrementalDb,
    name: &str,
    context: Option<&SourceContext>,
) -> Option<Arc<PackageIndex>> {
    if let Some(input) = LibrarySyntaxInput::try_get(db) {
        let data = input.data(db);
        if let Some(package) = data.packages.get(name) {
            let source = context
                .and_then(|context| context.source.path())
                .and_then(|owner| data.environments.get(owner))
                .and_then(|packages| packages.get(name))
                .unwrap_or(&package.source);
            let selected = select_edition_source(db, source);
            if !selected.certain {
                return None;
            }
            return data
                .variants
                .get(&(name.to_string(), selected.options))
                .cloned();
        }
    }
    LibraryIndex::try_get(db)?.packages(db).0.get(name).cloned()
}

/// Associations are inputs; their text is held in ordinary low-durability
/// SourceFiles. A tracked query never discovers or reads files.
#[salsa::input(singleton)]
pub struct SyntaxContexts {
    #[returns(ref)]
    pub unavailable: BTreeSet<PathBuf>,
    #[returns(ref)]
    pub direct: BTreeSet<PathBuf>,
    #[returns(ref)]
    pub files: BTreeMap<PathBuf, Vec<SourceContext>>,
    #[returns(ref)]
    pub metadata: BTreeMap<PathBuf, SourceFile>,
}

#[derive(Default, PartialEq, Eq)]
struct LivePackageMembership {
    roots: Vec<SourceContext>,
    files: BTreeMap<PathBuf, Vec<SourceContext>>,
}

#[salsa::input(singleton)]
struct LivePackageSyntax {
    #[returns(ref)]
    membership: Arc<LivePackageMembership>,
}

#[salsa::tracked(returns(ref))]
pub fn source_editions(
    db: &dyn IncrementalDb,
    file: SourceFile,
) -> Arc<Vec<(SourceContext, EditionSelection)>> {
    Arc::new(
        file.path(db)
            .as_deref()
            .map_or_else(Vec::new, |path| tracked_source_editions(db, path)),
    )
}

fn tracked_source_editions(
    db: &dyn IncrementalDb,
    path: &Path,
) -> Vec<(SourceContext, EditionSelection)> {
    let input = SyntaxContexts::try_get(db);
    let library = LibrarySyntaxInput::try_get(db).map(|input| input.data(db));
    let mut contexts = library
        .and_then(|data| data.files.get(path))
        .cloned()
        .unwrap_or_default();
    let mut retained = &[][..];
    if let Some(data) = library {
        retained = data
            .failed_manifest_files
            .get(path)
            .map_or(&[], Vec::as_slice);
        contexts.retain(|context| {
            let Some((name, package)) = data
                .packages
                .iter()
                .find(|(name, package)| package.entry(name) == context.entry)
            else {
                return true;
            };
            let selected = select_edition_source(db, &context.source);
            !selected.certain
                || data
                    .variants
                    .get(&(name.clone(), selected.options))
                    .is_none_or(|index| {
                        index
                            .members
                            .iter()
                            .any(|member| normalize_path(&package.root.join(member)) == *path)
                    })
        });
    }
    if let Some(input) = LivePackageSyntax::try_get(db) {
        let live = input.membership(db);
        contexts.retain(|context| {
            !live.roots.contains(context) || !select_edition_source(db, &context.source).certain
        });
        for context in live.files.get(path).into_iter().flatten() {
            if !contexts.contains(context) {
                contexts.push(context.clone());
            }
        }
    }
    for context in retained {
        if !contexts.contains(context) {
            contexts.push(context.clone());
        }
    }
    if let Some(input) = input
        && (!input.direct(db).contains(path) || contexts.is_empty())
    {
        for context in input.files(db).get(path).into_iter().flatten() {
            if !contexts.contains(context) {
                contexts.push(context.clone());
            }
        }
    }
    contexts.sort_by_key(|context| std::cmp::Reverse(context.owner_depth(path)));
    contexts
        .into_iter()
        .map(|context| {
            let mut selection = select_edition_source(db, &context.source);
            if selection.certain && retained.contains(&context) {
                // The live metadata may have recovered before the disk
                // reload. Require a successful resolve to confirm the pin.
                selection = context.source.select(None);
            }
            (context, selection)
        })
        .collect()
}

#[salsa::tracked(returns(clone))]
pub fn primary_parse_options(db: &dyn IncrementalDb, file: SourceFile) -> ParseOptions {
    source_editions(db, file)
        .first()
        .map(|(_, selected)| selected.options)
        .unwrap_or_default()
}

/// The cached parse of a file. The `GreenNode` is not `Eq`, so
/// [`parsed_document`] is `no_eq`: salsa never compares parse outputs and
/// relies purely on input (text) change detection to invalidate. Sound because
/// the tree is a pure function of the text.
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    pub green: rowan::GreenNode,
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// The previous parse of a file, kept *outside* salsa as the incremental
/// reparse base. A successful reparse is byte-identical to a full parse
/// (Tenet 4, enforced in [`crate::parser::reparse`]), so this cache only ever
/// changes how fast [`parsed_document`] computes, never what it returns —
/// which is what makes reading and writing it from inside the otherwise-pure
/// tracked query sound.
#[derive(Debug, Clone)]
pub struct PrevParse {
    pub options: ParseOptions,
    pub text: Arc<str>,
    pub green: rowan::GreenNode,
    pub diagnostics: Vec<ParseDiagnostic>,
}

#[salsa::db]
pub trait IncrementalDb: salsa::Database {
    fn reparse_prev_with_options(
        &self,
        file: SourceFile,
        options: ParseOptions,
    ) -> Option<Arc<PrevParse>> {
        self.reparse_prev(file)
            .filter(|prev| prev.options == options)
    }
    fn reparse_pending_edits_with_options(
        &self,
        file: SourceFile,
        _options: ParseOptions,
    ) -> Vec<Edit> {
        self.reparse_pending_edits(file)
    }

    /// The cached previous parse for `file`, if any (the incremental-reparse
    /// base). The no-op default keeps bare test databases working without a
    /// cache — they simply always full-parse.
    fn reparse_prev(&self, _file: SourceFile) -> Option<Arc<PrevParse>> {
        None
    }

    /// Stage the edits that transform `file`'s previous text into its current
    /// one. `Some` appends to the pending chain (a `didChange` batch that a
    /// coalesced analysis never got to consume must still be replayed); `None`
    /// marks the transform unknown and clears it.
    fn reparse_stage_edits(&self, _file: SourceFile, _edits: Option<Vec<Edit>>) {}

    /// The edit chain staged for `file` since its reparse base was written.
    /// Peeks — the chain is drained by [`Self::reparse_store`], so a query that
    /// is cancelled before it stores leaves the chain intact for the next one.
    fn reparse_pending_edits(&self, _file: SourceFile) -> Vec<Edit> {
        Vec::new()
    }

    /// Store `prev` as the reparse base for `file` and drop the first
    /// `_consumed` staged edits, atomically. `tier` records which strategy
    /// produced this parse (`None` for a full parse) and is logged.
    fn reparse_store(
        &self,
        _file: SourceFile,
        _prev: PrevParse,
        _tier: Option<ReparseTier>,
        _consumed: usize,
    ) {
    }

    /// Drop the base and the pending chain for `file`, after its text changed
    /// by some route that carries no edits (a disk revert, a closed buffer).
    fn reparse_evict(&self, _file: SourceFile) {}
}

/// Parse `file`'s text into a cached green tree plus diagnostics.
///
/// Tries an incremental reparse off the previous parse of this file:
///
/// 1. the precise chain the language server staged from the client's
///    `didChange` batches, offered to [`reparse_edits`]. A single verified edit
///    goes straight to [`reparse`], avoiding whole-text diff recovery;
/// 2. failing that, a single edit recovered by [`diff_edit`] from the two whole
///    texts, offered to [`reparse`]. This is the only route for a text that
///    changed by some path carrying no edits at all (a disk revert, the CLI);
/// 3. failing that, a full parse.
///
/// The chain goes first because it is the only step that can answer scattered
/// edits at all. The collapsed edit spans everything between the first change
/// and the last, and the top-level tier would have to fragment-parse that whole
/// region plus both its boundary guards — more than the full parse it is
/// avoiding — so the tier declines a region that wide up front
/// (`parser::reparse::region_is_too_wide`, `crates/fatou-parser/benches/reparse.rs`). Replaying the
/// chain splices each edit on its own instead, two orders of magnitude cheaper
/// than the full parse; when it misses, step 2 is still there. The reverse
/// order is only better for edits that are *adjacent*, where it saves a few
/// tens of microseconds.
///
/// Every path yields a result identical to `parse_with_options(text, options)` (Tenet 4), so the
/// side-channel never affects query semantics — salsa only sees text changes.
#[salsa::tracked(returns(ref), no_eq)]
pub fn parsed_document_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> ParsedDocument {
    let text = file.text(db);
    let staged = db.reparse_pending_edits_with_options(file, options);
    let prev = db.reparse_prev_with_options(file, options);

    // Text unchanged: the base already holds what a full parse would return, so
    // hand it back rather than recomputing it. Salsa usually spares us the call
    // entirely, so this is the cold path — a re-execution after an eviction, or
    // a write that set the text back to what the base holds. Nothing is stored:
    // the base has not moved, and the staged chain stays anchored to it (its
    // net effect is a no-op, so a later edit appended to it still describes the
    // transform from the base). The `ptr_eq` is the free half of the check —
    // the base normally holds the very allocation salsa tracks — and the
    // compare behind it is what makes the guard correct for a base rebuilt
    // from equal-but-distinct text.
    if let Some(prev) = prev
        .as_ref()
        .filter(|prev| Arc::ptr_eq(&prev.text, text) || prev.text == *text)
    {
        return ParsedDocument {
            green: prev.green.clone(),
            diagnostics: prev.diagnostics.clone(),
        };
    }

    let reparsed = prev.and_then(|prev| {
        let base = ReparseBase {
            text: &prev.text,
            green: &prev.green,
            diagnostics: &prev.diagnostics,
            options: prev.options,
        };
        reparse_edits_with_options(base, &staged, text, options).or_else(|| {
            let edit = diff_edit(&prev.text, text);
            reparse_with_options(base, &edit, text, options)
        })
    });

    let tier = reparsed.as_ref().map(|r| r.tier);
    let (green, diagnostics) = match reparsed {
        Some(r) => (r.green, r.diagnostics),
        None => {
            let parsed = parse_with_options(text, options);
            (parsed.cst.green().to_owned(), parsed.diagnostics)
        }
    };

    // Store last, after all fallible work, so a panic or salsa cancellation
    // mid-query never leaves a base whose text and tree disagree. The staged
    // chain is drained here unconditionally — whether or not step 2 used it —
    // because the base now sits at the current text, so every peeked edit is
    // behind it either way.
    db.reparse_store(
        file,
        PrevParse {
            options,
            text: text.clone(),
            green: green.clone(),
            diagnostics: diagnostics.clone(),
        },
        tier,
        staged.len(),
    );

    ParsedDocument { green, diagnostics }
}

pub fn parsed_document(db: &dyn IncrementalDb, file: SourceFile) -> &ParsedDocument {
    parsed_document_with_options(db, file, primary_parse_options(db, file))
}

/// The parse diagnostics for `file` (empty when it parses cleanly).
pub fn parse_diagnostics(db: &dyn IncrementalDb, file: SourceFile) -> &[ParseDiagnostic] {
    parse_diagnostics_with_options(db, file, primary_parse_options(db, file))
}

/// Materialize the cached parse for `file` as a fresh `SyntaxNode` cursor.
pub fn parsed_tree_root(db: &dyn IncrementalDb, file: SourceFile) -> SyntaxNode {
    parsed_tree_root_with_options(db, file, primary_parse_options(db, file))
}

/// The per-file semantic model (scope tree, bindings, reads, and documentation),
/// built from the cached parse. Unlike [`parsed_document`] this query keeps
/// structural `Eq`:
/// when an edit leaves the model unchanged (the model carries text ranges, so
/// this means same-shape edits), salsa backdates it and dependents are not
/// re-run. Range-free projections such as [`file_exports`] and
/// [`file_free_reads`] provide the invalidation barrier for position-shifting
/// edits.
#[salsa::tracked(returns(ref))]
pub fn semantic_model_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> SemanticModel {
    SemanticModel::build(&parsed_tree_root_with_options(db, file, options))
}

pub fn semantic_model(db: &dyn IncrementalDb, file: SourceFile) -> &SemanticModel {
    semantic_model_with_options(db, file, primary_parse_options(db, file))
}

/// Static documentation in source order, with source positions and literal
/// spelling removed. Equal decoded payloads backdate across Julia body edits,
/// attachment shifts, and changes to escaping or indentation.
#[salsa::tracked(returns(ref))]
pub fn static_docstring_payloads_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> Arc<[String]> {
    crate::documentation::static_documentation(semantic_model_with_options(db, file, options))
        .map(|(_, decoded)| decoded.as_str().to_string())
        .collect()
}

pub fn static_docstring_payloads(db: &dyn IncrementalDb, file: SourceFile) -> &Arc<[String]> {
    static_docstring_payloads_with_options(db, file, primary_parse_options(db, file))
}

/// The demand-only Markdown navigation index. Depend only on decoded payloads
/// so edits to Julia code do not repeat the file's documentation parses.
#[salsa::tracked(returns(ref))]
pub fn documentation_index_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> Arc<crate::documentation::DocumentationIndex> {
    Arc::new(crate::documentation::DocumentationIndex::build(
        static_docstring_payloads_with_options(db, file, options)
            .iter()
            .map(String::as_str),
    ))
}

pub fn documentation_index(
    db: &dyn IncrementalDb,
    file: SourceFile,
) -> &Arc<crate::documentation::DocumentationIndex> {
    documentation_index_with_options(db, file, primary_parse_options(db, file))
}

/// The per-file control-flow graph: one region per function-like body and
/// `module` body, plus the file top level (see [`FileControlFlow`]). Built on
/// the cached parse. Like [`semantic_model`] it keeps structural `Eq`, so an
/// edit that leaves the graph unchanged is backdated and CFG-dependent work is
/// not re-run.
#[salsa::tracked(returns(ref))]
pub fn control_flow_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> FileControlFlow {
    FileControlFlow::build(&parsed_tree_root_with_options(db, file, options))
}

pub fn control_flow(db: &dyn IncrementalDb, file: SourceFile) -> &FileControlFlow {
    control_flow_with_options(db, file, primary_parse_options(db, file))
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
pub fn file_exports_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> BTreeSet<String> {
    project::file_exports(semantic_model_with_options(db, file, options))
}

pub fn file_exports(db: &dyn IncrementalDb, file: SourceFile) -> &BTreeSet<String> {
    file_exports_with_options(db, file, primary_parse_options(db, file))
}

/// The names the file reads but binds nowhere in it
/// ([`crate::project::file_free_reads`]). The mirror firewall to
/// [`file_exports`].
#[salsa::tracked(returns(ref))]
pub fn file_free_reads_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> BTreeSet<String> {
    project::file_free_reads(semantic_model_with_options(db, file, options))
}

pub fn file_free_reads(db: &dyn IncrementalDb, file: SourceFile) -> &BTreeSet<String> {
    file_free_reads_with_options(db, file, primary_parse_options(db, file))
}

/// The module-qualified names the file references, each a full dotted path
/// ([`crate::project::file_qualified_reads`]).
#[salsa::tracked(returns(ref))]
pub fn file_qualified_reads_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> BTreeSet<String> {
    project::file_qualified_reads(semantic_model_with_options(db, file, options))
}

pub fn file_qualified_reads(db: &dyn IncrementalDb, file: SourceFile) -> &BTreeSet<String> {
    file_qualified_reads_with_options(db, file, primary_parse_options(db, file))
}

/// The file's static `include("literal")` edges, in source order, resolved
/// against the file's own directory ([`crate::project::include_edges`]). The
/// path is an input field set once, so this re-runs only on a text edit and
/// backdates when the edges are unchanged.
#[salsa::tracked(returns(ref))]
pub fn include_edges_with_options(
    db: &dyn IncrementalDb,
    file: SourceFile,
    options: ParseOptions,
) -> Vec<IncludeEdge> {
    let root = parsed_tree_root_with_options(db, file, options);
    let base_dir = file.path(db).as_deref().and_then(Path::parent);
    project::include_edges(&root, base_dir)
}

pub fn include_edges(db: &dyn IncrementalDb, file: SourceFile) -> &Vec<IncludeEdge> {
    include_edges_with_options(db, file, primary_parse_options(db, file))
}

/// The names in `file`'s `[deps]`, when it parses as a project file — the
/// derived replacement for a dependency map pushed in on each re-harvest. Read
/// through [`IncrementalDatabase::declared_deps`], which maps a package name to
/// its project file via [`ProjectFiles`].
///
/// `None` for a file that does not parse. A half-typed `[deps]` table must make
/// `unresolved-import` go *quiet* rather than report a name as undeclared: the
/// same failure direction the lint's cold path already takes (`crate::lsp::lint`).
///
/// This is the firewall in the project file's direction. An edit anywhere else
/// in the file — `[compat]`, `version`, a comment — returns an equal set and
/// backdates, so no consumer re-runs; `tests/salsa_incremental.rs` pins both
/// that and the converse, that this never reaches a `.jl` parse.
#[salsa::tracked(returns(clone))]
pub fn project_declared_deps(
    db: &dyn IncrementalDb,
    file: SourceFile,
) -> Option<Arc<DeclaredDeps>> {
    // The path is carried only for the error this discards; an in-memory
    // project file (no path) parses just the same.
    let path = file
        .path(db)
        .as_deref()
        .unwrap_or_else(|| Path::new("Project.toml"));
    let project = crate::environment::parse_project_text(path, file.text(db)).ok()?;
    Some(Arc::new(project.declared_dep_names()))
}

/// One unresolvable static `include("literal")` site: the decoded `path` written
/// in `from` whose target is not a package member (the file does not exist or
/// was not reached). Range-free — the include call's span is recovered from a
/// fresh parse of `from` when the diagnostic is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedInclude {
    pub from: PathBuf,
    pub path: String,
}

/// One `include` back-edge that closes a cycle: `from` statically includes `to`,
/// which transitively includes `from` again. The diagnostic attaches to the
/// `include("path")` call in `from`. Only true cycles are recorded; a file
/// included twice along disjoint paths (a diamond) is not a cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleEdge {
    pub from: PathBuf,
    pub path: String,
    pub to: PathBuf,
}

/// The package's transitive `include` graph, re-derived purely from the seeded
/// [`WorkspaceFiles`] set and each member's [`include_edges`] firewall (never
/// from the filesystem, so it stays incremental: editing one member re-runs only
/// that file's `include_edges`, then this cheap re-derivation). Keyed on
/// normalized absolute paths — a tracked query holds `&dyn IncrementalDb` and so
/// cannot reach the concrete db's path->`SourceFile` map, and paths keep the
/// value `Eq` for backdating.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectGraph {
    /// The include closure, entry first, in depth-first source order (mirroring
    /// the harvester's recursive walk).
    pub nodes: Vec<PathBuf>,
    /// Each file to the files it statically includes, in source order.
    pub forward: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Each file to the files that include it.
    pub reverse: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Each member's host module path (the nested `module` its `include`
    /// lexically landed in), empty for the root module. The resolution
    /// authority for nested-`module` file membership, read through the
    /// [`host_module_of`] firewall; the harvester's parallel record
    /// ([`PackageIndex::member_modules`](crate::index::model::PackageIndex)) is
    /// held in lockstep by a parity test.
    pub host_modules: BTreeMap<PathBuf, Vec<String>>,
    /// `include` back-edges that close a cycle.
    pub cycles: Vec<CycleEdge>,
    /// Static includes whose target is not a package member.
    pub unresolved: Vec<UnresolvedInclude>,
}

/// DFS coloring: `Gray` while a file is on the current stack (re-entering it is a
/// true include cycle); `Black` once fully walked (re-entering is a diamond).
#[derive(Clone, Copy, PartialEq, Eq)]
enum IncludeColor {
    Gray,
    Black,
}

/// The transitive include graph of every workspace package, merged (see
/// [`ProjectGraph`]; paths under distinct package roots cannot collide, so one
/// shared graph is unambiguous). Demand-only: pulled by the diagnostics path
/// and, through [`host_module_of`], by the workspace resolution tier — never by
/// an eager query. Empty when no folder is a package project or the member
/// files have not been seeded.
#[salsa::tracked(returns(ref))]
pub fn project_graph(db: &dyn IncrementalDb) -> ProjectGraph {
    let mut graph = ProjectGraph::default();
    let (Some(wf), Some(lib)) = (WorkspaceFiles::try_get(db), LibraryIndex::try_get(db)) else {
        return graph;
    };
    let mut names = lib.workspaces(db).clone();
    names.sort();

    // Normalized path -> seeded input, so an include target resolves to a member
    // without reaching the concrete db's path map (unavailable from `&dyn`).
    let mut by_path: HashMap<PathBuf, SourceFile> = HashMap::new();
    for &file in wf.files(db) {
        if let Some(path) = file.path(db) {
            by_path.insert(normalize_path(path), file);
        }
    }

    // The walks share `color` (and the graph): first visit wins across
    // packages too, matching today's diamond semantics should one package's
    // closure reach into another's files.
    let mut color: HashMap<PathBuf, IncludeColor> = HashMap::new();
    for name in names {
        let Some(root) = lib.roots(db).0.get(&name).cloned() else {
            continue;
        };
        let entry = normalize_path(&root.join("src").join(format!("{name}.jl")));
        let Some(&entry_file) = by_path.get(&entry) else {
            continue; // Not seeded (yet); skip this package, keep the rest.
        };
        if color.contains_key(&entry) {
            continue; // Already reached from an earlier package's closure.
        }
        walk_include_graph(
            db,
            &by_path,
            entry,
            entry_file,
            primary_parse_options(db, entry_file),
            Vec::new(),
            &name,
            &mut color,
            &mut graph,
        );
    }
    graph
}

#[salsa::tracked(returns(ref))]
fn project_graph_with_options(
    db: &dyn IncrementalDb,
    entry: SourceFile,
    options: ParseOptions,
) -> ProjectGraph {
    let mut by_path: HashMap<_, _> = WorkspaceFiles::try_get(db)
        .into_iter()
        .flat_map(|input| input.files(db))
        .filter_map(|&file| Some((normalize_path(file.path(db).as_deref()?), file)))
        .collect();
    if let Some(input) = ScriptFiles::try_get(db) {
        for (path, file) in input.files(db) {
            if let Some(file) = file {
                by_path.entry(path.clone()).or_insert(*file);
            }
        }
    }
    let mut graph = ProjectGraph::default();
    let Some(path) = entry.path(db) else {
        return graph;
    };
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    walk_include_graph(
        db,
        &by_path,
        path.clone(),
        entry,
        options,
        Vec::new(),
        name,
        &mut HashMap::new(),
        &mut graph,
    );
    graph
}

/// Depth-first, source-order walk of the include graph from `path` (host module
/// `host`), first visit winning so `host_modules`/`nodes` match the harvester's
/// recursive walk. `pkg` is the package name, stripped once as a leading segment
/// at the root level to reproduce the harvester's root-module absorption (the
/// entry's top-level `module <Pkg>` is the synthesized root, not a nesting).
#[allow(clippy::too_many_arguments)]
fn walk_include_graph(
    db: &dyn IncrementalDb,
    by_path: &HashMap<PathBuf, SourceFile>,
    path: PathBuf,
    file: SourceFile,
    options: ParseOptions,
    host: Vec<String>,
    pkg: &str,
    color: &mut HashMap<PathBuf, IncludeColor>,
    graph: &mut ProjectGraph,
) {
    color.insert(path.clone(), IncludeColor::Gray);
    graph.nodes.push(path.clone());
    graph.host_modules.insert(path.clone(), host.clone());

    for edge in include_edges_with_options(db, file, options) {
        let target = match &edge.target {
            Some(target) => normalize_path(target),
            None => {
                graph.unresolved.push(UnresolvedInclude {
                    from: path.clone(),
                    path: edge.path.clone(),
                });
                continue;
            }
        };
        let Some(&child_file) = by_path.get(&target) else {
            graph.unresolved.push(UnresolvedInclude {
                from: path.clone(),
                path: edge.path.clone(),
            });
            continue;
        };
        graph
            .forward
            .entry(path.clone())
            .or_default()
            .push(target.clone());
        graph
            .reverse
            .entry(target.clone())
            .or_default()
            .push(path.clone());

        // host(child) = host(parent) ++ host_suffix, with the absorbed root
        // module dropped: it only surfaces as a leading `pkg` segment at the
        // root level (parent host empty).
        let mut child_host = host.clone();
        child_host.extend(edge.host_suffix.iter().cloned());
        if host.is_empty() && child_host.first().map(String::as_str) == Some(pkg) {
            child_host.remove(0);
        }

        match color.get(&target) {
            Some(IncludeColor::Gray) => graph.cycles.push(CycleEdge {
                from: path.clone(),
                path: edge.path.clone(),
                to: target,
            }),
            Some(IncludeColor::Black) => {}
            None => walk_include_graph(
                db, by_path, target, child_file, options, child_host, pkg, color, graph,
            ),
        }
    }
    color.insert(path, IncludeColor::Black);
}

/// The host module path of `file` within the workspace package, from the
/// project graph — the resolution authority for nested-`module` file
/// membership. Empty for the root module, and as the fallback for a file the
/// graph does not reach (an orphan under `src/`, or a pathless in-memory
/// document), preserving the pre-nested-module behavior.
///
/// A per-file firewall over [`project_graph`]: the graph is one shared `Eq`
/// value, so an include-edge edit anywhere re-derives it — but when this file's
/// own host is unchanged the equal result backdates, and file-keyed dependents
/// (e.g. [`file_workspace_occurrences`]) are not re-run.
#[salsa::tracked(returns(ref))]
pub fn host_module_of(db: &dyn IncrementalDb, file: SourceFile) -> Vec<String> {
    let Some(path) = file.path(db) else {
        return Vec::new();
    };
    project_graph(db)
        .host_modules
        .get(&normalize_path(path))
        .cloned()
        .unwrap_or_default()
}

/// The workspace package whose source root contains `path` — the gate of
/// `workspace_member`, shared by the path- and file-keyed variants. With
/// several workspace packages the *longest* matching `src/` prefix wins, so a
/// package folder nested inside another folder claims its own files.
fn workspace_package_for(db: &dyn IncrementalDb, path: &Path) -> Option<Arc<PackageIndex>> {
    let index = LibraryIndex::try_get(db)?;
    let path = normalize_path(path);
    let mut best: Option<(usize, &String)> = None;
    for name in index.workspaces(db) {
        let Some(root) = index.roots(db).0.get(name) else {
            continue;
        };
        let src = normalize_path(&root.join("src"));
        if !path.starts_with(&src) {
            continue;
        }
        let depth = src.components().count();
        if best.is_none_or(|(d, _)| depth > d) {
            best = Some((depth, name));
        }
    }
    let (_, name) = best?;
    library_package_for_context(db, name, None)
}

/// [`PackageSource::workspace_member`] keyed by [`SourceFile`], for tracked
/// queries: the host comes through the [`host_module_of`] firewall, so a graph
/// change that leaves this file's host unchanged does not re-run the caller.
fn workspace_member_of(
    db: &dyn IncrementalDb,
    file: SourceFile,
) -> Option<(Arc<PackageIndex>, ModulePath)> {
    let path = file.path(db).as_deref()?;
    let pkg = workspace_package_for(db, path)?;
    let host = host_module_of(db, file).iter().map(SmolStr::new).collect();
    Some((pkg, host))
}

/// The reverse-occurrence projection for one file: every occurrence of a
/// workspace top-level symbol, keyed by `(namespace, name)`, from this file's
/// own module-global bindings and its free reads resolving to the workspace
/// tier. See [`Resolver::workspace_occurrences`].
///
/// Reads the [`LibraryIndex`] input (through [`DbPackages`]) and the file's
/// host module (through the [`host_module_of`] firewall), so a re-harvest or a
/// change to this file's own host invalidates it; otherwise re-runs only when
/// the file's [`semantic_model`] changes. **Demand-only:** the aggregate
/// [`workspace_reference_index`] and the
/// references/rename read jobs pull it lazily. It must never become a dependency
/// of an eager query (e.g. [`parse_diagnostics`]), or every keystroke in any
/// member file would recompute the whole reverse index.
#[salsa::tracked(returns(ref))]
pub fn file_workspace_occurrences(
    db: &dyn IncrementalDb,
    file: SourceFile,
) -> WorkspaceOccurrences {
    let model = semantic_model(db, file);
    let packages = DbPackages(db);
    let map = Resolver::new(model, &packages)
        .with_workspace(workspace_member_of(db, file))
        .workspace_occurrences();
    WorkspaceOccurrences(map)
}

/// The per-name occurrence buckets of one file (the [`file_workspace_occurrences`]
/// projection). Wrapped so the map stays an opaque whole-value leaf, keeping
/// the [`OccurrenceRec`]/[`SmolStr`] leaves salsa-free.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceOccurrences(pub BTreeMap<OccurrenceKey, Vec<OccurrenceRec>>);

/// A [`PackageSource`] over the raw db, so a tracked query can build a
/// [`Resolver`] whose [`LibraryIndex`] reads are recorded as salsa dependencies.
struct DbPackages<'a>(&'a dyn IncrementalDb);

impl PackageSource for DbPackages<'_> {
    fn package(&self, name: &str) -> Option<Arc<PackageIndex>> {
        library_package_for_context(self.0, name, None)
    }

    fn package_root(&self, name: &str) -> Option<PathBuf> {
        LibraryIndex::try_get(self.0)?
            .roots(self.0)
            .0
            .get(name)
            .cloned()
    }
}

/// The workspace package's reverse-occurrence index: every occurrence of each
/// top-level symbol across *all* member files, keyed by `(namespace, name)`,
/// each tagged with the [`SourceFile`] it lives in. Cross-file references and
/// rename read this directly. Unions [`file_workspace_occurrences`] over the
/// [`WorkspaceFiles`] set, so editing one member re-runs only that file's
/// projection before the (cheap) re-union.
///
/// **Demand-only** — pulled by the references/rename read jobs, never by an
/// eager query. Empty when there is no workspace package or its files have not
/// been seeded.
#[salsa::tracked(returns(ref))]
pub fn workspace_reference_index(db: &dyn IncrementalDb) -> WorkspaceReferenceIndex {
    let mut out: BTreeMap<OccurrenceKey, Vec<(SourceFile, OccurrenceRec)>> = BTreeMap::new();
    let Some(wf) = WorkspaceFiles::try_get(db) else {
        return WorkspaceReferenceIndex::default();
    };
    for &file in wf.files(db) {
        let projection = file_workspace_occurrences(db, file);
        for (key, recs) in &projection.0 {
            let bucket = out.entry(key.clone()).or_default();
            bucket.extend(recs.iter().map(|rec| (file, *rec)));
        }
    }
    WorkspaceReferenceIndex(out)
}

#[salsa::tracked(returns(ref))]
fn context_workspace_occurrences(
    db: &dyn IncrementalDb,
    file: SourceFile,
    entry: SourceFile,
    options: ParseOptions,
    context: SourceContext,
) -> WorkspaceOccurrences {
    let graph = project_graph_with_options(db, entry, options);
    let model = semantic_model_with_options(db, file, options);
    struct Packages<'a> {
        db: &'a dyn IncrementalDb,
        context: Option<&'a SourceContext>,
    }
    impl PackageSource for Packages<'_> {
        fn package(&self, name: &str) -> Option<Arc<PackageIndex>> {
            library_package_for_context(self.db, name, self.context)
        }
    }
    let packages = Packages {
        db,
        context: Some(&context),
    };
    let workspace = entry
        .path(db)
        .as_deref()
        .and_then(|path| workspace_package_for(db, path))
        .and_then(|package| {
            Some((
                packages.package(&package.name)?,
                graph
                    .host_modules
                    .get(file.path(db).as_deref()?)
                    .into_iter()
                    .flatten()
                    .map(SmolStr::new)
                    .collect(),
            ))
        });
    WorkspaceOccurrences(
        Resolver::new(model, &packages)
            .with_workspace(workspace)
            .workspace_occurrences(),
    )
}

#[salsa::tracked(returns(ref))]
fn context_workspace_reference_index(
    db: &dyn IncrementalDb,
    entry: SourceFile,
    options: ParseOptions,
    context: SourceContext,
) -> WorkspaceReferenceIndex {
    let mut out: BTreeMap<OccurrenceKey, Vec<(SourceFile, OccurrenceRec)>> = BTreeMap::new();
    let graph = project_graph_with_options(db, entry, options);
    if let Some(input) = WorkspaceFiles::try_get(db) {
        for &file in input.files(db) {
            if file
                .path(db)
                .as_ref()
                .is_none_or(|path| !graph.nodes.contains(path))
            {
                continue;
            }
            for (key, records) in
                &context_workspace_occurrences(db, file, entry, options, context.clone()).0
            {
                out.entry(key.clone())
                    .or_default()
                    .extend(records.iter().map(|record| (file, *record)));
            }
        }
    }
    WorkspaceReferenceIndex(out)
}

/// The unioned reverse-occurrence index (the [`workspace_reference_index`]
/// query). Wrapped so it stays an opaque whole-value leaf. No `Debug` derive:
/// [`SourceFile`] is a salsa input without one.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WorkspaceReferenceIndex(pub BTreeMap<OccurrenceKey, Vec<(SourceFile, OccurrenceRec)>>);

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
    /// Per-file incremental-reparse state, outside salsa: a hit changes how fast
    /// [`parsed_document`] computes, never what it returns (see [`PrevParse`]).
    /// Shared across clones so snapshots warm the same cache.
    ///
    /// The base and the pending chain live under *one* lock because
    /// [`IncrementalDb::reparse_store`] has to advance both atomically — it runs
    /// on a read-pool thread while [`IncrementalDb::reparse_stage_edits`] runs on
    /// the analysis thread.
    reparse_cache: Arc<Mutex<ReparseCache>>,
}

/// The incremental-reparse side-channel for one file: the previous parse to
/// splice against, and the chain of edits staged since that parse.
#[derive(Debug, Default)]
struct FileReparseState {
    editions: BTreeMap<ParseOptions, EditionReparseState>,
    /// When this entry was last touched, for eviction. See [`ReparseCache`].
    used: u64,
    /// Whether this file has shown it benefits from a base: an editor staged a
    /// real edit chain against it, or a splice actually landed. See
    /// [`ReparseCache`] for what the flag buys.
    hot: bool,
}

#[derive(Debug, Default)]
struct EditionReparseState {
    prev: Option<Arc<PrevParse>>,
    pending: Vec<Edit>,
}

/// How many files keep a reparse base. Every file the include graph reaches
/// runs [`parsed_document`], but only the handful the editor is actually
/// touching ever gets a *hit*: the rest would hold a second copy of their text
/// (salsa already holds one) plus a green tree, for nothing.
const MAX_REPARSE_BASES: usize = 64;

/// The reparse side-channel for all files, bounded by [`MAX_REPARSE_BASES`].
///
/// Eviction is least-recently-used, approximated by a monotone counter stamped
/// on each entry as it is read or written. Dropping an entry only costs its
/// file a full parse, so the policy needs no more precision than that.
///
/// Recency alone is not enough, though, because the two populations are not
/// comparable. A [`project_graph`] or [`workspace_reference_index`] sweep
/// parses every member of the workspace, and each one stores a base it can
/// never hit — a sweep past the budget would fill every slot *and* stamp
/// itself more recent than every open buffer, so one find-references would
/// cost every buffer its base. So entries are two classes: an entry is *hot*
/// once it has shown it benefits from a base, and eviction drops cold entries
/// first, reaching hot ones only if the hot set alone is over budget.
///
/// No `Debug`: salsa only formats a `SourceFile` with its database in hand, and
/// [`IncrementalDatabase`] elides this cache from its own `Debug` anyway.
#[derive(Default)]
struct ReparseCache {
    files: HashMap<SourceFile, FileReparseState>,
    clock: u64,
}

impl ReparseCache {
    /// The entry for `file`, created if absent, stamped as just used.
    fn touch(&mut self, file: SourceFile) -> &mut FileReparseState {
        self.clock += 1;
        let clock = self.clock;
        let state = self.files.entry(file).or_default();
        state.used = clock;
        state
    }

    /// Drop the least recently used entries until the cache is within budget,
    /// cold ones first. Stamps are unique — `touch` bumps a monotone clock — so
    /// "everything at or below the `n`th smallest" drops exactly `n` entries.
    fn evict_over_budget(&mut self) {
        if self.files.len() <= MAX_REPARSE_BASES {
            return;
        }
        let hot = self.files.values().filter(|state| state.hot).count();

        if hot <= MAX_REPARSE_BASES {
            // The ordinary case: enough cold entries to absorb the overflow, so
            // no file the editor is in loses its base.
            let over = self.files.len() - MAX_REPARSE_BASES;
            let mut cold: Vec<u64> = self
                .files
                .values()
                .filter(|state| !state.hot)
                .map(|state| state.used)
                .collect();
            cold.select_nth_unstable(over - 1);
            let threshold = cold[over - 1];
            self.files
                .retain(|_, state| state.hot || state.used > threshold);
        } else {
            // More hot files than the budget: plain LRU across them, and every
            // cold entry goes.
            let cut = hot - MAX_REPARSE_BASES;
            let mut stamps: Vec<u64> = self
                .files
                .values()
                .filter(|state| state.hot)
                .map(|state| state.used)
                .collect();
            stamps.select_nth_unstable(cut - 1);
            let threshold = stamps[cut - 1];
            self.files
                .retain(|_, state| state.hot && state.used > threshold);
        }
    }
}

impl Default for IncrementalDatabase {
    fn default() -> Self {
        let db = Self {
            storage: salsa::Storage::new(None),
            source_map: Arc::new(Mutex::new(FileSourceMap::default())),
            reparse_cache: Arc::new(Mutex::new(ReparseCache::default())),
        };
        SyntaxContexts::new(
            &db,
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        LibrarySyntaxInput::new(&db, Arc::new(crate::index::LibrarySyntax::default()));
        LivePackageSyntax::new(&db, Arc::default());
        db
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
            reparse_cache: Arc::clone(&self.reparse_cache),
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
impl IncrementalDb for IncrementalDatabase {
    fn reparse_prev(&self, file: SourceFile) -> Option<Arc<PrevParse>> {
        self.reparse_prev_with_options(file, primary_parse_options(self, file))
    }

    fn reparse_prev_with_options(
        &self,
        file: SourceFile,
        options: ParseOptions,
    ) -> Option<Arc<PrevParse>> {
        let mut cache = self.reparse_state();
        let prev = cache.files.get(&file)?.editions.get(&options)?.prev.clone();
        cache.touch(file);
        prev
    }

    fn reparse_stage_edits(&self, file: SourceFile, edits: Option<Vec<Edit>>) {
        let options = primary_parse_options(self, file);
        let mut cache = self.reparse_state();
        let state = cache.touch(file);
        state.editions.entry(options).or_default();
        if edits.is_some() {
            state.hot = true;
        }
        for edition in state.editions.values_mut() {
            match &edits {
                Some(edits) => edition.pending.extend(edits.iter().cloned()),
                None => edition.pending.clear(),
            }
            let bytes: usize = edition.pending.iter().map(|edit| edit.insert.len()).sum();
            if edition.pending.len() > MAX_CHAIN_EDITS || bytes > MAX_CHAIN_INSERT_BYTES {
                edition.pending.clear();
            }
        }
        cache.evict_over_budget();
    }

    fn reparse_pending_edits(&self, file: SourceFile) -> Vec<Edit> {
        self.reparse_pending_edits_with_options(file, primary_parse_options(self, file))
    }

    fn reparse_pending_edits_with_options(
        &self,
        file: SourceFile,
        options: ParseOptions,
    ) -> Vec<Edit> {
        self.reparse_state()
            .files
            .get(&file)
            .and_then(|state| state.editions.get(&options))
            .map(|state| state.pending.clone())
            .unwrap_or_default()
    }

    fn reparse_store(
        &self,
        file: SourceFile,
        prev: PrevParse,
        tier: Option<ReparseTier>,
        consumed: usize,
    ) {
        match tier {
            Some(tier) => log::trace!(
                "reparse: {:?} tier spliced {} bytes ({consumed} staged edits)",
                tier,
                prev.text.len()
            ),
            None => log::trace!("reparse: full parse of {} bytes", prev.text.len()),
        }
        let mut cache = self.reparse_state();
        let state = cache.touch(file);
        // A splice that landed is proof this file's base earns its slot, which
        // is the signal a client on full-buffer sync gives instead of a chain.
        state.hot |= tier.is_some();
        let edition = state.editions.entry(prev.options).or_default();
        edition.prev = Some(Arc::new(prev));
        // Drain the *prefix* the caller saw, not the whole chain: a stage can
        // land between the peek and this store (an `upsert_file` that finds the
        // text unchanged takes no `&mut db`, so it neither blocks nor cancels
        // the read in flight), and that edit still has to be replayed. Draining
        // unconditionally — even when the chain went unused because it was over
        // budget or stale — is what keeps this self-healing: the base has
        // advanced to the current text either way, so a chain kept back would
        // be stale forever after.
        edition.pending.drain(..consumed.min(edition.pending.len()));
        cache.evict_over_budget();
    }

    fn reparse_evict(&self, file: SourceFile) {
        self.reparse_state().files.remove(&file);
    }
}

/// Budget for a staged edit chain, enforced when edits are staged. The byte
/// bound matters independently of the count: sixteen pasted blocks are sixteen
/// edits but can be megabytes, and the chain is replayed against a full copy of
/// the text.
const MAX_CHAIN_EDITS: usize = 16;
const MAX_CHAIN_INSERT_BYTES: usize = 64 * 1024;

impl IncrementalDatabase {
    pub(crate) fn retain_failed_manifest_memberships(
        &self,
        syntax: &mut crate::index::LibrarySyntax,
        failed: &BTreeSet<PathBuf>,
    ) {
        syntax.retain_failed_manifest_memberships(LibrarySyntaxInput::get(self).data(self), failed);
        syntax.retain_failed_manifest_files(
            &LivePackageSyntax::get(self).membership(self).files,
            failed,
        );
    }

    /// Loading roots for a potentially changed include, from the last
    /// harvest and any closures already expanded using editor buffers.
    pub(crate) fn package_contexts(&self, path: &Path) -> impl Iterator<Item = &SourceContext> {
        let data = LibrarySyntaxInput::get(self).data(self);
        let direct = SyntaxContexts::get(self);
        let manifest_contexts =
            direct
                .files(self)
                .get(path)
                .into_iter()
                .flatten()
                .filter(move |context| {
                    direct.direct(self).contains(path)
                        && matches!(
                            context.source,
                            crate::parse_context::EditionSource::Manifest { .. }
                        )
                });
        data.files
            .get(path)
            .into_iter()
            .flatten()
            .chain(self.live_package_contexts(path).filter(|context| {
                data.files
                    .get(&context.entry)
                    .is_some_and(|contexts| contexts.contains(context))
            }))
            .chain(manifest_contexts)
    }

    pub(crate) fn changed_unindexed_package_roots(&self) -> impl Iterator<Item = &SourceContext> {
        let data = LibrarySyntaxInput::get(self).data(self);
        data.unindexed_options
            .iter()
            .filter_map(|(context, options)| {
                (select_edition_source(self, &context.source).options != *options)
                    .then_some(context)
            })
    }

    pub(crate) fn live_package_contexts(
        &self,
        path: &Path,
    ) -> impl Iterator<Item = &SourceContext> {
        LivePackageSyntax::get(self)
            .membership(self)
            .files
            .get(path)
            .into_iter()
            .flatten()
    }

    pub(crate) fn set_live_package_contexts(
        &mut self,
        roots: Vec<SourceContext>,
        catalog: crate::parse_context::ContextCatalog,
        buffers: &BTreeMap<PathBuf, Arc<str>>,
    ) -> bool {
        let input = LivePackageSyntax::get(self);
        for root in &roots {
            if let Some(path) = root.source.path() {
                self.seed_disk_file(path);
            }
        }
        let membership = LivePackageMembership {
            roots,
            files: catalog
                .files
                .into_iter()
                .map(|(path, contexts)| {
                    (
                        path,
                        contexts.into_iter().map(|(context, _)| context).collect(),
                    )
                })
                .collect(),
        };
        for (path, disk) in catalog.sources {
            if let Some(text) = buffers.get(&path).cloned().or_else(|| disk.ok()) {
                self.upsert_file(&path, text);
            }
        }
        if **input.membership(self) == membership {
            return false;
        }
        input.set_membership(self).to(Arc::new(membership));
        true
    }

    pub fn set_library_syntax(&mut self, data: crate::index::LibrarySyntax) {
        let metadata: BTreeSet<_> = data
            .packages
            .values()
            .map(|package| &package.source)
            .chain(
                data.environments
                    .values()
                    .flat_map(|packages| packages.values()),
            )
            .chain(
                data.unindexed_files
                    .values()
                    .flatten()
                    .map(|context| &context.source),
            )
            .filter_map(|source| source.path())
            .collect();
        for path in metadata {
            self.seed_disk_file(path);
        }
        let input = LibrarySyntaxInput::get(self);
        if **input.data(self) != data {
            input.set_data(self).to(Arc::new(data));
        }
    }

    pub fn install_syntax_variant(
        &mut self,
        name: String,
        options: ParseOptions,
        index: Arc<PackageIndex>,
    ) {
        self.install_syntax_variants([(name, options, index)]);
    }

    pub(crate) fn install_syntax_variants(
        &mut self,
        variants: impl IntoIterator<Item = (String, ParseOptions, Arc<PackageIndex>)>,
    ) {
        let input = LibrarySyntaxInput::get(self);
        let mut data = (**input.data(self)).clone();
        for (name, options, index) in variants {
            if data.packages.contains_key(&name) {
                data.variants.insert((name, options), index);
            }
        }
        data.rebuild_memberships();
        if **input.data(self) != data {
            input.set_data(self).to(Arc::new(data));
        }
    }

    /// Discard the cached trees of packages whose disk sources changed. Keep
    /// their last known memberships until the new variants arrive together.
    pub(crate) fn invalidate_workspace_variants(&mut self, paths: &BTreeSet<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let input = LibrarySyntaxInput::get(self);
        let data = input.data(self);
        let names: BTreeSet<_> = self
            .workspace_packages()
            .into_iter()
            .filter(|name| {
                let Some(package) = data.packages.get(name) else {
                    return false;
                };
                let src = package.root.join("src");
                let ext = package.root.join("ext");
                let entry = package.entry(name);
                paths.iter().any(|path| {
                    path.starts_with(&src)
                        || path.starts_with(&ext)
                        || data.files.get(path).is_some_and(|contexts| {
                            contexts.iter().any(|context| context.entry == entry)
                        })
                })
            })
            .collect();
        if names.is_empty() {
            return;
        }
        let mut data = (**data).clone();
        let before = data.variants.len();
        data.variants.retain(|(name, _), _| !names.contains(name));
        if data.variants.len() != before {
            input.set_data(self).to(Arc::new(data));
        }
    }

    pub fn pending_syntax_harvests(&self) -> Vec<(String, PathBuf, ParseOptions)> {
        let input = LibrarySyntaxInput::get(self);
        let data = input.data(self);
        let mut work = BTreeMap::new();
        for (name, package) in &data.packages {
            let sources = std::iter::once(&package.source).chain(
                data.environments
                    .values()
                    .filter_map(|sources| sources.get(name)),
            );
            for source in sources {
                let selected = select_edition_source(self, source);
                if selected.certain
                    && !data
                        .variants
                        .contains_key(&(name.clone(), selected.options))
                {
                    work.insert((name.clone(), selected.options), package.root.clone());
                }
            }
        }
        work.into_iter()
            .map(|((name, options), root)| (name, root, options))
            .collect()
    }

    /// Install loading contexts without replacing authoritative live text.
    pub fn set_source_contexts(&mut self, path: &Path, mut contexts: Vec<SourceContext>) {
        use crate::parse_context::EditionSource;
        for context in &mut contexts {
            context.entry = normalize_path(&context.entry);
            match &mut context.source {
                EditionSource::Project { path, .. } | EditionSource::Manifest { path, .. } => {
                    *path = normalize_path(path);
                }
                EditionSource::Fixed(_) => {}
            }
        }
        self.update_source_contexts([(normalize_path(path), contexts, false)]);
    }

    /// Replace a script load's associations together. Paths that left all
    /// configured programs return to direct discovery.
    pub(crate) fn replace_script_contexts(
        &mut self,
        previous: &BTreeSet<PathBuf>,
        contexts: BTreeMap<PathBuf, Vec<SourceContext>>,
    ) -> bool {
        let buffers = self.metadata_buffers();
        let removed: Vec<_> = previous
            .iter()
            .filter(|path| !contexts.contains_key(*path))
            .map(|path| Self::direct_context(path, &buffers))
            .collect();
        self.update_source_contexts(
            contexts
                .into_iter()
                .map(|(path, contexts)| (path, contexts, false))
                .chain(removed),
        )
    }

    fn direct_context(
        path: &Path,
        buffers: &BTreeMap<PathBuf, Arc<str>>,
    ) -> (PathBuf, Vec<SourceContext>, bool) {
        let env = crate::environment::EnvContext::from_process(path.to_path_buf());
        let (roots, _) = crate::parse_context::manifest_contexts(
            &[path.to_path_buf()],
            None,
            false,
            env.clone(),
        );
        let catalog = crate::parse_context::ContextCatalog::load(&roots, buffers, None);
        let mut contexts: Vec<_> = catalog
            .selections(path)
            .iter()
            .map(|(context, _)| context.clone())
            .collect();
        if contexts.is_empty() {
            contexts.push(crate::parse_context::discover(path, &env, buffers));
        }
        (path.to_path_buf(), contexts, true)
    }

    fn update_source_contexts(
        &mut self,
        updates: impl IntoIterator<Item = (PathBuf, Vec<SourceContext>, bool)>,
    ) -> bool {
        let input = SyntaxContexts::get(self);
        let mut direct = input.direct(self).clone();
        let mut files = input.files(self).clone();
        for (path, contexts, is_direct) in updates {
            for context in &contexts {
                if let Some(source) = context.source.path() {
                    self.seed_disk_file(source);
                }
            }
            if is_direct {
                direct.insert(path.clone());
            } else {
                direct.remove(&path);
            }
            files.insert(path, contexts);
        }
        let changed = *input.direct(self) != direct || *input.files(self) != files;
        if *input.direct(self) != direct {
            input.set_direct(self).to(direct);
        }
        if *input.files(self) != files {
            input.set_files(self).to(files);
        }
        changed
    }

    fn track_default_context(&mut self, path: &Path, file: SourceFile) {
        let input = SyntaxContexts::get(self);
        if crate::environment::is_environment_file(path) {
            if input.unavailable(self).contains(path) {
                let mut unavailable = input.unavailable(self).clone();
                unavailable.remove(path);
                input.set_unavailable(self).to(unavailable);
            }
            if input.metadata(self).get(path) != Some(&file) {
                let mut metadata = input.metadata(self).clone();
                metadata.insert(path.to_path_buf(), file);
                input.set_metadata(self).to(metadata);
            }
        } else {
            self.ensure_source_context(path);
        }
    }

    pub(crate) fn ensure_source_context(&mut self, path: &Path) {
        if SyntaxContexts::get(self).files(self).contains_key(path) {
            return;
        }
        let buffers = self.metadata_buffers();
        let context = if LibrarySyntaxInput::get(self)
            .data(self)
            .files
            .contains_key(path)
        {
            // The library already registered the manifest closure. Keep a
            // cheap direct fallback for when that membership disappears.
            let env = crate::environment::EnvContext::from_process(path.to_path_buf());
            let context = crate::parse_context::discover(path, &env, &buffers);
            (path.to_path_buf(), vec![context], true)
        } else {
            Self::direct_context(path, &buffers)
        };
        self.update_source_contexts([context]);
    }

    pub(crate) fn metadata_buffers(&self) -> BTreeMap<PathBuf, Arc<str>> {
        self.source_map()
            .by_path
            .iter()
            .filter(|(path, _)| {
                crate::environment::is_environment_file(path)
                    && !SyntaxContexts::get(self).unavailable(self).contains(*path)
            })
            .map(|(path, file)| (path.clone(), Arc::clone(file.text(self))))
            .collect()
    }

    /// New, deleted, and renamed project files can change which project owns a
    /// loose buffer. Known package/dependency/include contexts stay authoritative.
    pub fn rediscover_source_contexts(&mut self) {
        let input = SyntaxContexts::get(self);
        let buffers = self.metadata_buffers();
        let contexts: Vec<_> = input
            .direct(self)
            .iter()
            .map(|path| Self::direct_context(path, &buffers))
            .collect();
        self.update_source_contexts(contexts);
    }

    pub fn source_editions(
        &self,
        file: SourceFile,
    ) -> &Arc<Vec<(SourceContext, EditionSelection)>> {
        source_editions(self, file)
    }
    pub fn parse_options(&self, file: SourceFile) -> ParseOptions {
        primary_parse_options(self, file)
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the path→input map, recovering from poisoning. A panic while the
    /// lock is held (now caught by the analysis thread's `guard`, see
    /// `lsp::analysis_thread`) would otherwise poison the mutex and turn every
    /// later access into a hard crash, defeating the point of the panic guard.
    /// The map is a plain lookup table with no cross-field invariant a panic
    /// could leave half-updated, so proceeding past poison is safe.
    fn source_map(&self) -> MutexGuard<'_, FileSourceMap> {
        self.source_map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock the reparse side-channel, recovering from poisoning for the same
    /// reason [`Self::source_map`] does. Losing this cache costs a full parse,
    /// never a wrong answer.
    fn reparse_state(&self) -> MutexGuard<'_, ReparseCache> {
        self.reparse_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Track an in-memory document with no on-disk path. Gets a fresh
    /// [`FileId`] and a `None` path, so it never aliases another file.
    pub fn add_file(&self, text: impl Into<Arc<str>>) -> SourceFile {
        let id = self.source_map().alloc_id();
        SourceFile::new(self, id, None, text.into())
    }

    /// Track (or reuse) the file at `path`, replacing its text. Equivalent path
    /// spellings map to the same input.
    pub fn upsert_file(&mut self, path: &Path, text: impl Into<Arc<str>>) -> SourceFile {
        let key = normalize_path(path);
        let text = text.into();
        let existing = self.source_map().by_path.get(&key).copied();
        let file = match existing {
            Some(file) => {
                // Salsa's setter never compares, so this guard — the compare,
                // not the `ptr_eq` — is what stops a no-op upsert from
                // invalidating the world. The `ptr_eq` in front only makes the
                // common case free: a shared allocation proves equality
                // without reading a byte. It is written out rather than left to
                // `Arc`'s own `PartialEq`, whose identical short-circuit is an
                // unspecified `std` optimization, and this is a path whose cost
                // the benches pin.
                let tracked = file.text(self);
                if !Arc::ptr_eq(tracked, &text) && *tracked != text {
                    file.set_text(self).to(text);
                }
                file
            }
            None => {
                let id = self.source_map().alloc_id();
                let file = SourceFile::new(self, id, Some(key.clone()), text);
                self.source_map().by_path.insert(key.clone(), file);
                file
            }
        };
        self.track_default_context(&key, file);
        file
    }

    /// Look up the input tracked for `path`, if any.
    pub fn lookup_file(&self, path: &Path) -> Option<SourceFile> {
        let key = normalize_path(path);
        self.source_map().by_path.get(&key).copied()
    }

    pub fn set_file_text(&mut self, file: SourceFile, text: impl Into<Arc<str>>) {
        file.set_text(self).to(text.into());
    }

    /// The text currently tracked for `file`.
    pub fn file_text(&self, file: SourceFile) -> &str {
        file.text(self)
    }

    /// The on-disk path `file` was tracked under, or `None` for an in-memory
    /// document.
    pub fn file_path(&self, file: SourceFile) -> Option<PathBuf> {
        file.path(self).clone()
    }

    pub fn parsed_tree(&self, file: SourceFile) -> SyntaxNode {
        parsed_tree_root(self, file)
    }

    /// The cached control-flow graph for `file` (the [`control_flow`] query).
    pub fn control_flow(&self, file: SourceFile) -> &FileControlFlow {
        control_flow(self, file)
    }

    /// Replace the whole harvested library index, its source roots, and the
    /// workspace-package names, at HIGH durability. Creates the singleton input
    /// on first call. Re-analyze open files after a swap: dependents of the
    /// index (once they exist) will have been invalidated.
    pub fn set_library(
        &mut self,
        packages: BTreeMap<String, Arc<PackageIndex>>,
        roots: BTreeMap<String, PathBuf>,
        workspaces: Vec<String>,
    ) {
        let packages = LibraryPackages(packages);
        let roots = LibraryRoots(roots);
        match LibraryIndex::try_get(self) {
            Some(index) => {
                index
                    .set_packages(self)
                    .with_durability(Durability::HIGH)
                    .to(packages);
                index
                    .set_roots(self)
                    .with_durability(Durability::HIGH)
                    .to(roots);
                index
                    .set_workspaces(self)
                    .with_durability(Durability::HIGH)
                    .to(workspaces);
            }
            None => {
                // Creating the singleton input registers it in storage; the
                // handle is refetched via `try_get` on later calls.
                let _ = LibraryIndex::builder(packages, roots, workspaces, LibraryDeps::default())
                    .durability(Durability::HIGH)
                    .new(self);
            }
        }
    }

    /// Replace what the manifest pinned: every package's version and kind,
    /// keyed by name, at HIGH durability. Creates the singleton input on first
    /// call, exactly as [`set_library`](Self::set_library) does, so either may
    /// run first.
    ///
    /// Written separately from `set_library`'s three because it is a different
    /// map of a different thing: those are products of the *harvest*, keyed by
    /// what was successfully indexed, while this comes from the *manifest* and
    /// deliberately carries names that were never indexed at all. Both ride the
    /// same cadence — one `LibraryMessage::Full` sets both.
    pub fn set_library_deps(&mut self, deps: BTreeMap<String, PackageMeta>) {
        let deps = LibraryDeps(deps);
        match LibraryIndex::try_get(self) {
            Some(index) => {
                index
                    .set_deps(self)
                    .with_durability(Durability::HIGH)
                    .to(deps);
            }
            None => {
                let _ = LibraryIndex::builder(
                    LibraryPackages::default(),
                    LibraryRoots::default(),
                    Vec::new(),
                    deps,
                )
                .durability(Durability::HIGH)
                .new(self);
            }
        }
    }

    /// Replace the whole harvested library index, preserving any existing source
    /// roots and workspace names. Convenience for callers (tests) that only
    /// supply package data.
    pub fn set_library_packages(&mut self, packages: BTreeMap<String, Arc<PackageIndex>>) {
        let (roots, workspaces) = LibraryIndex::try_get(self)
            .map(|lib| (lib.roots(self).0.clone(), lib.workspaces(self).clone()))
            .unwrap_or_default();
        self.set_library(packages, roots, workspaces);
    }

    /// Track each workspace package's project file and record the name → input
    /// mapping at HIGH durability. Kept apart from
    /// [`set_library`](Self::set_library) because it tracks the project files
    /// rather than the harvest: a `Project.toml` edit changes what
    /// [`declared_deps`](Self::declared_deps) answers without a re-harvest, and
    /// a re-harvest leaves the text alone.
    ///
    /// Seeding goes through [`seed_disk_file`](Self::seed_disk_file), which is
    /// create-or-return: a re-resolve must never clobber an open, unsaved
    /// `Project.toml` buffer with the disk copy. A path that is neither tracked
    /// nor readable contributes no entry, exactly as an unresolved environment
    /// contributes none.
    pub fn set_project_files(&mut self, paths: BTreeMap<String, PathBuf>) {
        let mut files: BTreeMap<String, SourceFile> = BTreeMap::new();
        for (name, path) in paths {
            if let Some(file) = self.seed_disk_file(&path) {
                files.insert(name, file);
            }
        }
        match ProjectFiles::try_get(self) {
            Some(input) => {
                input
                    .set_files(self)
                    .with_durability(Durability::HIGH)
                    .to(files);
            }
            None => {
                let _ = ProjectFiles::builder(files)
                    .durability(Durability::HIGH)
                    .new(self);
            }
        }
    }

    /// The declared direct dependencies of the workspace package `name`, when
    /// the environment has resolved and that package is one under development.
    /// Derived from the project file's tracked text (see
    /// [`project_declared_deps`]), so an editor's unsaved `[deps]` edit counts.
    pub fn declared_deps(&self, name: &str) -> Option<Arc<DeclaredDeps>> {
        project_declared_deps(self, self.project_file_of(name)?)
    }

    /// The declared dependencies read off the project file tracked at `path`,
    /// for a caller holding a path rather than a package name — the language
    /// server's gate for whether a project-file keystroke changed anything a
    /// consumer would notice.
    ///
    /// `None` unless `path` is a **workspace package's** project file, as
    /// registered by [`set_project_files`](Self::set_project_files). An editor
    /// opens project files that belong to no package — a `docs/Project.toml`, a
    /// depot one followed from a manifest link — and their text is tracked like
    /// any other buffer, but nothing reads their `[deps]`. Answering with it
    /// would re-lint the whole workspace on every character typed in a file no
    /// consumer has ever asked about.
    ///
    /// Also `None` for an untracked or unparseable file.
    pub fn declared_deps_of_file(&self, path: &Path) -> Option<Arc<DeclaredDeps>> {
        let file = self.lookup_file(path)?;
        let claimed = ProjectFiles::try_get(self)?
            .files(self)
            .values()
            .any(|tracked| *tracked == file);
        claimed.then(|| project_declared_deps(self, file))?
    }

    /// The input tracking package `name`'s project file, if one is registered.
    fn project_file_of(&self, name: &str) -> Option<SourceFile> {
        ProjectFiles::try_get(self)?.files(self).get(name).copied()
    }

    /// Insert or replace a single package's index, keeping the rest (and the
    /// roots and workspace names). Cheap: the map's other entries are `Arc`
    /// pointer clones. This is the on-save re-harvest path for a workspace
    /// package.
    pub fn set_package_index(&mut self, name: impl Into<String>, index: Arc<PackageIndex>) {
        let mut packages = LibraryIndex::try_get(self)
            .map(|lib| lib.packages(self).0.clone())
            .unwrap_or_default();
        packages.insert(name.into(), index);
        self.set_library_packages(packages);
    }

    /// Track the file at `path` for the reverse-occurrence index, seeding its
    /// text from disk on first sight. **Create-or-return, never clobber:** if the
    /// path is already tracked (an open editor buffer or a previously seeded
    /// member) the existing input is returned untouched, because the tracked text
    /// is authoritative and a stale disk read must never overwrite an open,
    /// unsaved buffer. Inputs are created at the default (LOW) durability, like
    /// open files, since a member's text changes per keystroke once it is opened.
    /// Returns `None` if the file cannot be read and was not already tracked.
    pub fn seed_disk_file(&mut self, path: &Path) -> Option<SourceFile> {
        let key = normalize_path(path);
        if let Some(file) = self.lookup_file(&key) {
            return Some(file);
        }
        if !key.is_file() {
            return None;
        }
        let text = std::fs::read_to_string(&key).ok()?;
        let id = self.source_map().alloc_id();
        let file = SourceFile::new(self, id, Some(key.clone()), Arc::from(text));
        self.source_map().by_path.insert(key.clone(), file);
        self.track_default_context(&key, file);
        Some(file)
    }

    /// Install a completed load without overwriting any live editor buffer.
    /// Failed reads remove availability even if a stale SourceFile still exists.
    pub fn set_script_sources(
        &mut self,
        entries: &[PathBuf],
        sources: &project::scripts::ScriptSources,
        buffers: &BTreeMap<PathBuf, Arc<str>>,
    ) {
        let files = sources
            .iter()
            .map(|(path, disk)| {
                let text = buffers.get(path).or_else(|| disk.as_ref().ok());
                (
                    path.clone(),
                    text.map(|text| self.upsert_file(path, Arc::clone(text))),
                )
            })
            .collect();
        let entries = entries.iter().cloned().collect();
        match ScriptFiles::try_get(self) {
            Some(input) => {
                if *input.files(self) != files {
                    input.set_files(self).to(files);
                }
                if *input.entries(self) != entries {
                    input.set_entries(self).to(entries);
                }
                if *input.pending(self) {
                    input.set_pending(self).to(false);
                }
            }
            None => {
                ScriptFiles::new(self, files, entries, false);
            }
        }
    }

    pub(crate) fn set_scripts_pending(&mut self) {
        match ScriptFiles::try_get(self) {
            Some(input) if !input.pending(self) => {
                input.set_pending(self).to(true);
            }
            None => {
                ScriptFiles::new(self, BTreeMap::new(), BTreeSet::new(), true);
            }
            _ => {}
        }
    }

    /// Loading state is a leaf; checking it must not demand program analysis
    /// on the writer thread when a background result arrives.
    pub(crate) fn scripts_pending(&self) -> bool {
        ScriptFiles::try_get(self).is_none_or(|input| *input.pending(self))
    }

    /// Replace the reverse-index file set, preserving unchanged membership.
    pub fn set_workspace_files(&mut self, files: Vec<SourceFile>) {
        match WorkspaceFiles::try_get(self) {
            Some(wf) if *wf.files(self) != files => {
                wf.set_files(self).to(files);
            }
            None => {
                let _ = WorkspaceFiles::builder(files, Vec::new()).new(self);
            }
            _ => {}
        }
    }

    /// Revert the tracked input for `path` to its on-disk text, if the file is
    /// tracked and readable. Used when an editor closes a member file: the
    /// discarded (possibly unsaved) buffer must not linger in the
    /// reverse-occurrence index, where on-disk content is authoritative once the
    /// document is closed. Unreadable or deleted Julia files lose their live
    /// text. Untracked files or unchanged text need no database write.
    ///
    /// Evicts the file's incremental-reparse state first, before any of those
    /// early returns: the buffer this path was being edited through is gone, so
    /// both the base and any chain staged against it are dead weight, and a
    /// deleted-file revert must not leave them behind.
    pub fn revert_file_to_disk(&mut self, path: &Path) {
        let Some(file) = self.lookup_file(path) else {
            return;
        };
        self.reparse_evict(file);
        let Ok(text) = std::fs::read_to_string(path) else {
            if crate::environment::is_environment_file(path) {
                let input = SyntaxContexts::get(self);
                let mut unavailable = input.unavailable(self).clone();
                if unavailable.insert(normalize_path(path)) {
                    input.set_unavailable(self).to(unavailable);
                }
            } else if !file.text(self).is_empty() {
                file.set_text(self).to(Arc::from(""));
            }
            return;
        };
        if crate::environment::is_environment_file(path) {
            self.track_default_context(&normalize_path(path), file);
        }
        if **file.text(self) != *text {
            file.set_text(self).to(Arc::from(text));
        }
    }

    /// Seed every workspace package's member files as inputs and register their
    /// union as the reverse-index file set. A no-op when no folder is a package
    /// project or nothing has been harvested yet. Called right after the library
    /// index is swapped in (on the analysis thread's write path).
    pub fn seed_workspace_members(&mut self) -> bool {
        let names = self.workspace_packages();
        let mut files: Vec<SourceFile> = Vec::new();
        let mut seen: std::collections::HashSet<SourceFile> = std::collections::HashSet::new();
        let mut entries = BTreeSet::new();
        let mut uncertain_members = BTreeSet::new();
        for name in names {
            let Some(root) = self.package_root(&name) else {
                continue;
            };
            let Some(pkg) = self.library_package(&name) else {
                let data = LibrarySyntaxInput::get(self).data(self);
                if data
                    .packages
                    .get(&name)
                    .is_some_and(|package| !select_edition_source(self, &package.source).certain)
                {
                    for ((variant, _), index) in &data.variants {
                        if *variant == name {
                            uncertain_members
                                .extend(index.members.iter().map(|path| root.join(path)));
                        }
                    }
                }
                continue;
            };
            entries.insert(normalize_path(&root.join("src").join(format!("{name}.jl"))));
            // Dedup across packages: overlapping folders must not register a
            // file twice, or the reference index would double-count it.
            for rel in &pkg.members {
                if let Some(file) = self.seed_disk_file(&root.join(rel))
                    && seen.insert(file)
                {
                    files.push(file);
                }
            }
        }
        for (path, contexts) in &LivePackageSyntax::get(self).membership(self).files {
            if contexts
                .iter()
                .any(|context| entries.contains(&context.entry))
                && let Some(file) = self.lookup_file(path)
                && seen.insert(file)
            {
                files.push(file);
            }
        }
        // Pin a canonical (by-path) order so an unchanged member set always
        // seeds an identical `Vec`, independent of the iteration order of
        // `names`/`members` above. `WorkspaceFiles` is compared by value, so a
        // reharvest that merely reshuffles that order would otherwise bump the
        // input revision and force a needless `workspace_reference_index`
        // recompute. Every seeded member has a path (`seed_disk_file` sets one),
        // and each path maps to a single handle, so the sort is total and stable.
        files.sort_by(|a, b| a.path(self).cmp(b.path(self)));
        let changed = WorkspaceFiles::try_get(self)
            .map_or(!files.is_empty(), |input| *input.files(self) != files);
        self.set_workspace_files(files);
        let rename_only: Vec<_> = uncertain_members
            .iter()
            .filter_map(|path| self.seed_disk_file(path))
            .filter(|file| seen.insert(*file))
            .collect();
        let input = WorkspaceFiles::get(self);
        if *input.rename_only(self) != rename_only {
            input.set_rename_only(self).to(rename_only);
        }
        changed
    }

    /// The harvested index for `name`, if the library has been populated and
    /// contains it.
    pub fn library_package(&self, name: &str) -> Option<Arc<PackageIndex>> {
        library_package_for_context(self, name, None)
    }

    /// The absolute source root of package `name` (the directory its
    /// `DefLocation` paths are relative to), if known.
    pub fn package_root(&self, name: &str) -> Option<PathBuf> {
        LibraryIndex::try_get(self)?
            .roots(self)
            .0
            .get(name)
            .cloned()
    }

    /// The version and kind the manifest pinned for package `name`, if it
    /// pinned one. Present for packages whose source was never found, and
    /// absent for the Base/Core/stdlib floor, which no manifest pins.
    pub fn package_meta(&self, name: &str) -> Option<PackageMeta> {
        LibraryIndex::try_get(self)?.deps(self).0.get(name).cloned()
    }

    /// The names of the packages under development (one per workspace folder
    /// that is a package project), empty when none.
    pub fn workspace_packages(&self) -> Vec<String> {
        LibraryIndex::try_get(self)
            .map(|lib| lib.workspaces(self).clone())
            .unwrap_or_default()
    }

    /// The workspace package and the host module path of `path`, when `path` is
    /// one of its source files (a file under the package's source root). The host
    /// module is the nested `module` the file's `include` lexically landed in
    /// (empty for the root module), from the [`project_graph`]'s
    /// [`host_module_of`] firewall, so the file's globals and free reads resolve
    /// against it rather than always the root. A path not tracked as an input
    /// (e.g. a freshly opened, not-yet-seeded buffer) falls back to the root
    /// module rather than dropping out of the workspace.
    pub fn workspace_member(&self, path: &Path) -> Option<(Arc<PackageIndex>, ModulePath)> {
        let pkg = workspace_package_for(self, path)?;
        let host = self
            .lookup_file(path)
            .map(|file| {
                host_module_of(self, file)
                    .iter()
                    .map(SmolStr::new)
                    .collect()
            })
            .unwrap_or_default();
        Some((pkg, host))
    }

    /// Just the workspace package for `path` (see [`workspace_member`](Self::workspace_member)).
    pub fn workspace_module(&self, path: &Path) -> Option<Arc<PackageIndex>> {
        self.workspace_member(path).map(|(pkg, _)| pkg)
    }

    /// Mint a read-only [`Analysis`] snapshot: a short-lived db clone wrapped so
    /// callers can only *read*. Drop it promptly — an outstanding clone blocks
    /// the next write (salsa is single-writer; see the [`Clone`] impl).
    pub fn snapshot(&self) -> Analysis {
        Analysis(self.clone(), None, None)
    }
}

/// A read-only handle onto the incremental database, à la rust-analyzer's
/// `Analysis` (vs. its writer `AnalysisHost`). Wraps a short-lived clone of the
/// analysis thread's [`IncrementalDatabase`] and exposes *only* read queries,
/// so a read job cannot call `upsert_file` or salsa setters — the single-writer
/// invariant is encoded in the type system rather than left to convention.
#[derive(Clone)]
pub struct Analysis(
    IncrementalDatabase,
    Option<ParseOptions>,
    Option<SourceContext>,
);

impl Analysis {
    /// Capture a read's editions before dispatch, including documents still
    /// queued for their first analysis and therefore lacking a Salsa input.
    pub(crate) fn capture_source_editions(
        &self,
        path: &Path,
    ) -> Vec<(SourceContext, EditionSelection)> {
        if let Some(file) = self.lookup_file(path) {
            return self.source_editions(file).as_ref().clone();
        }
        let path = normalize_path(path);
        let known = tracked_source_editions(&self.0, &path);
        if !known.is_empty() {
            return known;
        }
        let buffers = self.0.metadata_buffers();
        let (_, contexts, _) = IncrementalDatabase::direct_context(&path, &buffers);
        contexts
            .into_iter()
            .map(|context| {
                let selection = context.selection(&buffers);
                (context, selection)
            })
            .collect()
    }

    pub fn options_for_path(&self, path: &Path) -> ParseOptions {
        salsa::Cancelled::catch(std::panic::AssertUnwindSafe(|| {
            self.tracked_options_for_path(path)
        }))
        .unwrap_or_else(|_| self.1.unwrap_or_default())
    }
    fn tracked_options_for_path(&self, path: &Path) -> ParseOptions {
        if let Some(file) = self.lookup_file(path) {
            return self.parse_options(file);
        }
        if self.2.is_none()
            && let Some(options) = self.1
        {
            return options;
        }
        let path = normalize_path(path);
        let known = tracked_source_editions(&self.0, &path);
        if let (Some(options), Some(context)) = (self.1, &self.2)
            && (context.entry == path || known.iter().any(|(candidate, _)| candidate == context))
        {
            return options;
        }
        known
            .first()
            .map_or_else(ParseOptions::default, |(_, selection)| selection.options)
    }
    pub fn with_context(&self, context: SourceContext, options: ParseOptions) -> Self {
        Self(self.0.clone(), Some(options), Some(context))
    }

    pub(crate) fn context(&self) -> Option<&SourceContext> {
        self.2.as_ref()
    }

    pub fn with_options(&self, options: ParseOptions) -> Self {
        Self(self.0.clone(), Some(options), self.2.clone())
    }
    pub fn parse_options(&self, file: SourceFile) -> ParseOptions {
        if let Some(options) = self.1
            && (self.2.is_none()
                || self
                    .source_editions(file)
                    .iter()
                    .any(|(context, _)| Some(context) == self.2.as_ref()))
        {
            return options;
        }
        primary_parse_options(&self.0, file)
    }
    pub fn source_editions(
        &self,
        file: SourceFile,
    ) -> &Arc<Vec<(SourceContext, EditionSelection)>> {
        source_editions(&self.0, file)
    }

    pub(crate) fn import_projection(&self, file: SourceFile) -> Arc<project::scripts::ScriptFile> {
        Arc::clone(script_file_with_options(
            &self.0,
            file,
            self.parse_options(file),
        ))
    }

    pub fn workspace_import_usage(&self) -> project::scripts::ScriptAnalysis {
        // Singleton creation is not a tracked dependency. Do not memoize the
        // absence of inputs before the first workspace harvest installs them.
        let Some(library) = LibraryIndex::try_get(&self.0) else {
            return project::scripts::ScriptAnalysis {
                pending: true,
                ..Default::default()
            };
        };
        if library.workspaces(&self.0).is_empty() {
            return project::scripts::ScriptAnalysis::default();
        }
        let Some(files) = WorkspaceFiles::try_get(&self.0) else {
            return project::scripts::ScriptAnalysis {
                pending: true,
                ..Default::default()
            };
        };
        project::scripts::ScriptAnalysis {
            programs: workspace_import_programs(&self.0, files, library)
                .iter()
                .filter(|program| {
                    self.2
                        .as_ref()
                        .is_none_or(|context| context.entry == program.entry)
                })
                .cloned()
                .collect(),
            pending: false,
        }
    }

    /// Resolve configured programs against tracked source, never disk. A
    /// configuration not loaded yet cannot establish absence of a name.
    pub fn scripts(&self, entries: &[PathBuf]) -> project::scripts::ScriptAnalysis {
        use project::scripts::{ScriptAnalysis, ScriptProgram};
        let Some(input) = ScriptFiles::try_get(&self.0) else {
            return ScriptAnalysis {
                pending: !entries.is_empty(),
                ..Default::default()
            };
        };
        let pending = *input.pending(&self.0)
            || entries
                .iter()
                .any(|entry| !input.entries(&self.0).contains(entry));
        let programs = entries
            .iter()
            .filter(|entry| {
                self.2.as_ref().is_none_or(|context| {
                    if matches!(
                        context.source,
                        crate::parse_context::EditionSource::Fixed(_)
                            | crate::parse_context::EditionSource::Project {
                                role: crate::parse_context::SourceRole::Script,
                                ..
                            }
                    ) {
                        return input
                            .files(&self.0)
                            .get(*entry)
                            .copied()
                            .flatten()
                            .is_some_and(|file| {
                                self.source_editions(file)
                                    .iter()
                                    .any(|(candidate, selected)| {
                                        candidate.entry == **entry
                                            && candidate.source == context.source
                                            && Some(selected.options) == self.1
                                    })
                            });
                    }
                    context.entry == **entry
                })
            })
            .map(
                |entry| match input.files(&self.0).get(entry).copied().flatten() {
                    Some(file) => Arc::clone(script_program_with_options(
                        &self.0,
                        file,
                        self.parse_options(file),
                    )),
                    None => Arc::new(ScriptProgram::build(entry, |_| None)),
                },
            )
            .collect();
        ScriptAnalysis { programs, pending }
    }

    /// The `SourceFile` input currently tracked for `path`, if any.
    pub fn lookup_file(&self, path: &Path) -> Option<SourceFile> {
        self.0.lookup_file(path)
    }

    /// The text currently tracked for `file`.
    pub fn file_text(&self, file: SourceFile) -> &str {
        self.0.file_text(file)
    }

    pub(crate) fn file_text_arc(&self, file: SourceFile) -> Arc<str> {
        Arc::clone(file.text(&self.0))
    }

    /// Parse diagnostics for `file` (empty when it parses cleanly).
    pub fn parse_diagnostics(&self, file: SourceFile) -> &[ParseDiagnostic] {
        parse_diagnostics_with_options(&self.0, file, self.parse_options(file))
    }

    /// A fresh `SyntaxNode` over the cached parse tree.
    pub fn parsed_tree(&self, file: SourceFile) -> SyntaxNode {
        parsed_tree_root_with_options(&self.0, file, self.parse_options(file))
    }

    /// The cached semantic model for `file`.
    pub fn semantic_model(&self, file: SourceFile) -> &SemanticModel {
        semantic_model_with_options(&self.0, file, self.parse_options(file))
    }

    /// Markdown navigation cached by the file's static documentation payloads.
    pub fn documentation_index(
        &self,
        file: SourceFile,
    ) -> &Arc<crate::documentation::DocumentationIndex> {
        documentation_index_with_options(&self.0, file, self.parse_options(file))
    }

    /// The cached control-flow graph for `file` (the [`control_flow`] query).
    pub fn control_flow(&self, file: SourceFile) -> &FileControlFlow {
        control_flow_with_options(&self.0, file, self.parse_options(file))
    }

    /// The file's top-level definitions (the [`file_exports`] firewall query).
    pub fn file_exports(&self, file: SourceFile) -> &BTreeSet<String> {
        file_exports_with_options(&self.0, file, self.parse_options(file))
    }

    /// The names the file reads but binds nowhere (the [`file_free_reads`]
    /// firewall query).
    pub fn file_free_reads(&self, file: SourceFile) -> &BTreeSet<String> {
        file_free_reads_with_options(&self.0, file, self.parse_options(file))
    }

    /// The module-qualified names the file references (the
    /// [`file_qualified_reads`] firewall query).
    pub fn file_qualified_reads(&self, file: SourceFile) -> &BTreeSet<String> {
        file_qualified_reads_with_options(&self.0, file, self.parse_options(file))
    }

    /// The file's static `include` edges (the [`include_edges`] firewall query).
    pub fn include_edges(&self, file: SourceFile) -> &[IncludeEdge] {
        include_edges_with_options(&self.0, file, self.parse_options(file))
    }

    /// The workspace reverse-occurrence index: every member file's occurrences of
    /// every top-level symbol, keyed by `(namespace, name)`. Backs cross-file
    /// references and rename. See [`workspace_reference_index`].
    pub fn workspace_reference_index(&self) -> &WorkspaceReferenceIndex {
        if let Some(context) = &self.2
            && let Some(entry) = self.lookup_file(&context.entry)
        {
            return context_workspace_reference_index(
                &self.0,
                entry,
                self.1.unwrap_or_else(|| self.parse_options(entry)),
                context.clone(),
            );
        }
        workspace_reference_index(&self.0)
    }

    /// The seeded workspace member files (the [`WorkspaceFiles`] input), for a
    /// consumer that must fan out over exactly the package's source set without
    /// going through a query. Owned rather than borrowed: the handles are
    /// `Copy`, matching [`workspace_packages`](Self::workspace_packages).
    pub fn workspace_files(&self) -> Vec<SourceFile> {
        WorkspaceFiles::try_get(&self.0)
            .map(|files| files.files(&self.0).clone())
            .unwrap_or_default()
    }

    pub(crate) fn file_rename_sources(&self) -> Vec<SourceFile> {
        let mut files = self.workspace_files();
        if let Some(input) = WorkspaceFiles::try_get(&self.0) {
            files.extend(input.rename_only(&self.0));
        }
        if let Some(input) = ScriptFiles::try_get(&self.0) {
            files.extend(input.files(&self.0).values().filter_map(|file| *file));
        }
        files
    }

    /// The package's transitive `include` graph (see [`project_graph`]): closure,
    /// forward/reverse edges, host modules, cycles, and unresolved includes.
    pub fn project_graph(&self) -> &ProjectGraph {
        if let Some(context) = &self.2
            && let Some(entry) = self.lookup_file(&context.entry)
            && WorkspaceFiles::try_get(&self.0)
                .is_some_and(|files| files.files(&self.0).contains(&entry))
        {
            return project_graph_with_options(
                &self.0,
                entry,
                self.1.unwrap_or_else(|| self.parse_options(entry)),
            );
        }
        project_graph(&self.0)
    }

    /// The on-disk path tracked for `file` (the reverse index tags each
    /// occurrence with its [`SourceFile`]; this turns that back into a path).
    pub fn file_path_of(&self, file: SourceFile) -> Option<PathBuf> {
        self.0.file_path(file)
    }

    /// The text currently tracked for `file` (the buffer if open, else the
    /// seeded disk text) — consistent with the reverse index within one snapshot.
    pub fn file_text_of(&self, file: SourceFile) -> &str {
        self.0.file_text(file)
    }

    /// The harvested index for package `name`, if present.
    pub fn library_package(&self, name: &str) -> Option<Arc<PackageIndex>> {
        library_package_for_context(&self.0, name, self.2.as_ref())
    }

    /// The absolute source root of package `name`, if the live server has
    /// located the depot and harvested it.
    pub fn package_root(&self, name: &str) -> Option<PathBuf> {
        self.0.package_root(name)
    }

    /// The version and kind the manifest pinned for package `name`. See
    /// [`IncrementalDatabase::package_meta`].
    pub fn package_meta(&self, name: &str) -> Option<PackageMeta> {
        self.0.package_meta(name)
    }

    /// The names of the packages under development, empty when none.
    pub fn workspace_packages(&self) -> Vec<String> {
        self.0.workspace_packages()
    }

    /// The tracked project file of a workspace package, including unsaved text.
    pub(crate) fn project_file_of(&self, name: &str) -> Option<SourceFile> {
        self.0.project_file_of(name)
    }

    /// The declared direct dependencies of the workspace package `name`. See
    /// [`IncrementalDatabase::declared_deps`].
    pub fn declared_deps(&self, name: &str) -> Option<Arc<DeclaredDeps>> {
        self.0.declared_deps(name)
    }

    /// The workspace package and host module path of `path`, when `path` is one
    /// of its source files. See [`IncrementalDatabase::workspace_member`].
    pub fn workspace_member(&self, path: &Path) -> Option<(Arc<PackageIndex>, ModulePath)> {
        let direct = SyntaxContexts::try_get(&self.0)
            .is_some_and(|input| input.direct(&self.0).contains(&normalize_path(path)));
        let known_package = LibrarySyntaxInput::try_get(&self.0).is_some_and(|input| {
            input
                .data(&self.0)
                .files
                .contains_key(&normalize_path(path))
        }) || LivePackageSyntax::try_get(&self.0).is_some_and(|input| {
            input
                .membership(&self.0)
                .files
                .contains_key(&normalize_path(path))
        });
        if direct && !known_package {
            let (package, host) = self.0.workspace_member(path)?;
            return Some((self.library_package(&package.name)?, host));
        }
        if let Some(context) = &self.2 {
            let library = LibraryIndex::try_get(&self.0)?;
            let name = library.workspaces(&self.0).iter().find(|name| {
                library.roots(&self.0).0.get(*name).is_some_and(|root| {
                    normalize_path(&root.join("src").join(format!("{name}.jl"))) == context.entry
                })
            })?;
            let package = self.library_package(name)?;
            let host = self
                .project_graph()
                .host_modules
                .get(&normalize_path(path))?
                .iter()
                .map(SmolStr::new)
                .collect();
            return Some((package, host));
        }
        let (package, host) = self.0.workspace_member(path)?;
        Some((self.library_package(&package.name)?, host))
    }

    /// Just the workspace package for `path`. See
    /// [`IncrementalDatabase::workspace_module`].
    pub fn workspace_module(&self, path: &Path) -> Option<Arc<PackageIndex>> {
        self.workspace_member(path).map(|(package, _)| package)
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
        Resolver::new(self.semantic_model(file), self)
            .with_workspace(self.workspace_module_for(file))
            .resolve(name, offset, namespace)
    }

    /// Every name visible at `offset` in `file`, in the shared masking order
    /// with shadowed names dropped. For completion.
    pub fn visible_names(
        &self,
        file: SourceFile,
        offset: TextSize,
        namespace: Namespace,
    ) -> Vec<Candidate> {
        Resolver::new(self.semantic_model(file), self)
            .with_workspace(self.workspace_module_for(file))
            .visible(offset, namespace)
    }

    /// The workspace package and host module path `file` belongs to (tier 2), if
    /// it is one of the workspace package's source files.
    fn workspace_module_for(&self, file: SourceFile) -> Option<(Arc<PackageIndex>, ModulePath)> {
        let path = self.0.file_path(file)?;
        self.workspace_member(&path)
    }
}

/// The library seen through a read-only snapshot, so a [`Resolver`] can run off
/// the analysis thread.
impl PackageSource for Analysis {
    fn syntax_options(&self) -> ParseOptions {
        self.1.unwrap_or_default()
    }
    fn syntax_options_at(&self, path: &Path) -> ParseOptions {
        self.options_for_path(path)
    }

    fn package(&self, name: &str) -> Option<Arc<PackageIndex>> {
        self.library_package(name)
    }

    fn package_root(&self, name: &str) -> Option<PathBuf> {
        Analysis::package_root(self, name)
    }

    fn workspace_member(&self, path: &Path) -> Option<(Arc<PackageIndex>, ModulePath)> {
        Analysis::workspace_member(self, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_metadata_keeps_parses_and_switching_editions_reuses_each_view() {
        use salsa::Database as _;
        let executions = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&executions);
        let mut db = IncrementalDatabase {
            storage: salsa::Storage::new(Some(Box::new(move |event| {
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    recorded.lock().unwrap().push(database_key);
                }
            }))),
            ..Default::default()
        };
        SyntaxContexts::new(
            &db,
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        );
        LibrarySyntaxInput::new(&db, Arc::new(crate::index::LibrarySyntax::default()));
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("Project.toml");
        let metadata = "[syntax]\njulia_version = \"1.13\"\n";
        std::fs::write(&project, metadata).unwrap();
        let file = db.upsert_file(&dir.path().join("src/Foo.jl"), "typegroup = 1\n");
        let runs = |db: &IncrementalDatabase| {
            executions
                .lock()
                .unwrap()
                .iter()
                .filter(|key| {
                    db.ingredient_debug_name(key.ingredient_index())
                        == "parsed_document_with_options"
                })
                .count()
        };
        assert!(parse_diagnostics(&db, file).is_empty());
        assert_eq!(runs(&db), 1);
        db.upsert_file(&project, format!("# comment\n{metadata}"));
        assert!(parse_diagnostics(&db, file).is_empty());
        assert_eq!(
            runs(&db),
            1,
            "normalized selection backdates unchanged options"
        );
        db.upsert_file(&project, metadata.replace("1.13", "1.14"));
        assert!(!parse_diagnostics(&db, file).is_empty());
        assert_eq!(runs(&db), 2);
        db.upsert_file(&project, metadata);
        assert!(parse_diagnostics(&db, file).is_empty());
        assert_eq!(runs(&db), 2, "the old edition still owns its cached parse");
    }

    #[test]
    fn documentation_parse_query_runs_only_when_payloads_change() {
        use salsa::Database as _;

        let executions = Arc::new(Mutex::new(Vec::new()));
        let recorded = executions.clone();
        let mut db = IncrementalDatabase {
            storage: salsa::Storage::new(Some(Box::new(move |event| {
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    recorded.lock().unwrap().push(database_key);
                }
            }))),
            ..Default::default()
        };
        let index_runs = |db: &IncrementalDatabase| {
            executions
                .lock()
                .unwrap()
                .iter()
                .filter(|key| {
                    db.ingredient_debug_name(key.ingredient_index())
                        == "documentation_index_with_options"
                })
                .count()
        };
        let original = "x = 1\n\"# Heading\\n\\nSome prose.\"\nf() = x\n";
        let file = db.add_file(original);
        semantic_model(&db, file);
        assert_eq!(
            index_runs(&db),
            0,
            "ordinary analysis must not demand Markdown navigation"
        );
        documentation_index(&db, file);
        documentation_index(&db, file);
        assert_eq!(index_runs(&db), 1);

        for text in [
            original.replace("x = 1", "x = 10000"),
            original.replace("Heading", "\\u0048eading"),
        ] {
            db.set_file_text(file, text);
            documentation_index(&db, file);
            assert_eq!(
                index_runs(&db),
                1,
                "equal payloads must prevent reparsing, not just compare equal afterward"
            );
        }
        db.set_file_text(file, original.replace("Some prose", "Other prose"));
        documentation_index(&db, file);
        assert_eq!(
            index_runs(&db),
            2,
            "a changed payload must be parsed even when its anchors are unchanged"
        );

        let other = db.add_file("\"# Other file\"\ng() = 0\n");
        documentation_index(&db, other);
        assert_eq!(index_runs(&db), 3);
        db.set_file_text(other, "g() = 0\n");
        documentation_index(&db, file);
        assert_eq!(
            index_runs(&db),
            3,
            "another file's edit must not reparse this file's Markdown"
        );
    }

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
        let a = db.upsert_file(Path::new("/tmp/a.jl"), "x = 1\n");
        let b = db.upsert_file(Path::new("/tmp/./a.jl"), "x = 2\n");
        assert!(a == b, "equivalent path spellings should reuse one input");
        assert_eq!(parsed_tree_root(&db, a).to_string(), "x = 2\n");
    }

    #[test]
    fn poisoned_source_map_lock_recovers() {
        // Panicking while the `source_map` mutex is held poisons it. With the
        // analysis thread's `guard` now catching such panics, the next db access
        // must recover past the poison rather than crash the sole writer.
        let mut db = IncrementalDatabase::new();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = db.source_map.lock().unwrap();
            panic!("poison the source map");
        }));
        assert!(poisoned.is_err(), "the closure should have panicked");
        assert!(db.source_map.is_poisoned(), "the mutex should be poisoned");

        // Every path through the recovery helper still works.
        let file = db.upsert_file(Path::new("/tmp/x.jl"), "x = 1\n");
        assert!(
            db.lookup_file(Path::new("/tmp/x.jl")) == Some(file),
            "lookup should find the just-upserted file after poison recovery"
        );
        assert_eq!(parsed_tree_root(&db, file).to_string(), "x = 1\n");
    }
}
