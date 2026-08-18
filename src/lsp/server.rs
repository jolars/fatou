//! Server entry points: the initialize handshake, advertised capabilities, and
//! the main event loop that wires the channels, pools, and threads together.

use std::collections::HashSet;
use std::error::Error;
use std::path::PathBuf;

use crossbeam_channel::{TryRecvError, select};
use lsp_server::{Connection, Message};
use lsp_types::{
    CallHierarchyServerCapability, ClientCapabilities, CodeActionKind, CodeActionOptions,
    CodeActionProviderCapability, CompletionOptions, DiagnosticOptions,
    DiagnosticServerCapabilities, DocumentLinkOptions, FileOperationFilter, FileOperationPattern,
    FileOperationPatternKind, FileOperationRegistrationOptions, FoldingRangeProviderCapability,
    HoverProviderCapability, InitializeParams, InlayHintOptions, InlayHintServerCapabilities,
    OneOf, PositionEncodingKind, RenameOptions, SelectionRangeProviderCapability,
    SemanticTokensFullOptions, SemanticTokensOptions, ServerCapabilities, SignatureHelpOptions,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions, Uri, WorkspaceFileOperationsServerCapabilities,
    WorkspaceFoldersServerCapabilities, WorkspaceServerCapabilities,
};

use std::sync::Arc;

use crate::environment::EnvContext;
use crate::incremental::normalize_path;
use crate::index::{
    IndexCache, PackageIndex, dev_packages, harvest_libraries_parallel, harvest_workspace,
};
use crate::text::PositionEncoding;

use super::analysis_thread::{
    AnalysisRequest, LibraryMessage, SyncMessage, guard, spawn_analysis_thread,
};
use super::environment_diagnostics::{
    EnvironmentFindings, environment_diagnostics, resolve_failure_diagnostics,
};
use super::progress::HarvestProgress;
use super::read_jobs::ReadJob;
use super::semantic_tokens::legend;
use super::state::{GlobalState, Outbound};
use super::task_pool::{TaskPool, index_pool_size, read_pool_size};
use super::uri::to_path;

pub(crate) type DynError = Box<dyn Error + Sync + Send>;

/// Run the language server on stdio until the client shuts it down.
pub fn run() -> Result<(), DynError> {
    let (connection, io_threads) = Connection::stdio();
    serve(&connection)?;
    io_threads.join()?;
    Ok(())
}

/// Perform the initialize handshake on `connection`, then run the message loop.
/// Split out from [`run`] so tests can drive it over an in-memory connection.
///
/// The handshake is two-step ([`Connection::initialize_start`] /
/// [`Connection::initialize_finish`]) rather than [`Connection::initialize`]
/// because the advertised capabilities depend on the client's: the position
/// encoding is negotiated from `general.positionEncodings`.
pub fn serve(connection: &Connection) -> Result<(), DynError> {
    let (id, params) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;
    let encoding = negotiate_position_encoding(&params.capabilities);
    let workspace_roots = workspace_roots(&params);
    // Watching only pays off with a workspace to keep fresh; without roots the
    // harvester never runs and every watched event would be dropped anyway.
    let register_watchers =
        supports_watched_files_registration(&params.capabilities) && !workspace_roots.is_empty();
    let pull_diagnostics = supports_pull_diagnostics(&params.capabilities);
    let diagnostic_refresh = supports_diagnostic_refresh(&params.capabilities);
    let inlay_hint_refresh = supports_inlay_hint_refresh(&params.capabilities);
    let work_done_progress = supports_work_done_progress(&params.capabilities);
    let result =
        serde_json::json!({ "capabilities": capabilities_json(encoding, pull_diagnostics) });
    connection.initialize_finish(id, result)?;
    main_loop(
        connection,
        encoding,
        workspace_roots,
        register_watchers,
        pull_diagnostics,
        diagnostic_refresh,
        inlay_hint_refresh,
        work_done_progress,
        params.initialization_options,
    )
}

/// Whether the client pulls diagnostics (`textDocument/diagnostic`). With
/// pull support the server advertises a diagnostic provider and keeps the
/// push path only for files with no open buffer; without it, push stays the
/// sole channel (the fallback).
fn supports_pull_diagnostics(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .text_document
        .as_ref()
        .is_some_and(|text_document| text_document.diagnostic.is_some())
}

/// Whether the client accepts `workspace/diagnostic/refresh`, the server's
/// nudge to re-pull open documents after a re-harvest changes the include
/// graph.
fn supports_diagnostic_refresh(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.diagnostic.as_ref())
        .and_then(|diagnostic| diagnostic.refresh_support)
        .unwrap_or(false)
}

/// Whether the client accepts `workspace/inlayHint/refresh`, the server's nudge
/// to re-request hints once the harvest supplies the versions they report.
///
/// Checked rather than assumed because the spec has this request force a
/// *global* recalculation of every visible hint: a client that never claimed it
/// should not be handed one.
fn supports_inlay_hint_refresh(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.inlay_hint.as_ref())
        .and_then(|inlay_hint| inlay_hint.refresh_support)
        .unwrap_or(false)
}

/// Whether the client accepts a dynamic `workspace/didChangeWatchedFiles`
/// registration. File watching has no static server capability: without this,
/// the server never hears about external file events and relies on saves
/// alone.
fn supports_watched_files_registration(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files.as_ref())
        .and_then(|caps| caps.dynamic_registration)
        .unwrap_or(false)
}

/// Whether the client accepts server-initiated work-done progress
/// (`window/workDoneProgress/create` plus `$/progress`), the channel the
/// harvester reports its indexing passes on. There is no matching
/// `ServerCapabilities` field: server-initiated progress is gated on this
/// client capability alone.
fn supports_work_done_progress(capabilities: &ClientCapabilities) -> bool {
    capabilities
        .window
        .as_ref()
        .and_then(|window| window.work_done_progress)
        .unwrap_or(false)
}

/// The workspace roots to resolve Julia environments against: every workspace
/// folder in client order (deduped on the normalized path), falling back to the
/// (deprecated) `root_uri` when the client sent no folders. Empty when the
/// client opened no folder at all (a single loose file); the loader then does
/// nothing.
fn workspace_roots(params: &InitializeParams) -> Vec<PathBuf> {
    let folder_uris: Vec<&lsp_types::Uri> = match params.workspace_folders.as_deref() {
        Some(folders) if !folders.is_empty() => folders.iter().map(|f| &f.uri).collect(),
        #[allow(deprecated)]
        _ => params.root_uri.iter().collect(),
    };
    let mut seen = std::collections::HashSet::new();
    folder_uris
        .into_iter()
        .filter_map(to_path)
        .filter(|path| seen.insert(normalize_path(path)))
        .collect()
}

/// Pick the position encoding for the session: UTF-8 (plain byte offsets, no
/// re-encoding on our side) when the client offers it, otherwise the mandatory
/// LSP default of UTF-16.
fn negotiate_position_encoding(capabilities: &ClientCapabilities) -> PositionEncoding {
    let offered = capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_deref())
        .unwrap_or_default();
    if offered.contains(&PositionEncodingKind::UTF8) {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    }
}

fn server_capabilities(encoding: PositionEncoding, pull_diagnostics: bool) -> ServerCapabilities {
    ServerCapabilities {
        // Advertised only to a client that pulls: pushing and pulling the
        // same document's diagnostics would double them up, so per-document
        // publishes are gated off in the same breath (see `GlobalState`).
        diagnostic_provider: pull_diagnostics.then(|| {
            DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("fatou".to_string()),
                // Include-graph diagnostics cross files: an include edit in
                // one member can change another member's report.
                inter_file_dependencies: true,
                workspace_diagnostics: false,
                work_done_progress_options: Default::default(),
            })
        }),
        position_encoding: Some(match encoding {
            PositionEncoding::Utf8 => PositionEncodingKind::UTF8,
            PositionEncoding::Utf16 => PositionEncodingKind::UTF16,
        }),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                // Save notifications trigger a re-harvest of the workspace
                // package so cross-file navigation reflects added/removed
                // top-level symbols; the text is not needed (we read from disk).
                save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                ..Default::default()
            },
        )),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            work_done_progress_options: Default::default(),
            resolve_provider: None,
        })),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            // `.` opens member completion, `@` opens macro completion, and `\`
            // opens the LaTeX/emoji input sequences.
            trigger_characters: Some(vec![".".to_string(), "@".to_string(), "\\".to_string()]),
            resolve_provider: Some(true),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            // `(` opens signature help, `,` (also a retrigger) advances the
            // active parameter.
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: Some(vec![",".to_string()]),
            work_done_progress_options: Default::default(),
        }),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        document_link_provider: Some(DocumentLinkOptions {
            // Targets resolve eagerly (a lexical path join, no I/O worth
            // deferring), so no `documentLink/resolve`.
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        }),
        // Project files only: a dependency's resolved version, beside its UUID.
        // Julia documents answer an empty list — inlay *type* hints would need
        // inference, and fatou runs no Julia. Advertised globally all the same,
        // as every other per-kind capability here is.
        inlay_hint_provider: Some(OneOf::Right(InlayHintServerCapabilities::Options(
            InlayHintOptions {
                // Label and tooltip are both built up front from the library
                // map already in memory, so there is nothing to defer.
                resolve_provider: Some(false),
                work_done_progress_options: Default::default(),
            },
        ))),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(
            SemanticTokensOptions {
                work_done_progress_options: Default::default(),
                legend: legend(),
                range: None,
                // Delta so an unchanged re-pull answers an empty edit list
                // instead of the full token set (see `semantic_tokens_delta`).
                full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
            }
            .into(),
        ),
        workspace: Some(WorkspaceServerCapabilities {
            // Every folder from `initialize` gets the full workspace treatment;
            // dynamic add/remove (`didChangeWorkspaceFolders`) is not handled
            // yet, so change notifications are not requested.
            workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                supported: Some(true),
                change_notifications: None,
            }),
            file_operations: Some(WorkspaceFileOperationsServerCapabilities {
                // `willRename` returns the edits that keep the `include` graph
                // intact; `didRename` refreshes membership for a client that
                // cannot register file watchers dynamically. Advertised
                // unconditionally: a server capability the client does not
                // implement is simply ignored, unlike a registration.
                will_rename: Some(rename_file_operations()),
                did_rename: Some(rename_file_operations()),
                ..Default::default()
            }),
        }),
        ..Default::default()
    }
}

/// The file-operation filters both rename capabilities register: `.jl` sources,
/// plus any folder (a folder rename moves every source under it, and the
/// client reports only the folder).
fn rename_file_operations() -> FileOperationRegistrationOptions {
    let filter = |glob: &str, matches| FileOperationFilter {
        scheme: Some("file".to_string()),
        pattern: FileOperationPattern {
            glob: glob.to_string(),
            matches: Some(matches),
            options: None,
        },
    };
    FileOperationRegistrationOptions {
        filters: vec![
            filter("**/*.jl", FileOperationPatternKind::File),
            filter("**", FileOperationPatternKind::Folder),
        ],
    }
}

/// The `initialize` result's `capabilities` value. Built from the serialized
/// [`ServerCapabilities`] because lsp-types 0.97 has no
/// `type_hierarchy_provider` field (the request, param, and item types exist;
/// the capability field was never added upstream), so it is injected into the
/// serialized map here.
fn capabilities_json(encoding: PositionEncoding, pull_diagnostics: bool) -> serde_json::Value {
    let mut capabilities = serde_json::to_value(server_capabilities(encoding, pull_diagnostics))
        .expect("server capabilities serialize");
    capabilities["typeHierarchyProvider"] = serde_json::Value::Bool(true);
    capabilities
}

/// The main event loop: dispatch incoming JSON-RPC messages and analysis
/// results. Owns no salsa database (see the module docs); joins the analysis
/// thread before returning.
#[allow(clippy::too_many_arguments)]
fn main_loop(
    connection: &Connection,
    encoding: PositionEncoding,
    workspace_roots: Vec<PathBuf>,
    register_watchers: bool,
    pull_diagnostics: bool,
    diagnostic_refresh: bool,
    inlay_hint_refresh: bool,
    work_done_progress: bool,
    initialization_options: Option<serde_json::Value>,
) -> Result<(), DynError> {
    let (out_tx, out_rx) = crossbeam_channel::unbounded::<Outbound>();
    let (analysis_tx, analysis_rx) = crossbeam_channel::unbounded::<AnalysisRequest>();
    let (read_tx, read_rx) = crossbeam_channel::unbounded::<ReadJob>();
    let (library_tx, library_rx) = crossbeam_channel::unbounded::<LibraryMessage>();
    // Harvest signals from the main loop to the workspace harvester: a changed
    // source file's path (saves and watched events; the harvester ignores paths
    // outside every workspace package) or an environment-file change.
    let (harvest_tx, harvest_rx) = crossbeam_channel::unbounded::<HarvestSignal>();
    // Disk-sync signals from the main loop to the analysis thread: a file's
    // path, whose tracked input is reverted to on-disk text (a closed
    // document's discarded buffer, or a watched file changed outside any open
    // buffer).
    let (sync_tx, sync_rx) = crossbeam_channel::unbounded::<SyncMessage>();

    // Resolve the environment and harvest its packages off the event loop: it
    // walks the filesystem and parses all of Base, so it must not block the
    // handshake (nor shutdown — the thread is detached). The result is swapped
    // into the db when it lands; every feature stays usable in the meantime, and
    // library go-to-definition/completion start answering once it arrives. The
    // same thread re-harvests the workspace package on each harvest signal, and
    // reports each pass on the client's message channel directly (it never
    // touches the db, so progress stays off the `Outbound` path). A client
    // without work-done support gets a `None` sender and no progress at all.
    let progress_sender = work_done_progress.then(|| connection.sender.clone());
    spawn_workspace_harvester(
        workspace_roots,
        library_tx,
        harvest_rx,
        progress_sender,
        // Diagnostics on the environment files, unlike progress, need main-loop
        // state: `publish_merged` unions them with whatever else holds the same
        // URI, so they take the `Outbound` path rather than the direct one.
        out_tx.clone(),
        encoding,
    );

    // The read pool serves latency-sensitive work (formatting, the analysis
    // read-phase). Its workers must outlive both `state` and the analysis
    // thread; the drop order at the end of this function guarantees that.
    let read_pool = TaskPool::new("fatou-lsp-read", read_pool_size());
    let analysis_handle = spawn_analysis_thread(
        analysis_rx,
        read_rx,
        library_rx,
        sync_rx,
        // The main loop keeps a clone so finished read jobs route back here for
        // version-gating (see `GlobalState::on_read_reply`).
        out_tx.clone(),
        read_pool.spawner(),
        encoding,
        // The per-edit push is the fallback for a client that cannot pull.
        !pull_diagnostics,
    );

    let mut state = GlobalState::new(
        connection.sender.clone(),
        out_tx,
        analysis_tx,
        read_tx,
        harvest_tx,
        sync_tx,
        encoding,
        pull_diagnostics,
        diagnostic_refresh,
        inlay_hint_refresh,
        initialization_options,
    );

    // `initialize_finish` has already consumed the client's `initialized`
    // notification (lsp-server handles it inside the handshake), so the
    // registration request is legal from the first turn of the loop.
    if register_watchers {
        state.register_file_watchers();
    }

    // Dispatch one client message; returns `true` to break the loop (a valid
    // shutdown or a disconnected channel). Guarded so a panic in one handler
    // can't take down the main loop (which would zombie the server: the
    // analysis thread keeps running but no one drives it).
    let handle_client = |state: &mut GlobalState, msg| -> Result<bool, DynError> {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(true);
                }
                guard("on_request", || state.on_request(req));
            }
            Message::Notification(note) => {
                guard("on_notification", || state.on_notification(note));
            }
            Message::Response(_) => {}
        }
        Ok(false)
    };

    loop {
        // Drain queued client messages before servicing outbound results, so a
        // `$/cancelRequest` already in the channel is applied before a read
        // reply waiting in `out_rx` — cancellation stays prompt, and a cancel
        // sent right after its request reliably beats the reply. Diagnostics and
        // read replies are version-gated, so deferring them a beat is safe.
        match connection.receiver.try_recv() {
            Ok(msg) => {
                if handle_client(&mut state, msg)? {
                    break;
                }
                continue;
            }
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        select! {
            recv(connection.receiver) -> msg => {
                let Ok(msg) = msg else { break };
                if handle_client(&mut state, msg)? {
                    break;
                }
            }
            recv(out_rx) -> outbound => {
                let Ok(outbound) = outbound else { break };
                guard("on_outbound", || state.on_outbound(outbound));
            }
        }
    }

    // Dropping `state` drops `analysis_tx`/`read_tx` → the analysis thread's
    // recv disconnects → it exits and drops the db. The library loader is
    // detached; it ends on its own (or when its send fails after teardown).
    drop(state);
    let _ = analysis_handle.join();
    Ok(())
}

/// A signal from the main loop to the workspace harvester thread.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HarvestSignal {
    /// A source file changed on disk (a save, or a watched create, change, or
    /// delete): re-harvest the workspace package owning it, if any.
    Source(PathBuf),
    /// An environment file (a `Project.toml` or `Manifest.toml` flavor)
    /// changed: re-resolve every workspace environment and re-harvest from
    /// scratch.
    Environment,
}

/// Resolve the Julia environment of every workspace root, harvest the merged
/// library on a detached background thread, then stay alive serving harvest
/// signals: a source signal re-harvests the workspace package owning the file,
/// and an environment signal starts the resolve-and-harvest cycle over (a
/// `Pkg.add`, or a created or deleted `Project.toml`, must reshape the whole
/// library).
///
/// Only runs when the client provided at least one workspace root: without one
/// there is no project to resolve against (a single loose file), and resolving
/// the machine's default environment would harvest all of Base for no benefit —
/// notably in the in-memory server tests, which open no folder. Best-effort: an
/// unresolved environment or harvest failure simply leaves the library empty
/// (or without that folder's contribution), and the failure is reported as a
/// diagnostic on the file that caused it.
///
/// The root set is fixed for the session (`didChangeWorkspaceFolders` is not
/// handled), so a folder never leaves and there is no per-folder teardown.
fn spawn_workspace_harvester(
    workspace_roots: Vec<PathBuf>,
    library_tx: crossbeam_channel::Sender<LibraryMessage>,
    signal_rx: crossbeam_channel::Receiver<HarvestSignal>,
    progress_sender: Option<crossbeam_channel::Sender<Message>>,
    out_tx: crossbeam_channel::Sender<Outbound>,
    encoding: PositionEncoding,
) {
    if workspace_roots.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("fatou-index-loader".to_string())
        .spawn(move || {
            let progress = HarvestProgress::new(progress_sender);
            // The index pool: a dedicated rayon pool for the parallel harvest,
            // capped below the read pool (see `index_pool_size`) so a cold-cache
            // fan-out cannot saturate every core and starve reads. Built once and
            // reused across every re-resolve.
            let index_pool = match rayon::ThreadPoolBuilder::new()
                .num_threads(index_pool_size())
                .thread_name(|i| format!("fatou-index-{i}"))
                .build()
            {
                Ok(pool) => pool,
                Err(err) => {
                    log::error!("failed to build index pool: {err}");
                    return; // Best-effort: the server runs without a library index.
                }
            };
            // The on-disk index cache, or `None` when no cache directory could be
            // resolved — harvesting is then still parallel, just uncached.
            let cache = IndexCache::open();
            if cache.is_none() {
                log::debug!("index cache disabled: no cache directory resolved");
            }
            // The environment files that carried a diagnostic on the previous
            // pass, so a fixed one can be cleared exactly once. Declared outside
            // the loop: one pass in, one full replacement of the published set
            // out.
            let mut published_env_files: HashSet<Uri> = HashSet::new();
            'resolve: loop {
                // Report the full pass (initial resolve, and every re-resolve a
                // `continue 'resolve` restarts) as one begin/report/end cycle.
                progress.begin("Indexing Julia environment", "Resolving environment");
                // One environment per folder, deduped on the resolved project file:
                // two folders under one project (or a user-set `JULIA_PROJECT`,
                // which wins over every folder's walk-up) collapse to one.
                let mut envs = Vec::new();
                let mut projects = std::collections::HashSet::new();
                let mut findings = EnvironmentFindings::new();
                for root in &workspace_roots {
                    let ctx = EnvContext::from_process(root.clone());
                    let env = match crate::environment::resolve(&ctx) {
                        Ok(Some(env)) => env,
                        Ok(None) => continue,
                        Err(err) => {
                            // Best-effort is unchanged — this folder still
                            // contributes nothing to the library. What is new is
                            // that the user is told why, on the file at fault,
                            // instead of the failure being swallowed. Two roots
                            // that walk up to the same broken file name it once.
                            let (path, diags) = resolve_failure_diagnostics(&err, encoding, |p| {
                                std::fs::read_to_string(p).ok()
                            });
                            findings.entry(path).or_insert(diags);
                            continue;
                        }
                    };
                    if projects.insert(normalize_path(&env.project_file)) {
                        findings.extend(environment_diagnostics(&env, encoding, |path| {
                            std::fs::read_to_string(path).ok()
                        }));
                        envs.push(env);
                    }
                }
                publish_environment_diagnostics(&out_tx, findings, &mut published_env_files);
                // An empty resolve still sends: a deleted `Project.toml` must clear
                // the previously harvested library (and the first send with nothing
                // resolved is a cheap no-op harvest).
                let devs = dev_packages(&envs);
                progress.report("Indexing Base and packages");
                let library = harvest_libraries_parallel(&envs, cache.as_ref(), &index_pool);
                let indexed = library.packages.len();
                if library_tx.send(LibraryMessage::Full(library)).is_err() {
                    return; // The analysis thread is gone; stop harvesting.
                }
                progress.end(&format!("Indexed {indexed} packages"));

                // With packages under development, re-harvest the one whose files a
                // source signal touches (a `src/` prefix check, longest prefix
                // winning for nested folders — the same rule as
                // `workspace_package_for`). Signals elsewhere, and every source
                // signal when no folder is a package, are ignored.
                let prefixes: Vec<(crate::environment::DevPackage, PathBuf)> = devs
                    .into_iter()
                    .map(|dev| {
                        let src = normalize_path(&dev.root.join("src"));
                        (dev, src)
                    })
                    .collect();
                // The last index sent per package, so an unchanged re-harvest is
                // skipped. A save touching a `src/` file re-harvests, but body-only
                // and formatting-only edits leave the public API identical;
                // resending then would force a `set_package_index` db write that
                // needlessly cancels in-flight diagnostics (the write races the
                // very format-on-save that triggered the save). Only send on a real
                // change.
                let mut last: std::collections::HashMap<String, Arc<PackageIndex>> =
                    std::collections::HashMap::new();
                while let Ok(signal) = signal_rx.recv() {
                    let changed = match signal {
                        HarvestSignal::Environment => {
                            // Coalesce the burst (`Pkg.add` rewrites the project and
                            // manifest together; an editor save and its watched
                            // event double-fire): drain everything queued — the
                            // full re-resolve subsumes any drained source signal.
                            while signal_rx.try_recv().is_ok() {}
                            continue 'resolve;
                        }
                        HarvestSignal::Source(path) => normalize_path(&path),
                    };
                    let Some((dev, _)) = prefixes
                        .iter()
                        .filter(|(_, src)| changed.starts_with(src))
                        .max_by_key(|(_, src)| src.components().count())
                    else {
                        continue;
                    };
                    // A matched source signal re-harvests one package: a short
                    // cycle so the editor shows the same spinner as a full pass.
                    progress.begin(
                        "Indexing Julia environment",
                        &format!("Re-indexing {}", dev.name),
                    );
                    let index = Arc::new(harvest_workspace(dev));
                    progress.end(&format!("Re-indexed {}", dev.name));
                    if last.get(&dev.name) == Some(&index) {
                        continue;
                    }
                    last.insert(dev.name.clone(), Arc::clone(&index));
                    if library_tx
                        .send(LibraryMessage::Package {
                            name: dev.name.clone(),
                            index,
                        })
                        .is_err()
                    {
                        return; // The analysis thread is gone; stop harvesting.
                    }
                }
                return; // The main loop is gone; stop harvesting.
            }
        });
    // A spawn failure is non-fatal: the server runs without a library index.
    debug_assert!(spawned.is_ok(), "spawn index loader thread");
    drop(spawned);
}

/// Send one [`Outbound::EnvironmentDiagnostics`] per file with findings, plus an
/// empty one for every file that carried findings on the previous pass and has
/// none now — the clear-on-fix half, mirroring the analysis thread's
/// `published_graph_files`. `published` is replaced with this pass's set, so a
/// file already cleared is not cleared again on every later re-resolve.
///
/// A dead channel is ignored rather than fatal: the harvester is detached, so
/// after teardown the receiver is simply gone, and unlike a dead library
/// channel it does not make the remaining harvest pointless.
fn publish_environment_diagnostics(
    out_tx: &crossbeam_channel::Sender<Outbound>,
    findings: EnvironmentFindings,
    published: &mut HashSet<Uri>,
) {
    let mut now = HashSet::new();
    for (path, diags) in findings {
        let Some(uri) = super::uri::from_path(&path) else {
            continue;
        };
        now.insert(uri.clone());
        let _ = out_tx.send(Outbound::EnvironmentDiagnostics { uri, diags });
    }
    for uri in published.difference(&now) {
        let _ = out_tx.send(Outbound::EnvironmentDiagnostics {
            uri: uri.clone(),
            diags: Vec::new(),
        });
    }
    *published = now;
}

#[cfg(test)]
mod tests {
    use lsp_types::GeneralClientCapabilities;

    use super::*;

    /// A file's findings, keyed for [`publish_environment_diagnostics`].
    fn findings_for(path: &str) -> EnvironmentFindings {
        let mut findings = EnvironmentFindings::new();
        findings.insert(
            PathBuf::from(path),
            vec![lsp_types::Diagnostic {
                message: "broken".to_string(),
                ..Default::default()
            }],
        );
        findings
    }

    /// The sends of one pass, as `(uri, is_empty)` pairs.
    fn drain(rx: &crossbeam_channel::Receiver<Outbound>) -> Vec<(String, bool)> {
        let mut seen = Vec::new();
        while let Ok(outbound) = rx.try_recv() {
            match outbound {
                Outbound::EnvironmentDiagnostics { uri, diags } => {
                    seen.push((uri.as_str().to_string(), diags.is_empty()));
                }
                other => panic!("unexpected outbound: {:?}", std::mem::discriminant(&other)),
            }
        }
        seen
    }

    /// A fixed problem must clear exactly once. Re-clearing on every later
    /// re-resolve would publish an empty diagnostic set per harvest forever,
    /// which is the classic shape of this bug.
    #[test]
    fn environment_diagnostics_clear_once_when_fixed() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut published = HashSet::new();
        let path = if cfg!(windows) {
            r"C:\work\Project.toml"
        } else {
            "/work/Project.toml"
        };

        publish_environment_diagnostics(&tx, findings_for(path), &mut published);
        let first = drain(&rx);
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(!first[0].1, "the finding publishes non-empty");
        assert_eq!(published.len(), 1);

        // Pass two: the problem is gone, so the file is cleared.
        publish_environment_diagnostics(&tx, EnvironmentFindings::new(), &mut published);
        let second = drain(&rx);
        assert_eq!(second.len(), 1, "{second:?}");
        assert!(second[0].1, "the clear publishes empty");
        assert_eq!(second[0].0, first[0].0, "the same URI");
        assert!(published.is_empty());

        // Pass three: nothing left to clear.
        publish_environment_diagnostics(&tx, EnvironmentFindings::new(), &mut published);
        assert!(drain(&rx).is_empty(), "a cleared file is not cleared again");
    }

    fn caps_offering(encodings: Option<Vec<PositionEncodingKind>>) -> ClientCapabilities {
        ClientCapabilities {
            general: Some(GeneralClientCapabilities {
                position_encodings: encodings,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Advertised globally, like every other per-kind capability here, and with
    /// no resolve step: the label and tooltip are both built from the library
    /// map already in memory.
    #[test]
    fn advertises_inlay_hints_without_a_resolve_step() {
        let caps = capabilities_json(PositionEncoding::Utf16, false);
        assert_eq!(
            caps["inlayHintProvider"]["resolveProvider"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn advertises_file_operation_rename_filters() {
        let caps = capabilities_json(PositionEncoding::Utf16, false);
        let operations = &caps["workspace"]["fileOperations"];
        for kind in ["willRename", "didRename"] {
            let filters = operations[kind]["filters"]
                .as_array()
                .unwrap_or_else(|| panic!("{kind} filters"));
            let described: Vec<(&str, &str)> = filters
                .iter()
                .map(|filter| {
                    (
                        filter["pattern"]["glob"].as_str().expect("a glob"),
                        filter["pattern"]["matches"].as_str().expect("a kind"),
                    )
                })
                .collect();
            assert_eq!(described, [("**/*.jl", "file"), ("**", "folder")]);
            assert!(filters.iter().all(|filter| filter["scheme"] == "file"));
        }
    }

    #[test]
    fn negotiation_defaults_to_utf16() {
        // No `general` capabilities at all, and `general` without an
        // `positionEncodings` offer, both fall back to the mandatory default.
        let none = ClientCapabilities::default();
        assert_eq!(negotiate_position_encoding(&none), PositionEncoding::Utf16);
        assert_eq!(
            negotiate_position_encoding(&caps_offering(None)),
            PositionEncoding::Utf16
        );
        assert_eq!(
            negotiate_position_encoding(&caps_offering(Some(vec![
                PositionEncodingKind::UTF16,
                PositionEncodingKind::UTF32,
            ]))),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn negotiation_prefers_offered_utf8() {
        assert_eq!(
            negotiate_position_encoding(&caps_offering(Some(vec![
                PositionEncodingKind::UTF16,
                PositionEncodingKind::UTF8,
            ]))),
            PositionEncoding::Utf8
        );
    }

    fn folder(uri: &str) -> lsp_types::WorkspaceFolder {
        lsp_types::WorkspaceFolder {
            uri: uri.parse().unwrap(),
            name: String::new(),
        }
    }

    /// The platform path a `file:` URI decodes to, so assertions hold on
    /// Windows too.
    fn path_of(uri: &str) -> PathBuf {
        to_path(&uri.parse().unwrap()).unwrap()
    }

    #[test]
    fn workspace_roots_takes_every_folder_in_client_order() {
        let params = InitializeParams {
            workspace_folders: Some(vec![folder("file:///work/b"), folder("file:///work/a")]),
            ..Default::default()
        };
        assert_eq!(
            workspace_roots(&params),
            vec![path_of("file:///work/b"), path_of("file:///work/a")]
        );
    }

    #[test]
    fn workspace_roots_dedups_equivalent_folders() {
        let params = InitializeParams {
            workspace_folders: Some(vec![
                folder("file:///work/a"),
                folder("file:///work/./a"),
                folder("file:///work/b"),
            ]),
            ..Default::default()
        };
        assert_eq!(
            workspace_roots(&params),
            vec![path_of("file:///work/a"), path_of("file:///work/b")]
        );
    }

    #[test]
    fn workspace_roots_falls_back_to_root_uri() {
        #[allow(deprecated)]
        let params = InitializeParams {
            root_uri: Some("file:///work/a".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(workspace_roots(&params), vec![path_of("file:///work/a")]);

        // Folders, when present, win over the deprecated root_uri; an empty
        // folder list falls back too.
        #[allow(deprecated)]
        let both = InitializeParams {
            workspace_folders: Some(vec![folder("file:///work/b")]),
            root_uri: Some("file:///work/a".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(workspace_roots(&both), vec![path_of("file:///work/b")]);
        #[allow(deprecated)]
        let empty_folders = InitializeParams {
            workspace_folders: Some(Vec::new()),
            root_uri: Some("file:///work/a".parse().unwrap()),
            ..Default::default()
        };
        assert_eq!(
            workspace_roots(&empty_folders),
            vec![path_of("file:///work/a")]
        );
    }

    #[test]
    fn no_folders_yields_no_roots() {
        assert!(workspace_roots(&InitializeParams::default()).is_empty());
    }

    #[test]
    fn pull_diagnostics_require_the_client_capability() {
        assert!(!supports_pull_diagnostics(&ClientCapabilities::default()));
        let caps = ClientCapabilities {
            text_document: Some(lsp_types::TextDocumentClientCapabilities {
                diagnostic: Some(lsp_types::DiagnosticClientCapabilities::default()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(supports_pull_diagnostics(&caps));

        // The provider is advertised exactly when the client pulls.
        assert!(
            server_capabilities(PositionEncoding::Utf16, true)
                .diagnostic_provider
                .is_some()
        );
        assert!(
            server_capabilities(PositionEncoding::Utf16, false)
                .diagnostic_provider
                .is_none()
        );
    }

    /// The type-hierarchy capability rides the serialized JSON (lsp-types 0.97
    /// has no struct field for it); the injection must not clobber the
    /// struct-borne capabilities around it.
    #[test]
    fn type_hierarchy_capability_is_injected_into_the_json() {
        let capabilities = capabilities_json(PositionEncoding::Utf16, false);
        assert_eq!(
            capabilities["typeHierarchyProvider"],
            serde_json::json!(true)
        );
        assert_eq!(
            capabilities["callHierarchyProvider"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn diagnostic_refresh_requires_the_client_capability() {
        assert!(!supports_diagnostic_refresh(&ClientCapabilities::default()));
        let caps = ClientCapabilities {
            workspace: Some(lsp_types::WorkspaceClientCapabilities {
                diagnostic: Some(lsp_types::DiagnosticWorkspaceClientCapabilities {
                    refresh_support: Some(true),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(supports_diagnostic_refresh(&caps));
    }

    #[test]
    fn inlay_hint_refresh_requires_the_client_capability() {
        assert!(!supports_inlay_hint_refresh(&ClientCapabilities::default()));
        let caps = ClientCapabilities {
            workspace: Some(lsp_types::WorkspaceClientCapabilities {
                inlay_hint: Some(lsp_types::InlayHintWorkspaceClientCapabilities {
                    refresh_support: Some(true),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(supports_inlay_hint_refresh(&caps));
    }

    #[test]
    fn watcher_registration_requires_the_client_capability() {
        assert!(!supports_watched_files_registration(
            &ClientCapabilities::default()
        ));
        let caps = ClientCapabilities {
            workspace: Some(lsp_types::WorkspaceClientCapabilities {
                did_change_watched_files: Some(
                    lsp_types::DidChangeWatchedFilesClientCapabilities {
                        dynamic_registration: Some(true),
                        relative_pattern_support: None,
                    },
                ),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(supports_watched_files_registration(&caps));
    }
}
