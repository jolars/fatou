//! Server entry points: the initialize handshake, advertised capabilities, and
//! the main event loop that wires the channels, pools, and threads together.

use std::error::Error;

use crossbeam_channel::select;
use lsp_server::{Connection, Message};
use lsp_types::{OneOf, ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind};

use super::analysis_thread::{AnalysisRequest, spawn_analysis_thread};
use super::read_jobs::ReadJob;
use super::state::{GlobalState, Outbound};
use super::task_pool::{TaskPool, read_pool_size};

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
pub fn serve(connection: &Connection) -> Result<(), DynError> {
    connection.initialize(serde_json::to_value(server_capabilities())?)?;
    main_loop(connection)
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        document_formatting_provider: Some(OneOf::Left(true)),
        ..Default::default()
    }
}

/// The main event loop: dispatch incoming JSON-RPC messages and analysis
/// results. Owns no salsa database (see the module docs); joins the analysis
/// thread before returning.
fn main_loop(connection: &Connection) -> Result<(), DynError> {
    let (out_tx, out_rx) = crossbeam_channel::unbounded::<Outbound>();
    let (analysis_tx, analysis_rx) = crossbeam_channel::unbounded::<AnalysisRequest>();
    let (read_tx, read_rx) = crossbeam_channel::unbounded::<ReadJob>();

    // The read pool serves latency-sensitive work (formatting, the analysis
    // read-phase). Its workers must outlive both `state` and the analysis
    // thread; the drop order at the end of this function guarantees that.
    let read_pool = TaskPool::new("fatou-lsp-read", read_pool_size());
    let analysis_handle = spawn_analysis_thread(analysis_rx, read_rx, out_tx, read_pool.spawner());

    let mut state = GlobalState::new(connection.sender.clone(), analysis_tx, read_tx);

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
    // recv disconnects → it exits and drops the db.
    drop(state);
    let _ = analysis_handle.join();
    Ok(())
}
