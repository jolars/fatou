//! Background loading for script programs and package includes changed in
//! editor buffers. The analysis thread installs only the current generation.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};

use super::graph_diagnostics::{GraphSource, merge_script_diagnostics, script_graph_diagnostics};
use crate::incremental::{IncrementalDatabase, normalize_path};
use crate::parse_context::{
    ContextCatalog, EditionSource, SourceContext, SourceRole, nearest_project,
    workspace_package_entries,
};
use crate::project::may_contain_static_include;
use crate::project::scripts::{ScriptAnalysis, ScriptProgram, ScriptSources};
use crate::script_loading::load_contexts;
use crate::text::{PositionEncoding, TextBuffer};
use lsp_types::Diagnostic;

struct Document {
    text: Arc<TextBuffer>,
    version: i32,
    entries: Vec<PathBuf>,
    may_override_package_includes: bool,
}

struct Load {
    generation: u64,
    entries: Vec<PathBuf>,
    package_roots: Vec<SourceContext>,
    buffers: BTreeMap<PathBuf, Arc<TextBuffer>>,
    versions: BTreeMap<PathBuf, i32>,
    metadata: BTreeMap<PathBuf, Arc<str>>,
}

#[derive(Default)]
pub(super) struct Loaded {
    generation: u64,
    failed: bool,
    entries: Vec<PathBuf>,
    package_roots: Vec<SourceContext>,
    packages: ContextCatalog,
    sources: ScriptSources,
    contexts: BTreeMap<PathBuf, Vec<SourceContext>>,
    programs: Vec<Arc<ScriptProgram>>,
    diagnostics: BTreeMap<PathBuf, Vec<Diagnostic>>,
    versions: BTreeMap<PathBuf, i32>,
}

pub(super) struct ScriptProjects {
    documents: BTreeMap<PathBuf, Document>,
    metadata: BTreeMap<PathBuf, Arc<str>>,
    entries: Vec<PathBuf>,
    entries_changed: bool,
    package_roots: Vec<SourceContext>,
    package_paths: BTreeSet<PathBuf>,
    generation: u64,
    loading: bool,
    failed: bool,
    sources: BTreeSet<PathBuf>,
    programs: Vec<Arc<ScriptProgram>>,
    errors: BTreeMap<PathBuf, String>,
    diagnostics: BTreeMap<PathBuf, Vec<Diagnostic>>,
    versions: BTreeMap<PathBuf, i32>,
    load: Sender<Load>,
    pub ready: Receiver<Loaded>,
}

impl Default for ScriptProjects {
    fn default() -> Self {
        Self::new(PositionEncoding::Utf16)
    }
}

impl ScriptProjects {
    pub fn new(encoding: PositionEncoding) -> Self {
        let (load, jobs) = unbounded::<Load>();
        let (done, ready) = unbounded();
        std::thread::Builder::new()
            .name("fatou-scripts".into())
            .spawn(move || {
                while let Ok(mut job) = jobs.recv() {
                    while let Ok(newer) = jobs.try_recv() {
                        job = newer;
                    }
                    let generation = job.generation;
                    let finished = super::analysis_thread::guard("script loading", || {
                        let mut buffers: BTreeMap<_, _> = job
                            .buffers
                            .iter()
                            .map(|(path, text)| (path.clone(), text.text_arc()))
                            .collect();
                        buffers.extend(job.metadata);
                        let packages = ContextCatalog::load(&job.package_roots, &buffers, None);
                        let catalog = load_contexts(&job.entries, &buffers, None);
                        let scripts = ScriptAnalysis::from_catalog(&job.entries, &catalog);
                        let ContextCatalog { sources, files, .. } = catalog;
                        let contexts = files
                            .iter()
                            .map(|(path, contexts)| {
                                (
                                    path.clone(),
                                    contexts
                                        .iter()
                                        .map(|(context, _)| context.clone())
                                        .collect(),
                                )
                            })
                            .collect();
                        let mut diagnostics = BTreeMap::<_, Vec<_>>::new();
                        for program in &scripts.programs {
                            let single = ScriptAnalysis {
                                programs: vec![Arc::clone(program)],
                                pending: scripts.pending,
                            };
                            for (path, findings) in
                                script_graph_diagnostics(&single, encoding, |path| {
                                    let text = job.buffers.get(path).cloned().or_else(|| {
                                        let text = sources.get(path)?.as_ref().ok()?;
                                        Some(Arc::new(TextBuffer::new(Arc::clone(text))))
                                    })?;
                                    let tree = crate::parser::parse_with_options(
                                        &text,
                                        program.parse_options,
                                    )
                                    .cst;
                                    Some((text, tree))
                                })
                            {
                                let merged = diagnostics.entry(path).or_default();
                                for finding in findings {
                                    if !merged.contains(&finding) {
                                        merged.push(finding);
                                    }
                                }
                            }
                        }
                        let _ = done.send(Loaded {
                            generation,
                            failed: false,
                            entries: job.entries,
                            package_roots: job.package_roots,
                            packages,
                            sources,
                            contexts,
                            programs: scripts.programs,
                            diagnostics,
                            versions: job.versions,
                        });
                    });
                    if !finished {
                        let _ = done.send(Loaded {
                            generation,
                            failed: true,
                            ..Loaded::default()
                        });
                    }
                }
            })
            .expect("spawn script loader");
        Self {
            documents: BTreeMap::new(),
            metadata: BTreeMap::new(),
            entries: Vec::new(),
            entries_changed: false,
            package_roots: Vec::new(),
            package_paths: BTreeSet::new(),
            generation: 0,
            loading: false,
            failed: false,
            sources: BTreeSet::new(),
            programs: Vec::new(),
            errors: BTreeMap::new(),
            diagnostics: BTreeMap::new(),
            versions: BTreeMap::new(),
            load,
            ready,
        }
    }
}

impl ScriptProjects {
    pub fn update(
        &mut self,
        path: &Path,
        text: Arc<TextBuffer>,
        version: i32,
        entries: &[PathBuf],
        db: &IncrementalDatabase,
    ) {
        let path = normalize_path(path);
        let old = self.documents.get(&path);
        let newly_opened = old.is_none();
        if let Some(old) = old
            && (old.version > version
                || (old.version == version
                    && old.entries == entries
                    && (Arc::ptr_eq(&old.text, &text) || old.text == text)))
        {
            return;
        }
        let before = old
            .map(|doc| doc.text.text())
            .or_else(|| db.lookup_file(&path).map(|file| db.file_text(file)));
        // Every static include needs this spelling. Most body-only updates
        // can reuse the harvested memberships without loading any package.
        let include_changed = before.is_none_or(|before| {
            !std::ptr::eq(before, text.text())
                && before != text.text()
                && (may_contain_static_include(before) || may_contain_static_include(&text))
        });
        let may_override_package_includes =
            include_changed || old.is_some_and(|doc| doc.may_override_package_includes);
        self.documents.insert(
            path,
            Document {
                text,
                version,
                entries: entries.to_vec(),
                may_override_package_includes,
            },
        );
        let entries: Vec<_> = self.configured_entries();
        let script_changed = entries != self.entries;
        if script_changed {
            self.entries = entries;
            self.entries_changed = true;
        }
        let package_changed = (include_changed || newly_opened) && self.refresh_package_roots(db);
        if package_changed || script_changed || !self.entries.is_empty() || !self.sources.is_empty()
        {
            self.metadata = db.metadata_buffers();
            self.reload();
        }
    }

    pub fn metadata_changed(&mut self, db: &IncrementalDatabase) {
        self.refresh_package_roots(db);
        self.reload();
    }

    pub fn library_changed(&mut self, db: &IncrementalDatabase) {
        if self.refresh_package_roots(db) {
            self.reload();
        }
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn load_failed(&self) -> bool {
        self.failed
    }

    pub fn open_paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.documents.keys()
    }

    pub fn closed_or_changed(&mut self, path: &Path, db: &IncrementalDatabase) {
        let path = normalize_path(path);
        let closed = self.documents.remove(&path);
        let package_changed = (closed.is_some() || self.package_paths.contains(&path))
            && self.refresh_package_roots(db);
        let mut script_changed = false;
        if closed.is_some() || self.sources.contains(&path) {
            let entries = self.configured_entries();
            script_changed = entries != self.entries
                || self.failed
                || !self.sources.is_empty()
                || !entries.is_empty();
            self.entries = entries;
            self.entries_changed |= script_changed;
        }
        if package_changed || script_changed {
            self.reload();
        }
    }

    /// Mark the script graph pending during the next analysis write phase.
    /// Opening a document while another analysis runs only queues the load.
    pub fn mark_pending(&mut self, db: &mut IncrementalDatabase) {
        if std::mem::take(&mut self.entries_changed) {
            db.set_scripts_pending();
        }
    }

    fn refresh_package_roots(&mut self, db: &IncrementalDatabase) -> bool {
        self.metadata = db.metadata_buffers();
        let roots = self.live_package_roots(db);
        let reload = !roots.is_empty() || roots != self.package_roots;
        self.package_roots = roots;
        reload
    }

    fn live_package_roots(&self, db: &IncrementalDatabase) -> Vec<SourceContext> {
        let mut seen = HashSet::new();
        let mut roots: Vec<_> = self
            .documents
            .iter()
            .filter(|(_, doc)| doc.may_override_package_includes)
            .flat_map(|(path, _)| db.package_contexts(path))
            .chain(db.changed_unindexed_package_roots())
            .filter(|context| seen.insert((*context).clone()))
            .cloned()
            .collect();

        // Nested packages can supply loading contexts even when the editor
        // opened a parent folder that has no package index of its own.
        let projects: BTreeSet<_> = self
            .documents
            .keys()
            .filter_map(|path| nearest_project(path, &self.metadata))
            .chain(
                self.documents
                    .keys()
                    .flat_map(|path| db.live_package_contexts(path))
                    .filter_map(|context| match &context.source {
                        EditionSource::Project {
                            path,
                            role: SourceRole::Package,
                        } => Some(path.clone()),
                        _ => None,
                    }),
            )
            .collect();
        if !projects.is_empty() {
            let mut buffers = self.buffers();
            buffers.extend(self.metadata.clone());
            for project in projects {
                roots.extend(
                    workspace_package_entries(&project, &buffers)
                        .into_iter()
                        .filter(|root| {
                            // Preserve manifest editions for resolved pins.
                            db.package_contexts(&root.entry)
                                .all(|known| known.entry != root.entry)
                        })
                        .filter(|root| seen.insert(root.clone())),
                );
            }
        }
        roots
    }

    fn configured_entries(&self) -> Vec<PathBuf> {
        let mut seen = BTreeSet::new();
        self.documents
            .values()
            .flat_map(|doc| doc.entries.iter().cloned())
            .filter(|entry| seen.insert(entry.clone()))
            .collect()
    }

    fn buffers(&self) -> BTreeMap<PathBuf, Arc<str>> {
        self.documents
            .iter()
            .map(|(path, doc)| (path.clone(), doc.text.text_arc()))
            .collect()
    }

    fn reload(&mut self) {
        if self.entries.is_empty()
            && self.sources.is_empty()
            && self.package_roots.is_empty()
            && self.package_paths.is_empty()
            && !self.loading
        {
            self.failed = false;
            return;
        }
        self.generation += 1;
        self.loading = true;
        let _ = self.load.send(Load {
            generation: self.generation,
            entries: self.entries.clone(),
            package_roots: self.package_roots.clone(),
            buffers: self
                .documents
                .iter()
                .map(|(path, doc)| (path.clone(), Arc::clone(&doc.text)))
                .collect(),
            metadata: self.metadata.clone(),
            versions: self
                .documents
                .iter()
                .map(|(path, doc)| (path.clone(), doc.version))
                .collect(),
        });
    }

    /// Returns whether dependent diagnostics need refreshing. Edits that keep
    /// global definitions and reads equal do not refresh other documents.
    /// The caller must wait for in-flight analysis to finish: even an unchanged
    /// program can write source text and cancel Salsa readers.
    pub fn install(&mut self, loaded: Loaded, db: &mut IncrementalDatabase) -> bool {
        if loaded.generation != self.generation {
            return false;
        }
        self.loading = false;
        self.failed = loaded.failed;
        if loaded.failed {
            return false;
        }
        self.entries_changed = false;
        let errors: BTreeMap<_, _> = loaded
            .entries
            .iter()
            .filter_map(|entry| {
                loaded
                    .sources
                    .get(entry)
                    .and_then(|source| source.as_ref().err())
                    .map(|error| (entry.clone(), error.clone()))
            })
            .collect();
        for (entry, error) in &errors {
            if self.errors.get(entry) != Some(error) {
                log::warn!(
                    "cannot read script entry point {}: {error}",
                    entry.display()
                );
            }
        }
        self.errors = errors;
        let buffers = self.buffers();
        self.package_paths = loaded.packages.sources.keys().cloned().collect();
        let package_changed =
            db.set_live_package_contexts(loaded.package_roots, loaded.packages, &buffers);
        if package_changed {
            db.seed_workspace_members();
        }
        let was_pending = db.scripts_pending();
        let memberships_changed = db.replace_script_contexts(&self.sources, loaded.contexts);
        db.set_script_sources(&loaded.entries, &loaded.sources, &buffers);
        let changed = package_changed
            || was_pending
            || memberships_changed
            || self.programs != loaded.programs
            || self.diagnostics != loaded.diagnostics
            || loaded
                .diagnostics
                .keys()
                .any(|path| self.versions.get(path) != loaded.versions.get(path));
        self.sources = loaded.sources.into_keys().collect();
        self.programs = loaded.programs;
        self.diagnostics = loaded.diagnostics;
        self.versions = loaded.versions;
        changed
    }

    pub fn graph_diagnostics(
        &self,
        package: BTreeMap<PathBuf, Vec<Diagnostic>>,
    ) -> BTreeMap<PathBuf, Vec<Diagnostic>> {
        merge_script_diagnostics(package, &self.diagnostics, &self.sources)
    }

    pub fn diagnostic_source(&self, path: &Path) -> GraphSource {
        if self.sources.contains(path) {
            GraphSource::Script {
                version: self.versions.get(path).copied(),
            }
        } else {
            GraphSource::Package
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn include_diagnostics_refresh_for_new_spans_and_buffer_versions() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let entries = [entry.clone()];
        let mut db = IncrementalDatabase::default();
        let mut scripts = ScriptProjects::default();
        for (version, text, line) in [
            (1, "include(\"\")\nf() = 1\n", 0),
            (2, "\ninclude(\"\")\nf() = 1\n", 1),
            (3, "\ninclude(\"\")\nf() = 123\n", 1),
        ] {
            scripts.update(
                &entry,
                Arc::new(TextBuffer::new(text)),
                version,
                &entries,
                &db,
            );
            let loaded = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(scripts.install(loaded, &mut db));
            assert_eq!(scripts.diagnostics[&entry].len(), 1);
            assert_eq!(scripts.diagnostics[&entry][0].range.start.line, line);
            assert_eq!(
                scripts.diagnostic_source(&entry),
                GraphSource::Script {
                    version: Some(version)
                }
            );
        }
    }

    #[test]
    fn closing_the_last_document_rejects_an_inflight_load() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let entries = vec![entry.clone()];
        let mut db = IncrementalDatabase::default();
        let mut scripts = ScriptProjects::default();
        scripts.update(
            &entry,
            Arc::new(TextBuffer::new("x = 1\n")),
            1,
            &entries,
            &db,
        );
        let stale = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        scripts.closed_or_changed(&entry, &db);
        assert!(!scripts.install(stale, &mut db));
        let cleared = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        scripts.install(cleared, &mut db);
        assert!(scripts.sources.is_empty());
        assert!(scripts.programs.is_empty());
    }

    #[test]
    fn superseded_loads_cannot_overwrite_live_text_and_body_edits_do_not_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let entries = vec![entry.clone()];
        let mut db = IncrementalDatabase::default();
        let mut scripts = ScriptProjects::default();
        scripts.update(
            &entry,
            Arc::new(TextBuffer::new("old(x) = x\n")),
            1,
            &entries,
            &db,
        );
        let stale = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        scripts.update(
            &entry,
            Arc::new(TextBuffer::new("fresh(x) = x + 1\n")),
            2,
            &entries,
            &db,
        );
        assert!(!scripts.install(stale, &mut db));
        let current = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(scripts.install(current, &mut db));
        assert_eq!(
            db.snapshot().file_text_of(db.lookup_file(&entry).unwrap()),
            "fresh(x) = x + 1\n"
        );
        scripts.update(
            &entry,
            Arc::new(TextBuffer::new("fresh(x) = x + 123\n")),
            3,
            &entries,
            &db,
        );
        let body_edit = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!scripts.install(body_edit, &mut db));
        assert_eq!(
            db.snapshot().file_text_of(db.lookup_file(&entry).unwrap()),
            "fresh(x) = x + 123\n"
        );
    }
}
