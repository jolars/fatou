//! Main-loop state: open documents, request/notification dispatch, and the
//! version-gated diagnostic publish.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::Sender;
use lsp_server::{ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    Cancel, DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles,
    DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument, LogMessage,
    Notification as NotificationTrait, PublishDiagnostics,
};
use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, Completion, DocumentDiagnosticRequest, DocumentHighlightRequest,
    DocumentLinkRequest, DocumentSymbolRequest, FoldingRangeRequest, Formatting, GotoDefinition,
    HoverRequest, PrepareRenameRequest, RangeFormatting, References, RegisterCapability, Rename,
    Request as RequestTrait, ResolveCompletionItem, SelectionRangeRequest,
    SemanticTokensFullDeltaRequest, SemanticTokensFullRequest, SignatureHelpRequest,
    TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes,
    WorkspaceDiagnosticRefresh, WorkspaceSymbolRequest,
};
use lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CancelParams, CodeActionParams, CompletionItem, CompletionParams, Diagnostic,
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentDiagnosticParams,
    DocumentFormattingParams, DocumentHighlightParams, DocumentLinkParams,
    DocumentRangeFormattingParams, DocumentSymbolParams, FileSystemWatcher, FoldingRangeParams,
    GlobPattern, GotoDefinitionParams, HoverParams, LogMessageParams, MessageType, NumberOrString,
    PublishDiagnosticsParams, ReferenceParams, Registration, RegistrationParams, RenameParams,
    SelectionRangeParams, SemanticTokensDeltaParams, SemanticTokensParams, SignatureHelpParams,
    TextDocumentPositionParams, TypeHierarchyPrepareParams, TypeHierarchySubtypesParams,
    TypeHierarchySupertypesParams, Uri, WorkspaceSymbolParams,
};

use crate::config::CONFIG_FILE_NAME;
use crate::environment::is_environment_file;
use crate::text::{Edit, PositionEncoding, apply_content_changes};

use super::analysis_thread::AnalysisRequest;
use super::config::{ConfigStore, ResolvedConfig};
use super::read_jobs::{ReadJob, ReadReply};
use super::server::HarvestSignal;
use super::uri;

/// An open document's live buffer and client-reported version.
#[derive(Debug, Clone)]
struct Document {
    text: String,
    version: i32,
}

/// Messages from the analysis thread back to the main loop.
pub(crate) enum Outbound {
    /// Per-file parse diagnostics for `uri` at `version`; published only if still
    /// current (the open buffer is still at that version).
    Diagnostics {
        uri: Uri,
        version: i32,
        diags: Vec<Diagnostic>,
    },
    /// Project-level include-graph diagnostics (unresolved includes, cycles) for
    /// `uri`. Version-free: they attach to a member file that need not be open,
    /// and an empty list clears a file that no longer has any. Merged with the
    /// file's parse diagnostics before publishing (a single `publishDiagnostics`
    /// replaces *all* diagnostics for a URI).
    ProjectDiagnostics { uri: Uri, diags: Vec<Diagnostic> },
    /// A re-harvest changed the include graph: a pull-model client should
    /// re-pull its open documents (`workspace/diagnostic/refresh`). Sent once
    /// per harvest; the main loop forwards it only when the client supports
    /// both pull diagnostics and the refresh request.
    DiagnosticsRefresh,
    /// A finished read job (hover, format, …). The worker routes its response
    /// here instead of straight to the client so the main loop can version-gate
    /// it against the buffer the read was dispatched on (stale → `ContentModified`)
    /// and drop it when a `$/cancelRequest` already answered the request. Always
    /// a `Message::Response`; the request id rides inside it. See
    /// [`GlobalState::on_read_reply`].
    ReadReply { message: Message },
}

/// What an in-flight read was dispatched against, kept in
/// [`GlobalState::inflight_reads`] so its reply can be version-gated and a
/// `$/cancelRequest` can find and cancel it. A non-document read (e.g.
/// `workspace/symbol`) carries `None`: it is cancellable but never stale.
struct ReadMeta {
    uri: Option<Uri>,
    version: Option<i32>,
}

pub(crate) struct GlobalState {
    documents: HashMap<Uri, Document>,
    sender: Sender<Message>,
    /// The analysis thread's outbound channel, cloned into every dispatched
    /// [`ReadReply`] so a finished read job routes back here for version-gating
    /// (see [`Outbound::ReadReply`]) rather than answering the client directly.
    out_tx: Sender<Outbound>,
    /// Read requests dispatched but not yet answered, keyed by request id. An
    /// entry is inserted at dispatch and removed when the reply is gated and
    /// forwarded, or when `$/cancelRequest` cancels it. Its absence at reply
    /// time means the request was already answered (cancelled), so the reply is
    /// dropped — the main loop is the sole responder, so every id is answered
    /// exactly once.
    inflight_reads: HashMap<RequestId, ReadMeta>,
    analysis_tx: Sender<AnalysisRequest>,
    /// Channel to the analysis thread for read-only jobs (formatting). The
    /// analysis thread owns the salsa db, so it mints a short-lived clone per
    /// job and runs the read off-thread against the cached parse. See
    /// [`run_read`](super::read_jobs::run_read).
    read_tx: Sender<ReadJob>,
    /// Harvest signals to the workspace harvester: a changed source file's
    /// path (it re-harvests the workspace package owning the file) or an
    /// environment-file change (it re-resolves every workspace environment).
    harvest_tx: Sender<HarvestSignal>,
    /// Disk-sync signals to the analysis thread: a file's path, whose tracked
    /// input is reverted to on-disk text. Sent when a document closes (a
    /// discarded buffer must not linger in the reverse-occurrence index) and
    /// when a watched file changes outside any open buffer (the stale seeded
    /// text must catch up with disk).
    sync_tx: Sender<PathBuf>,
    /// The position encoding negotiated at initialize, fixed for the session.
    encoding: PositionEncoding,
    /// Whether the client pulls diagnostics (`textDocument/diagnostic`). When
    /// set, the per-edit push path is off for open documents (the pull report
    /// carries parse + lint + graph diagnostics); pushes remain only for files
    /// with no open buffer, which carry include-graph problems the client
    /// never pulls.
    pull_diagnostics: bool,
    /// Whether the client accepts `workspace/diagnostic/refresh`, the nudge to
    /// re-pull after a re-harvest changes the include graph.
    diagnostic_refresh: bool,
    /// Sequence number for server-to-client refresh requests, so each carries
    /// a fresh JSON-RPC id.
    refresh_seq: u64,
    /// The latest per-file parse diagnostics, kept so a project-diagnostic update
    /// can republish the union (a `publishDiagnostics` replaces *all* diagnostics
    /// for a URI). Cleared when a document closes.
    parse_diags: HashMap<Uri, Vec<Diagnostic>>,
    /// The latest include-graph diagnostics per file, kept so a parse-diagnostic
    /// update can republish the union. Set/cleared by the analysis thread on each
    /// re-harvest.
    graph_diags: HashMap<Uri, Vec<Diagnostic>>,
    /// Per-document configuration: discovered `fatou.toml` shadowing
    /// editor-pushed settings (see [`ConfigStore`]). Owned by the main loop
    /// alone; resolved config travels with each dispatched request.
    config: ConfigStore,
}

impl GlobalState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sender: Sender<Message>,
        out_tx: Sender<Outbound>,
        analysis_tx: Sender<AnalysisRequest>,
        read_tx: Sender<ReadJob>,
        harvest_tx: Sender<HarvestSignal>,
        sync_tx: Sender<PathBuf>,
        encoding: PositionEncoding,
        pull_diagnostics: bool,
        diagnostic_refresh: bool,
        initialization_options: Option<serde_json::Value>,
    ) -> Self {
        let (config, warnings) = ConfigStore::new(initialization_options);
        let state = Self {
            documents: HashMap::new(),
            parse_diags: HashMap::new(),
            graph_diags: HashMap::new(),
            sender,
            out_tx,
            inflight_reads: HashMap::new(),
            analysis_tx,
            read_tx,
            harvest_tx,
            sync_tx,
            encoding,
            pull_diagnostics,
            diagnostic_refresh,
            refresh_seq: 0,
            config,
        };
        state.log_warnings(warnings);
        state
    }

    pub(crate) fn on_request(&mut self, req: Request) {
        match req.method.as_str() {
            CodeActionRequest::METHOD => self.on_code_action(req),
            DocumentDiagnosticRequest::METHOD => self.on_document_diagnostic(req),
            Formatting::METHOD => self.on_formatting(req),
            RangeFormatting::METHOD => self.on_range_formatting(req),
            DocumentSymbolRequest::METHOD => self.on_document_symbols(req),
            WorkspaceSymbolRequest::METHOD => self.on_workspace_symbols(req),
            FoldingRangeRequest::METHOD => self.on_folding_ranges(req),
            DocumentLinkRequest::METHOD => self.on_document_links(req),
            SelectionRangeRequest::METHOD => self.on_selection_ranges(req),
            SemanticTokensFullRequest::METHOD => self.on_semantic_tokens_full(req),
            SemanticTokensFullDeltaRequest::METHOD => self.on_semantic_tokens_full_delta(req),
            Completion::METHOD => self.on_completion(req),
            ResolveCompletionItem::METHOD => self.on_completion_resolve(req),
            HoverRequest::METHOD => self.on_hover(req),
            SignatureHelpRequest::METHOD => self.on_signature_help(req),
            GotoDefinition::METHOD => self.on_definition(req),
            References::METHOD => self.on_references(req),
            DocumentHighlightRequest::METHOD => self.on_document_highlight(req),
            PrepareRenameRequest::METHOD => self.on_prepare_rename(req),
            Rename::METHOD => self.on_rename(req),
            CallHierarchyPrepare::METHOD => self.on_prepare_call_hierarchy(req),
            CallHierarchyIncomingCalls::METHOD => self.on_call_hierarchy_incoming(req),
            CallHierarchyOutgoingCalls::METHOD => self.on_call_hierarchy_outgoing(req),
            TypeHierarchyPrepare::METHOD => self.on_prepare_type_hierarchy(req),
            TypeHierarchySupertypes::METHOD => self.on_type_hierarchy_supertypes(req),
            TypeHierarchySubtypes::METHOD => self.on_type_hierarchy_subtypes(req),
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

    fn on_document_diagnostic(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<DocumentDiagnosticParams>(DocumentDiagnosticRequest::METHOD)
        else {
            self.respond_err(id, "invalid documentDiagnostic params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            // The spec wants a report, not null; an unknown document has none.
            let empty = serde_json::to_value(super::read_jobs::full_report(Vec::new()))
                .expect("empty diagnostic report serializes");
            self.respond_ok(id, empty);
            return;
        };
        let rules = Arc::clone(&self.config_for(&uri).rules);
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::DocumentDiagnostic {
            id,
            path: path_for(&uri),
            text,
            rules,
            previous_result_id: params.previous_result_id,
            sender: reply,
        });
    }

    fn on_code_action(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<CodeActionParams>(CodeActionRequest::METHOD) else {
            self.respond_err(id, "invalid codeAction params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let rules = Arc::clone(&self.config_for(&uri).rules);
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::CodeAction {
            id,
            path: path_for(&uri),
            text,
            range: params.range,
            rules,
            uri,
            sender: reply,
        });
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
        let style = self.config_for(&uri).style;
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::Format {
            id,
            path: path_for(&uri),
            text,
            style,
            sender: reply,
        });
    }

    fn on_range_formatting(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<DocumentRangeFormattingParams>(RangeFormatting::METHOD)
        else {
            self.respond_err(id, "invalid rangeFormatting params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let style = self.config_for(&uri).style;
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::FormatRange {
            id,
            path: path_for(&uri),
            text,
            range: params.range,
            style,
            sender: reply,
        });
    }

    fn on_document_symbols(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<DocumentSymbolParams>(DocumentSymbolRequest::METHOD)
        else {
            self.respond_err(id, "invalid documentSymbol params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::DocumentSymbols {
            id,
            path: path_for(&uri),
            text,
            sender: reply,
        });
    }

    /// `workspace/symbol` is not tied to a text document; it searches the
    /// harvested index of the package under development, so there is no buffer to
    /// look up — the query goes straight to the analysis thread.
    fn on_workspace_symbols(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<WorkspaceSymbolParams>(WorkspaceSymbolRequest::METHOD)
        else {
            self.respond_err(id, "invalid workspaceSymbol params");
            return;
        };
        let reply = self.read_reply(id.clone(), None);
        self.dispatch_read(ReadJob::WorkspaceSymbols {
            id,
            query: params.query,
            sender: reply,
        });
    }

    fn on_folding_ranges(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<FoldingRangeParams>(FoldingRangeRequest::METHOD) else {
            self.respond_err(id, "invalid foldingRange params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::FoldingRanges {
            id,
            path: path_for(&uri),
            text,
            sender: reply,
        });
    }

    fn on_document_links(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<DocumentLinkParams>(DocumentLinkRequest::METHOD) else {
            self.respond_err(id, "invalid documentLink params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::DocumentLinks {
            id,
            path: path_for(&uri),
            text,
            sender: reply,
        });
    }

    fn on_selection_ranges(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<SelectionRangeParams>(SelectionRangeRequest::METHOD)
        else {
            self.respond_err(id, "invalid selectionRange params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::SelectionRanges {
            id,
            path: path_for(&uri),
            text,
            positions: params.positions,
            sender: reply,
        });
    }

    fn on_semantic_tokens_full(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<SemanticTokensParams>(SemanticTokensFullRequest::METHOD)
        else {
            self.respond_err(id, "invalid semanticTokens params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::SemanticTokensFull {
            id,
            path: path_for(&uri),
            text,
            sender: reply,
        });
    }

    fn on_semantic_tokens_full_delta(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<SemanticTokensDeltaParams>(SemanticTokensFullDeltaRequest::METHOD)
        else {
            self.respond_err(id, "invalid semanticTokens/full/delta params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::SemanticTokensDelta {
            id,
            path: path_for(&uri),
            text,
            previous_result_id: params.previous_result_id,
            sender: reply,
        });
    }

    fn on_completion(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<CompletionParams>(Completion::METHOD) else {
            self.respond_err(id, "invalid completion params");
            return;
        };
        let uri = params.text_document_position.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::Completion {
            id,
            path: path_for(&uri),
            text,
            position: params.text_document_position.position,
            sender: reply,
        });
    }

    fn on_hover(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<HoverParams>(HoverRequest::METHOD) else {
            self.respond_err(id, "invalid hover params");
            return;
        };
        let uri = params.text_document_position_params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::Hover {
            id,
            path: path_for(&uri),
            text,
            position: params.text_document_position_params.position,
            sender: reply,
        });
    }

    fn on_signature_help(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<SignatureHelpParams>(SignatureHelpRequest::METHOD)
        else {
            self.respond_err(id, "invalid signatureHelp params");
            return;
        };
        let uri = params.text_document_position_params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::SignatureHelp {
            id,
            path: path_for(&uri),
            text,
            position: params.text_document_position_params.position,
            sender: reply,
        });
    }

    fn on_definition(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<GotoDefinitionParams>(GotoDefinition::METHOD) else {
            self.respond_err(id, "invalid definition params");
            return;
        };
        let uri = params.text_document_position_params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::Definition {
            id,
            path: path_for(&uri),
            position: params.text_document_position_params.position,
            uri,
            text,
            sender: reply,
        });
    }

    fn on_references(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<ReferenceParams>(References::METHOD) else {
            self.respond_err(id, "invalid references params");
            return;
        };
        let uri = params.text_document_position.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::References {
            id,
            path: path_for(&uri),
            position: params.text_document_position.position,
            include_declaration: params.context.include_declaration,
            uri,
            text,
            sender: reply,
        });
    }

    fn on_document_highlight(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<DocumentHighlightParams>(DocumentHighlightRequest::METHOD)
        else {
            self.respond_err(id, "invalid documentHighlight params");
            return;
        };
        let uri = params.text_document_position_params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::DocumentHighlight {
            id,
            path: path_for(&uri),
            position: params.text_document_position_params.position,
            text,
            sender: reply,
        });
    }

    fn on_prepare_rename(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<TextDocumentPositionParams>(PrepareRenameRequest::METHOD)
        else {
            self.respond_err(id, "invalid prepareRename params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::PrepareRename {
            id,
            path: path_for(&uri),
            position: params.position,
            text,
            sender: reply,
        });
    }

    fn on_rename(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<RenameParams>(Rename::METHOD) else {
            self.respond_err(id, "invalid rename params");
            return;
        };
        let uri = params.text_document_position.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::Rename {
            id,
            path: path_for(&uri),
            position: params.text_document_position.position,
            new_name: params.new_name,
            uri,
            text,
            sender: reply,
        });
    }

    fn on_prepare_call_hierarchy(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<CallHierarchyPrepareParams>(CallHierarchyPrepare::METHOD)
        else {
            self.respond_err(id, "invalid prepareCallHierarchy params");
            return;
        };
        let uri = params.text_document_position_params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::PrepareCallHierarchy {
            id,
            path: path_for(&uri),
            position: params.text_document_position_params.position,
            uri,
            text,
            sender: reply,
        });
    }

    /// Incoming/outgoing carry a [`CallHierarchyItem`](lsp_types::CallHierarchyItem)
    /// rather than a document position, and the item's file may be a closed
    /// member — so there is no buffer lookup; the read job re-derives the text
    /// off the snapshot (the `workspace/symbol` pattern).
    fn on_call_hierarchy_incoming(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<CallHierarchyIncomingCallsParams>(CallHierarchyIncomingCalls::METHOD)
        else {
            self.respond_err(id, "invalid incomingCalls params");
            return;
        };
        let reply = self.read_reply(id.clone(), None);
        self.dispatch_read(ReadJob::CallHierarchyIncoming {
            id,
            item: Box::new(params.item),
            sender: reply,
        });
    }

    fn on_call_hierarchy_outgoing(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<CallHierarchyOutgoingCallsParams>(CallHierarchyOutgoingCalls::METHOD)
        else {
            self.respond_err(id, "invalid outgoingCalls params");
            return;
        };
        let reply = self.read_reply(id.clone(), None);
        self.dispatch_read(ReadJob::CallHierarchyOutgoing {
            id,
            item: Box::new(params.item),
            sender: reply,
        });
    }

    fn on_prepare_type_hierarchy(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<TypeHierarchyPrepareParams>(TypeHierarchyPrepare::METHOD)
        else {
            self.respond_err(id, "invalid prepareTypeHierarchy params");
            return;
        };
        let uri = params.text_document_position_params.text_document.uri;
        let Some(text) = self.documents.get(&uri).map(|d| d.text.clone()) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::PrepareTypeHierarchy {
            id,
            path: path_for(&uri),
            position: params.text_document_position_params.position,
            uri,
            text,
            sender: reply,
        });
    }

    /// Supertypes/subtypes carry a [`TypeHierarchyItem`](lsp_types::TypeHierarchyItem)
    /// rather than a document position, and the item's file may be a closed
    /// member — so there is no buffer lookup; the read job re-derives the text
    /// off the snapshot (the call-hierarchy expansion pattern).
    fn on_type_hierarchy_supertypes(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<TypeHierarchySupertypesParams>(TypeHierarchySupertypes::METHOD)
        else {
            self.respond_err(id, "invalid supertypes params");
            return;
        };
        let reply = self.read_reply(id.clone(), None);
        self.dispatch_read(ReadJob::TypeHierarchySupertypes {
            id,
            item: Box::new(params.item),
            sender: reply,
        });
    }

    fn on_type_hierarchy_subtypes(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) =
            req.extract::<TypeHierarchySubtypesParams>(TypeHierarchySubtypes::METHOD)
        else {
            self.respond_err(id, "invalid subtypes params");
            return;
        };
        let reply = self.read_reply(id.clone(), None);
        self.dispatch_read(ReadJob::TypeHierarchySubtypes {
            id,
            item: Box::new(params.item),
            sender: reply,
        });
    }

    fn on_completion_resolve(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, item)) = req.extract::<CompletionItem>(ResolveCompletionItem::METHOD) else {
            self.respond_err(id, "invalid completionItem/resolve params");
            return;
        };
        let reply = self.read_reply(id.clone(), None);
        self.dispatch_read(ReadJob::CompletionResolve {
            id,
            item: Box::new(item),
            sender: reply,
        });
    }

    /// Register an in-flight read against the buffer it was dispatched on and
    /// hand back its [`ReadReply`] channel. The recorded `(uri, version)` lets
    /// the reply be version-gated ([`Self::on_read_reply`]) and lets a
    /// `$/cancelRequest` find it; a non-document read passes `uri: None`.
    fn read_reply(&mut self, id: RequestId, uri: Option<&Uri>) -> ReadReply {
        let version = uri.and_then(|uri| self.documents.get(uri).map(|doc| doc.version));
        self.inflight_reads.insert(
            id,
            ReadMeta {
                uri: uri.cloned(),
                version,
            },
        );
        ReadReply::new(self.out_tx.clone())
    }

    /// Hand a read job to the analysis thread; if its channel is gone
    /// (shutdown), answer with `null` directly so the client is not left
    /// waiting, and forget the now-unanswerable in-flight entry.
    fn dispatch_read(&mut self, job: ReadJob) {
        if let Err(crossbeam_channel::SendError(job)) = self.read_tx.send(job) {
            let (id, _reply) = job.into_reply_parts();
            self.inflight_reads.remove(&id);
            let _ = self.sender.send(Message::Response(Response::new_ok(
                id,
                serde_json::Value::Null,
            )));
        }
    }

    pub(crate) fn on_notification(&mut self, note: Notification) {
        match note.method.as_str() {
            Cancel::METHOD => {
                if let Ok(params) = note.extract::<CancelParams>(Cancel::METHOD) {
                    self.on_cancel(params.id);
                }
            }
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
                    // A pull client takes over an opened document's
                    // diagnostics: clear any include-graph problems pushed
                    // while it had no buffer, or they would double up with the
                    // pull report's.
                    if self.pull_diagnostics && self.graph_diags.contains_key(&uri) {
                        self.publish(uri.clone(), Vec::new(), None);
                    }
                    self.send_analysis(uri, None);
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
                    let edits =
                        apply_content_changes(&mut doc.text, params.content_changes, self.encoding);
                    doc.version = params.text_document.version;
                    self.send_analysis(uri, edits);
                }
            }
            DidSaveTextDocument::METHOD => {
                if let Ok(params) =
                    note.extract::<DidSaveTextDocumentParams>(DidSaveTextDocument::METHOD)
                {
                    // Signal the workspace harvester with the saved path; it
                    // re-harvests the workspace package if the file belongs to
                    // it, or re-resolves the environment if the save touched a
                    // project or manifest file. A dead channel (no workspace)
                    // is a no-op.
                    if let Some(path) = uri::to_path(&params.text_document.uri) {
                        let _ = self.harvest_tx.send(harvest_signal(path));
                    }
                }
            }
            DidChangeWatchedFiles::METHOD => {
                if let Ok(params) =
                    note.extract::<DidChangeWatchedFilesParams>(DidChangeWatchedFiles::METHOD)
                {
                    self.on_watched_files(params);
                }
            }
            DidChangeConfiguration::METHOD => {
                if let Ok(params) =
                    note.extract::<DidChangeConfigurationParams>(DidChangeConfiguration::METHOD)
                {
                    // Follow-up: on an empty `settings` payload, pull the
                    // authoritative values via `workspace/configuration` — that
                    // needs response routing the main loop doesn't have yet
                    // (it drops all `Message::Response`).
                    let warnings = self.config.set_client_settings(params.settings);
                    self.log_warnings(warnings);
                    self.on_config_changed();
                }
            }
            DidCloseTextDocument::METHOD => {
                if let Ok(params) =
                    note.extract::<DidCloseTextDocumentParams>(DidCloseTextDocument::METHOD)
                {
                    let uri = params.text_document.uri;
                    self.documents.remove(&uri);
                    // Revert the tracked input to on-disk text: the closed
                    // buffer's (possibly unsaved) edits must not linger in the
                    // reverse-occurrence index. A dead channel is a no-op.
                    if let Some(path) = uri::to_path(&uri) {
                        let _ = self.sync_tx.send(path);
                    }
                    // Drop the buffer's parse diagnostics, but keep any project-
                    // level include-graph diagnostics (they attach to the file on
                    // disk, open or not): republish just those.
                    self.parse_diags.remove(&uri);
                    self.publish_merged(uri, None);
                }
            }
            _ => {}
        }
    }

    /// Handle a `workspace/didChangeWatchedFiles` batch. An environment-file
    /// event escalates to one environment re-resolve for the whole batch (which
    /// subsumes any per-package re-harvest); otherwise each `.jl` event
    /// re-harvests the workspace package owning the file, so created and
    /// deleted members refresh the membership. A `.jl` file with no open buffer
    /// is first synced to disk — the seeded text must not go stale when the
    /// file changes outside the editor — while an open buffer stays
    /// authoritative until it closes (a create not yet tracked and a delete no
    /// longer readable both sync as no-ops; the re-harvest itself adds or drops
    /// the member).
    fn on_watched_files(&mut self, params: DidChangeWatchedFilesParams) {
        // A created, changed, or deleted `fatou.toml` reshapes discovery for
        // every cached directory; drop the cache wholesale and re-derive the
        // open documents' diagnostics. The `.jl` guard below skips these
        // events, so they never reach the harvest/sync plumbing.
        let config_changed = params.changes.iter().any(|event| {
            uri::to_path(&event.uri).is_some_and(|path| {
                path.file_name()
                    .is_some_and(|name| name == CONFIG_FILE_NAME)
            })
        });
        if config_changed {
            self.config.invalidate_discovered();
            self.on_config_changed();
        }
        let environment_changed = params
            .changes
            .iter()
            .filter_map(|event| uri::to_path(&event.uri))
            .any(|path| is_environment_file(&path));
        for event in &params.changes {
            let Some(path) = uri::to_path(&event.uri) else {
                continue;
            };
            if is_environment_file(&path) || path.extension().is_none_or(|ext| ext != "jl") {
                continue;
            }
            if !self.documents.contains_key(&event.uri) {
                let _ = self.sync_tx.send(path.clone());
            }
            if !environment_changed {
                let _ = self.harvest_tx.send(HarvestSignal::Source(path));
            }
        }
        if environment_changed {
            let _ = self.harvest_tx.send(HarvestSignal::Environment);
        }
    }

    /// Ask the client to watch the files whose external changes matter: `.jl`
    /// sources (workspace membership and the cross-file indexes), the
    /// environment files (the project/manifest flavors, which steer
    /// resolution), and `fatou.toml` (per-document configuration
    /// discovery). Called once by the main loop as it starts — past
    /// `initialize_finish`, which has already consumed the client's
    /// `initialized`, so the protocol permits server-to-client requests. The
    /// client's response carries nothing and is ignored.
    pub(crate) fn register_file_watchers(&self) {
        let watchers = [
            "**/*.jl",
            "**/Project.toml",
            "**/JuliaProject.toml",
            "**/Manifest.toml",
            "**/JuliaManifest.toml",
            "**/Manifest-v*.toml",
            "**/fatou.toml",
        ]
        .into_iter()
        .map(|glob| FileSystemWatcher {
            glob_pattern: GlobPattern::String(glob.to_string()),
            // The default kind: create + change + delete.
            kind: None,
        })
        .collect();
        let params = RegistrationParams {
            registrations: vec![Registration {
                id: "fatou-watched-files".to_string(),
                method: DidChangeWatchedFiles::METHOD.to_string(),
                register_options: Some(
                    serde_json::to_value(DidChangeWatchedFilesRegistrationOptions { watchers })
                        .expect("watcher registration options serialize"),
                ),
            }],
        };
        let _ = self.sender.send(Message::Request(Request {
            id: RequestId::from("fatou-register-watched-files".to_string()),
            method: RegisterCapability::METHOD.to_string(),
            params: serde_json::to_value(params).expect("registration params serialize"),
        }));
    }

    pub(crate) fn on_outbound(&mut self, outbound: Outbound) {
        match outbound {
            Outbound::Diagnostics {
                uri,
                version,
                diags,
            } => {
                // A pull client fetches these itself; the push path is off for
                // open documents (defense in depth — the analysis thread does
                // not produce this outbound then).
                if self.pull_diagnostics {
                    return;
                }
                // Stale results (a newer edit superseded this analysis, or the
                // document closed) are dropped: the newer version's analysis
                // will produce its own `Outbound`.
                if !matches!(self.documents.get(&uri), Some(d) if d.version == version) {
                    return;
                }
                self.parse_diags.insert(uri.clone(), diags);
                self.publish_merged(uri, Some(version));
            }
            Outbound::ProjectDiagnostics { uri, diags } => {
                if diags.is_empty() {
                    self.graph_diags.remove(&uri);
                } else {
                    self.graph_diags.insert(uri.clone(), diags);
                }
                // With a pull client, an *open* document's graph diagnostics
                // travel in its pull report (the refresh nudge below triggers
                // the re-pull); pushing them too would double them up. Files
                // with no open buffer keep the push — the client never pulls
                // them.
                if self.pull_diagnostics && self.documents.contains_key(&uri) {
                    return;
                }
                let version = self.documents.get(&uri).map(|d| d.version);
                self.publish_merged(uri, version);
            }
            Outbound::DiagnosticsRefresh => self.send_diagnostic_refresh(),
            Outbound::ReadReply { message } => self.on_read_reply(message),
        }
    }

    /// Gate a finished read job's response and forward it to the client. The
    /// main loop is the sole responder for reads, so this answers each request
    /// id exactly once:
    ///
    /// - id no longer in-flight → the request was already answered (a
    ///   `$/cancelRequest` cancelled it); drop this reply.
    /// - the buffer it was computed against moved on (a newer edit landed, or
    ///   the document closed) → `ContentModified`, so the client re-requests
    ///   against the current buffer instead of applying a stale result.
    /// - otherwise → forward the worker's response unchanged.
    fn on_read_reply(&mut self, message: Message) {
        let Message::Response(response) = message else {
            return;
        };
        let Some(meta) = self.inflight_reads.remove(&response.id) else {
            return;
        };
        if let (Some(uri), Some(version)) = (&meta.uri, meta.version)
            && !matches!(self.documents.get(uri), Some(doc) if doc.version == version)
        {
            let stale = Response::new_err(
                response.id,
                ErrorCode::ContentModified as i32,
                "document was modified".to_string(),
            );
            let _ = self.sender.send(Message::Response(stale));
            return;
        }
        let _ = self.sender.send(Message::Response(response));
    }

    /// Handle `$/cancelRequest`. If the id names a read still in flight, answer
    /// it now with `RequestCancelled` and forget it — its eventual worker reply
    /// finds no entry and is dropped, so the request is still answered exactly
    /// once. An unknown id (already answered, or never a tracked read) is a
    /// no-op, as the spec allows. The in-flight salsa work runs to completion;
    /// only its result is discarded (cooperative cancellation).
    fn on_cancel(&mut self, id: NumberOrString) {
        let id = match id {
            NumberOrString::Number(n) => RequestId::from(n),
            NumberOrString::String(s) => RequestId::from(s),
        };
        if self.inflight_reads.remove(&id).is_some() {
            let cancelled = Response::new_err(
                id,
                ErrorCode::RequestCanceled as i32,
                "request cancelled".to_string(),
            );
            let _ = self.sender.send(Message::Response(cancelled));
        }
    }

    /// Publish the union of `uri`'s parse and include-graph diagnostics — a
    /// single `publishDiagnostics` replaces *all* diagnostics for a URI, so the
    /// two sources must be sent together or each would clobber the other.
    fn publish_merged(&self, uri: Uri, version: Option<i32>) {
        let mut diagnostics = self.parse_diags.get(&uri).cloned().unwrap_or_default();
        if let Some(graph) = self.graph_diags.get(&uri) {
            diagnostics.extend(graph.iter().cloned());
        }
        self.publish(uri, diagnostics, version);
    }

    /// Send an analysis request for `uri`'s current buffer to the analysis
    /// thread, carrying the lint rules resolved for the document.
    ///
    /// `edits` are the byte edits that produced the buffer from the one the
    /// previous request carried, for the incremental reparse to replay. `None`
    /// means the transform is unknown — a fresh `didOpen`, a whole-buffer
    /// replacement, or a re-analysis of unchanged text under new rules — and
    /// the reparse falls back to diffing the two texts.
    fn send_analysis(&mut self, uri: Uri, edits: Option<Vec<Edit>>) {
        let rules = Arc::clone(&self.config_for(&uri).rules);
        let Some(doc) = self.documents.get(&uri) else {
            return;
        };
        let _ = self.analysis_tx.send(AnalysisRequest {
            path: path_for(&uri),
            text: doc.text.clone(),
            version: doc.version,
            rules,
            edits,
            uri,
        });
    }

    /// Resolve the configuration for `uri`, logging any warnings its load
    /// raised (once per load — cache hits are silent).
    fn config_for(&mut self, uri: &Uri) -> Arc<ResolvedConfig> {
        let (config, warnings) = self.config.for_uri(uri);
        self.log_warnings(warnings);
        config
    }

    /// Re-derive the open documents' diagnostics after a configuration change
    /// (editor-pushed settings, or a `fatou.toml` event). A pull client is
    /// nudged to re-pull (each pull resolves config afresh); a push client
    /// gets a re-analysis per open document — same version, new rules — which
    /// the analysis thread's coalescing absorbs.
    fn on_config_changed(&mut self) {
        if self.pull_diagnostics {
            self.send_diagnostic_refresh();
        } else {
            let uris: Vec<Uri> = self.documents.keys().cloned().collect();
            for uri in uris {
                self.send_analysis(uri, None);
            }
        }
    }

    /// Ask a pull client to re-pull its open documents
    /// (`workspace/diagnostic/refresh`); a no-op without the client
    /// capability (such a client re-pulls on its next edit anyway).
    fn send_diagnostic_refresh(&mut self) {
        if !(self.pull_diagnostics && self.diagnostic_refresh) {
            return;
        }
        self.refresh_seq += 1;
        let _ = self.sender.send(Message::Request(Request {
            id: RequestId::from(format!("fatou-diagnostic-refresh-{}", self.refresh_seq)),
            method: WorkspaceDiagnosticRefresh::METHOD.to_string(),
            params: serde_json::Value::Null,
        }));
    }

    fn log_warnings(&self, warnings: Vec<String>) {
        for warning in warnings {
            self.log_message(MessageType::WARNING, warning);
        }
    }

    /// Send a `window/logMessage` — the log channel, not a popup, so config
    /// warnings on keystroke-adjacent events stay unobtrusive.
    fn log_message(&self, typ: MessageType, message: String) {
        let note = Notification::new(
            LogMessage::METHOD.to_string(),
            LogMessageParams { typ, message },
        );
        let _ = self.sender.send(Message::Notification(note));
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

/// Classify a changed path for the harvester: an environment file warrants a
/// full re-resolve, anything else a re-harvest of the package owning it.
fn harvest_signal(path: PathBuf) -> HarvestSignal {
    if is_environment_file(&path) {
        HarvestSignal::Environment
    } else {
        HarvestSignal::Source(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Receiver, unbounded};
    use std::str::FromStr;

    /// A `GlobalState` wired to in-memory channels, returned with the client's
    /// receiver so a test can assert on the responses it emits. Only the read
    /// registry, version gate, and cancel paths are exercised here — those
    /// touch nothing but `documents`, `inflight_reads`, and the client sender.
    fn test_state() -> (GlobalState, Receiver<Message>) {
        let (client_tx, client_rx) = unbounded();
        let (out_tx, _out_rx) = unbounded();
        let (analysis_tx, _analysis_rx) = unbounded();
        let (read_tx, _read_rx) = unbounded();
        let (harvest_tx, _harvest_rx) = unbounded();
        let (sync_tx, _sync_rx) = unbounded();
        let state = GlobalState::new(
            client_tx,
            out_tx,
            analysis_tx,
            read_tx,
            harvest_tx,
            sync_tx,
            PositionEncoding::Utf16,
            false,
            false,
            None,
        );
        // The auxiliary receivers drop here; the methods under test only touch
        // `documents`, `inflight_reads`, and the client sender, so a
        // disconnected analysis/out channel is harmless. Discard any startup log.
        while client_rx.try_recv().is_ok() {}
        (state, client_rx)
    }

    fn uri(path: &str) -> Uri {
        Uri::from_str(path).unwrap()
    }

    fn open(state: &mut GlobalState, uri: &Uri, version: i32) {
        state.documents.insert(
            uri.clone(),
            Document {
                text: String::new(),
                version,
            },
        );
    }

    fn ok_reply(id: i32) -> Message {
        Message::Response(Response::new_ok(
            RequestId::from(id),
            serde_json::json!("ok"),
        ))
    }

    /// The error code of the next response, or `None` for an `Ok` response.
    fn next_error_code(rx: &Receiver<Message>) -> Option<i32> {
        match rx.try_recv().expect("a response") {
            Message::Response(resp) => resp.response_result.err().map(|e| e.code),
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn read_reply_forwards_when_version_matches() {
        let (mut state, rx) = test_state();
        let doc = uri("file:///a.jl");
        open(&mut state, &doc, 1);
        let _reply = state.read_reply(RequestId::from(1), Some(&doc));

        state.on_read_reply(ok_reply(1));

        assert_eq!(
            next_error_code(&rx),
            None,
            "a current read forwards its result"
        );
        assert!(state.inflight_reads.is_empty(), "the entry is consumed");
    }

    #[test]
    fn read_reply_is_content_modified_when_superseded() {
        let (mut state, rx) = test_state();
        let doc = uri("file:///a.jl");
        open(&mut state, &doc, 1);
        let _reply = state.read_reply(RequestId::from(1), Some(&doc));
        // A newer edit lands while the read was in flight.
        state.documents.get_mut(&doc).unwrap().version = 2;

        state.on_read_reply(ok_reply(1));

        assert_eq!(
            next_error_code(&rx),
            Some(ErrorCode::ContentModified as i32),
            "a stale read is rejected so the client re-requests"
        );
    }

    #[test]
    fn read_reply_is_content_modified_when_document_closed() {
        let (mut state, rx) = test_state();
        let doc = uri("file:///a.jl");
        open(&mut state, &doc, 1);
        let _reply = state.read_reply(RequestId::from(1), Some(&doc));
        state.documents.remove(&doc);

        state.on_read_reply(ok_reply(1));

        assert_eq!(
            next_error_code(&rx),
            Some(ErrorCode::ContentModified as i32),
            "a closed document's read is stale"
        );
    }

    #[test]
    fn cancel_answers_and_suppresses_the_reply() {
        let (mut state, rx) = test_state();
        let doc = uri("file:///a.jl");
        open(&mut state, &doc, 1);
        let _reply = state.read_reply(RequestId::from(1), Some(&doc));

        state.on_cancel(NumberOrString::Number(1));
        assert_eq!(
            next_error_code(&rx),
            Some(ErrorCode::RequestCanceled as i32),
            "cancel answers the request promptly"
        );
        assert!(state.inflight_reads.is_empty());

        // The worker's late reply finds no entry and is dropped: exactly once.
        state.on_read_reply(ok_reply(1));
        assert!(
            rx.try_recv().is_err(),
            "a cancelled request is not answered twice"
        );
    }

    #[test]
    fn cancel_of_unknown_id_is_a_noop() {
        let (mut state, rx) = test_state();
        state.on_cancel(NumberOrString::Number(99));
        assert!(
            rx.try_recv().is_err(),
            "an already-answered or unknown id draws no response"
        );
    }

    #[test]
    fn non_document_read_is_never_content_modified() {
        let (mut state, rx) = test_state();
        // A `workspace/symbol`-style read: no uri, no version.
        let _reply = state.read_reply(RequestId::from(1), None);

        state.on_read_reply(ok_reply(1));
        assert_eq!(
            next_error_code(&rx),
            None,
            "a document-less read has no version to go stale"
        );

        // It is still cancellable.
        let _reply = state.read_reply(RequestId::from(2), None);
        state.on_cancel(NumberOrString::Number(2));
        assert_eq!(
            next_error_code(&rx),
            Some(ErrorCode::RequestCanceled as i32)
        );
    }
}
