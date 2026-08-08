//! Read-only jobs serviced off the analysis thread's cached state.

use std::path::PathBuf;

use crossbeam_channel::{SendError, Sender};
use lsp_server::{ErrorCode, Message, RequestId, Response};

use lsp_types::{
    CallHierarchyItem, CodeActionOrCommand, CompletionItem, CompletionResponse,
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, DocumentSymbolResponse, FileRename,
    FullDocumentDiagnosticReport, GotoDefinitionResponse, Position, Range,
    RelatedFullDocumentDiagnosticReport, RelatedUnchangedDocumentDiagnosticReport,
    TypeHierarchyItem, UnchangedDocumentDiagnosticReport, Uri, WorkspaceSymbolResponse,
};

use std::sync::Arc;

use crate::formatter::FormatStyle;
use crate::incremental::Analysis;
use crate::text::PositionEncoding;

use super::call_hierarchy::{
    incoming_calls_via_db, outgoing_calls_via_db, prepare_call_hierarchy_via_db,
};
use super::code_action::code_actions_via_db;
use super::completion::{completion_via_db, resolve_completion};
use super::definition::definition_via_db;
use super::document_link::document_links_via_db;
use super::folding::folding_ranges_via_db;
use super::format::{format_edits_via_db, format_range_edits_via_db};
use super::hover::hover_via_db;
use super::lint::ServerRules;
use super::pull_diagnostics::document_diagnostics_via_db;
use super::references::{document_highlights_via_db, references_via_db};
use super::rename::{prepare_rename_via_db, rename_via_db};
use super::rename_files::will_rename_files_via_db;
use super::result_id::content_hash;
use super::selection::selection_ranges_via_db;
use super::semantic_tokens::{semantic_tokens_delta, semantic_tokens_via_db};
use super::signature_help::signature_help_via_db;
use super::state::Outbound;
use super::symbols::document_symbols_via_db;
use super::type_hierarchy::{prepare_type_hierarchy_via_db, subtypes_via_db, supertypes_via_db};
use super::workspace_symbols::workspace_symbols_via_db;

/// A read job's reply channel. Rather than answering the client directly, a
/// worker routes its response back through the main loop (as
/// [`Outbound::ReadReply`]), which owns the document versions: it gates the
/// reply on the version the read was dispatched against (a superseded buffer
/// becomes `ContentModified`) and drops it entirely when a `$/cancelRequest`
/// landed while the job ran. The `send` shape mirrors the `Sender<Message>` the
/// workers replied on before, so [`run_read`] is unchanged.
pub(crate) struct ReadReply {
    out_tx: Sender<Outbound>,
}

impl ReadReply {
    pub(crate) fn new(out_tx: Sender<Outbound>) -> Self {
        Self { out_tx }
    }

    /// Route `message` (always a `Message::Response`) to the main loop for
    /// gating. Errors only if the main loop is gone (shutdown), like the old
    /// direct send.
    pub(crate) fn send(self, message: Message) -> Result<(), SendError<Outbound>> {
        self.out_tx.send(Outbound::ReadReply { message })
    }
}

/// A read-only request the analysis thread services by cloning its salsa db
/// and running the work off-thread on the read pool. Each variant carries the
/// live buffer `text` and the [`ReadReply`] channel so the worker can reply;
/// the analysis thread only adds the db snapshot. See [`run_read`].
pub(crate) enum ReadJob {
    CodeAction {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        range: Range,
        rules: Arc<ServerRules>,
        sender: ReadReply,
    },
    DocumentDiagnostic {
        id: RequestId,
        path: PathBuf,
        text: String,
        rules: Arc<ServerRules>,
        /// The `resultId` the client last held for this document, if any; a
        /// match against the freshly computed id collapses the report to
        /// `Unchanged` (see [`diagnostic_report`]).
        previous_result_id: Option<String>,
        sender: ReadReply,
    },
    Format {
        id: RequestId,
        path: PathBuf,
        text: String,
        style: FormatStyle,
        sender: ReadReply,
    },
    FormatRange {
        id: RequestId,
        path: PathBuf,
        text: String,
        range: Range,
        style: FormatStyle,
        sender: ReadReply,
    },
    DocumentSymbols {
        id: RequestId,
        path: PathBuf,
        text: String,
        sender: ReadReply,
    },
    WorkspaceSymbols {
        id: RequestId,
        query: String,
        sender: ReadReply,
    },
    FoldingRanges {
        id: RequestId,
        path: PathBuf,
        text: String,
        sender: ReadReply,
    },
    DocumentLinks {
        id: RequestId,
        path: PathBuf,
        text: String,
        sender: ReadReply,
    },
    SelectionRanges {
        id: RequestId,
        path: PathBuf,
        text: String,
        positions: Vec<Position>,
        sender: ReadReply,
    },
    SemanticTokensFull {
        id: RequestId,
        path: PathBuf,
        text: String,
        sender: ReadReply,
    },
    SemanticTokensDelta {
        id: RequestId,
        path: PathBuf,
        text: String,
        /// The `resultId` from the client's last full or delta response; a
        /// match against the freshly computed id answers an empty delta.
        previous_result_id: String,
        sender: ReadReply,
    },
    Completion {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: ReadReply,
    },
    CompletionResolve {
        id: RequestId,
        item: Box<CompletionItem>,
        sender: ReadReply,
    },
    Hover {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: ReadReply,
    },
    SignatureHelp {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: ReadReply,
    },
    Definition {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        position: Position,
        sender: ReadReply,
    },
    References {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        position: Position,
        include_declaration: bool,
        sender: ReadReply,
    },
    DocumentHighlight {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: ReadReply,
    },
    PrepareRename {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: ReadReply,
    },
    Rename {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        position: Position,
        new_name: String,
        sender: ReadReply,
    },
    /// Document-less: a rename batch names files the editor is about to move,
    /// most of which have no open buffer, so the worker reads every scanned
    /// file's text off the snapshot. `open_docs` widens the scan past the
    /// seeded members to whatever the client currently has open.
    WillRenameFiles {
        id: RequestId,
        files: Vec<FileRename>,
        open_docs: Vec<PathBuf>,
        sender: ReadReply,
    },
    PrepareCallHierarchy {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        position: Position,
        sender: ReadReply,
    },
    /// Document-less (like `CompletionResolve`): the item's file may be a
    /// closed member, so the worker resolves its text off the snapshot.
    CallHierarchyIncoming {
        id: RequestId,
        item: Box<CallHierarchyItem>,
        sender: ReadReply,
    },
    CallHierarchyOutgoing {
        id: RequestId,
        item: Box<CallHierarchyItem>,
        sender: ReadReply,
    },
    PrepareTypeHierarchy {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        position: Position,
        sender: ReadReply,
    },
    /// Document-less (like `CallHierarchyIncoming`): the item's file may be a
    /// closed member, so the worker resolves its text off the snapshot.
    TypeHierarchySupertypes {
        id: RequestId,
        item: Box<TypeHierarchyItem>,
        sender: ReadReply,
    },
    TypeHierarchySubtypes {
        id: RequestId,
        item: Box<TypeHierarchyItem>,
        sender: ReadReply,
    },
}

impl ReadJob {
    /// Recover the request `id` and reply channel from an undeliverable job so
    /// the client still gets a (null) response instead of hanging.
    pub(crate) fn into_reply_parts(self) -> (RequestId, ReadReply) {
        match self {
            ReadJob::CodeAction { id, sender, .. } => (id, sender),
            ReadJob::DocumentDiagnostic { id, sender, .. } => (id, sender),
            ReadJob::Format { id, sender, .. } => (id, sender),
            ReadJob::FormatRange { id, sender, .. } => (id, sender),
            ReadJob::DocumentSymbols { id, sender, .. } => (id, sender),
            ReadJob::WorkspaceSymbols { id, sender, .. } => (id, sender),
            ReadJob::FoldingRanges { id, sender, .. } => (id, sender),
            ReadJob::DocumentLinks { id, sender, .. } => (id, sender),
            ReadJob::SelectionRanges { id, sender, .. } => (id, sender),
            ReadJob::SemanticTokensFull { id, sender, .. } => (id, sender),
            ReadJob::SemanticTokensDelta { id, sender, .. } => (id, sender),
            ReadJob::Completion { id, sender, .. } => (id, sender),
            ReadJob::CompletionResolve { id, sender, .. } => (id, sender),
            ReadJob::Hover { id, sender, .. } => (id, sender),
            ReadJob::SignatureHelp { id, sender, .. } => (id, sender),
            ReadJob::Definition { id, sender, .. } => (id, sender),
            ReadJob::References { id, sender, .. } => (id, sender),
            ReadJob::DocumentHighlight { id, sender, .. } => (id, sender),
            ReadJob::PrepareRename { id, sender, .. } => (id, sender),
            ReadJob::Rename { id, sender, .. } => (id, sender),
            ReadJob::WillRenameFiles { id, sender, .. } => (id, sender),
            ReadJob::PrepareCallHierarchy { id, sender, .. } => (id, sender),
            ReadJob::CallHierarchyIncoming { id, sender, .. } => (id, sender),
            ReadJob::CallHierarchyOutgoing { id, sender, .. } => (id, sender),
            ReadJob::PrepareTypeHierarchy { id, sender, .. } => (id, sender),
            ReadJob::TypeHierarchySupertypes { id, sender, .. } => (id, sender),
            ReadJob::TypeHierarchySubtypes { id, sender, .. } => (id, sender),
        }
    }
}

/// An id-less full report, used for the unknown-document case (a never-opened
/// path the client can't hold a prior `resultId` for, so there is nothing to
/// match against). Reports for open documents go through [`diagnostic_report`],
/// which keys them by content so a re-pull can answer `Unchanged`.
pub(crate) fn full_report(items: Vec<lsp_types::Diagnostic>) -> DocumentDiagnosticReportResult {
    DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
        RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: None,
                items,
            },
        },
    ))
}

/// Build the pull-diagnostic report for `items`, keyed by a content hash so an
/// unchanged file re-pulls cheaply. When the client's `previous_result_id`
/// matches the freshly computed id the findings are unchanged since its last
/// pull, so the report collapses to `Unchanged` (id only, no items); otherwise
/// it is a `Full` report carrying the new id for the client to echo back next
/// time.
pub(crate) fn diagnostic_report(
    items: Vec<lsp_types::Diagnostic>,
    previous_result_id: Option<&str>,
) -> DocumentDiagnosticReportResult {
    let result_id = content_hash(&items);
    if previous_result_id == Some(result_id.as_str()) {
        return DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(
            RelatedUnchangedDocumentDiagnosticReport {
                related_documents: None,
                unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                    result_id,
                },
            },
        ));
    }
    DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
        RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: Some(result_id),
                items,
            },
        },
    ))
}

/// Service a read-only job against a db `snapshot`, replying to the client.
/// Runs on a read-pool worker; the `snapshot` is dropped on return so it never
/// blocks the analysis thread's next write longer than the job itself.
pub(crate) fn run_read(snapshot: Analysis, job: ReadJob, encoding: PositionEncoding) {
    match job {
        ReadJob::CodeAction {
            id,
            uri,
            path,
            text,
            range,
            rules,
            sender,
        } => {
            let actions: Vec<CodeActionOrCommand> =
                code_actions_via_db(&snapshot, &uri, &path, &text, range, encoding, &rules);
            let _ = sender.send(Message::Response(Response::new_ok(id, actions)));
        }
        ReadJob::DocumentDiagnostic {
            id,
            path,
            text,
            rules,
            previous_result_id,
            sender,
        } => {
            let items = document_diagnostics_via_db(&snapshot, &path, &text, encoding, &rules);
            let result = diagnostic_report(items, previous_result_id.as_deref());
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::Format {
            id,
            path,
            text,
            style,
            sender,
        } => {
            let result = format_edits_via_db(&snapshot, &path, &text, style, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::FormatRange {
            id,
            path,
            text,
            range,
            style,
            sender,
        } => {
            let result = format_range_edits_via_db(&snapshot, &path, &text, range, style, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::DocumentSymbols {
            id,
            path,
            text,
            sender,
        } => {
            let symbols = document_symbols_via_db(&snapshot, &path, &text, encoding);
            let result = DocumentSymbolResponse::Nested(symbols);
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::WorkspaceSymbols { id, query, sender } => {
            let symbols = workspace_symbols_via_db(&snapshot, &query, encoding);
            let result = WorkspaceSymbolResponse::Nested(symbols);
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::FoldingRanges {
            id,
            path,
            text,
            sender,
        } => {
            // Folds are line-only, so the position encoding is irrelevant.
            let folds = folding_ranges_via_db(&snapshot, &path, &text);
            let _ = sender.send(Message::Response(Response::new_ok(id, folds)));
        }
        ReadJob::DocumentLinks {
            id,
            path,
            text,
            sender,
        } => {
            let links = document_links_via_db(&snapshot, &path, &text, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, links)));
        }
        ReadJob::SelectionRanges {
            id,
            path,
            text,
            positions,
            sender,
        } => {
            let ranges = selection_ranges_via_db(&snapshot, &path, &text, &positions, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, ranges)));
        }
        ReadJob::SemanticTokensFull {
            id,
            path,
            text,
            sender,
        } => {
            let tokens = semantic_tokens_via_db(&snapshot, &path, &text, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, tokens)));
        }
        ReadJob::SemanticTokensDelta {
            id,
            path,
            text,
            previous_result_id,
            sender,
        } => {
            let tokens = semantic_tokens_via_db(&snapshot, &path, &text, encoding);
            let result = semantic_tokens_delta(tokens, &previous_result_id);
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::Completion {
            id,
            path,
            text,
            position,
            sender,
        } => {
            let items = completion_via_db(&snapshot, &path, &text, position, encoding);
            let result = CompletionResponse::Array(items);
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::CompletionResolve { id, item, sender } => {
            let resolved = resolve_completion(&snapshot, *item);
            let _ = sender.send(Message::Response(Response::new_ok(id, resolved)));
        }
        ReadJob::Hover {
            id,
            path,
            text,
            position,
            sender,
        } => {
            let hover = hover_via_db(&snapshot, &path, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, hover)));
        }
        ReadJob::SignatureHelp {
            id,
            path,
            text,
            position,
            sender,
        } => {
            let help = signature_help_via_db(&snapshot, &path, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, help)));
        }
        ReadJob::Definition {
            id,
            uri,
            path,
            text,
            position,
            sender,
        } => {
            let mut locations =
                definition_via_db(&snapshot, &uri, &path, &text, position, encoding);
            // A single site stays scalar (a plain jump); several methods of a
            // function come back as an array (the client shows a picker).
            let result = match locations.len() {
                0 => None,
                1 => Some(GotoDefinitionResponse::Scalar(locations.remove(0))),
                _ => Some(GotoDefinitionResponse::Array(locations)),
            };
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::References {
            id,
            uri,
            path,
            text,
            position,
            include_declaration,
            sender,
        } => {
            let locations = references_via_db(
                &snapshot,
                &uri,
                &path,
                &text,
                position,
                encoding,
                include_declaration,
            );
            let _ = sender.send(Message::Response(Response::new_ok(id, locations)));
        }
        ReadJob::DocumentHighlight {
            id,
            path,
            text,
            position,
            sender,
        } => {
            let highlights =
                document_highlights_via_db(&snapshot, &path, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, highlights)));
        }
        ReadJob::PrepareRename {
            id,
            path,
            text,
            position,
            sender,
        } => {
            let result = prepare_rename_via_db(&snapshot, &path, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::Rename {
            id,
            uri,
            path,
            text,
            position,
            new_name,
            sender,
        } => {
            let response =
                match rename_via_db(&snapshot, &uri, &path, &text, position, &new_name, encoding) {
                    Ok(edit) => Response::new_ok(id, edit),
                    Err(message) => Response::new_err(id, ErrorCode::InvalidParams as i32, message),
                };
            let _ = sender.send(Message::Response(response));
        }
        ReadJob::WillRenameFiles {
            id,
            files,
            open_docs,
            sender,
        } => {
            let edit = will_rename_files_via_db(&snapshot, &files, &open_docs, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, edit)));
        }
        ReadJob::PrepareCallHierarchy {
            id,
            uri,
            path,
            text,
            position,
            sender,
        } => {
            let items =
                prepare_call_hierarchy_via_db(&snapshot, &uri, &path, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, items)));
        }
        ReadJob::CallHierarchyIncoming { id, item, sender } => {
            let calls = incoming_calls_via_db(&snapshot, &item, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, calls)));
        }
        ReadJob::CallHierarchyOutgoing { id, item, sender } => {
            let calls = outgoing_calls_via_db(&snapshot, &item, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, calls)));
        }
        ReadJob::PrepareTypeHierarchy {
            id,
            uri,
            path,
            text,
            position,
            sender,
        } => {
            let items =
                prepare_type_hierarchy_via_db(&snapshot, &uri, &path, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, items)));
        }
        ReadJob::TypeHierarchySupertypes { id, item, sender } => {
            let items = supertypes_via_db(&snapshot, &item, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, items)));
        }
        ReadJob::TypeHierarchySubtypes { id, item, sender } => {
            let items = subtypes_via_db(&snapshot, &item, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, items)));
        }
    }
}
