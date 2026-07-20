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

use crate::incremental::IncrementalDatabase;
use crate::index::{HarvestedLibrary, PackageIndex};
use crate::text::PositionEncoding;

use super::format::parse_diagnostics_to_lsp;
use super::graph_diagnostics::graph_diagnostics;
use super::lint::lint_diagnostics_via_db;
use super::read_jobs::{ReadJob, run_read};
use super::state::Outbound;
use super::task_pool::Spawner;

/// An analysis request handed to the dedicated analysis thread: refresh the
/// diagnostics for `uri`'s live buffer at `version`.
pub(crate) struct AnalysisRequest {
    pub(crate) uri: Uri,
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    pub(crate) version: i32,
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
                    if let Ok(path) = path {
                        self.db.revert_file_to_disk(&path);
                    }
                }
                recv(library_rx) -> msg => {
                    // The background harvester delivered an index update: swap it
                    // into the db (a write). Later requests read it from their
                    // snapshot; open files need no re-analysis because no
                    // diagnostic depends on the library yet.
                    match msg {
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
                    }
                }
                recv(analysis_rx) -> msg => {
                    let Ok(req) = msg else { break };
                    // Coalesce: keep only the latest version per URI, so a fast
                    // typist's stale edits are dropped before they're analyzed.
                    self.enqueue(req);
                    while let Ok(more) = analysis_rx.try_recv() {
                        self.enqueue(more);
                    }
                    self.try_dispatch();
                }
                recv(done_rx) -> done => {
                    let Ok(done) = done else { continue };
                    // Free the slot only if this `done` is for the *current*
                    // in-flight analysis — a late `done` from a superseded one
                    // (older version) must not clear the new analysis.
                    if matches!(&self.inflight, Some(f) if f.uri == done.uri && f.version == done.version) {
                        self.inflight = None;
                    }
                    self.try_dispatch();
                }
                recv(read_rx) -> job => {
                    let Ok(job) = job else { continue };
                    // Mint a short-lived read-only snapshot and run the job off
                    // this thread. The clone is dropped inside `run_read`, so
                    // the next write isn't blocked once the read finishes (or a
                    // racing write trips `salsa::Cancelled`, handled by the
                    // job's fallback).
                    let snapshot = self.db.snapshot();
                    let encoding = self.encoding;
                    self.read_spawner.spawn(move || run_read(snapshot, job, encoding));
                }
            }
        }
    }

    /// Add `req` to the pending queue, keeping the highest version per URI
    /// (guards against an out-of-order lower version clobbering a newer one).
    fn enqueue(&mut self, req: AnalysisRequest) {
        // Even a stale-version duplicate signals recent activity on this URI.
        self.active = Some(req.uri.clone());
        match self.pending.get(&req.uri) {
            Some(existing) if existing.version >= req.version => {}
            _ => {
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
    fn start(&mut self, req: AnalysisRequest) {
        // Write-phase: push the live buffer into the persistent db. Cheap —
        // the parse is a lazy salsa query deferred to the read-phase.
        let file = self.db.upsert_file(&req.path, req.text.clone());

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
                        diags.extend(lint_diagnostics_via_db(&snapshot, &path, &text, encoding));
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
}
