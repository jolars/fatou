//! Server entry points: the initialize handshake, advertised capabilities, and
//! the main event loop that wires the channels, pools, and threads together.

use std::error::Error;
use std::path::PathBuf;

use crossbeam_channel::select;
use lsp_server::{Connection, Message};
use lsp_types::{
    ClientCapabilities, CompletionOptions, FoldingRangeProviderCapability, HoverProviderCapability,
    InitializeParams, OneOf, PositionEncodingKind, RenameOptions, SelectionRangeProviderCapability,
    SemanticTokensFullOptions, SemanticTokensOptions, ServerCapabilities, SignatureHelpOptions,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions,
};

use std::sync::Arc;

use crate::environment::EnvContext;
use crate::incremental::normalize_path;
use crate::index::{harvest_library, harvest_workspace};
use crate::text::PositionEncoding;

use super::analysis_thread::{AnalysisRequest, LibraryMessage, spawn_analysis_thread};
use super::read_jobs::ReadJob;
use super::semantic_tokens::legend;
use super::state::{GlobalState, Outbound};
use super::task_pool::{TaskPool, read_pool_size};
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
    let workspace_root = workspace_root(&params);
    let result = serde_json::json!({ "capabilities": server_capabilities(encoding) });
    connection.initialize_finish(id, result)?;
    main_loop(connection, encoding, workspace_root)
}

/// The workspace root to resolve the Julia environment against: the first
/// workspace folder, then the (deprecated) `root_uri`, decoded to a path. `None`
/// when the client opened no folder (a single loose file); the loader then falls
/// back to the process working directory.
fn workspace_root(params: &InitializeParams) -> Option<PathBuf> {
    #[allow(deprecated)]
    params
        .workspace_folders
        .as_ref()
        .and_then(|folders| folders.first())
        .map(|folder| &folder.uri)
        .or(params.root_uri.as_ref())
        .and_then(to_path)
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

fn server_capabilities(encoding: PositionEncoding) -> ServerCapabilities {
    ServerCapabilities {
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
        document_formatting_provider: Some(OneOf::Left(true)),
        document_range_formatting_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            // `.` opens member completion, `@` opens macro completion.
            trigger_characters: Some(vec![".".to_string(), "@".to_string()]),
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
        signature_help_provider: Some(SignatureHelpOptions {
            // `(` opens signature help, `,` (also a retrigger) advances the
            // active parameter.
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: Some(vec![",".to_string()]),
            work_done_progress_options: Default::default(),
        }),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(
            SemanticTokensOptions {
                work_done_progress_options: Default::default(),
                legend: legend(),
                range: None,
                full: Some(SemanticTokensFullOptions::Bool(true)),
            }
            .into(),
        ),
        ..Default::default()
    }
}

/// The main event loop: dispatch incoming JSON-RPC messages and analysis
/// results. Owns no salsa database (see the module docs); joins the analysis
/// thread before returning.
fn main_loop(
    connection: &Connection,
    encoding: PositionEncoding,
    workspace_root: Option<PathBuf>,
) -> Result<(), DynError> {
    let (out_tx, out_rx) = crossbeam_channel::unbounded::<Outbound>();
    let (analysis_tx, analysis_rx) = crossbeam_channel::unbounded::<AnalysisRequest>();
    let (read_tx, read_rx) = crossbeam_channel::unbounded::<ReadJob>();
    let (library_tx, library_rx) = crossbeam_channel::unbounded::<LibraryMessage>();
    // Save signals from the main loop to the workspace harvester: the saved
    // file's path (the harvester ignores saves outside the workspace package).
    let (save_tx, save_rx) = crossbeam_channel::unbounded::<PathBuf>();
    // Close signals from the main loop to the analysis thread: the closed file's
    // path, reverted to on-disk text so a discarded buffer leaves the index.
    let (close_tx, close_rx) = crossbeam_channel::unbounded::<PathBuf>();

    // Resolve the environment and harvest its packages off the event loop: it
    // walks the filesystem and parses all of Base, so it must not block the
    // handshake (nor shutdown — the thread is detached). The result is swapped
    // into the db when it lands; every feature stays usable in the meantime, and
    // library go-to-definition/completion start answering once it arrives. The
    // same thread re-harvests the workspace package on each save signal.
    spawn_workspace_harvester(workspace_root, library_tx, save_rx);

    // The read pool serves latency-sensitive work (formatting, the analysis
    // read-phase). Its workers must outlive both `state` and the analysis
    // thread; the drop order at the end of this function guarantees that.
    let read_pool = TaskPool::new("fatou-lsp-read", read_pool_size());
    let analysis_handle = spawn_analysis_thread(
        analysis_rx,
        read_rx,
        library_rx,
        close_rx,
        out_tx,
        read_pool.spawner(),
        encoding,
    );

    let mut state = GlobalState::new(
        connection.sender.clone(),
        analysis_tx,
        read_tx,
        save_tx,
        close_tx,
        encoding,
    );

    loop {
        select! {
            recv(connection.receiver) -> msg => {
                let Ok(msg) = msg else { break };
                match msg {
                    Message::Request(req) => {
                        if connection.handle_shutdown(&req)? {
                            break;
                        }
                        state.on_request(req);
                    }
                    Message::Notification(note) => state.on_notification(note),
                    Message::Response(_) => {}
                }
            }
            recv(out_rx) -> outbound => {
                let Ok(outbound) = outbound else { break };
                state.on_outbound(outbound);
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

/// Resolve the Julia environment for `workspace_root`, harvest its library on a
/// detached background thread, then stay alive re-harvesting the workspace
/// package whenever a save signal names one of its files.
///
/// Only runs when the client provided a workspace root: without one there is no
/// project to resolve against (a single loose file), and resolving the machine's
/// default environment would harvest all of Base for no benefit — notably in the
/// in-memory server tests, which open no folder. Best-effort: an unresolved
/// environment or harvest failure simply leaves the library empty.
fn spawn_workspace_harvester(
    workspace_root: Option<PathBuf>,
    library_tx: crossbeam_channel::Sender<LibraryMessage>,
    save_rx: crossbeam_channel::Receiver<PathBuf>,
) {
    let Some(root) = workspace_root else { return };
    let spawned = std::thread::Builder::new()
        .name("fatou-index-loader".to_string())
        .spawn(move || {
            let ctx = EnvContext::from_process(root);
            let Ok(Some(env)) = crate::environment::resolve(&ctx) else {
                return;
            };
            let dev = env.dev_package();
            let _ = library_tx.send(LibraryMessage::Full(harvest_library(&env)));

            // With a package under development, re-harvest it on each save that
            // touches one of its files (a `src/` prefix check). Saves elsewhere,
            // and every save when the workspace is not a package, are ignored.
            let Some(dev) = dev else { return };
            let src = normalize_path(&dev.root.join("src"));
            while let Ok(saved) = save_rx.recv() {
                if !normalize_path(&saved).starts_with(&src) {
                    continue;
                }
                let index = Arc::new(harvest_workspace(&dev));
                if library_tx
                    .send(LibraryMessage::Package {
                        name: dev.name.clone(),
                        index,
                    })
                    .is_err()
                {
                    break; // The analysis thread is gone; stop harvesting.
                }
            }
        });
    // A spawn failure is non-fatal: the server runs without a library index.
    debug_assert!(spawned.is_ok(), "spawn index loader thread");
    drop(spawned);
}

#[cfg(test)]
mod tests {
    use lsp_types::GeneralClientCapabilities;

    use super::*;

    fn caps_offering(encodings: Option<Vec<PositionEncodingKind>>) -> ClientCapabilities {
        ClientCapabilities {
            general: Some(GeneralClientCapabilities {
                position_encodings: encodings,
                ..Default::default()
            }),
            ..Default::default()
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
}
