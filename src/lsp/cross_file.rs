//! Shared plumbing for cross-file references and rename over the package under
//! development.
//!
//! Both features classify the workspace top-level symbol at the cursor the same
//! way (via [`Resolver::workspace_symbol_at`](crate::resolve::Resolver::workspace_symbol_at))
//! and then gather every occurrence of it across the package's member files from
//! the reverse-occurrence index
//! ([`workspace_reference_index`](crate::incremental::Analysis::workspace_reference_index)).
//! References turn the gathered sites into [`Location`]s; rename turns them into
//! a multi-file [`WorkspaceEdit`](lsp_types::WorkspaceEdit). The one shared step
//! — grouping the index's `(SourceFile, occurrence)` pairs into per-file ranges
//! — lives here so the two features cannot drift.

use lsp_types::{Range, Uri};
use rowan::TextSize;

use crate::incremental::{Analysis, SourceFile};
use crate::resolve::{OccurrenceKey, OccurrenceRec, Resolver};
use crate::text::{LineIndex, PositionEncoding};

use super::uri::from_path;

/// One gathered occurrence of a workspace symbol: the file it lives in (as an
/// LSP [`Uri`]), its range in the negotiated encoding, and whether it is the
/// definition site (so references can honor `includeDeclaration`).
pub(crate) struct CrossFileSite {
    pub uri: Uri,
    pub range: Range,
    pub is_def: bool,
}

/// The workspace top-level symbol at `offset` in the file at `path`, if the
/// cursor is on one — the `(module path, namespace, name)` key that identifies
/// it in the reverse-occurrence index. `None` for a local, a library symbol, or
/// a non-member file — the caller then falls back to its intra-file path.
pub(crate) fn workspace_symbol_at(
    snapshot: &Analysis,
    path: &std::path::Path,
    model: &crate::semantic::SemanticModel,
    offset: TextSize,
) -> Option<OccurrenceKey> {
    let workspace = snapshot.workspace_member(path);
    Resolver::new(model, snapshot)
        .with_workspace(workspace)
        .workspace_symbol_at(offset)
}

/// Every occurrence of the workspace symbol `(namespace, name)` across the
/// package's member files, materialized into per-file ranges. Reads the
/// reverse-occurrence index and converts each byte span through the owning
/// file's tracked text (the buffer if open, else the seeded disk text — kept
/// consistent with the index within one snapshot). Empty when the symbol has no
/// recorded occurrences (e.g. the member set has not been seeded yet), letting
/// the caller fall back to its intra-file path.
pub(crate) fn gather_sites(
    snapshot: &Analysis,
    symbol: &OccurrenceKey,
    encoding: PositionEncoding,
) -> Vec<CrossFileSite> {
    let index = snapshot.workspace_reference_index();
    let Some(bucket) = index.0.get(symbol) else {
        return Vec::new();
    };

    // Group by file so each file's text is line-indexed once (a single file
    // usually holds several occurrences).
    let mut by_file: std::collections::HashMap<SourceFile, Vec<OccurrenceRec>> =
        std::collections::HashMap::new();
    for &(file, rec) in bucket {
        by_file.entry(file).or_default().push(rec);
    }

    let mut out = Vec::new();
    for (file, recs) in by_file {
        let Some(path) = snapshot.file_path_of(file) else {
            continue;
        };
        let Some(uri) = from_path(&path) else {
            continue;
        };
        let line_index = LineIndex::new(snapshot.file_text_of(file));
        for rec in recs {
            out.push(CrossFileSite {
                uri: uri.clone(),
                range: Range {
                    start: line_index.byte_to_position(rec.range.start().into(), encoding),
                    end: line_index.byte_to_position(rec.range.end().into(), encoding),
                },
                is_def: rec.is_def,
            });
        }
    }
    // Deterministic order: by file, then position.
    out.sort_by(|a, b| {
        (a.uri.as_str(), a.range.start.line, a.range.start.character).cmp(&(
            b.uri.as_str(),
            b.range.start.line,
            b.range.start.character,
        ))
    });
    out
}

/// Test scaffolding shared by the references and rename cross-file tests: a
/// hand-built database standing in for a harvested, seeded workspace package.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::incremental::{IncrementalDatabase, SourceFile};
    use crate::index::model::{DefLocation, Span};
    use crate::index::{FunctionGroup, ModuleIndex, PackageIndex};

    /// The absolute path of member file `rel` under the fixture package root.
    /// Normalized so it is drive-absolute on Windows too (a bare `/work/...` is
    /// root-relative there), matching what the database stores after upsert and
    /// keeping the URIs the tests build in sync with the gathered ones.
    pub(crate) fn member_path(rel: &str) -> PathBuf {
        crate::incremental::normalize_path(&PathBuf::from("/work/MyPkg/src").join(rel))
    }

    /// Build a database for a workspace package `MyPkg` (root `/work/MyPkg`) whose
    /// top level defines `functions`, with `files` (relative name, text) tracked
    /// as its seeded member sources. Returns the db and the member handles in the
    /// given order.
    pub(crate) fn workspace_db(
        functions: &[&str],
        files: &[(&str, &str)],
    ) -> (IncrementalDatabase, Vec<SourceFile>) {
        let loc = DefLocation {
            file: "src/MyPkg.jl".into(),
            range: Span { start: 0, end: 0 },
        };
        let root = ModuleIndex {
            name: "MyPkg".to_string(),
            bare: false,
            loc: loc.clone(),
            exports: Vec::new(),
            functions: functions
                .iter()
                .map(|f| FunctionGroup {
                    name: f.to_string(),
                    owner: None,
                    methods: Vec::new(),
                    doc: None,
                })
                .collect(),
            types: Vec::new(),
            consts: Vec::new(),
            macros: Vec::new(),
            submodules: Vec::new(),
        };
        let members: Vec<PathBuf> = files.iter().map(|(rel, _)| member_path(rel)).collect();
        // No include structure: every member's host falls back to the root
        // module through the project graph; nested-module membership tests
        // seed an entry file with real `include`s instead.
        let pkg = PackageIndex {
            name: "MyPkg".to_string(),
            root,
            members,
            member_modules: Default::default(),
            diagnostics: Vec::new(),
        };

        let mut db = IncrementalDatabase::new();
        let handles: Vec<SourceFile> = files
            .iter()
            .map(|(rel, text)| db.upsert_file(&member_path(rel), text.to_string()))
            .collect();

        let mut packages = BTreeMap::new();
        packages.insert("MyPkg".to_string(), Arc::new(pkg));
        let mut roots = BTreeMap::new();
        roots.insert("MyPkg".to_string(), PathBuf::from("/work/MyPkg"));
        db.set_library(packages, roots, vec!["MyPkg".to_string()]);
        db.set_workspace_files(handles.clone());
        (db, handles)
    }
}
