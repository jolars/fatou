//! The dedicated analysis thread: sole owner (and sole *writer*) of the
//! persistent salsa database.
//!
//! Each analysis splits into a cheap write-phase (`&mut db`, on this thread:
//! upsert the live buffer) and a read-phase (`&db` only: the parse query plus
//! diagnostic conversion) that runs on the read pool holding a short-lived db
//! clone, so the thread returns to its `select!` immediately. Requests are
//! coalesced (latest version per URI) and scheduled by [`decide`]: at most one
//! analysis in flight, the most-recently-edited URI preferred when several are
//! pending, canceled only when superseded by a strictly-newer edit of the
//! *same* URI.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, select};
use lsp_types::{Diagnostic, Uri};
use salsa::Database as _;

use crate::environment::is_project_file;
use crate::incremental::{IncrementalDatabase, IncrementalDb};
use crate::index::{DeclaredDeps, HarvestedLibrary, PackageIndex};
use crate::parser::{Edit, ParseOptions};
use crate::text::{PositionEncoding, TextBuffer};

use super::graph_diagnostics::{GraphSource, graph_diagnostics};
use super::lint::ServerRules;
use super::read_jobs::{ReadJob, run_read};
use super::rename_files::renamed_package_projects;
use super::script_projects::{Loaded, ScriptProjects};
use super::server::HarvestSignal;
use super::state::Outbound;
use super::task_pool::Spawner;

/// Run `f` on the analysis thread, catching any panic so a single malformed
/// request can't take down the sole salsa-db writer and, with it, the whole
/// server. This mirrors the read pool's per-job `catch_unwind`
/// (`task_pool::TaskPool`); the analysis thread and main loop were the two
/// places a panic still meant process death. Returns `true` if `f` ran to
/// completion, `false` if it panicked (logged). The db's `source_map` mutex
/// recovers from poisoning (see `IncrementalDatabase`), so a panic mid-write
/// leaves the db usable for the next request rather than bricked. Also used by
/// the main loop (`server::main_loop`) to isolate request handlers.
pub(crate) fn guard(label: &str, f: impl FnOnce()) -> bool {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(()) => true,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&'static str>()
                .copied()
                .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("<non-string panic payload>");
            log::error!("analysis thread caught panic in {label}: {msg}");
            false
        }
    }
}

/// An analysis request handed to the dedicated analysis thread: refresh the
/// diagnostics for `uri`'s live buffer at `version`.
pub(crate) struct AnalysisRequest {
    pub(crate) context_epoch: u64,
    pub(crate) uri: Uri,
    pub(crate) path: PathBuf,
    pub(crate) text: Arc<TextBuffer>,
    pub(crate) version: i32,
    /// The lint rules the main loop resolved for this document (discovered
    /// `fatou.toml` shadowing editor-pushed settings), current at dispatch.
    pub(crate) rules: Arc<ServerRules>,
    /// The byte edits that produced `text` from the previously sent one, for
    /// the incremental reparse to replay; `None` when the transform is unknown
    /// (a `didOpen`, a whole-buffer replacement, a re-analysis for new rules).
    pub(crate) edits: Option<Vec<Edit>>,
}

/// Ordered writes to the sole Salsa owner. A close follows every analysis
/// request for the discarded buffer.
pub(crate) enum SyncMessage {
    Analyze(AnalysisRequest),
    SourceChanged(PathBuf),
    RefreshLibrary,
    /// Revert this path's tracked input to on-disk text: a document closed (its
    /// discarded buffer must not linger in the reverse-occurrence index) or a
    /// watched file changed outside any open buffer.
    Revert(PathBuf),
    /// Track this text for `path`: an open project-file buffer, which is never
    /// dispatched as an analysis (it is not Julia) but whose `[deps]` the
    /// linter reads through `project_declared_deps`.
    SetText {
        path: PathBuf,
        text: Arc<str>,
        context_epoch: u64,
        edition_changed: bool,
    },
    MetadataRevert {
        path: PathBuf,
        context_epoch: u64,
    },
    /// Refresh project files edited by an entry rename, then request a full
    /// resolve. The client may have no file watchers to report those TOML edits.
    RenamedProjects {
        files: Vec<lsp_types::FileRename>,
        open_projects: Vec<PathBuf>,
    },
}

/// A full resolve or a batch of variants harvested under live editions.
/// Generations keep a superseded harvest from restoring stale memberships.
pub(crate) enum LibraryMessage {
    Full {
        generation: u64,
        library: Box<HarvestedLibrary>,
        failed_manifests: BTreeSet<PathBuf>,
    },
    Syntax {
        generation: u64,
        variants: Vec<(String, ParseOptions, Arc<PackageIndex>)>,
    },
}

/// Spawn the dedicated analysis thread that owns the persistent salsa database.
/// `library_rx` delivers the harvested package index once the background loader
/// has resolved the environment (and later single-package re-harvests); the
/// thread swaps it into the db as a write.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_analysis_thread(
    read_rx: Receiver<ReadJob>,
    library_rx: Receiver<LibraryMessage>,
    sync_rx: Receiver<SyncMessage>,
    out_tx: Sender<Outbound>,
    read_spawner: Spawner,
    encoding: PositionEncoding,
    push_diagnostics: bool,
    harvest: Sender<HarvestSignal>,
    has_workspace: bool,
    membership_generation: Arc<AtomicU64>,
) -> JoinHandle<()> {
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<AnalyzeDone>();
    std::thread::Builder::new()
        .name("fatou-analysis".to_string())
        .spawn(move || {
            let mut worker = AnalysisWorker {
                context_epoch: 0,
                harvest,
                full_harvest: has_workspace.then_some(0),
                syntax_harvest: None,
                harvest_generation: 0,
                harvest_failed: false,
                changed_sources: BTreeSet::new(),
                syntax_dirty: false,
                membership_generation,
                deferred_edits: Vec::new(),
                scripts: ScriptProjects::new(encoding),
                pending_script_load: None,
                db: IncrementalDatabase::default(),
                out_tx,
                done_tx,
                inflight: None,
                pending: HashMap::new(),
                active: None,
                read_spawner,
                encoding,
                push_diagnostics,
                published_graph_files: HashSet::new(),
                package_graph_diags: BTreeMap::new(),
                import_projections: HashMap::new(),
            };
            worker.run(&read_rx, library_rx, &sync_rx, &done_rx);
        })
        .expect("spawn analysis thread")
}

/// Signal from a finished read-phase ([`AnalysisWorker::start`]) back to the
/// analysis thread: the analysis for `uri`@`version` has completed (or unwound
/// on cancellation) and dropped its db clone, so the in-flight slot is free.
struct AnalyzeDone {
    context_epoch: u64,
    uri: Uri,
    version: i32,
    import_projection: Option<(PathBuf, Arc<crate::project::scripts::ScriptFile>)>,
}

/// The single in-flight read-phase analysis, if any.
struct InflightAnalyze {
    uri: Uri,
    version: i32,
}

/// What [`AnalysisWorker::try_dispatch`] should do given the in-flight analysis
/// and the pending queue. Pure decision (see [`decide`]) so it can be
/// unit-tested.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DispatchAction {
    /// Idle with nothing queued, or busy with no newer edit for the in-flight
    /// URI: leave the in-flight analysis running and wait for its `done`.
    Wait,
    /// The slot is free; start a fresh analysis for this URI.
    Start(Uri),
    /// A strictly-newer edit for the *in-flight* URI arrived; cancel the
    /// running analysis and start this URI. Only ever the in-flight URI — a
    /// different pending URI must never cancel the in-flight one (it would
    /// silently drop that file's diagnostics).
    SupersedeAndStart(Uri),
}

/// Decide the next dispatch action. `inflight` is the running analysis's
/// `(uri, version)`, if any; `pending` maps each queued URI to its latest
/// version; `active` is the most-recently-edited URI, preferred when several
/// URIs are pending (a bulk dirty) so the focused document is analyzed first.
/// Cancel only on a strictly-newer edit of the *same* URI.
pub(crate) fn decide(
    inflight: Option<(&Uri, i32)>,
    pending: &HashMap<Uri, i32>,
    active: Option<&Uri>,
) -> DispatchAction {
    match inflight {
        None => match active
            .filter(|uri| pending.contains_key(*uri))
            .or_else(|| pending.keys().next())
        {
            Some(uri) => DispatchAction::Start(uri.clone()),
            None => DispatchAction::Wait,
        },
        Some((uri, version)) => {
            if pending.get(uri).is_some_and(|&v| v > version) {
                DispatchAction::SupersedeAndStart(uri.clone())
            } else {
                DispatchAction::Wait
            }
        }
    }
}

struct AnalysisWorker {
    harvest: Sender<HarvestSignal>,
    /// Keep rewrites queued until both the disk resolve and any follow-up
    /// variants needed by live metadata have landed.
    full_harvest: Option<u64>,
    syntax_harvest: Option<u64>,
    harvest_generation: u64,
    harvest_failed: bool,
    changed_sources: BTreeSet<PathBuf>,
    syntax_dirty: bool,
    /// A rewrite reply must still describe the memberships used by its read.
    membership_generation: Arc<AtomicU64>,
    deferred_edits: Vec<ReadJob>,
    context_epoch: u64,
    scripts: ScriptProjects,
    /// Installing even a body-only load can write tracked text and cancel
    /// Salsa readers, so keep the latest load until the analysis slot is free.
    pending_script_load: Option<Loaded>,
    db: IncrementalDatabase,
    out_tx: Sender<Outbound>,
    /// Read-phase workers signal completion here so the analysis thread can
    /// free the in-flight slot and dispatch the next pending analysis.
    done_tx: Sender<AnalyzeDone>,
    /// The single in-flight read-phase analysis, if any. At most one runs at a
    /// time: the write-phase needs exclusive `&mut db`, and salsa cancellation
    /// is global, so a second concurrent analysis couldn't be canceled
    /// selectively.
    inflight: Option<InflightAnalyze>,
    /// Coalesced queue: the latest pending request per URI.
    pending: HashMap<Uri, AnalysisRequest>,
    /// The most-recently-edited URI (requests come only from didOpen and
    /// didChange, so the last-received request tracks the focused document).
    /// Preferred by [`decide`] when a bulk dirty queues several URIs at once.
    active: Option<Uri>,
    /// Submit-side handle onto the read pool, shared with the main loop. Used
    /// for read jobs (formatting) and the analysis read-phase.
    read_spawner: Spawner,
    /// The position encoding negotiated at initialize, fixed for the session.
    encoding: PositionEncoding,
    /// Whether the per-edit read-phase publishes diagnostics. Off for a
    /// pull-model client: the write-phase still keeps the db current (and the
    /// parse it warms serves the next pull), but computing and publishing the
    /// diagnostics here would double the client's own pulls.
    push_diagnostics: bool,
    /// The URIs that carried include-graph diagnostics at the last re-harvest, so
    /// a file whose problems are fixed gets an explicit empty publish to clear it.
    published_graph_files: HashSet<Uri>,
    package_graph_diags: BTreeMap<PathBuf, Vec<Diagnostic>>,
    /// Range-free definitions and uses last analyzed for each package buffer.
    import_projections: HashMap<PathBuf, Arc<crate::project::scripts::ScriptFile>>,
}

impl AnalysisWorker {
    fn run(
        &mut self,
        read_rx: &Receiver<ReadJob>,
        mut library_rx: Receiver<LibraryMessage>,
        sync_rx: &Receiver<SyncMessage>,
        done_rx: &Receiver<AnalyzeDone>,
    ) {
        loop {
            select! {
                recv(self.scripts.ready) -> loaded => {
                    if let Ok(loaded) = loaded {
                        guard("script install", || {
                            self.drain_sync(sync_rx);
                            self.on_scripts_loaded(loaded);
                        });
                    }
                }
                recv(sync_rx) -> msg => {
                    let Ok(msg) = msg else { break };
                    guard("sync", || {
                        self.on_sync_message(msg);
                        self.drain_sync(sync_rx);
                    });
                }
                recv(library_rx) -> msg => {
                    guard("library", || {
                        self.drain_sync(sync_rx);
                        match msg {
                            Ok(msg) => self.on_library_message(msg),
                            Err(_) => {
                                library_rx = crossbeam_channel::never();
                                self.harvest_failed |= self.full_harvest.take().is_some()
                                    || self.syntax_harvest.take().is_some();
                            }
                        }
                    });
                }
                recv(done_rx) -> done => {
                    if let Ok(done) = done {
                        guard("done", || {
                            self.drain_sync(sync_rx);
                            self.on_done(done);
                        });
                    }
                }
                recv(read_rx) -> job => {
                    let Ok(job) = job else { break };
                    guard("read-dispatch", || {
                        self.drain_sync(sync_rx);
                        if job.produces_edits() {
                            self.deferred_edits.push(job);
                        } else {
                            self.try_dispatch();
                            self.dispatch_read(job);
                        }
                    });
                }
            }
            guard("dispatch", || {
                self.schedule_syntax_harvest();
                self.try_dispatch();
                self.dispatch_ready_edits();
            });
        }
    }

    fn drain_sync(&mut self, sync_rx: &Receiver<SyncMessage>) {
        while let Ok(msg) = sync_rx.try_recv() {
            self.on_sync_message(msg);
        }
    }

    fn dispatch_ready_edits(&mut self) {
        if self.full_harvest.is_some() || self.syntax_harvest.is_some() || self.scripts.loading() {
            return;
        }
        for job in std::mem::take(&mut self.deferred_edits) {
            // A workspace edit must see the current text of its other files.
            // Document-only formatting can use the text captured by the client.
            if !self.pending.is_empty()
                && matches!(
                    job,
                    ReadJob::Rename { .. } | ReadJob::WillRenameFiles { .. }
                )
            {
                self.deferred_edits.push(job);
                continue;
            }
            if self.harvest_failed || self.scripts.load_failed() {
                let (id, reply) = job.into_reply_parts();
                use lsp_server::{ErrorCode, Message, Response};
                let _ = reply.send(Message::Response(Response::new_err(
                    id,
                    ErrorCode::RequestFailed as i32,
                    "source loading contexts could not be determined".into(),
                )));
            } else {
                self.dispatch_read(job);
            }
        }
    }

    fn dispatch_read(&self, job: ReadJob) {
        let job =
            job.with_membership_generation(self.membership_generation.load(Ordering::Acquire));
        let snapshot = self.db.snapshot();
        let contexts = job.contexts(&snapshot);
        let encoding = self.encoding;
        self.read_spawner
            .spawn(move || run_read(snapshot, job, encoding, contexts));
    }

    fn on_sync_message(&mut self, msg: SyncMessage) {
        match msg {
            SyncMessage::Analyze(req) => self.enqueue(req),
            SyncMessage::SourceChanged(path) => {
                self.changed_sources
                    .insert(crate::incremental::normalize_path(&path));
                self.syntax_dirty = true;
            }
            SyncMessage::RefreshLibrary => self.request_full_harvest(),
            SyncMessage::Revert(path) => self.on_sync(&path),
            SyncMessage::SetText {
                path,
                text,
                context_epoch,
                edition_changed,
            } => {
                self.context_epoch = self.context_epoch.max(context_epoch);
                self.on_project_text(&path, text);
                if edition_changed {
                    self.on_edition_changed();
                }
            }
            SyncMessage::MetadataRevert {
                path,
                context_epoch,
            } => {
                let edition_changed = context_epoch > self.context_epoch;
                self.context_epoch = self.context_epoch.max(context_epoch);
                self.on_sync(&path);
                if edition_changed {
                    self.on_edition_changed();
                }
            }
            SyncMessage::RenamedProjects {
                files,
                open_projects,
            } => {
                let projects = renamed_package_projects(&self.db.snapshot(), &files);
                for path in &projects {
                    if !open_projects.contains(path) {
                        self.on_sync(path);
                    }
                }
                if !projects.is_empty() {
                    self.request_full_harvest();
                }
            }
        }
    }

    fn request_full_harvest(&mut self) {
        self.harvest_generation += 1;
        let generation = self.harvest_generation;
        if self
            .harvest
            .send(HarvestSignal::Environment(generation))
            .is_ok()
        {
            self.full_harvest = Some(generation);
            self.syntax_harvest = None;
            self.invalidate_edit_replies();
        }
    }

    fn schedule_syntax_harvest(&mut self) {
        if self.full_harvest.is_some() || !self.syntax_dirty {
            return;
        }
        self.syntax_dirty = false;
        self.db
            .invalidate_workspace_variants(&std::mem::take(&mut self.changed_sources));
        let work = self.db.pending_syntax_harvests();
        if work.is_empty() {
            self.syntax_harvest = None;
            if self.db.seed_workspace_members() {
                self.refresh_graph_diagnostics();
            }
            return;
        }
        self.harvest_generation += 1;
        let generation = self.harvest_generation;
        if self
            .harvest
            .send(HarvestSignal::Syntax { work, generation })
            .is_ok()
        {
            self.syntax_harvest = Some(generation);
            self.invalidate_edit_replies();
        } else {
            self.harvest_failed = true;
        }
    }

    fn on_library_message(&mut self, msg: LibraryMessage) {
        match msg {
            LibraryMessage::Full {
                generation,
                mut library,
                failed_manifests,
            } if self.full_harvest == Some(generation) => {
                let before = self.open_parse_options();
                self.full_harvest = None;
                self.db
                    .retain_failed_manifest_memberships(&mut library.syntax, &failed_manifests);
                self.db.set_library_syntax(library.syntax);
                self.db.set_project_files(library.project_files);
                self.db.set_library_deps(library.deps);
                self.db
                    .set_library(library.packages, library.roots, library.workspaces);
                self.db.seed_workspace_members();
                self.scripts.library_changed(&self.db);
                self.refresh_graph_diagnostics();
                let _ = self.out_tx.send(Outbound::InlayHintsRefresh);
                self.syntax_dirty = true;
                self.invalidate_edit_replies();
                self.refresh_tokens_if_edition_changed(before);
            }
            LibraryMessage::Syntax {
                generation,
                variants,
            } if self.syntax_harvest == Some(generation) => {
                let before = self.open_parse_options();
                self.syntax_harvest = None;
                self.db.install_syntax_variants(variants);
                self.db.seed_workspace_members();
                self.scripts.library_changed(&self.db);
                self.refresh_graph_diagnostics();
                self.invalidate_edit_replies();
                self.refresh_tokens_if_edition_changed(before);
            }
            _ => {}
        }
    }

    fn invalidate_edit_replies(&self) {
        self.membership_generation.fetch_add(1, Ordering::Release);
    }

    fn open_parse_options(&self) -> Vec<ParseOptions> {
        self.scripts
            .open_paths()
            .map(|path| {
                self.db
                    .lookup_file(path)
                    .map(|file| self.db.parse_options(file))
                    .unwrap_or_default()
            })
            .collect()
    }

    fn refresh_tokens_if_edition_changed(&self, before: Vec<ParseOptions>) {
        if self.open_parse_options() != before {
            let _ = self.out_tx.send(Outbound::SemanticTokensRefresh);
        }
    }

    fn on_edition_changed(&mut self) {
        self.db.rediscover_source_contexts();
        self.scripts.metadata_changed(&self.db);
        self.syntax_dirty = true;
        self.invalidate_edit_replies();
        let _ = self.out_tx.send(Outbound::DiagnosticsRefresh);
        let _ = self.out_tx.send(Outbound::SemanticTokensRefresh);
    }

    fn on_scripts_loaded(&mut self, loaded: Loaded) {
        if self.inflight.is_some() {
            self.pending_script_load = Some(loaded);
            return;
        }
        let before = self.open_parse_options();
        if self.scripts.install(loaded, &mut self.db) {
            self.invalidate_edit_replies();
            self.publish_graph_diagnostics();
        }
        self.refresh_tokens_if_edition_changed(before);
    }

    fn on_done(&mut self, done: AnalyzeDone) {
        if !matches!(&self.inflight, Some(f) if f.uri == done.uri && f.version == done.version) {
            return;
        }
        self.inflight = None;
        if done.context_epoch == self.context_epoch
            && let Some((path, projection)) = done.import_projection
        {
            let previous = self
                .import_projections
                .insert(path, Arc::clone(&projection));
            if previous.as_ref() != Some(&projection) {
                let _ = self.out_tx.send(Outbound::DiagnosticsRefresh);
            }
        }
    }

    /// Earlier analyses and the revert arrive on the same channel. Discard
    /// pending text for the closed buffer before restoring the disk input.
    fn on_sync(&mut self, path: &Path) {
        let dropped: Vec<Uri> = self
            .pending
            .iter()
            .filter(|(_, req)| req.path == path)
            .map(|(uri, _)| uri.clone())
            .collect();
        for uri in &dropped {
            self.pending.remove(uri);
        }
        // A closed document is no longer the focus to schedule ahead of others.
        if self
            .active
            .as_ref()
            .is_some_and(|uri| dropped.contains(uri))
        {
            self.active = None;
        }
        let declared_before = self.declared_deps_of(path);
        self.db.revert_file_to_disk(path);
        self.scripts.closed_or_changed(path, &self.db);
        if self.scripts.loading() {
            self.invalidate_edit_replies();
        }
        if self.import_projections.remove(path).is_some() {
            let _ = self.out_tx.send(Outbound::DiagnosticsRefresh);
        }
        // Closing an unsaved `Project.toml` puts the disk copy back in charge,
        // which can change what the package declares.
        self.refresh_if_declared_deps_changed(path, declared_before);
    }

    /// Track an open project file's buffer text. The buffer is authoritative
    /// over the disk copy from here on, so an unsaved `[deps]` edit reaches
    /// `unresolved-import` across the package without a save — the harvester's
    /// own re-resolve reads disk, but never clobbers a tracked input.
    fn on_project_text(&mut self, path: &Path, text: Arc<str>) {
        let declared_before = self.declared_deps_of(path);
        self.db.upsert_file(path, text);
        self.refresh_if_declared_deps_changed(path, declared_before);
    }

    /// The declared dependencies `path` contributes, or `None` when it is not a
    /// project file at all — so a `.jl` sync never pays for a TOML parse — and
    /// likewise `None` for one no workspace package claims, whose `[deps]` no
    /// consumer reads (see
    /// [`declared_deps_of_file`](IncrementalDatabase::declared_deps_of_file)).
    fn declared_deps_of(&self, path: &Path) -> Option<Arc<DeclaredDeps>> {
        is_project_file(path)
            .then(|| self.db.declared_deps_of_file(path))
            .flatten()
    }

    /// Nudge the open documents when a write to `path` changed what its package
    /// declares. Gated on the declared set rather than on the text: typing in
    /// `[compat]`, or anywhere else in the file, must cost nothing beyond the
    /// one memoized TOML parse.
    ///
    /// Emitted from this thread *after* the write, so a pull client's re-pull
    /// cannot be served against the older revision — the ordering
    /// [`refresh_graph_diagnostics`](Self::refresh_graph_diagnostics) already
    /// relies on.
    fn refresh_if_declared_deps_changed(&self, path: &Path, before: Option<Arc<DeclaredDeps>>) {
        if self.declared_deps_of(path) != before {
            let _ = self.out_tx.send(Outbound::DiagnosticsRefresh);
        }
    }

    /// Add `req` to the pending queue, keeping the highest version per URI
    /// (guards against an out-of-order lower version clobbering a newer one).
    ///
    /// Coalescing drops the superseded request wholesale, so its edits are
    /// carried onto the survivor rather than lost: the chain the db eventually
    /// sees has to describe the transform from the last *analyzed* text, not
    /// from the last enqueued one.
    fn enqueue(&mut self, mut req: AnalysisRequest) {
        self.db.ensure_source_context(&req.path);
        self.scripts.update(
            &req.path,
            Arc::clone(&req.text),
            req.version,
            &req.rules.entry_points,
            &self.db,
        );
        if self.scripts.loading() {
            self.invalidate_edit_replies();
        }
        // Even a stale-version duplicate signals recent activity on this URI.
        self.active = Some(req.uri.clone());
        match self.pending.get_mut(&req.uri) {
            Some(existing) if existing.version >= req.version => {
                if existing.version == req.version {
                    existing.context_epoch = existing.context_epoch.max(req.context_epoch);
                    existing.rules = Arc::clone(&req.rules);
                }
                // Dropping a request whose text differs loses the transform to
                // it and back, so the chain is no longer a description of how
                // the pending text was reached. Same text (an `on_config_changed`
                // re-analysis at the same version, new rules) changes nothing.
                if existing.text != req.text {
                    existing.edits = None;
                }
            }
            Some(existing) => {
                req.edits = match (existing.edits.take(), req.edits.take()) {
                    (Some(mut kept), Some(fresh)) => {
                        kept.extend(fresh);
                        Some(kept)
                    }
                    _ => None,
                };
                self.pending.insert(req.uri.clone(), req);
            }
            None => {
                self.pending.insert(req.uri.clone(), req);
            }
        }
    }

    /// Start the next analysis if the slot allows it (see [`decide`]). Cancels
    /// the in-flight analysis only when superseded by a newer edit of the
    /// *same* URI.
    fn try_dispatch(&mut self) {
        if self.inflight.is_none()
            && let Some(loaded) = self.pending_script_load.take()
        {
            self.on_scripts_loaded(loaded);
        }
        let versions: HashMap<Uri, i32> = self
            .pending
            .iter()
            .map(|(uri, req)| (uri.clone(), req.version))
            .collect();
        let inflight = self.inflight.as_ref().map(|f| (&f.uri, f.version));
        let uri = match decide(inflight, &versions, self.active.as_ref()) {
            DispatchAction::Wait => return,
            DispatchAction::Start(uri) => uri,
            DispatchAction::SupersedeAndStart(uri) => {
                // Explicit cancellation: the write-phase may be a no-op (an
                // unchanged `upsert_file` doesn't bump the revision), so we
                // can't rely on it to unwind the running analysis. Blocks until
                // the old clone drops; safe — this thread holds no clone.
                self.db.trigger_cancellation();
                self.inflight = None;
                uri
            }
        };
        if let Some(req) = self.pending.remove(&uri) {
            self.start(req);
        }
    }

    /// Run one analysis: the write-phase (`&mut db`, on this thread), then the
    /// read-phase on the read pool holding a db clone. Returning to `select!`
    /// right after spawning keeps reads responsive and lets a fresher edit
    /// cancel the analysis.
    fn start(&mut self, mut req: AnalysisRequest) {
        self.scripts.mark_pending(&mut self.db);
        // Write-phase: push the live buffer into the persistent db. Cheap —
        // the parse is a lazy salsa query deferred to the read-phase.
        let file = self.db.upsert_file(&req.path, req.text.text_arc());
        // Hand the precise edits to the incremental reparse. Staged after the
        // text so the chain is never ahead of the buffer it describes, and
        // appended rather than replaced so a chain the previous read never got
        // to consume (a cancelled analysis) survives to be replayed.
        self.db.reparse_stage_edits(file, req.edits.take());

        // Read-phase on the read pool, holding a db clone. A superseding edit
        // (or any write) trips `salsa::Cancelled`, caught below so a canceled
        // analysis publishes nothing; the main loop's version gate is the
        // backstop.
        let snapshot = self.db.snapshot();
        let out_tx = self.out_tx.clone();
        let done_tx = self.done_tx.clone();
        let encoding = self.encoding;
        let AnalysisRequest {
            uri,
            path,
            text,
            version,
            context_epoch,
            rules,
            edits: _, // already staged on the db above
        } = req;
        self.inflight = Some(InflightAnalyze {
            uri: uri.clone(),
            version,
        });
        let push = self.push_diagnostics;
        self.read_spawner.spawn(move || {
            let import_projection = salsa::Cancelled::catch(AssertUnwindSafe(|| {
                (rules.entry_points.is_empty()
                    && rules.get(true).enabled().contains("unused-import")
                    && snapshot.workspace_member(&path).is_some())
                .then(|| (path.clone(), snapshot.import_projection(file)))
            }))
            .ok()
            .flatten();
            if push {
                let result = salsa::Cancelled::catch(AssertUnwindSafe(|| {
                    super::pull_diagnostics::source_diagnostics_via_db(
                        &snapshot, &path, &text, encoding, &rules,
                    )
                }));
                if let Ok(diags) = result {
                    let _ = out_tx.send(Outbound::Diagnostics {
                        context_epoch,
                        uri: uri.clone(),
                        version,
                        text: Arc::clone(&text),
                        diags,
                    });
                }
            }
            // The clone MUST drop before we signal `done`: `trigger_cancellation`
            // / the next write-phase blocks until it's gone, so a premature
            // `done` could let the analysis thread start a write that deadlocks
            // on this clone.
            drop(snapshot);
            let _ = done_tx.send(AnalyzeDone {
                context_epoch,
                uri,
                version,
                import_projection,
            });
        });
    }

    /// Recompute the include-graph diagnostics from the freshly seeded workspace
    /// and publish them per member file, clearing any file whose problems are now
    /// gone. Runs on each (re-)harvest — the same save cadence as the rest of the
    /// workspace index; a live edit that changes an `include` is reflected on the
    /// next save.
    fn refresh_graph_diagnostics(&mut self) {
        let updates: BTreeMap<PathBuf, Vec<Diagnostic>> = {
            let snapshot = self.db.snapshot();
            let mut merged = BTreeMap::<_, Vec<_>>::new();
            for name in snapshot.workspace_packages() {
                let Some(root) = snapshot.package_root(&name) else {
                    continue;
                };
                let entry = crate::incremental::normalize_path(
                    &root.join("src").join(format!("{name}.jl")),
                );
                let Some(file) = snapshot.lookup_file(&entry) else {
                    continue;
                };
                for (context, selection) in snapshot
                    .source_editions(file)
                    .iter()
                    .filter(|(context, _)| context.entry == entry)
                {
                    let view = snapshot.with_context(context.clone(), selection.options);
                    let parser = snapshot.with_options(selection.options);
                    for (path, findings) in
                        graph_diagnostics(view.project_graph(), self.encoding, |path| {
                            let file = parser.lookup_file(path)?;
                            Some((
                                parser.file_text_of(file).to_string(),
                                parser.parsed_tree(file),
                            ))
                        })
                    {
                        let diagnostics = merged.entry(path).or_default();
                        for finding in findings {
                            if !diagnostics.contains(&finding) {
                                diagnostics.push(finding);
                            }
                        }
                    }
                }
            }
            merged
        };
        self.package_graph_diags = updates;
        self.publish_graph_diagnostics();
    }

    fn publish_graph_diagnostics(&mut self) {
        let updates = self
            .scripts
            .graph_diagnostics(self.package_graph_diags.clone());
        let mut now = HashSet::new();
        for (path, diags) in updates {
            if let Some(uri) = super::uri::from_path(&path) {
                now.insert(uri.clone());
                let _ = self.out_tx.send(Outbound::ProjectDiagnostics {
                    uri,
                    diags,
                    source: self.scripts.diagnostic_source(&path),
                });
            }
        }
        // A file that had diagnostics last time but none now needs an explicit
        // empty publish to clear its squiggles.
        for uri in self.published_graph_files.difference(&now) {
            let _ = self.out_tx.send(Outbound::ProjectDiagnostics {
                uri: uri.clone(),
                diags: Vec::new(),
                source: GraphSource::Package,
            });
        }
        self.published_graph_files = now;
        // A pull client's open documents don't get the pushes above; nudge it
        // to re-pull them. The main loop forwards this only when the client
        // supports pull plus the refresh request.
        let _ = self.out_tx.send(Outbound::DiagnosticsRefresh);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn uri_named(name: &str) -> Uri {
        Uri::from_str(&format!("file:///work/{name}")).unwrap()
    }

    impl AnalysisWorker {
        /// An idle worker with dead channels and a one-thread read pool that is
        /// dropped immediately. Fine for the queue-shaping tests, which never
        /// dispatch; a test that calls `start` must supply its own live pool and
        /// senders (see `try_dispatch_supersedes_inflight_and_triggers_cancellation`).
        fn for_test() -> Self {
            use crate::lsp::task_pool::TaskPool;
            use crate::text::PositionEncoding;

            Self {
                context_epoch: 0,
                harvest: crossbeam_channel::unbounded().0,
                full_harvest: None,
                syntax_harvest: None,
                harvest_generation: 0,
                harvest_failed: false,
                changed_sources: BTreeSet::new(),
                syntax_dirty: false,
                membership_generation: Default::default(),
                deferred_edits: Vec::new(),
                scripts: Default::default(),
                pending_script_load: None,
                db: IncrementalDatabase::default(),
                out_tx: crossbeam_channel::unbounded().0,
                done_tx: crossbeam_channel::unbounded().0,
                inflight: None,
                pending: HashMap::new(),
                active: None,
                read_spawner: TaskPool::new("test-analysis-read", 1).spawner(),
                encoding: PositionEncoding::Utf16,
                push_diagnostics: true,
                published_graph_files: HashSet::new(),
                package_graph_diags: BTreeMap::new(),
                import_projections: HashMap::new(),
            }
        }
    }

    #[test]
    fn decide_idle_starts_a_pending_uri() {
        let a = uri_named("a.jl");
        let pending = HashMap::from([(a.clone(), 1)]);
        assert_eq!(decide(None, &pending, None), DispatchAction::Start(a));
    }

    #[test]
    fn package_import_refreshes_follow_reads_and_ignore_stale_completions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("use.jl");
        let uri = super::super::uri::from_path(&path).unwrap();
        let (out_tx, out_rx) = crossbeam_channel::unbounded();
        let mut worker = AnalysisWorker {
            out_tx,
            ..AnalysisWorker::for_test()
        };
        let projection =
            |source| Arc::new(crate::project::scripts::ScriptFile::parse(&path, source));
        let first = projection("f(x) = sin(x) + 1\n");
        let literals = projection("\nf(x) = sin(x) + 123\n");
        let changed = projection("f(x) = cos(x) + 123\n");
        for (version, projection, refresh) in [
            (1, first, true),
            (2, literals.clone(), false),
            (3, changed, true),
        ] {
            worker.inflight = Some(InflightAnalyze {
                uri: uri.clone(),
                version,
            });
            worker.on_done(AnalyzeDone {
                context_epoch: 0,
                uri: uri.clone(),
                version,
                import_projection: Some((path.clone(), projection)),
            });
            assert_eq!(
                out_rx
                    .try_iter()
                    .any(|out| matches!(out, Outbound::DiagnosticsRefresh)),
                refresh
            );
        }
        worker.inflight = Some(InflightAnalyze {
            uri: uri.clone(),
            version: 4,
        });
        worker.on_done(AnalyzeDone {
            context_epoch: 0,
            uri,
            version: 2,
            import_projection: Some((path, literals)),
        });
        assert!(worker.inflight.is_some());
        assert!(out_rx.try_recv().is_err());
    }

    #[test]
    fn decide_idle_empty_queue_waits() {
        let pending: HashMap<Uri, i32> = HashMap::new();
        assert_eq!(decide(None, &pending, None), DispatchAction::Wait);
    }

    #[test]
    fn decide_idle_prefers_active_uri() {
        // Several URIs pending at once (a bulk dirty): the active document is
        // started first, not whatever HashMap order yields.
        let (a, b, c) = (uri_named("a.jl"), uri_named("b.jl"), uri_named("c.jl"));
        let pending = HashMap::from([(a, 1), (b.clone(), 1), (c, 1)]);
        assert_eq!(decide(None, &pending, Some(&b)), DispatchAction::Start(b));
    }

    #[test]
    fn decide_idle_active_not_pending_falls_back() {
        // The active document has no pending request (already analyzed): fall
        // back to any pending URI rather than waiting.
        let a = uri_named("a.jl");
        let pending = HashMap::from([(a.clone(), 1)]);
        assert_eq!(
            decide(None, &pending, Some(&uri_named("focused.jl"))),
            DispatchAction::Start(a)
        );
    }

    #[test]
    fn decide_active_never_cancels_a_different_inflight_uri() {
        // Activity on B must not cancel A's running analysis; B waits its turn.
        let (a, b) = (uri_named("a.jl"), uri_named("b.jl"));
        let pending = HashMap::from([(b.clone(), 1)]);
        assert_eq!(
            decide(Some((&a, 1)), &pending, Some(&b)),
            DispatchAction::Wait
        );
    }

    #[test]
    fn decide_supersedes_same_uri_newer_version() {
        let a = uri_named("a.jl");
        let pending = HashMap::from([(a.clone(), 2)]);
        assert_eq!(
            decide(Some((&a, 1)), &pending, None),
            DispatchAction::SupersedeAndStart(a)
        );
    }

    #[test]
    fn decide_waits_when_pending_same_uri_not_newer() {
        // A duplicate / same-version request for the in-flight URI must not
        // restart it.
        let a = uri_named("a.jl");
        let pending = HashMap::from([(a.clone(), 1)]);
        assert_eq!(decide(Some((&a, 1)), &pending, None), DispatchAction::Wait);
    }

    #[test]
    fn decide_never_cancels_a_different_uri() {
        // With A in flight and only *other* URIs queued, we wait for A's `done`
        // — we never cancel A to start B/C, which would silently drop A's
        // diagnostics.
        let a = uri_named("a.jl");
        let pending = HashMap::from([(uri_named("b.jl"), 5), (uri_named("c.jl"), 9)]);
        assert_eq!(decide(Some((&a, 1)), &pending, None), DispatchAction::Wait);
    }

    #[test]
    fn decide_drains_multiple_uris_one_at_a_time() {
        // Multiple queued URIs are dispatched only as the slot frees, and
        // `decide` never returns SupersedeAndStart for a URI other than the
        // in-flight one.
        let (a, b, c) = (uri_named("a.jl"), uri_named("b.jl"), uri_named("c.jl"));
        let mut pending = HashMap::from([(a.clone(), 1), (b.clone(), 1), (c.clone(), 1)]);

        // Idle: start some URI.
        let DispatchAction::Start(first) = decide(None, &pending, None) else {
            panic!("expected Start");
        };
        assert!(pending.contains_key(&first));
        pending.remove(&first);

        // Busy with `first`, two others still queued → wait, never supersede.
        assert_eq!(
            decide(Some((&first, 1)), &pending, None),
            DispatchAction::Wait
        );

        // Each `done` frees the slot; the next URI starts. Repeat to drain.
        let mut started = vec![first];
        while !pending.is_empty() {
            let DispatchAction::Start(next) = decide(None, &pending, None) else {
                panic!("expected Start");
            };
            pending.remove(&next);
            started.push(next);
        }
        started.sort_by_key(|u| u.as_str().to_string());
        assert_eq!(started, {
            let mut all = vec![a, b, c];
            all.sort_by_key(|u| u.as_str().to_string());
            all
        });
    }

    /// A superseding edit of the *in-flight* URI drives `try_dispatch` through
    /// its `SupersedeAndStart` arm, which calls `db.trigger_cancellation()` and
    /// re-dispatches. The pure `decide` tests cover the *choice*; this exercises
    /// the actual cancellation wiring (that the branch runs without deadlock and
    /// advances the in-flight slot to the newer version).
    #[test]
    fn try_dispatch_supersedes_inflight_and_triggers_cancellation() {
        use std::time::Duration;

        use crate::lsp::lint::ServerRules;
        use crate::lsp::task_pool::TaskPool;

        let a = uri_named("a.jl");
        let rules = ServerRules::defaults();
        let req = |version: i32| AnalysisRequest {
            context_epoch: 0,
            uri: a.clone(),
            path: PathBuf::from("/work/a.jl"),
            text: Arc::new(TextBuffer::from("x = 1\n")),
            version,
            rules: Arc::clone(&rules),
            edits: None,
        };

        // A real read pool so `start` can spawn its read-phase; kept alive for
        // the test so its workers don't disconnect mid-dispatch.
        let pool = TaskPool::new("test-analysis-read", 1);
        let (out_tx, _out_rx) = crossbeam_channel::unbounded::<Outbound>();
        let (done_tx, done_rx) = crossbeam_channel::unbounded::<AnalyzeDone>();
        let mut worker = AnalysisWorker {
            scripts: Default::default(),
            read_spawner: pool.spawner(),
            out_tx,
            done_tx,
            ..AnalysisWorker::for_test()
        };

        // Dispatch v1 and leave it "in flight": we never drain `done_rx`, so the
        // slot stays occupied regardless of whether the read-phase has finished.
        worker.enqueue(req(1));
        worker.try_dispatch();
        assert!(
            matches!(&worker.inflight, Some(f) if f.uri == a && f.version == 1),
            "v1 should be in flight after the first dispatch"
        );

        // A strictly-newer edit of the same URI must supersede: `try_dispatch`
        // takes the `SupersedeAndStart` arm, calls `trigger_cancellation`
        // (blocks only until the v1 clone drops on the pool thread), then starts
        // v2. `Wait`/`Start` are unreachable while v1 holds the slot.
        worker.enqueue(req(2));
        worker.try_dispatch();
        assert!(
            matches!(&worker.inflight, Some(f) if f.uri == a && f.version == 2),
            "supersede should advance the in-flight slot to v2, got {:?}",
            worker.inflight.as_ref().map(|f| f.version)
        );
        assert!(
            !worker.pending.contains_key(&a),
            "the superseding request should have been dispatched, not left pending"
        );

        // The restarted read-phase runs to completion (both v1 and v2 signal
        // `done` as their clones drop) — proves the harness doesn't deadlock.
        let mut done = 0;
        while done_rx.recv_timeout(Duration::from_secs(5)).is_ok() {
            done += 1;
            if done == 2 {
                break;
            }
        }
        assert_eq!(done, 2, "both analyses should have signaled done");
    }

    #[test]
    fn read_dispatch_leaves_an_untracked_buffer_queued_during_another_analysis() {
        use crate::lsp::read_jobs::ReadReply;
        use crate::lsp::task_pool::TaskPool;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("src/Foo.jl");
        let uri = super::super::uri::from_path(&path).unwrap();
        let text = Arc::new(TextBuffer::new("typegroup(x) = x\n"));
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "module Foo end\n").unwrap();
        std::fs::write(
            dir.path().join("Project.toml"),
            "name = 'Foo'\n[syntax]\njulia_version = '1.13'\n",
        )
        .unwrap();
        let pool = TaskPool::new("test-read-dispatch", 1);
        let mut worker = AnalysisWorker {
            read_spawner: pool.spawner(),
            inflight: Some(InflightAnalyze {
                uri: super::super::uri::from_path(&dir.path().join("a.jl")).unwrap(),
                version: 1,
            }),
            ..AnalysisWorker::for_test()
        };
        worker
            .db
            .set_script_sources(&[], &BTreeMap::new(), &BTreeMap::new());
        let mut rules = ServerRules::from_config(&Default::default()).0;
        rules.entry_points = vec![path.clone()];
        worker.enqueue(AnalysisRequest {
            context_epoch: 0,
            uri: uri.clone(),
            path: path.clone(),
            text: Arc::clone(&text),
            version: 1,
            rules: Arc::new(rules),
            edits: None,
        });
        worker.try_dispatch();
        let (sender, replies) = crossbeam_channel::unbounded();
        worker.dispatch_read(ReadJob::DocumentSymbols {
            id: 1.into(),
            path: path.clone(),
            text,
            sender: ReadReply::new(sender),
        });
        assert!(worker.pending.contains_key(&uri));
        assert!(worker.db.lookup_file(&path).is_none());
        assert!(!worker.db.scripts_pending());
        let Outbound::ReadReply {
            message: lsp_server::Message::Response(response),
            ..
        } = replies
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
        else {
            panic!("expected response");
        };
        let symbols = response.response_result.unwrap();
        assert_eq!(symbols[0]["name"], "typegroup", "{symbols}");
    }

    #[test]
    fn script_body_load_preserves_another_files_inflight_diagnostics() {
        use std::time::Duration;

        use crate::config::LintConfig;
        use crate::lsp::task_pool::TaskPool;

        let timeout = Duration::from_secs(5);
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let other = dir.path().join("other.jl");
        let original = "provided() = 1\ninclude(\"other.jl\")\n";
        let edited = "provided() = 2\ninclude(\"other.jl\")\n";
        let other_text = "f() = provided() + typo\n";
        std::fs::write(&other, other_text).unwrap();
        let (mut rules, _) = ServerRules::from_config(&LintConfig {
            select: Some(vec!["undefined-name".into()]),
            ..Default::default()
        });
        rules.entry_points = vec![entry.clone()];
        let rules = Arc::new(rules);
        let request = |path: &Path, text: &str, version| AnalysisRequest {
            context_epoch: 0,
            uri: super::super::uri::from_path(path).unwrap(),
            path: path.to_owned(),
            text: Arc::new(TextBuffer::from(text)),
            version,
            rules: Arc::clone(&rules),
            edits: None,
        };

        let pool = TaskPool::new("test-script-analysis", 1);
        let (out_tx, out_rx) = crossbeam_channel::unbounded();
        let (done_tx, done_rx) = crossbeam_channel::unbounded();
        let mut worker = AnalysisWorker {
            read_spawner: pool.spawner(),
            out_tx,
            done_tx,
            ..AnalysisWorker::for_test()
        };
        worker.scripts.update(
            &entry,
            Arc::new(TextBuffer::new(original)),
            1,
            &rules.entry_points,
            &worker.db,
        );
        let initial = worker.scripts.ready.recv_timeout(timeout).unwrap();
        worker.on_scripts_loaded(initial);
        assert!(matches!(
            out_rx.try_recv(),
            Ok(Outbound::DiagnosticsRefresh)
        ));

        // Hold the read pool so the other file's snapshot remains alive when
        // the script load arrives, independently of thread scheduling speed.
        let (release_tx, release_rx) = crossbeam_channel::unbounded::<()>();
        pool.spawner().spawn(move || {
            let _ = release_rx.recv();
        });
        worker.start(request(&other, other_text, 1));
        worker.enqueue(request(&entry, edited, 2));
        worker.try_dispatch();
        let loaded = worker.scripts.ready.recv_timeout(timeout).unwrap();

        let (handled_tx, handled_rx) = crossbeam_channel::unbounded();
        let writer = std::thread::spawn(move || {
            worker.on_scripts_loaded(loaded);
            handled_tx.send(()).unwrap();
            worker
        });
        let handled_before_read = handled_rx.recv_timeout(timeout).is_ok();
        // Release even on failure so an immediate Salsa write can unwind its
        // canceled reader and the test can join the writer without deadlocking.
        drop(release_tx);
        let mut worker = writer.join().unwrap();
        let done = done_rx.recv_timeout(timeout).unwrap();
        assert!(
            handled_before_read,
            "script loads must wait without blocking or canceling the active analysis"
        );
        let Outbound::Diagnostics {
            uri,
            version,
            diags,
            ..
        } = out_rx.try_recv().unwrap()
        else {
            panic!("expected the other file's diagnostics");
        };
        assert_eq!(uri, super::super::uri::from_path(&other).unwrap());
        assert_eq!(version, 1);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(
            diags[0].code,
            Some(lsp_types::NumberOrString::String("undefined-name".into()))
        );
        assert!(diags[0].message.contains("`typo`"));
        assert_eq!(
            worker.db.file_text(worker.db.lookup_file(&entry).unwrap()),
            original
        );
        assert_eq!(done.uri, uri);
        assert_eq!(done.version, version);

        worker.inflight = None;
        worker.try_dispatch();
        assert!(worker.pending_script_load.is_none());
        let done = done_rx.recv_timeout(timeout).unwrap();
        assert_eq!(done.uri, super::super::uri::from_path(&entry).unwrap());
        assert_eq!(done.version, 2);
        assert_eq!(
            worker.db.file_text(worker.db.lookup_file(&entry).unwrap()),
            edited
        );
        assert!(
            out_rx
                .try_iter()
                .all(|out| !matches!(out, Outbound::DiagnosticsRefresh))
        );
    }

    #[test]
    fn deferred_script_load_checks_generation_when_analysis_finishes() {
        use std::time::Duration;

        let timeout = Duration::from_secs(5);
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let entries = vec![entry.clone()];
        let (out_tx, out_rx) = crossbeam_channel::unbounded();
        let mut worker = AnalysisWorker {
            out_tx,
            ..AnalysisWorker::for_test()
        };
        worker.scripts.update(
            &entry,
            Arc::new(TextBuffer::new("old() = 1\n")),
            1,
            &entries,
            &worker.db,
        );
        worker.inflight = Some(InflightAnalyze {
            uri: super::super::uri::from_path(&dir.path().join("other.jl")).unwrap(),
            version: 1,
        });
        let loaded = worker.scripts.ready.recv_timeout(timeout).unwrap();
        worker.on_scripts_loaded(loaded);
        assert!(worker.db.lookup_file(&entry).is_none());
        assert!(out_rx.try_recv().is_err());

        // A newer edit can invalidate a load while it waits for the active
        // analysis. Completing that analysis must not install the old result.
        worker.scripts.update(
            &entry,
            Arc::new(TextBuffer::new("fresh() = 2\n")),
            2,
            &entries,
            &worker.db,
        );
        worker.inflight = None;
        worker.try_dispatch();
        assert!(worker.db.lookup_file(&entry).is_none());
        assert!(out_rx.try_recv().is_err());

        worker.inflight = Some(InflightAnalyze {
            uri: super::super::uri::from_path(&dir.path().join("other.jl")).unwrap(),
            version: 2,
        });
        let loaded = worker.scripts.ready.recv_timeout(timeout).unwrap();
        worker.on_scripts_loaded(loaded);
        assert!(worker.db.lookup_file(&entry).is_none());
        worker.inflight = None;
        worker.try_dispatch();
        assert_eq!(
            worker.db.file_text(worker.db.lookup_file(&entry).unwrap()),
            "fresh() = 2\n"
        );
        assert!(!worker.db.scripts_pending());
        assert!(matches!(
            out_rx.try_recv(),
            Ok(Outbound::DiagnosticsRefresh)
        ));
        assert!(out_rx.try_recv().is_err());
    }

    #[test]
    fn rewrites_wait_for_script_membership_and_tokens_refresh_on_an_edition_change() {
        use std::time::Duration;

        use crate::lsp::read_jobs::ReadReply;
        use crate::lsp::task_pool::TaskPool;

        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.jl");
        std::fs::write(
            dir.path().join("Project.toml"),
            "[syntax]\njulia_version = '1.13'\n",
        )
        .unwrap();
        let entry = dir.path().join("new/main.jl");
        std::fs::create_dir(entry.parent().unwrap()).unwrap();
        std::fs::write(
            entry.with_file_name("Project.toml"),
            "[syntax]\njulia_version = '1.14'\n",
        )
        .unwrap();
        std::fs::write(&entry, "include(\"../shared.jl\")\n").unwrap();
        let text = Arc::new(TextBuffer::new("typegroup  =  1\n"));
        let pool = TaskPool::new("test-deferred-format", 1);
        let (sender, receiver) = crossbeam_channel::unbounded();
        let mut worker = AnalysisWorker {
            out_tx: sender.clone(),
            read_spawner: pool.spawner(),
            ..AnalysisWorker::for_test()
        };
        worker.db.upsert_file(&shared, text.text_arc());
        worker
            .scripts
            .update(&shared, Arc::clone(&text), 1, &[entry], &worker.db);
        let loaded = worker
            .scripts
            .ready
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        worker.deferred_edits.push(ReadJob::Format {
            id: 1.into(),
            path: shared,
            text,
            style: Default::default(),
            safe: true,
            sender: ReadReply::new(sender),
        });
        worker.dispatch_ready_edits();
        assert_eq!(worker.deferred_edits.len(), 1);
        assert!(receiver.try_recv().is_err());

        worker.on_scripts_loaded(loaded);
        assert!(
            receiver
                .try_iter()
                .any(|out| matches!(out, Outbound::SemanticTokensRefresh))
        );
        worker.dispatch_ready_edits();
        let Outbound::ReadReply {
            message: lsp_server::Message::Response(response),
            ..
        } = receiver.recv_timeout(Duration::from_secs(5)).unwrap()
        else {
            panic!("expected formatting response");
        };
        assert!(response.response_result.is_err(), "{response:?}");
    }

    /// The write phase hands salsa the live buffer's own allocation, never a
    /// copy of it. A `req.text.text().to_string()` here would still type-check
    /// and still be correct — it would just copy the whole document on every
    /// keystroke, which is the one regression no other test or bench notices
    /// (`AGENTS.md`, "LSP", and `benches/salsa_keystroke.rs`).
    #[test]
    fn the_write_phase_shares_the_buffers_allocation() {
        use crate::lsp::lint::ServerRules;
        use crate::lsp::task_pool::TaskPool;

        // A live pool and live receivers: `start` spawns its read-phase, and a
        // dead channel would fail the send on the pool thread.
        let pool = TaskPool::new("test-analysis-share", 1);
        let (out_tx, _out_rx) = crossbeam_channel::unbounded::<Outbound>();
        let (done_tx, _done_rx) = crossbeam_channel::unbounded::<AnalyzeDone>();
        let mut worker = AnalysisWorker {
            scripts: Default::default(),
            read_spawner: pool.spawner(),
            out_tx,
            done_tx,
            ..AnalysisWorker::for_test()
        };

        let text = Arc::new(TextBuffer::from("x = 1\n"));
        let path = PathBuf::from("/work/share.jl");
        worker.start(AnalysisRequest {
            context_epoch: 0,
            uri: uri_named("share.jl"),
            path: path.clone(),
            text: Arc::clone(&text),
            version: 1,
            rules: ServerRules::defaults(),
            edits: None,
        });

        let file = worker
            .db
            .lookup_file(&path)
            .expect("the write phase tracks the file it upserts");
        assert!(
            Arc::ptr_eq(&text.text_arc(), file.text(&worker.db)),
            "the write phase must hand salsa the buffer's allocation, not a copy"
        );
    }

    /// Coalescing drops the superseded request wholesale, so its edits have to
    /// ride along on the survivor: what reaches the db must describe the
    /// transform from the last *analyzed* text, not from the last enqueued one.
    #[test]
    fn enqueue_accumulates_the_edits_of_coalesced_requests() {
        use crate::lsp::lint::ServerRules;

        let a = uri_named("a.jl");
        let rules = ServerRules::defaults();
        let req = |version: i32, text: &str, edits: Option<Vec<Edit>>| AnalysisRequest {
            context_epoch: 0,
            uri: a.clone(),
            path: PathBuf::from("/work/a.jl"),
            text: Arc::new(TextBuffer::from(text)),
            version,
            rules: Arc::clone(&rules),
            edits,
        };
        let edit = |at: usize, insert: &str| Edit {
            range: at..at,
            insert: insert.to_string(),
        };
        let mut worker = AnalysisWorker::for_test();

        worker.enqueue(req(1, "xy\n", Some(vec![edit(1, "x")])));
        worker.enqueue(req(2, "xyz\n", Some(vec![edit(2, "y")])));
        assert_eq!(
            worker.pending[&a].edits,
            Some(vec![edit(1, "x"), edit(2, "y")]),
            "v1's edits must survive its request being dropped"
        );

        // An unknown transform anywhere in the run poisons the whole chain:
        // the surviving text can no longer be reached by replaying it.
        worker.enqueue(req(3, "brand new\n", None));
        assert_eq!(worker.pending[&a].edits, None);

        // A re-analysis at the same version and text (new lint rules) is
        // dropped, and must not take a good chain down with it.
        let mut worker = AnalysisWorker::for_test();
        worker.enqueue(req(1, "xy\n", Some(vec![edit(1, "x")])));
        worker.enqueue(req(1, "xy\n", None));
        assert_eq!(worker.pending[&a].edits, Some(vec![edit(1, "x")]));

        // A dropped request whose text *differs*, though, means the pending
        // chain no longer describes how the pending text was reached.
        worker.enqueue(req(1, "different\n", None));
        assert_eq!(worker.pending[&a].edits, None);
    }

    /// A close takes the closed buffer's queued analyses with it. A later
    /// reopen on the ordered write channel is a new document.
    #[test]
    fn sync_drops_queued_analyses_for_the_closed_path() {
        use crate::lsp::lint::ServerRules;
        use crate::lsp::task_pool::TaskPool;

        let dir = tempfile::tempdir().unwrap();
        let (closed, other) = (dir.path().join("a.jl"), dir.path().join("b.jl"));
        std::fs::write(&closed, "on_disk = 1\n").unwrap();
        std::fs::write(&other, "b = 1\n").unwrap();
        let closed_uri = crate::lsp::uri::from_path(&closed).unwrap();
        let other_uri = crate::lsp::uri::from_path(&other).unwrap();

        let rules = ServerRules::defaults();
        let req = |uri: &Uri, path: &PathBuf, text: &str, version: i32| AnalysisRequest {
            context_epoch: 0,
            uri: uri.clone(),
            path: path.clone(),
            text: Arc::new(TextBuffer::from(text)),
            version,
            rules: Arc::clone(&rules),
            edits: None,
        };

        // A real read pool so the surviving request can actually dispatch.
        let pool = TaskPool::new("test-analysis-read", 1);
        let (sync_tx, sync_rx) = crossbeam_channel::unbounded();
        let mut worker = AnalysisWorker {
            scripts: Default::default(),
            read_spawner: pool.spawner(),
            ..AnalysisWorker::for_test()
        };

        // An earlier analysis already pushed the unsaved buffer into the db.
        worker.db.upsert_file(&closed, "unsaved = 2\n".to_string());
        for message in [
            SyncMessage::Analyze(req(&closed_uri, &closed, "unsaved = 3\n", 3)),
            SyncMessage::Analyze(req(&closed_uri, &closed, "unsaved = 4\n", 4)),
            SyncMessage::Analyze(req(&other_uri, &other, "b = 2\n", 1)),
            SyncMessage::Revert(closed.clone()),
        ] {
            sync_tx.send(message).unwrap();
        }
        worker.drain_sync(&sync_rx);
        worker.try_dispatch();

        assert!(
            !worker.pending.contains_key(&closed_uri),
            "the closed document's queued analyses should be gone"
        );
        assert!(
            matches!(&worker.inflight, Some(f) if f.uri == other_uri),
            "the still-open document's request should have been dispatched"
        );

        let file = worker.db.lookup_file(&closed).expect("tracked input");
        assert_eq!(
            worker.db.file_text(file),
            "on_disk = 1\n",
            "a stale queued analysis re-upserted the discarded buffer"
        );
    }

    #[test]
    fn guard_contains_a_panic_and_reports_completion() {
        // A panicking unit of work is caught and reported as not-completed, so
        // the analysis thread's `select!` loop (and the main loop) keep running
        // rather than the sole db writer dying and zombieing the server.
        assert!(guard("ok", || {}));
        let mut ran = false;
        assert!(!guard("boom", || {
            ran = true;
            panic!("kaboom");
        }));
        assert!(ran, "the guarded closure should have started");
    }
}
