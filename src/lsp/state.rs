//! Main-loop state: open documents, request/notification dispatch, and the
//! version-gated diagnostic publish.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use crossbeam_channel::Sender;
use lsp_server::{ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    Cancel, DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles,
    DidCloseTextDocument, DidOpenTextDocument, DidRenameFiles, DidSaveTextDocument, LogMessage,
    Notification as NotificationTrait, PublishDiagnostics,
};
use lsp_types::request::{
    CallHierarchyIncomingCalls, CallHierarchyOutgoingCalls, CallHierarchyPrepare,
    CodeActionRequest, Completion, DocumentDiagnosticRequest, DocumentHighlightRequest,
    DocumentLinkRequest, DocumentSymbolRequest, FoldingRangeRequest, Formatting, GotoDefinition,
    HoverRequest, InlayHintRefreshRequest, InlayHintRequest, PrepareRenameRequest, RangeFormatting,
    References, RegisterCapability, Rename, Request as RequestTrait, ResolveCompletionItem,
    SelectionRangeRequest, SemanticTokensFullDeltaRequest, SemanticTokensFullRequest,
    SignatureHelpRequest, TypeHierarchyPrepare, TypeHierarchySubtypes, TypeHierarchySupertypes,
    WillRenameFiles, WorkspaceDiagnosticRefresh, WorkspaceSymbolRequest,
};
use lsp_types::{
    CallHierarchyIncomingCallsParams, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CancelParams, CodeActionParams, CompletionItem, CompletionParams, Diagnostic,
    DidChangeConfigurationParams, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidChangeWatchedFilesRegistrationOptions, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentDiagnosticParams,
    DocumentFormattingParams, DocumentHighlightParams, DocumentLinkParams,
    DocumentRangeFormattingParams, DocumentSymbolParams, FileSystemWatcher, FoldingRangeParams,
    GlobPattern, GotoDefinitionParams, HoverParams, InlayHintParams, LogMessageParams, MessageType,
    NumberOrString, PublishDiagnosticsParams, ReferenceParams, Registration, RegistrationParams,
    RenameFilesParams, RenameParams, SelectionRangeParams, SemanticTokensDeltaParams,
    SemanticTokensParams, SignatureHelpParams, TextDocumentPositionParams,
    TypeHierarchyPrepareParams, TypeHierarchySubtypesParams, TypeHierarchySupertypesParams, Uri,
    WorkspaceSymbolParams,
};

use crate::config::CONFIG_FILE_NAME;
use crate::environment::{is_environment_file, is_manifest_file, is_project_file};
use crate::parser::Edit;
use crate::text::{PositionEncoding, TextBuffer, apply_content_changes};

use super::analysis_thread::{AnalysisRequest, SyncMessage};
use super::config::{ConfigStore, ResolvedConfig};
use super::read_jobs::{ReadJob, ReadReply};
use super::server::HarvestSignal;
use super::uri;

/// An open document's live buffer, client-reported version, and kind.
#[derive(Debug, Clone)]
struct Document {
    text: Arc<TextBuffer>,
    version: i32,
    kind: DocumentKind,
}

/// What an open document *is*, decided once at `didOpen` from its path and
/// carried on the buffer thereafter. Every fork that used to re-derive "is this
/// an environment file?" from the URI now reads this instead: the two TOML
/// kinds are not Julia, they never reach the parser, and each has a route of
/// its own ([`GlobalState::route_document`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentKind {
    /// Julia source: the analysis pipeline's business, and the kind nearly
    /// every language feature answers for.
    Julia,
    /// A `Project.toml` flavor. Its text reaches the database, because the
    /// linter derives the package's declared dependencies from it; its buffer
    /// also carries the file's own TOML diagnostics.
    Project,
    /// A `Manifest.toml` flavor. Diagnostics, plus the document links on its
    /// `path` entries — the one feature needing no environment resolve, which
    /// is otherwise the harvester's job and never reads a buffer.
    Manifest,
}

impl DocumentKind {
    /// A document's kind, from the file its URI names. A URI with no path
    /// behind it — an editor's untitled buffer — is Julia: it is what the
    /// editor opened the server for, and it has no file name to say otherwise.
    fn of(uri: &Uri) -> Self {
        match uri::to_path(uri) {
            Some(path) if is_project_file(&path) => Self::Project,
            Some(path) if is_manifest_file(&path) => Self::Manifest,
            _ => Self::Julia,
        }
    }
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
    /// Diagnostics on an *environment file* itself (`Project.toml`,
    /// `Manifest.toml`): TOML syntax failures and the checks over a resolved
    /// environment. Produced by the workspace harvester rather than the
    /// analysis thread — the resolved `Environment`, and the resolve failure,
    /// exist only there.
    ///
    /// Version-free like [`ProjectDiagnostics`](Self::ProjectDiagnostics), and
    /// an empty list clears. Unlike it, these have **no pull twin**: a pull
    /// report is served only for an open document, and an open environment
    /// file has no Julia analysis to carry them. So they always push, and a
    /// pull client neither suppresses nor re-supplies them.
    ///
    /// The harvester is not the only producer for these URIs: an *open*
    /// environment file also carries the text-only findings of its own buffer
    /// (`buffer_diags`), which arrive on the main loop rather than through this
    /// channel. [`GlobalState::publish_merged`] is where the two cadences meet.
    EnvironmentDiagnostics { uri: Uri, diags: Vec<Diagnostic> },
    /// A re-harvest changed the include graph: a pull-model client should
    /// re-pull its open documents (`workspace/diagnostic/refresh`). Sent once
    /// per harvest; the main loop forwards it only when the client supports
    /// both pull diagnostics and the refresh request.
    DiagnosticsRefresh,
    /// The harvested library landed, so what an open `Project.toml`'s inlay
    /// hints report — each dependency's resolved version — exists for the first
    /// time. Unlike a diagnostic, a hint has no push channel and a client
    /// re-requests one only on an edit or a scroll, so a file already open at
    /// startup would otherwise sit blank until the user touched it.
    ///
    /// Its own signal rather than a share of [`DiagnosticsRefresh`]: that one
    /// also fires on a project-file keystroke, and hints are re-requested on an
    /// edit anyway. The spec has this request force a *global* recalculation,
    /// which is worth spending once per harvest and not once per keystroke.
    InlayHintsRefresh,
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
    /// Text writes to the analysis thread for files the analysis pipeline does
    /// not own: a revert to on-disk text when a document closes (a discarded
    /// buffer must not linger in the reverse-occurrence index) or when a
    /// watched file changes outside any open buffer (the stale seeded text must
    /// catch up with disk), and an open project file's buffer, which is not
    /// Julia but whose `[deps]` the linter reads.
    sync_tx: Sender<SyncMessage>,
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
    /// Whether the client accepts `workspace/inlayHint/refresh`, the nudge to
    /// re-request hints once the harvest lands. Unlike a diagnostic, a hint has
    /// no push channel: without this an open `Project.toml` shows no version
    /// beside a UUID until the user happens to edit or scroll it.
    inlay_hint_refresh: bool,
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
    /// The latest environment-file diagnostics per file, kept so any other
    /// update republishes the union. Set/cleared by the workspace harvester on
    /// each re-resolve.
    ///
    /// Deliberately *not* dropped when a document closes (they describe the
    /// file on disk, open or not) nor cleared for a pull client on `didOpen`,
    /// as `graph_diags` is: there is no pull report that would re-supply them.
    env_diags: HashMap<Uri, Vec<Diagnostic>>,
    /// The latest diagnostics an *open* environment file's own buffer produced
    /// (see [`buffer_diagnostics`](super::environment_diagnostics::buffer_diagnostics)),
    /// the second producer for the same URIs as `env_diags` — this one at edit
    /// cadence rather than the harvester's resolve cadence. Non-empty entries
    /// only, and dropped when the buffer closes: it describes a text only the
    /// editor has.
    buffer_diags: HashMap<Uri, Vec<Diagnostic>>,
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
        sync_tx: Sender<SyncMessage>,
        encoding: PositionEncoding,
        pull_diagnostics: bool,
        diagnostic_refresh: bool,
        inlay_hint_refresh: bool,
        initialization_options: Option<serde_json::Value>,
    ) -> Self {
        let (config, warnings) = ConfigStore::new(initialization_options);
        let state = Self {
            documents: HashMap::new(),
            parse_diags: HashMap::new(),
            graph_diags: HashMap::new(),
            env_diags: HashMap::new(),
            buffer_diags: HashMap::new(),
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
            inlay_hint_refresh,
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
            InlayHintRequest::METHOD => self.on_inlay_hints(req),
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
            WillRenameFiles::METHOD => self.on_will_rename_files(req),
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
        // An environment file is TOML: there is nothing here to lint, and a
        // Julia parse of it would report nonsense (`julia_text` is what returns
        // nothing for one). Its own diagnostics reach the client by push (see
        // `Outbound::EnvironmentDiagnostics`), which stays the single route for
        // them whether or not the client pulls — its two producers both push,
        // so answering the pull with them as well would double them up.
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        // A project file links each dependency name to its package, a manifest
        // each `path` entry to the package it pins, and a Julia document each
        // static `include`. Anything else answers `null`.
        if let Some(text) = self.project_text(&uri) {
            let reply = self.read_reply(id.clone(), Some(&uri));
            self.dispatch_read(ReadJob::ProjectDocumentLinks {
                id,
                text,
                sender: reply,
            });
            return;
        }
        if let Some(text) = self.manifest_text(&uri) {
            let reply = self.read_reply(id.clone(), Some(&uri));
            self.dispatch_read(ReadJob::ManifestDocumentLinks {
                id,
                path: path_for(&uri),
                text,
                sender: reply,
            });
            return;
        }
        let Some(text) = self.julia_text(&uri) else {
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

    /// Inlay hints are a project file's alone: a dependency's resolved version
    /// beside its UUID. A Julia document answers an empty list rather than
    /// `null` — the capability is advertised globally, and an empty list is the
    /// honest answer to "what hints does this file have".
    fn on_inlay_hints(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<InlayHintParams>(InlayHintRequest::METHOD) else {
            self.respond_err(id, "invalid inlayHint params");
            return;
        };
        let uri = params.text_document.uri;
        let Some(text) = self.project_text(&uri) else {
            self.respond_ok(id, serde_json::json!([]));
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::ProjectInlayHints {
            id,
            text,
            range: params.range,
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let position = params.text_document_position_params.position;
        // A project file reports what a dependency name resolved to; a Julia
        // document reports a symbol. Anything else answers `null`.
        if let Some(text) = self.project_text(&uri) {
            let reply = self.read_reply(id.clone(), Some(&uri));
            self.dispatch_read(ReadJob::ProjectHover {
                id,
                text,
                position,
                sender: reply,
            });
            return;
        }
        let Some(text) = self.julia_text(&uri) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::Hover {
            id,
            path: path_for(&uri),
            text,
            position,
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
        let Some(text) = self.julia_text(&uri) else {
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
        let position = params.text_document_position_params.position;
        // A project file resolves a dependency name to its package's source; a
        // Julia document resolves a symbol. Anything else answers `null`.
        if let Some(text) = self.project_text(&uri) {
            let reply = self.read_reply(id.clone(), Some(&uri));
            self.dispatch_read(ReadJob::ProjectDefinition {
                id,
                text,
                position,
                sender: reply,
            });
            return;
        }
        let Some(text) = self.julia_text(&uri) else {
            self.respond_ok(id, serde_json::Value::Null);
            return;
        };
        let reply = self.read_reply(id.clone(), Some(&uri));
        self.dispatch_read(ReadJob::Definition {
            id,
            path: path_for(&uri),
            position,
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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

    /// `workspace/willRenameFiles` is not tied to a text document: it names
    /// files the editor is about to move, most of them closed. The scan runs
    /// off the snapshot's seeded members, widened by the open buffers so a
    /// non-member the harvest never reached still gets its includes fixed.
    fn on_will_rename_files(&mut self, req: Request) {
        let id = req.id.clone();
        let Ok((_, params)) = req.extract::<RenameFilesParams>(WillRenameFiles::METHOD) else {
            self.respond_err(id, "invalid willRenameFiles params");
            return;
        };
        let open_docs = self
            .documents
            .iter()
            .filter(|(_, doc)| doc.kind == DocumentKind::Julia)
            .filter_map(|(uri, _)| uri::to_path(uri))
            .collect::<Vec<_>>();
        let reply = self.read_reply(id.clone(), None);
        self.dispatch_read(ReadJob::WillRenameFiles {
            id,
            files: params.files,
            open_docs,
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
        let Some(text) = self.julia_text(&uri) else {
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
        let Some(text) = self.julia_text(&uri) else {
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

    /// The live buffer for `uri`, but only when the document is Julia. Every
    /// language feature goes through this rather than `documents` directly:
    /// with the environment files in the client's document selector, any
    /// request can arrive for a `Project.toml`, and answering a hover or a
    /// format for one would parse TOML as Julia and hand back nonsense. Such a
    /// request answers `null`, exactly as it does for a document the server
    /// never saw.
    fn julia_text(&self, uri: &Uri) -> Option<Arc<TextBuffer>> {
        self.documents
            .get(uri)
            .filter(|doc| doc.kind == DocumentKind::Julia)
            .map(|doc| Arc::clone(&doc.text))
    }

    /// The live buffer for `uri`, but only when the document is a project file.
    /// The sibling of [`julia_text`](Self::julia_text), and the *only* other
    /// door into the read pool: a feature that understands TOML asks here,
    /// everything Julia-backed keeps asking there, and a handler that calls
    /// neither still answers `null` as before.
    ///
    /// A `Manifest.toml` answers nothing here. Its text never reaches the
    /// database (nothing reads a manifest without an environment resolve), so
    /// every feature that consults one stops at this door; the single feature
    /// that does not asks [`manifest_text`](Self::manifest_text).
    fn project_text(&self, uri: &Uri) -> Option<Arc<TextBuffer>> {
        self.documents
            .get(uri)
            .filter(|doc| doc.kind == DocumentKind::Project)
            .map(|doc| Arc::clone(&doc.text))
    }

    /// The live buffer for `uri`, but only when the document is a manifest. The
    /// third and last door into the read pool, and the narrowest: document
    /// links are all a manifest answers, since a `path` entry is the only thing
    /// in one that anchors anywhere — see
    /// [`project_navigation`](super::project_navigation).
    fn manifest_text(&self, uri: &Uri) -> Option<Arc<TextBuffer>> {
        self.documents
            .get(uri)
            .filter(|doc| doc.kind == DocumentKind::Manifest)
            .map(|doc| Arc::clone(&doc.text))
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
                            text: Arc::new(TextBuffer::new(params.text_document.text)),
                            version: params.text_document.version,
                            // The one place a document's kind is decided; every
                            // later fork reads the tag rather than the path.
                            kind: DocumentKind::of(&uri),
                        },
                    );
                    // A pull client takes over an opened document's
                    // diagnostics: clear any include-graph problems pushed
                    // while it had no buffer, or they would double up with the
                    // pull report's.
                    if self.pull_diagnostics && self.graph_diags.contains_key(&uri) {
                        self.publish(uri.clone(), Vec::new(), None);
                    }
                    self.route_document(uri, None);
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
                    let edits = apply_content_changes(
                        Arc::make_mut(&mut doc.text),
                        params.content_changes,
                        self.encoding,
                    );
                    doc.version = params.text_document.version;
                    self.route_document(uri, edits);
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
            DidRenameFiles::METHOD => {
                if let Ok(params) = note.extract::<RenameFilesParams>(DidRenameFiles::METHOD) {
                    self.on_did_rename_files(params);
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
                        let _ = self.sync_tx.send(SyncMessage::Revert(path));
                    }
                    // Drop what the buffer produced — its parse diagnostics, and
                    // an environment file's live findings — but keep the
                    // include-graph and harvester diagnostics (they attach to
                    // the file on disk, open or not): republish just those.
                    self.parse_diags.remove(&uri);
                    self.buffer_diags.remove(&uri);
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
    /// deleted members refresh the membership. A file with no open buffer is
    /// first synced to disk — the seeded text must not go stale when the file
    /// changes outside the editor — while an open buffer stays authoritative
    /// until it closes (a create not yet tracked and a delete no longer
    /// readable both sync as no-ops; the re-harvest itself adds or drops the
    /// member).
    ///
    /// The sync covers the **project files too**, not just `.jl` sources: their
    /// `[deps]` is read off the tracked input now
    /// ([`project_declared_deps`](crate::incremental::project_declared_deps)),
    /// and the re-resolve an environment event triggers cannot refresh it —
    /// `set_project_files` is create-or-return, so as not to clobber an open
    /// buffer with the disk copy. Without the sync, a `pkg> add` in a terminal
    /// would leave `unresolved-import` answering to the text the file had when
    /// the server first harvested it.
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
            let is_env = is_environment_file(&path);
            if !is_env && path.extension().is_none_or(|ext| ext != "jl") {
                continue;
            }
            if !self.documents.contains_key(&event.uri) {
                let _ = self.sync_tx.send(SyncMessage::Revert(path.clone()));
            }
            // An environment event sets `environment_changed`, so this arm is
            // the `.jl` one: a project file re-resolves as a batch below rather
            // than naming a package to re-harvest on its own.
            if !environment_changed {
                let _ = self.harvest_tx.send(HarvestSignal::Source(path));
            }
        }
        if environment_changed {
            let _ = self.harvest_tx.send(HarvestSignal::Environment);
        }
    }

    /// Handle a `workspace/didRenameFiles` batch: refresh what the move
    /// invalidated, the way [`on_watched_files`](Self::on_watched_files) does
    /// for a delete/create pair. Largely redundant with the watched-file
    /// events an explorer rename also produces — its point is the client that
    /// cannot register watchers dynamically and so never sends them.
    ///
    /// A renamed *folder* names no file in particular, and walking it would put
    /// directory I/O on the main loop, so it escalates to one environment
    /// re-resolve (which subsumes every per-package re-harvest) plus a
    /// configuration re-derive — the folder may have carried a `fatou.toml` or
    /// a project file along. Folder renames are rare enough for that to be the
    /// cheap choice.
    fn on_did_rename_files(&mut self, params: RenameFilesParams) {
        let mut config_changed = false;
        let mut environment_changed = false;
        let mut sources: Vec<PathBuf> = Vec::new();
        for rename in &params.files {
            let paths: Vec<PathBuf> = [&rename.old_uri, &rename.new_uri]
                .into_iter()
                .filter_map(|text| Uri::from_str(text).ok())
                .filter_map(|uri| uri::to_path(&uri))
                .collect();
            // Only the new path can be stat'd — the old one no longer exists.
            if paths.iter().any(|path| path.is_dir()) {
                config_changed = true;
                environment_changed = true;
                continue;
            }
            for path in paths {
                if path
                    .file_name()
                    .is_some_and(|name| name == CONFIG_FILE_NAME)
                {
                    config_changed = true;
                } else if is_environment_file(&path) {
                    environment_changed = true;
                } else if path.extension().is_some_and(|ext| ext == "jl") {
                    sources.push(path);
                }
            }
        }
        if config_changed {
            self.config.invalidate_discovered();
            self.on_config_changed();
        }
        for path in sources {
            // The moved-from path is gone and the moved-to one is not tracked
            // yet; both sync as no-ops until the re-harvest settles membership.
            if uri::from_path(&path).is_none_or(|uri| !self.documents.contains_key(&uri)) {
                let _ = self.sync_tx.send(SyncMessage::Revert(path.clone()));
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
            Outbound::EnvironmentDiagnostics { uri, diags } => {
                if diags.is_empty() {
                    self.env_diags.remove(&uri);
                } else {
                    self.env_diags.insert(uri.clone(), diags);
                }
                // No pull suppression, unlike the include-graph arm above: a
                // pull report is served only for an open document, and an open
                // environment file has no Julia analysis to carry these, so the
                // push is their only route to any client.
                let version = self.documents.get(&uri).map(|d| d.version);
                self.publish_merged(uri, version);
            }
            Outbound::DiagnosticsRefresh => self.refresh_open_diagnostics(),
            Outbound::InlayHintsRefresh => self.send_inlay_hint_refresh(),
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

    /// Publish the union of `uri`'s parse, include-graph, and environment-file
    /// diagnostics — a single `publishDiagnostics` replaces *all* diagnostics
    /// for a URI, so every source must be sent together or each would clobber
    /// the others.
    ///
    /// The environment-file slot has two producers and takes only one of them:
    /// an open buffer's live findings **supersede** the harvester's set rather
    /// than joining it. The two are computed from different texts — the buffer
    /// and the file on disk — so their union is a report of neither, and a
    /// buffer that does not parse means the harvester's set (its own syntax
    /// verdict, or semantic findings whose ranges index the disk text)
    /// describes a file the user has already moved past. A buffer that parses
    /// contributes nothing and leaves the harvester's set standing, which is
    /// what keeps the semantic checks visible in an open file. The one seam:
    /// fixing a syntax error without saving lets the disk verdict reappear
    /// until the save — truthfully, since that is still the file Julia loads.
    fn publish_merged(&self, uri: Uri, version: Option<i32>) {
        let mut diagnostics = self.parse_diags.get(&uri).cloned().unwrap_or_default();
        let environment = self
            .buffer_diags
            .get(&uri)
            .or_else(|| self.env_diags.get(&uri));
        for source in [self.graph_diags.get(&uri), environment]
            .into_iter()
            .flatten()
        {
            diagnostics.extend(source.iter().cloned());
        }
        self.publish(uri, diagnostics, version);
    }

    /// Route a document's new text to whatever owns it, by
    /// [kind](DocumentKind) — the one fork every `didOpen`/`didChange` takes.
    ///
    /// Julia goes to the analysis pipeline. An environment file (`Project.toml`
    /// and friends) is TOML, and parsing it as Julia would publish nonsense
    /// parse errors that [`publish_merged`](Self::publish_merged) would union
    /// with the file's real diagnostics; it takes the TOML route instead, which
    /// re-derives the file's own findings from the buffer. A *project* file
    /// additionally writes its text to the database, because the linter derives
    /// the package's declared dependencies from it — that is what lets an
    /// unsaved `[deps]` edit reach `unresolved-import` across the package. A
    /// manifest sends no text: nothing reads one without an environment
    /// resolve, which is the harvester's job.
    ///
    /// `edits` belong to the Julia route alone (see
    /// [`send_analysis`](Self::send_analysis)).
    fn route_document(&mut self, uri: Uri, edits: Option<Vec<Edit>>) {
        match self.documents.get(&uri).map(|doc| doc.kind) {
            None => {}
            Some(DocumentKind::Julia) => self.send_analysis(uri, edits),
            Some(kind) => {
                if kind == DocumentKind::Project {
                    self.send_project_text(&uri);
                }
                self.refresh_buffer_diagnostics(uri);
            }
        }
    }

    /// Write an open project file's buffer to the database as text, so the
    /// linter's declared dependencies follow the editor rather than the disk.
    fn send_project_text(&self, uri: &Uri) {
        let (Some(path), Some(doc)) = (uri::to_path(uri), self.documents.get(uri)) else {
            return;
        };
        let _ = self.sync_tx.send(SyncMessage::SetText {
            path,
            text: doc.text.text().to_string(),
        });
    }

    /// Re-derive an open environment file's own diagnostics from its buffer and
    /// republish — the live half of the split stage 2 drew, and the second
    /// producer for these URIs.
    ///
    /// Only the buffer's own set is bookkept here; the merge with the
    /// harvester's is [`publish_merged`](Self::publish_merged)'s. A publish is
    /// sent only when there is something to say or something to take back, so a
    /// keystroke in a file that parses puts nothing on the wire.
    fn refresh_buffer_diagnostics(&mut self, uri: Uri) {
        let (Some(path), Some(doc)) = (uri::to_path(&uri), self.documents.get(&uri)) else {
            return;
        };
        let version = doc.version;
        let diags = super::environment_diagnostics::buffer_diagnostics(
            &path,
            doc.text.text(),
            self.encoding,
        );
        let had = self.buffer_diags.remove(&uri).is_some();
        if diags.is_empty() {
            if !had {
                return;
            }
        } else {
            self.buffer_diags.insert(uri.clone(), diags);
        }
        self.publish_merged(uri, Some(version));
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
            text: Arc::clone(&doc.text),
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
    /// (editor-pushed settings, or a `fatou.toml` event).
    fn on_config_changed(&mut self) {
        self.refresh_open_diagnostics();
    }

    /// Re-derive the open documents' diagnostics, whatever moved underneath
    /// them: a configuration change, a re-harvest, or a project-file edit that
    /// changed the package's declared dependencies. A pull client is nudged to
    /// re-pull (each pull resolves config and reads the db afresh); a push
    /// client gets a re-analysis per open document — same version, new
    /// premises — which the analysis thread's coalescing absorbs.
    ///
    /// A push client needs the second branch for the same reason a pull client
    /// needs the first: `unresolved-import` and `undefined-name` answer to the
    /// library and the declared deps, not to the buffer, so nothing else would
    /// ever revisit them for a document the user is not typing in.
    fn refresh_open_diagnostics(&mut self) {
        if self.pull_diagnostics {
            self.send_diagnostic_refresh();
        } else {
            // Environment files are skipped: they publish no analysis, their
            // own findings answer to the buffer rather than to the premises
            // that moved, and re-routing an unchanged project buffer would only
            // write it back to the db it just came from.
            let uris: Vec<Uri> = self
                .documents
                .iter()
                .filter(|(_, doc)| doc.kind == DocumentKind::Julia)
                .map(|(uri, _)| uri.clone())
                .collect();
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

    /// Ask the client to re-request inlay hints (`workspace/inlayHint/refresh`);
    /// a no-op without the client capability.
    ///
    /// Sent once per full harvest, which is when `package_meta` — the whole of
    /// what a hint reports — comes into existence. Nothing else recomputes a
    /// hint for a file the user is not typing in, so without this an open
    /// `Project.toml` shows a bare UUID for the rest of the session.
    fn send_inlay_hint_refresh(&mut self) {
        if !self.inlay_hint_refresh {
            return;
        }
        self.refresh_seq += 1;
        let _ = self.sender.send(Message::Request(Request {
            id: RequestId::from(format!("fatou-inlay-hint-refresh-{}", self.refresh_seq)),
            method: InlayHintRefreshRequest::METHOD.to_string(),
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

/// The filesystem path the db tracks `uri` under; a non-`file` URI (e.g. an
/// editor's untitled buffer) gets a synthetic path of its own.
fn path_for(uri: &Uri) -> PathBuf {
    uri::to_path_or_synthetic(uri)
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
    use lsp_types::Position;
    use std::str::FromStr;

    /// A `GlobalState` wired to in-memory channels, returned with the client's
    /// receiver so a test can assert on the responses it emits. Only the read
    /// registry, version gate, and cancel paths are exercised here — those
    /// touch nothing but `documents`, `inflight_reads`, and the client sender.
    fn test_state() -> (GlobalState, Receiver<Message>) {
        let (state, channels) = test_state_with_channels();
        // The auxiliary receivers drop here; the methods under test only touch
        // `documents`, `inflight_reads`, and the client sender, so a
        // disconnected analysis/out channel is harmless.
        (state, channels.client)
    }

    /// The receivers a dispatch test asserts on: what the state hands to the
    /// read pool, the harvester, and the disk-sync worker.
    struct TestChannels {
        client: Receiver<Message>,
        analysis: Receiver<AnalysisRequest>,
        read: Receiver<ReadJob>,
        harvest: Receiver<HarvestSignal>,
        sync: Receiver<SyncMessage>,
    }

    /// [`test_state`], keeping every auxiliary receiver alive.
    fn test_state_with_channels() -> (GlobalState, TestChannels) {
        let (client_tx, client_rx) = unbounded();
        let (out_tx, _out_rx) = unbounded();
        let (analysis_tx, analysis_rx) = unbounded();
        let (read_tx, read_rx) = unbounded();
        let (harvest_tx, harvest_rx) = unbounded();
        let (sync_tx, sync_rx) = unbounded();
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
            false,
            None,
        );
        // Discard any startup log.
        while client_rx.try_recv().is_ok() {}
        (
            state,
            TestChannels {
                client: client_rx,
                analysis: analysis_rx,
                read: read_rx,
                harvest: harvest_rx,
                sync: sync_rx,
            },
        )
    }

    fn uri(path: &str) -> Uri {
        Uri::from_str(path).unwrap()
    }

    /// The paths a batch of sync messages reverts to disk, in order.
    fn reverted(sync: &Receiver<SyncMessage>) -> Vec<PathBuf> {
        sync.try_iter()
            .map(|msg| match msg {
                SyncMessage::Revert(path) => path,
                SyncMessage::SetText { path, .. } => {
                    panic!("expected a revert, got a text write for {}", path.display())
                }
            })
            .collect()
    }

    fn open(state: &mut GlobalState, uri: &Uri, version: i32) {
        state.documents.insert(
            uri.clone(),
            Document {
                text: Arc::default(),
                version,
                kind: DocumentKind::of(uri),
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

    /// A dispatched read job carries an `Arc` clone of the live buffer rather
    /// than a copy of its text, so it shares the line table the main loop
    /// maintains. The edit that lands underneath a running job must therefore
    /// copy on write: the job keeps answering against the buffer it was
    /// dispatched for, and both buffers keep a table matching their own text.
    #[test]
    fn an_edit_under_an_inflight_read_copies_on_write() {
        let (mut state, _rx) = test_state();
        let doc = uri("file:///a.jl");
        let encoding = state.encoding;
        state.documents.insert(
            doc.clone(),
            Document {
                text: Arc::new(TextBuffer::from("x = 1\n")),
                version: 1,
                kind: DocumentKind::Julia,
            },
        );
        // What the analysis thread and any read job hold onto.
        let inflight = Arc::clone(&state.documents[&doc].text);

        let entry = state.documents.get_mut(&doc).unwrap();
        apply_content_changes(
            Arc::make_mut(&mut entry.text),
            vec![lsp_types::TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range::new(
                    lsp_types::Position::new(0, 5),
                    lsp_types::Position::new(0, 5),
                )),
                range_length: None,
                text: "\ny = 2".to_string(),
            }],
            encoding,
        );

        let live = Arc::clone(&state.documents[&doc].text);
        assert_eq!(&*inflight, "x = 1\n", "the in-flight read keeps its text");
        assert_eq!(&*live, "x = 1\ny = 2\n", "the document took the edit");
        for buffer in [&inflight, &live] {
            assert_eq!(
                buffer.line_starts(),
                &crate::text::LineStarts::new(buffer),
                "line table drifted from {:?}",
                buffer.text()
            );
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

    /// The `.jl` path `name` under a platform-native absolute directory, plus
    /// its `file:` URI. Unix-style `/work` is not absolute on Windows.
    fn native(name: &str) -> (PathBuf, String) {
        let path = if cfg!(windows) {
            PathBuf::from(format!("C:/work/{name}"))
        } else {
            PathBuf::from(format!("/work/{name}"))
        };
        let uri = uri::from_path(&path)
            .expect("a file URI")
            .as_str()
            .to_string();
        (path, uri)
    }

    fn will_rename_request(id: i32, params: serde_json::Value) -> Request {
        Request {
            id: RequestId::from(id),
            method: WillRenameFiles::METHOD.to_string(),
            params,
        }
    }

    #[test]
    fn will_rename_files_dispatches_a_document_less_read_job() {
        let (mut state, channels) = test_state_with_channels();
        let (open_path, open_uri) = native("open.jl");
        open(&mut state, &uri(&open_uri), 1);
        let (_, old_uri) = native("a.jl");
        let (_, new_uri) = native("sub/a.jl");

        state.on_request(will_rename_request(
            7,
            serde_json::json!({ "files": [{ "oldUri": old_uri, "newUri": new_uri }] }),
        ));

        match channels.read.try_recv().expect("a read job") {
            ReadJob::WillRenameFiles {
                id,
                files,
                open_docs,
                ..
            } => {
                assert_eq!(id, RequestId::from(7));
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].old_uri, old_uri);
                assert_eq!(open_docs, vec![open_path], "open buffers widen the scan");
            }
            _ => panic!("expected a WillRenameFiles job"),
        }
        assert!(
            channels.client.try_recv().is_err(),
            "the read pool answers, not the dispatch"
        );

        // Document-less, so no version can supersede it.
        state.on_read_reply(ok_reply(7));
        assert_eq!(next_error_code(&channels.client), None);
    }

    #[test]
    fn malformed_will_rename_params_answer_an_error() {
        let (mut state, channels) = test_state_with_channels();

        state.on_request(will_rename_request(8, serde_json::json!({ "files": 3 })));

        assert_eq!(
            next_error_code(&channels.client),
            Some(ErrorCode::InvalidParams as i32)
        );
        assert!(channels.read.try_recv().is_err(), "nothing is dispatched");
    }

    #[test]
    fn did_rename_files_syncs_and_reharvests_both_paths() {
        let (mut state, channels) = test_state_with_channels();
        let (old_path, old_uri) = native("src/a.jl");
        let (new_path, new_uri) = native("src/sub/a.jl");

        state.on_did_rename_files(RenameFilesParams {
            files: vec![lsp_types::FileRename { old_uri, new_uri }],
        });

        assert_eq!(
            reverted(&channels.sync),
            vec![old_path.clone(), new_path.clone()]
        );
        let signals: Vec<HarvestSignal> = channels.harvest.try_iter().collect();
        assert_eq!(
            signals,
            vec![
                HarvestSignal::Source(old_path),
                HarvestSignal::Source(new_path),
            ],
            "the vacated and the occupied path each re-harvest their package"
        );
    }

    /// A `Project.toml` changed outside the editor — `pkg> add`, a branch
    /// switch — must reach the tracked input, exactly as a `.jl` file does. The
    /// declared dependencies are read off that input now, so without the sync
    /// the re-resolve this also triggers would leave `unresolved-import`
    /// answering to the text the file had when the server started.
    #[test]
    fn a_watched_project_file_with_no_buffer_syncs_to_disk() {
        let (mut state, channels) = test_state_with_channels();
        let (path, _) = native("Project.toml");

        state.on_watched_files(DidChangeWatchedFilesParams {
            changes: vec![lsp_types::FileEvent {
                uri: uri::from_path(&path).expect("a file URI"),
                typ: lsp_types::FileChangeType::CHANGED,
            }],
        });

        assert_eq!(reverted(&channels.sync), vec![path]);
        assert_eq!(
            channels.harvest.try_iter().collect::<Vec<_>>(),
            vec![HarvestSignal::Environment],
            "and it still escalates to a full re-resolve"
        );
    }

    /// An open buffer stays authoritative: the editor owns the text until it
    /// closes, and reverting under it would drop the user's unsaved `[deps]`.
    #[test]
    fn a_watched_project_file_with_an_open_buffer_does_not_sync() {
        let (mut state, channels) = test_state_with_channels();
        let (path, _) = native("Project.toml");
        let uri = uri::from_path(&path).expect("a file URI");
        open(&mut state, &uri, 1);

        state.on_watched_files(DidChangeWatchedFilesParams {
            changes: vec![lsp_types::FileEvent {
                uri: uri.clone(),
                typ: lsp_types::FileChangeType::CHANGED,
            }],
        });

        assert!(channels.sync.try_recv().is_err());
    }

    #[test]
    fn a_renamed_project_file_escalates_to_an_environment_resolve() {
        let (mut state, channels) = test_state_with_channels();
        let (_, old_uri) = native("Project.toml");
        let (_, new_uri) = native("JuliaProject.toml");

        state.on_did_rename_files(RenameFilesParams {
            files: vec![lsp_types::FileRename { old_uri, new_uri }],
        });

        let signals: Vec<HarvestSignal> = channels.harvest.try_iter().collect();
        assert_eq!(signals, vec![HarvestSignal::Environment]);
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

    /// Each non-`file:` buffer needs a tracked path of its own: sharing one
    /// would put two editor buffers on a single `SourceFile` (and a single
    /// reparse base), so every edit to one invalidates the other's chain.
    #[test]
    fn non_file_uris_get_distinct_tracked_paths() {
        let (first, second) = (uri("untitled:Untitled-1"), uri("untitled:Untitled-2"));
        assert_ne!(
            path_for(&first),
            path_for(&second),
            "two untitled buffers must not share one tracked path"
        );
        assert_eq!(
            path_for(&first),
            path_for(&first),
            "the same buffer must map to a stable path across requests"
        );
        // A `file:` URI is unaffected.
        assert_eq!(
            path_for(&uri("file:///work/a.jl")),
            path_for(&uri("file:///work/a.jl"))
        );
        assert_ne!(path_for(&first), path_for(&uri("file:///work/a.jl")));
    }

    /// Drive a `didOpen` through the notification path, as a client would.
    fn did_open(state: &mut GlobalState, uri: &Uri, text: &str) {
        let note = Notification::new(
            DidOpenTextDocument::METHOD.to_string(),
            DidOpenTextDocumentParams {
                text_document: lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: text.to_string(),
                },
            },
        );
        state.on_notification(note);
    }

    /// Drive a whole-buffer `didChange`, as a client would.
    fn did_change(state: &mut GlobalState, uri: &Uri, version: i32, text: &str) {
        let note = Notification::new(
            DidChangeTextDocument::METHOD.to_string(),
            DidChangeTextDocumentParams {
                text_document: lsp_types::VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.to_string(),
                }],
            },
        );
        state.on_notification(note);
    }

    /// The check IDs a published set carries, in order.
    fn codes(diags: &[Diagnostic]) -> Vec<&str> {
        diags
            .iter()
            .filter_map(|diag| match &diag.code {
                Some(NumberOrString::String(code)) => Some(code.as_str()),
                _ => None,
            })
            .collect()
    }

    /// An environment file is TOML, so it must never reach the Julia analysis.
    /// The document is still tracked — `didChange`/`didClose` stay consistent —
    /// but no `AnalysisRequest` is dispatched for it.
    #[test]
    fn opening_an_environment_file_dispatches_no_analysis() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");

        did_open(&mut state, &project, "[deps]\n");

        assert!(
            state.documents.contains_key(&project),
            "the buffer is still tracked"
        );
        assert!(
            channels.analysis.try_recv().is_err(),
            "a TOML buffer must not be parsed as Julia"
        );
    }

    /// The guard is keyed on the file name, not on being TOML at all: a Julia
    /// source in the same directory still analyzes.
    #[test]
    fn opening_a_julia_file_still_dispatches_analysis() {
        let (mut state, channels) = test_state_with_channels();
        let source = uri("file:///work/src/a.jl");

        did_open(&mut state, &source, "x = 1\n");

        assert!(
            channels.analysis.try_recv().is_ok(),
            "a Julia buffer analyzes as before"
        );
        assert!(
            channels.sync.try_recv().is_err(),
            "a Julia buffer reaches the db through the analysis pipeline, not the sync channel"
        );
    }

    /// An open project file's buffer reaches the database as *text*: the linter
    /// derives the package's declared dependencies from it, so an unsaved
    /// `[deps]` edit must count before any save.
    #[test]
    fn a_project_file_buffer_is_written_to_the_database() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");

        did_open(&mut state, &project, "[deps]\n");
        let opened = channels
            .sync
            .try_recv()
            .expect("the open buffer is written");
        assert!(
            matches!(&opened, SyncMessage::SetText { text, .. } if text == "[deps]\n"),
            "didOpen sends the buffer text"
        );

        state.on_notification(Notification::new(
            DidChangeTextDocument::METHOD.to_string(),
            DidChangeTextDocumentParams {
                text_document: lsp_types::VersionedTextDocumentIdentifier {
                    uri: project,
                    version: 2,
                },
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "[deps]\nFoo = \"x\"\n".to_string(),
                }],
            },
        ));
        let changed = channels.sync.try_recv().expect("the edit is written");
        assert!(
            matches!(&changed, SyncMessage::SetText { text, .. } if text.contains("Foo")),
            "didChange sends the edited buffer text"
        );
        assert!(
            channels.analysis.try_recv().is_err(),
            "and still never as a Julia analysis"
        );
    }

    /// A manifest takes neither fork: nothing reads it without an environment
    /// resolve, which is the harvester's job and reads disk.
    #[test]
    fn a_manifest_buffer_is_not_written_to_the_database() {
        let (mut state, channels) = test_state_with_channels();

        did_open(&mut state, &uri("file:///work/Manifest.toml"), "[deps]\n");

        assert!(channels.sync.try_recv().is_err());
        assert!(channels.analysis.try_recv().is_err());
    }

    /// The diagnostics published for `uri`, from the next `publishDiagnostics`
    /// notification on the wire.
    fn next_publish(rx: &Receiver<Message>) -> (Vec<Diagnostic>, Option<i32>) {
        match rx.try_recv().expect("a publishDiagnostics notification") {
            Message::Notification(note) => {
                assert_eq!(note.method, PublishDiagnostics::METHOD);
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(note.params).expect("publish params");
                (params.diagnostics, params.version)
            }
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    fn diag(message: &str) -> Diagnostic {
        Diagnostic {
            message: message.to_string(),
            ..Default::default()
        }
    }

    /// A refresh nudge reaches a *push* client too. It has no
    /// `workspace/diagnostic/refresh` to answer, so the open documents are
    /// re-analyzed instead — otherwise `unresolved-import` would keep answering
    /// to premises that have since moved, in every file the user is not typing
    /// in. The project file itself is skipped: it publishes no analysis.
    #[test]
    fn a_refresh_reanalyzes_a_push_clients_open_documents() {
        let (mut state, channels) = test_state_with_channels();
        let source = uri("file:///work/src/a.jl");
        let project = uri("file:///work/Project.toml");
        assert!(!state.pull_diagnostics, "this client pushes");
        open(&mut state, &source, 3);
        open(&mut state, &project, 1);

        state.on_outbound(Outbound::DiagnosticsRefresh);

        let requested: Vec<Uri> = channels.analysis.try_iter().map(|req| req.uri).collect();
        assert_eq!(requested, vec![source]);
        assert!(
            channels.sync.try_recv().is_err(),
            "and the project buffer is not written back to the db it came from"
        );
    }

    /// A `publishDiagnostics` replaces *all* diagnostics for a URI, so the
    /// three sources — parse, include-graph, environment file — must travel
    /// together. Each arriving alone must republish the union rather than the
    /// slice that happened to change.
    #[test]
    fn every_diagnostic_source_republishes_the_union() {
        let (mut state, channels) = test_state_with_channels();
        let doc = uri("file:///work/Project.toml");

        // A closed file: environment diagnostics attach to it regardless.
        state.on_outbound(Outbound::EnvironmentDiagnostics {
            uri: doc.clone(),
            diags: vec![diag("no [compat] bound on julia")],
        });
        let (diags, version) = next_publish(&channels.client);
        assert_eq!(diags.len(), 1);
        assert_eq!(version, None, "a closed file has no version");

        // The same URI opened and parsed: both sources publish together.
        open(&mut state, &doc, 7);
        state.on_outbound(Outbound::Diagnostics {
            uri: doc.clone(),
            version: 7,
            diags: vec![diag("parse error")],
        });
        let (diags, version) = next_publish(&channels.client);
        assert_eq!(
            diags.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(),
            vec!["parse error", "no [compat] bound on julia"],
        );
        assert_eq!(version, Some(7));

        // Clearing one source leaves the other standing.
        state.on_outbound(Outbound::EnvironmentDiagnostics {
            uri: doc.clone(),
            diags: Vec::new(),
        });
        let (diags, _) = next_publish(&channels.client);
        assert_eq!(
            diags.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(),
            vec!["parse error"],
        );
    }

    /// Environment diagnostics describe the file on disk, so closing a buffer
    /// drops its parse diagnostics and keeps theirs.
    #[test]
    fn environment_diagnostics_survive_a_close() {
        let (mut state, channels) = test_state_with_channels();
        let doc = uri("file:///work/Project.toml");
        open(&mut state, &doc, 1);
        state.on_outbound(Outbound::EnvironmentDiagnostics {
            uri: doc.clone(),
            diags: vec![diag("no [compat] bound on julia")],
        });
        let _ = next_publish(&channels.client);

        state.on_notification(Notification::new(
            DidCloseTextDocument::METHOD.to_string(),
            DidCloseTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: doc.clone() },
            },
        ));

        let (diags, _) = next_publish(&channels.client);
        assert_eq!(
            diags.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(),
            vec!["no [compat] bound on julia"],
        );
    }

    /// The pull twin of the guard: a client that opens an environment file and
    /// then pulls gets an empty report rather than a Julia parse of its TOML.
    #[test]
    fn pulling_diagnostics_for_an_environment_file_reports_nothing() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");
        open(&mut state, &project, 1);

        let req = Request::new(
            RequestId::from(1),
            DocumentDiagnosticRequest::METHOD.to_string(),
            DocumentDiagnosticParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: project.clone(),
                },
                identifier: None,
                previous_result_id: None,
                partial_result_params: Default::default(),
                work_done_progress_params: Default::default(),
            },
        );
        state.on_request(req);

        assert_eq!(
            next_error_code(&channels.client),
            None,
            "an empty report, not an error"
        );
        assert!(
            channels.read.try_recv().is_err(),
            "no read job is dispatched for a TOML buffer"
        );
    }

    /// An open environment file's *own* diagnostics come from its buffer, at
    /// edit cadence: a `Project.toml` that does not parse reports on itself
    /// with no save and no harvest.
    #[test]
    fn a_broken_project_buffer_publishes_its_syntax_error() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");

        did_open(&mut state, &project, "name = \"Demo\"\nuuid = \n");

        let (diags, version) = next_publish(&channels.client);
        let [diag] = &diags[..] else {
            panic!("expected one diagnostic, got {diags:?}");
        };
        assert_eq!(codes(&diags), vec!["toml-syntax"]);
        assert_eq!(diag.range.start.line, 1, "the `uuid = ` line");
        assert_eq!(version, Some(1), "tagged with the buffer it describes");
    }

    /// A manifest buffer takes the same route: nothing reads it without a
    /// resolve, but its syntax is text-only and so is the buffer's to answer.
    #[test]
    fn a_broken_manifest_buffer_publishes_its_syntax_error() {
        let (mut state, channels) = test_state_with_channels();
        let manifest = uri("file:///work/Manifest.toml");

        did_open(&mut state, &manifest, "[[deps\n");

        assert_eq!(
            codes(&next_publish(&channels.client).0),
            vec!["toml-syntax"]
        );
        assert!(
            channels.sync.try_recv().is_err(),
            "and still never reaches the database"
        );
    }

    /// Two producers write one environment file's diagnostics: the harvester,
    /// off disk at resolve cadence, and the open buffer, at edit cadence. They
    /// are never unioned — computed from different texts, their union is a
    /// report of neither. A live syntax error supersedes the harvester's set
    /// (whose ranges index a text the user has moved past), and a buffer that
    /// parses hands the file back.
    #[test]
    fn a_live_syntax_error_supersedes_the_harvesters_findings() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");
        state.on_outbound(Outbound::EnvironmentDiagnostics {
            uri: project.clone(),
            diags: vec![diag("no [compat] bound on julia")],
        });
        let _ = next_publish(&channels.client);

        // Opened, and it parses: the buffer has nothing to say, so the
        // harvester's finding stands and nothing is republished.
        did_open(&mut state, &project, "name = \"Demo\"\n");
        assert!(
            channels.client.try_recv().is_err(),
            "a clean buffer publishes nothing of its own"
        );

        // Edited into a syntax error: the buffer's verdict is the fresher one.
        did_change(&mut state, &project, 2, "name = \"Demo\"\nuuid = \n");
        let (diags, version) = next_publish(&channels.client);
        assert_eq!(codes(&diags), vec!["toml-syntax"]);
        assert_eq!(version, Some(2));

        // Fixed, still unsaved: the harvester's set takes the file back.
        did_change(&mut state, &project, 3, "name = \"Demo\"\n");
        let (diags, _) = next_publish(&channels.client);
        assert_eq!(
            diags.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(),
            vec!["no [compat] bound on julia"],
        );
    }

    /// The live findings go with the buffer: closing hands the file back to the
    /// harvester's set, which describes it on disk, open or not.
    #[test]
    fn closing_an_environment_buffer_drops_its_live_findings() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");
        state.on_outbound(Outbound::EnvironmentDiagnostics {
            uri: project.clone(),
            diags: vec![diag("no [compat] bound on julia")],
        });
        let _ = next_publish(&channels.client);
        did_open(&mut state, &project, "uuid = \n");
        assert_eq!(
            codes(&next_publish(&channels.client).0),
            vec!["toml-syntax"]
        );

        state.on_notification(Notification::new(
            DidCloseTextDocument::METHOD.to_string(),
            DidCloseTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: project },
            },
        ));

        let (diags, _) = next_publish(&channels.client);
        assert_eq!(
            diags.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(),
            vec!["no [compat] bound on julia"],
        );
    }

    /// With the environment files in the client's document selector, every
    /// request can now arrive for one. Nothing **Julia-backed** may answer: a
    /// format or a document symbol would parse TOML as Julia and hand back
    /// nonsense, so they answer `null`, as they do for a document the server
    /// never saw. The TOML-aware features are the exception, and go through
    /// `project_text` instead — see the two tests below.
    #[test]
    fn language_features_answer_nothing_for_an_environment_document() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");
        did_open(&mut state, &project, "[deps]\n");

        let document = lsp_types::TextDocumentIdentifier { uri: project };
        let requests = [
            Request::new(
                RequestId::from(1),
                Formatting::METHOD.to_string(),
                DocumentFormattingParams {
                    text_document: document.clone(),
                    options: Default::default(),
                    work_done_progress_params: Default::default(),
                },
            ),
            Request::new(
                RequestId::from(2),
                DocumentSymbolRequest::METHOD.to_string(),
                DocumentSymbolParams {
                    text_document: document.clone(),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            ),
        ];
        for request in requests {
            let id = request.id.clone();
            state.on_request(request);
            match channels.client.try_recv().expect("a response") {
                Message::Response(resp) => {
                    assert_eq!(resp.id, id);
                    assert_eq!(
                        resp.response_result.ok(),
                        Some(serde_json::Value::Null),
                        "for {id}"
                    );
                }
                other => panic!("expected a response, got {other:?}"),
            }
        }
        assert!(
            channels.read.try_recv().is_err(),
            "and no TOML buffer reaches the read pool for a Julia-backed feature"
        );
    }

    /// The other side of the fork: a project file *does* reach the read pool
    /// for a feature that understands TOML, so a `[deps]` name can resolve to
    /// its package.
    #[test]
    fn a_project_buffer_reaches_the_read_pool_for_navigation() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");
        did_open(&mut state, &project, "[deps]\nA = \"x\"\n");

        state.on_request(definition_request(1, &project));

        match channels.read.try_recv().expect("a read job") {
            ReadJob::ProjectDefinition { id, position, .. } => {
                assert_eq!(id, RequestId::from(1));
                assert_eq!(position, Position::new(1, 0));
            }
            _ => panic!("expected a ProjectDefinition read job"),
        }
    }

    /// A manifest still answers nothing for the features that resolve a name
    /// through the environment: its text never reaches the database, and a
    /// manifest names no package the way a `[deps]` key does. Document links
    /// are the one exception — see the test below.
    #[test]
    fn a_manifest_answers_nothing_for_navigation() {
        let (mut state, channels) = test_state_with_channels();
        let manifest = uri("file:///work/Manifest.toml");
        did_open(&mut state, &manifest, "manifest_format = \"2.0\"\n");

        state.on_request(definition_request(1, &manifest));

        match channels.client.try_recv().expect("a response") {
            Message::Response(resp) => {
                assert_eq!(resp.response_result.ok(), Some(serde_json::Value::Null));
            }
            other => panic!("expected a response, got {other:?}"),
        }
        assert!(channels.read.try_recv().is_err(), "and no read job");
    }

    /// The exception: a manifest's `path` entries *are* document links, so its
    /// buffer reaches the read pool for that one request — carrying the path it
    /// resolves those entries against, which no other project-file job needs.
    #[test]
    fn a_manifest_buffer_reaches_the_read_pool_for_document_links() {
        let (mut state, channels) = test_state_with_channels();
        let manifest = uri("file:///work/Manifest.toml");
        did_open(&mut state, &manifest, "[[deps.A]]\npath = \"../A\"\n");

        state.on_request(Request::new(
            RequestId::from(1),
            DocumentLinkRequest::METHOD.to_string(),
            DocumentLinkParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: manifest.clone(),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        ));

        match channels.read.try_recv().expect("a read job") {
            ReadJob::ManifestDocumentLinks { id, path, .. } => {
                assert_eq!(id, RequestId::from(1));
                assert_eq!(path, path_for(&manifest));
            }
            _ => panic!("expected a ManifestDocumentLinks read job"),
        }
    }

    /// Inlay hints are a project file's alone, but unlike the other three a
    /// Julia document answers an **empty list** rather than `null`: the
    /// capability is advertised globally, and "this file has no hints" is the
    /// honest answer, not "I do not do hints".
    #[test]
    fn inlay_hints_answer_empty_for_a_julia_document() {
        let (mut state, channels) = test_state_with_channels();
        let source = uri("file:///work/a.jl");
        did_open(&mut state, &source, "x = 1\n");
        // Drain the analysis the open dispatched.
        let _ = channels.analysis.try_recv();

        state.on_request(inlay_hint_request(1, &source));

        match channels.client.try_recv().expect("a response") {
            Message::Response(resp) => {
                assert_eq!(resp.response_result.ok(), Some(serde_json::json!([])));
            }
            other => panic!("expected a response, got {other:?}"),
        }
        assert!(channels.read.try_recv().is_err(), "and no read job");
    }

    #[test]
    fn a_project_buffer_reaches_the_read_pool_for_inlay_hints() {
        let (mut state, channels) = test_state_with_channels();
        let project = uri("file:///work/Project.toml");
        did_open(&mut state, &project, "[deps]\nA = \"x\"\n");

        state.on_request(inlay_hint_request(1, &project));

        match channels.read.try_recv().expect("a read job") {
            ReadJob::ProjectInlayHints { id, range, .. } => {
                assert_eq!(id, RequestId::from(1));
                assert_eq!(range.end, Position::new(2, 0));
            }
            _ => panic!("expected a ProjectInlayHints read job"),
        }
    }

    /// The library map arrives with the harvest, seconds after `initialize`,
    /// and it is the whole of what a `[deps]` hint reports. A client
    /// re-requests hints only on an edit or a scroll, so without the nudge a
    /// `Project.toml` opened at startup shows a bare UUID for the session.
    #[test]
    fn a_landed_harvest_asks_the_client_to_re_request_hints() {
        let (mut state, channels) = test_state_with_channels();
        state.inlay_hint_refresh = true;

        state.on_outbound(Outbound::InlayHintsRefresh);

        match channels.client.try_recv().expect("a request") {
            Message::Request(req) => assert_eq!(req.method, InlayHintRefreshRequest::METHOD),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    /// The spec has the request force a *global* recalculation of every visible
    /// hint, so a client that never claimed the capability is not handed one.
    #[test]
    fn an_inlay_hint_refresh_needs_the_client_capability() {
        let (mut state, channels) = test_state_with_channels();
        assert!(!state.inlay_hint_refresh, "the default test client");

        state.on_outbound(Outbound::InlayHintsRefresh);

        assert!(channels.client.try_recv().is_err());
    }

    /// A `textDocument/inlayHint` over the first two lines of `uri`.
    fn inlay_hint_request(id: i32, uri: &Uri) -> Request {
        Request::new(
            RequestId::from(id),
            InlayHintRequest::METHOD.to_string(),
            InlayHintParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                range: lsp_types::Range::new(Position::new(0, 0), Position::new(2, 0)),
                work_done_progress_params: Default::default(),
            },
        )
    }

    /// A `textDocument/definition` at line 1, column 0 of `uri`.
    fn definition_request(id: i32, uri: &Uri) -> Request {
        Request::new(
            RequestId::from(id),
            GotoDefinition::METHOD.to_string(),
            GotoDefinitionParams {
                text_document_position_params: lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(1, 0),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        )
    }

    /// A buffer with no file name behind it — an editor's untitled document —
    /// is Julia: it is what the editor opened the server for, and there is no
    /// file name to say otherwise.
    #[test]
    fn an_untitled_buffer_is_julia() {
        assert_eq!(
            DocumentKind::of(&uri("untitled:Untitled-1")),
            DocumentKind::Julia
        );
        assert_eq!(
            DocumentKind::of(&uri("file:///work/Project.toml")),
            DocumentKind::Project
        );
        assert_eq!(
            DocumentKind::of(&uri("file:///work/Manifest-v1.11.toml")),
            DocumentKind::Manifest
        );
        assert_eq!(
            DocumentKind::of(&uri("file:///work/src/a.jl")),
            DocumentKind::Julia
        );
    }
}
