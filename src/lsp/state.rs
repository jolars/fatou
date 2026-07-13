//! Main-loop state: open documents, request/notification dispatch, and the
//! version-gated diagnostic publish.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Sender;
use lsp_server::{ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    DidSaveTextDocument, Notification as NotificationTrait, PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentDiagnosticRequest, DocumentHighlightRequest,
    DocumentSymbolRequest, FoldingRangeRequest, Formatting, GotoDefinition, HoverRequest,
    PrepareRenameRequest, RangeFormatting, References, RegisterCapability, Rename,
    Request as RequestTrait, ResolveCompletionItem, SelectionRangeRequest,
    SemanticTokensFullRequest, SignatureHelpRequest, WorkspaceDiagnosticRefresh,
    WorkspaceSymbolRequest,
};
use lsp_types::{
    CodeActionParams, CompletionItem, CompletionParams, Diagnostic, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidChangeWatchedFilesRegistrationOptions,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentDiagnosticParams, DocumentFormattingParams, DocumentHighlightParams,
    DocumentRangeFormattingParams, DocumentSymbolParams, FileSystemWatcher, FoldingRangeParams,
    GlobPattern, GotoDefinitionParams, HoverParams, PublishDiagnosticsParams, ReferenceParams,
    Registration, RegistrationParams, RenameParams, SelectionRangeParams, SemanticTokensParams,
    SignatureHelpParams, TextDocumentPositionParams, Uri, WorkspaceSymbolParams,
};

use crate::environment::is_environment_file;
use crate::formatter::FormatStyle;
use crate::text::{PositionEncoding, apply_content_changes};

use super::analysis_thread::AnalysisRequest;
use super::read_jobs::ReadJob;
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
}

impl GlobalState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sender: Sender<Message>,
        analysis_tx: Sender<AnalysisRequest>,
        read_tx: Sender<ReadJob>,
        harvest_tx: Sender<HarvestSignal>,
        sync_tx: Sender<PathBuf>,
        encoding: PositionEncoding,
        pull_diagnostics: bool,
        diagnostic_refresh: bool,
    ) -> Self {
        Self {
            documents: HashMap::new(),
            parse_diags: HashMap::new(),
            graph_diags: HashMap::new(),
            sender,
            analysis_tx,
            read_tx,
            harvest_tx,
            sync_tx,
            encoding,
            pull_diagnostics,
            diagnostic_refresh,
            refresh_seq: 0,
        }
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
            SelectionRangeRequest::METHOD => self.on_selection_ranges(req),
            SemanticTokensFullRequest::METHOD => self.on_semantic_tokens_full(req),
            Completion::METHOD => self.on_completion(req),
            ResolveCompletionItem::METHOD => self.on_completion_resolve(req),
            HoverRequest::METHOD => self.on_hover(req),
            SignatureHelpRequest::METHOD => self.on_signature_help(req),
            GotoDefinition::METHOD => self.on_definition(req),
            References::METHOD => self.on_references(req),
            DocumentHighlightRequest::METHOD => self.on_document_highlight(req),
            PrepareRenameRequest::METHOD => self.on_prepare_rename(req),
            Rename::METHOD => self.on_rename(req),
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
        self.dispatch_read(ReadJob::DocumentDiagnostic {
            id,
            path: path_for(&uri),
            text,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::CodeAction {
            id,
            path: path_for(&uri),
            text,
            range: params.range,
            uri,
            sender: self.sender.clone(),
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
        // Style resolution mirrors full formatting: defaults for now.
        self.dispatch_read(ReadJob::FormatRange {
            id,
            path: path_for(&uri),
            text,
            range: params.range,
            style: FormatStyle::default(),
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::DocumentSymbols {
            id,
            path: path_for(&uri),
            text,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::WorkspaceSymbols {
            id,
            query: params.query,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::FoldingRanges {
            id,
            path: path_for(&uri),
            text,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::SelectionRanges {
            id,
            path: path_for(&uri),
            text,
            positions: params.positions,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::SemanticTokensFull {
            id,
            path: path_for(&uri),
            text,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::Completion {
            id,
            path: path_for(&uri),
            text,
            position: params.text_document_position.position,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::Hover {
            id,
            path: path_for(&uri),
            text,
            position: params.text_document_position_params.position,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::SignatureHelp {
            id,
            path: path_for(&uri),
            text,
            position: params.text_document_position_params.position,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::Definition {
            id,
            path: path_for(&uri),
            position: params.text_document_position_params.position,
            uri,
            text,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::References {
            id,
            path: path_for(&uri),
            position: params.text_document_position.position,
            include_declaration: params.context.include_declaration,
            uri,
            text,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::DocumentHighlight {
            id,
            path: path_for(&uri),
            position: params.text_document_position_params.position,
            text,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::PrepareRename {
            id,
            path: path_for(&uri),
            position: params.position,
            text,
            sender: self.sender.clone(),
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
        self.dispatch_read(ReadJob::Rename {
            id,
            path: path_for(&uri),
            position: params.text_document_position.position,
            new_name: params.new_name,
            uri,
            text,
            sender: self.sender.clone(),
        });
    }

    fn on_completion_resolve(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, item)) = req.extract::<CompletionItem>(ResolveCompletionItem::METHOD) else {
            self.respond_err(id, "invalid completionItem/resolve params");
            return;
        };
        self.dispatch_read(ReadJob::CompletionResolve {
            id,
            item: Box::new(item),
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
                    // A pull client takes over an opened document's
                    // diagnostics: clear any include-graph problems pushed
                    // while it had no buffer, or they would double up with the
                    // pull report's.
                    if self.pull_diagnostics && self.graph_diags.contains_key(&uri) {
                        self.publish(uri.clone(), Vec::new(), None);
                    }
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
                    apply_content_changes(&mut doc.text, params.content_changes, self.encoding);
                    doc.version = params.text_document.version;
                    self.send_analysis(uri);
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
    /// sources (workspace membership and the cross-file indexes) and the
    /// environment files (the project/manifest flavors, which steer
    /// resolution). Called once by the main loop as it starts — past
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
            Outbound::DiagnosticsRefresh => {
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

/// Classify a changed path for the harvester: an environment file warrants a
/// full re-resolve, anything else a re-harvest of the package owning it.
fn harvest_signal(path: PathBuf) -> HarvestSignal {
    if is_environment_file(&path) {
        HarvestSignal::Environment
    } else {
        HarvestSignal::Source(path)
    }
}
