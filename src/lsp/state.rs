//! Main-loop state: open documents, request/notification dispatch, and the
//! version-gated diagnostic publish.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Sender;
use lsp_server::{ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument,
    Notification as NotificationTrait, PublishDiagnostics,
};
use lsp_types::request::{Formatting, Request as RequestTrait};
use lsp_types::{
    Diagnostic, DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentFormattingParams, PublishDiagnosticsParams, Uri,
};

use crate::formatter::FormatStyle;
use crate::text::apply_content_changes;

use super::analysis_thread::AnalysisRequest;
use super::read_jobs::ReadJob;
use super::uri;

/// An open document's live buffer and client-reported version.
#[derive(Debug, Clone)]
struct Document {
    text: String,
    version: i32,
}

/// Messages from the analysis thread back to the main loop.
pub(crate) enum Outbound {
    /// Diagnostics for `uri` at `version`; published only if still current.
    Diagnostics {
        uri: Uri,
        version: i32,
        diags: Vec<Diagnostic>,
    },
}

pub(crate) struct GlobalState {
    documents: HashMap<Uri, Document>,
    sender: Sender<Message>,
    analysis_tx: Sender<AnalysisRequest>,
    /// Channel to the analysis thread for read-only jobs (formatting). The
    /// analysis thread owns the salsa db, so it mints a short-lived clone per
    /// job and runs the read off-thread against the cached parse. See
    /// [`run_read`](super::read_jobs::run_read).
    read_tx: Sender<ReadJob>,
}

impl GlobalState {
    pub(crate) fn new(
        sender: Sender<Message>,
        analysis_tx: Sender<AnalysisRequest>,
        read_tx: Sender<ReadJob>,
    ) -> Self {
        Self {
            documents: HashMap::new(),
            sender,
            analysis_tx,
            read_tx,
        }
    }

    pub(crate) fn on_request(&mut self, req: Request) {
        match req.method.as_str() {
            Formatting::METHOD => self.on_formatting(req),
            _ => {
                let resp = Response::new_err(
                    req.id,
                    ErrorCode::MethodNotFound as i32,
                    format!("unhandled method: {}", req.method),
                );
                let _ = self.sender.send(Message::Response(resp));
            }
        }
    }

    fn on_formatting(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<DocumentFormattingParams>(Formatting::METHOD) else {
            self.respond_err(id, "invalid formatting params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        // Style resolution (fatou.toml discovery, editor-pushed settings) is a
        // later roadmap item; the LSP formats with the defaults for now.
        self.dispatch_read(ReadJob::Format {
            id,
            path: path_for(&uri),
            text,
            style: FormatStyle::default(),
            sender: self.sender.clone(),
        });
    }

    /// Hand a read job to the analysis thread; if its channel is gone
    /// (shutdown), answer with `null` so the client is not left waiting.
    fn dispatch_read(&self, job: ReadJob) {
        if let Err(crossbeam_channel::SendError(job)) = self.read_tx.send(job) {
            let (id, sender) = job.into_reply_parts();
            let _ = sender.send(Message::Response(Response::new_ok(
                id,
                serde_json::Value::Null,
            )));
        }
    }

    pub(crate) fn on_notification(&mut self, note: Notification) {
        match note.method.as_str() {
            DidOpenTextDocument::METHOD => {
                if let Ok(params) =
                    note.extract::<DidOpenTextDocumentParams>(DidOpenTextDocument::METHOD)
                {
                    let uri = params.text_document.uri;
                    self.documents.insert(
                        uri.clone(),
                        Document {
                            text: params.text_document.text,
                            version: params.text_document.version,
                        },
                    );
                    self.send_analysis(uri);
                }
            }
            DidChangeTextDocument::METHOD => {
                if let Ok(params) =
                    note.extract::<DidChangeTextDocumentParams>(DidChangeTextDocument::METHOD)
                {
                    let uri = params.text_document.uri;
                    // A change for a never-opened document has no buffer to
                    // splice into; drop it.
                    let Some(doc) = self.documents.get_mut(&uri) else {
                        return;
                    };
                    apply_content_changes(&mut doc.text, params.content_changes);
                    doc.version = params.text_document.version;
                    self.send_analysis(uri);
                }
            }
            DidCloseTextDocument::METHOD => {
                if let Ok(params) =
                    note.extract::<DidCloseTextDocumentParams>(DidCloseTextDocument::METHOD)
                {
                    let uri = params.text_document.uri;
                    self.documents.remove(&uri);
                    // Tell the client to clear stale diagnostics.
                    self.publish(uri, Vec::new(), None);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn on_outbound(&mut self, outbound: Outbound) {
        match outbound {
            Outbound::Diagnostics {
                uri,
                version,
                diags,
            } => {
                // Stale results (a newer edit superseded this analysis, or the
                // document closed) are dropped: the newer version's analysis
                // will produce its own `Outbound`.
                if !matches!(self.documents.get(&uri), Some(d) if d.version == version) {
                    return;
                }
                self.publish(uri, diags, Some(version));
            }
        }
    }

    /// Send an analysis request for `uri`'s current buffer to the analysis
    /// thread.
    fn send_analysis(&mut self, uri: Uri) {
        let Some(doc) = self.documents.get(&uri) else {
            return;
        };
        let _ = self.analysis_tx.send(AnalysisRequest {
            path: path_for(&uri),
            text: doc.text.clone(),
            version: doc.version,
            uri,
        });
    }

    fn publish(&self, uri: Uri, diagnostics: Vec<Diagnostic>, version: Option<i32>) {
        let params = PublishDiagnosticsParams {
            uri,
            diagnostics,
            version,
        };
        let note = Notification::new(PublishDiagnostics::METHOD.to_string(), params);
        let _ = self.sender.send(Message::Notification(note));
    }

    fn respond_ok(&self, id: RequestId, value: serde_json::Value) {
        let _ = self
            .sender
            .send(Message::Response(Response::new_ok(id, value)));
    }

    fn respond_err(&self, id: RequestId, message: &str) {
        let resp = Response::new_err(id, ErrorCode::InvalidParams as i32, message.to_string());
        let _ = self.sender.send(Message::Response(resp));
    }
}

/// The filesystem path the db tracks `uri` under. Non-`file` URIs (e.g. an
/// editor's untitled buffer) share a synthetic fallback path.
fn path_for(uri: &Uri) -> PathBuf {
    uri::to_path(uri).unwrap_or_else(|| PathBuf::from("untitled.jl"))
}
