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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, select};
use lsp_types::{Diagnostic, Uri};
use salsa::Database as _;

use crate::incremental::{IncrementalDatabase, IncrementalDb};
use crate::index::{HarvestedLibrary, PackageIndex};
use crate::text::{Edit, PositionEncoding};

use super::format::parse_diagnostics_to_lsp;
use super::graph_diagnostics::graph_diagnostics;
use super::lint::{ServerRules, lint_diagnostics_via_db};
use super::read_jobs::{ReadJob, run_read};
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
    pub(crate) uri: Uri,
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    pub(crate) version: i32,
    /// The lint rules the main loop resolved for this document (discovered
    /// `fatou.toml` shadowing editor-pushed settings), current at dispatch.
    pub(crate) rules: Arc<ServerRules>,
    /// The byte edits that produced `text` from the previously sent one, for
    /// the incremental reparse to replay; `None` when the transform is unknown
    /// (a `didOpen`, a whole-buffer replacement, a re-analysis for new rules).
    pub(crate) edits: Option<Vec<Edit>>,
}

/// A library-index update delivered to the analysis thread by the background
/// harvester. The full harvest lands once at startup; a re-harvest of the
/// workspace package (on save) lands as a single-package swap.
pub(crate) enum LibraryMessage {
    /// The whole harvested environment (Base/stdlib, deps, and the workspace
    /// package): replace the library index wholesale.
    Full(HarvestedLibrary),
    /// A re-harvested single package (the workspace package on save): swap just
    /// its entry, keeping the rest and the workspace name.
    Package {
        name: String,
        index: Arc<PackageIndex>,
    },
}

/// Spawn the dedicated analysis thread that owns the persistent salsa database.
/// `library_rx` delivers the harvested package index once the background loader
/// has resolved the environment (and later single-package re-harvests); the
/// thread swaps it into the db as a write.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_analysis_thread(
    analysis_rx: Receiver<AnalysisRequest>,
    read_rx: Receiver<ReadJob>,
    library_rx: Receiver<LibraryMessage>,
    sync_rx: Receiver<PathBuf>,
    out_tx: Sender<Outbound>,
    read_spawner: Spawner,
    encoding: PositionEncoding,
    push_diagnostics: bool,
) -> JoinHandle<()> {
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<AnalyzeDone>();
    std::thread::Builder::new()
        .name("fatou-analysis".to_string())
        .spawn(move || {
            let mut worker = AnalysisWorker {
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
            };
            worker.run(&analysis_rx, &read_rx, &library_rx, &sync_rx, &done_rx);
        })
        .expect("spawn analysis thread")
}

/// Signal from a finished read-phase ([`AnalysisWorker::start`]) back to the
/// analysis thread: the analysis for `uri`@`version` has completed (or unwound
/// on cancellation) and dropped its db clone, so the in-flight slot is free.
struct AnalyzeDone {
    uri: Uri,
    version: i32,
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
}

impl AnalysisWorker {
    fn run(
        &mut self,
        analysis_rx: &Receiver<AnalysisRequest>,
        read_rx: &Receiver<ReadJob>,
        library_rx: &Receiver<LibraryMessage>,
        sync_rx: &Receiver<PathBuf>,
        done_rx: &Receiver<AnalyzeDone>,
    ) {
        loop {
            select! {
                recv(sync_rx) -> path => {
                    // An editor closed a document, or a watched file changed
                    // outside any open buffer: revert its tracked input to
                    // on-disk text so a discarded buffer (or stale seeded text)
                    // stops contributing to the reverse-occurrence index. A
                    // no-op for a non-member or a buffer already matching disk.
                    // Guarded so a panic mid-revert can't kill the sole writer.
                    guard("sync", || {
                        if let Ok(path) = path {
                            self.db.revert_file_to_disk(&path);
                        }
                    });
                }
                recv(library_rx) -> msg => {
                    // The background harvester delivered an index update: swap it
                    // into the db (a write). Later requests read it from their
                    // snapshot; open files need no re-analysis because no
                    // diagnostic depends on the library yet.
                    guard("library", || match msg {
                        Ok(LibraryMessage::Full(lib)) => {
                            self.db.set_library(lib.packages, lib.roots, lib.workspaces);
                            // Seed the workspace packages' member files as inputs
                            // so cross-file references/rename can index them.
                            self.db.seed_workspace_members();
                            self.refresh_graph_diagnostics();
                        }
                        Ok(LibraryMessage::Package { name, index }) => {
                            self.db.set_package_index(name, index);
                            self.db.seed_workspace_members();
                            self.refresh_graph_diagnostics();
                        }
                        Err(_) => {}
                    });
                }
                recv(analysis_rx) -> msg => {
                    let Ok(req) = msg else { break };
                    // Coalesce: keep only the latest version per URI, so a fast
                    // typist's stale edits are dropped before they're analyzed.
                    guard("analyze", || {
                        self.enqueue(req);
                        while let Ok(more) = analysis_rx.try_recv() {
                            self.enqueue(more);
                        }
                        self.try_dispatch();
                    });
                }
                recv(done_rx) -> done => {
                    let Ok(done) = done else { continue };
                    // Free the slot only if this `done` is for the *current*
                    // in-flight analysis — a late `done` from a superseded one
                    // (older version) must not clear the new analysis.
                    guard("done", || {
                        if matches!(&self.inflight, Some(f) if f.uri == done.uri && f.version == done.version) {
                            self.inflight = None;
                        }
                        self.try_dispatch();
                    });
                }
                recv(read_rx) -> job => {
                    let Ok(job) = job else { continue };
                    // Mint a short-lived read-only snapshot and run the job off
                    // this thread. The clone is dropped inside `run_read`, so
                    // the next write isn't blocked once the read finishes (or a
                    // racing write trips `salsa::Cancelled`, handled by the
                    // job's fallback).
                    guard("read-dispatch", || {
                        let snapshot = self.db.snapshot();
                        let encoding = self.encoding;
                        self.read_spawner.spawn(move || run_read(snapshot, job, encoding));
                    });
                }
            }
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
        // Even a stale-version duplicate signals recent activity on this URI.
        self.active = Some(req.uri.clone());
        match self.pending.get_mut(&req.uri) {
            Some(existing) if existing.version >= req.version => {
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
        // Write-phase: push the live buffer into the persistent db. Cheap —
        // the parse is a lazy salsa query deferred to the read-phase.
        let file = self.db.upsert_file(&req.path, req.text.clone());
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
            rules,
            edits: _, // already staged on the db above
        } = req;
        self.inflight = Some(InflightAnalyze {
            uri: uri.clone(),
            version,
        });
        let push = self.push_diagnostics;
        self.read_spawner.spawn(move || {
            if push {
                let result = salsa::Cancelled::catch(AssertUnwindSafe(|| {
                    let mut diags =
                        parse_diagnostics_to_lsp(snapshot.parse_diagnostics(file), &text, encoding);
                    // Lint findings join the same publish, but only on a clean
                    // tree: rules would misfire on error-recovered shapes, and a
                    // broken buffer's parse errors are the actionable signal.
                    if diags.is_empty() {
                        diags.extend(lint_diagnostics_via_db(
                            &snapshot, &path, &text, encoding, &rules,
                        ));
                    }
                    diags
                }));
                if let Ok(diags) = result {
                    let _ = out_tx.send(Outbound::Diagnostics {
                        uri: uri.clone(),
                        version,
                        diags,
                    });
                }
            }
            // The clone MUST drop before we signal `done`: `trigger_cancellation`
            // / the next write-phase blocks until it's gone, so a premature
            // `done` could let the analysis thread start a write that deadlocks
            // on this clone.
            drop(snapshot);
            let _ = done_tx.send(AnalyzeDone { uri, version });
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
            let graph = snapshot.project_graph();
            graph_diagnostics(graph, self.encoding, |path| {
                let file = snapshot.lookup_file(path)?;
                Some((
                    snapshot.file_text_of(file).to_string(),
                    snapshot.parsed_tree(file),
                ))
            })
        };

        let mut now = HashSet::new();
        for (path, diags) in updates {
            if let Some(uri) = super::uri::from_path(&path) {
                now.insert(uri.clone());
                let _ = self
                    .out_tx
                    .send(Outbound::ProjectDiagnostics { uri, diags });
            }
        }
        // A file that had diagnostics last time but none now needs an explicit
        // empty publish to clear its squiggles.
        for uri in self.published_graph_files.difference(&now) {
            let _ = self.out_tx.send(Outbound::ProjectDiagnostics {
                uri: uri.clone(),
                diags: Vec::new(),
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
            uri: a.clone(),
            path: PathBuf::from("/work/a.jl"),
            text: "x = 1\n".to_string(),
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

    /// Coalescing drops the superseded request wholesale, so its edits have to
    /// ride along on the survivor: what reaches the db must describe the
    /// transform from the last *analyzed* text, not from the last enqueued one.
    #[test]
    fn enqueue_accumulates_the_edits_of_coalesced_requests() {
        use crate::lsp::lint::ServerRules;

        let a = uri_named("a.jl");
        let rules = ServerRules::defaults();
        let req = |version: i32, text: &str, edits: Option<Vec<Edit>>| AnalysisRequest {
            uri: a.clone(),
            path: PathBuf::from("/work/a.jl"),
            text: text.to_string(),
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
