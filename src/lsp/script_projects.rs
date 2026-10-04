//! Background loading for explicitly configured script programs. The analysis
//! thread owns this state and installs results only for the current generation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};

use super::graph_diagnostics::{GraphSource, merge_script_diagnostics, script_graph_diagnostics};
use crate::incremental::{IncrementalDatabase, normalize_path};
use crate::project::scripts::{ScriptAnalysis, ScriptProgram, ScriptSources};
use crate::script_loading::load_sources;
use crate::text::{PositionEncoding, TextBuffer};
use lsp_types::Diagnostic;

struct Document {
    text: Arc<TextBuffer>,
    version: i32,
    entries: Vec<PathBuf>,
}

struct Load {
    generation: u64,
    entries: Vec<PathBuf>,
    buffers: BTreeMap<PathBuf, Arc<TextBuffer>>,
    versions: BTreeMap<PathBuf, i32>,
}

pub(super) struct Loaded {
    generation: u64,
    entries: Vec<PathBuf>,
    sources: ScriptSources,
    programs: Vec<Arc<ScriptProgram>>,
    diagnostics: BTreeMap<PathBuf, Vec<Diagnostic>>,
    versions: BTreeMap<PathBuf, i32>,
}

pub(super) struct ScriptProjects {
    documents: BTreeMap<PathBuf, Document>,
    entries: Vec<PathBuf>,
    generation: u64,
    loading: bool,
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
                    super::analysis_thread::guard("script loading", || {
                        let buffers = job
                            .buffers
                            .iter()
                            .map(|(path, text)| (path.clone(), text.text_arc()))
                            .collect();
                        let sources = load_sources(&job.entries, &buffers);
                        let scripts = ScriptAnalysis::from_sources(&job.entries, &sources);
                        let diagnostics = script_graph_diagnostics(&scripts, encoding, |path| {
                            let text = job.buffers.get(path).cloned().or_else(|| {
                                sources
                                    .get(path)?
                                    .as_ref()
                                    .ok()
                                    .map(|text| Arc::new(TextBuffer::new(Arc::clone(text))))
                            })?;
                            let tree = crate::parser::parse(&text).cst;
                            Some((text, tree))
                        });
                        let _ = done.send(Loaded {
                            generation: job.generation,
                            entries: job.entries,
                            sources,
                            programs: scripts.programs,
                            diagnostics,
                            versions: job.versions,
                        });
                    });
                }
            })
            .expect("spawn script loader");
        Self {
            documents: BTreeMap::new(),
            entries: Vec::new(),
            generation: 0,
            loading: false,
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
        db: &mut IncrementalDatabase,
    ) {
        let path = normalize_path(path);
        if let Some(old) = self.documents.get(&path)
            && (old.version > version
                || (old.version == version
                    && old.entries == entries
                    && (Arc::ptr_eq(&old.text, &text) || old.text == text)))
        {
            return;
        }
        self.documents.insert(
            path,
            Document {
                text,
                version,
                entries: entries.to_vec(),
            },
        );
        let entries: Vec<_> = self
            .documents
            .values()
            .flat_map(|doc| doc.entries.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if entries != self.entries {
            self.entries = entries;
            db.set_scripts_pending();
        }
        self.reload();
    }

    pub fn closed_or_changed(&mut self, path: &Path, db: &mut IncrementalDatabase) {
        let path = normalize_path(path);
        let closed = self.documents.remove(&path).is_some();
        if closed || self.sources.contains(&path) {
            self.entries = self
                .documents
                .values()
                .flat_map(|doc| doc.entries.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if self.loading || !self.sources.is_empty() || !self.entries.is_empty() {
                db.set_scripts_pending();
                self.reload();
            }
        }
    }

    fn buffers(&self) -> BTreeMap<PathBuf, Arc<str>> {
        self.documents
            .iter()
            .map(|(path, doc)| (path.clone(), doc.text.text_arc()))
            .collect()
    }

    fn reload(&mut self) {
        if self.entries.is_empty() && self.sources.is_empty() && !self.loading {
            return;
        }
        self.generation += 1;
        self.loading = true;
        let _ = self.load.send(Load {
            generation: self.generation,
            entries: self.entries.clone(),
            buffers: self
                .documents
                .iter()
                .map(|(path, doc)| (path.clone(), Arc::clone(&doc.text)))
                .collect(),
            versions: self
                .documents
                .iter()
                .map(|(path, doc)| (path.clone(), doc.version))
                .collect(),
        });
    }

    /// Returns whether dependent diagnostics need refreshing. Body-only edits
    /// leave the range-free programs equal and do not refresh other documents.
    /// The caller must wait for in-flight analysis to finish: even an unchanged
    /// program can write source text and cancel Salsa readers.
    pub fn install(&mut self, loaded: Loaded, db: &mut IncrementalDatabase) -> bool {
        if loaded.generation != self.generation {
            return false;
        }
        self.loading = false;
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
        let was_pending = db.scripts_pending();
        db.set_script_sources(&loaded.entries, &loaded.sources, &self.buffers());
        let changed = was_pending
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
                &mut db,
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
            &mut db,
        );
        let stale = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        scripts.closed_or_changed(&entry, &mut db);
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
            &mut db,
        );
        let stale = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        scripts.update(
            &entry,
            Arc::new(TextBuffer::new("fresh(x) = x\n")),
            2,
            &entries,
            &mut db,
        );
        assert!(!scripts.install(stale, &mut db));
        let current = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(scripts.install(current, &mut db));
        assert_eq!(
            db.snapshot().file_text_of(db.lookup_file(&entry).unwrap()),
            "fresh(x) = x\n"
        );
        scripts.update(
            &entry,
            Arc::new(TextBuffer::new("fresh(x) = x + 123\n")),
            3,
            &entries,
            &mut db,
        );
        let body_edit = scripts.ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!scripts.install(body_edit, &mut db));
        assert_eq!(
            db.snapshot().file_text_of(db.lookup_file(&entry).unwrap()),
            "fresh(x) = x + 123\n"
        );
    }
}
