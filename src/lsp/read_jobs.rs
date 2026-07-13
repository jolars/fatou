//! Read-only jobs serviced off the analysis thread's cached state.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use lsp_server::{ErrorCode, Message, RequestId, Response};

use lsp_types::{
    CodeActionOrCommand, CompletionItem, CompletionResponse, DocumentDiagnosticReport,
    DocumentDiagnosticReportResult, DocumentSymbolResponse, FullDocumentDiagnosticReport,
    GotoDefinitionResponse, Position, Range, RelatedFullDocumentDiagnosticReport, Uri,
    WorkspaceSymbolResponse,
};

use crate::formatter::FormatStyle;
use crate::incremental::Analysis;
use crate::text::PositionEncoding;

use super::code_action::code_actions_via_db;
use super::completion::{completion_via_db, resolve_completion};
use super::definition::definition_via_db;
use super::folding::folding_ranges_via_db;
use super::format::{format_edits_via_db, format_range_edits_via_db};
use super::hover::hover_via_db;
use super::pull_diagnostics::document_diagnostics_via_db;
use super::references::{document_highlights_via_db, references_via_db};
use super::rename::{prepare_rename_via_db, rename_via_db};
use super::selection::selection_ranges_via_db;
use super::semantic_tokens::semantic_tokens_via_db;
use super::signature_help::signature_help_via_db;
use super::symbols::document_symbols_via_db;
use super::workspace_symbols::workspace_symbols_via_db;

/// A read-only request the analysis thread services by cloning its salsa db
/// and running the work off-thread on the read pool. Each variant carries the
/// live buffer `text` and the client `sender` so the worker can reply
/// directly; the analysis thread only adds the db snapshot. See [`run_read`].
pub(crate) enum ReadJob {
    CodeAction {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        range: Range,
        sender: Sender<Message>,
    },
    DocumentDiagnostic {
        id: RequestId,
        path: PathBuf,
        text: String,
        sender: Sender<Message>,
    },
    Format {
        id: RequestId,
        path: PathBuf,
        text: String,
        style: FormatStyle,
        sender: Sender<Message>,
    },
    FormatRange {
        id: RequestId,
        path: PathBuf,
        text: String,
        range: Range,
        style: FormatStyle,
        sender: Sender<Message>,
    },
    DocumentSymbols {
        id: RequestId,
        path: PathBuf,
        text: String,
        sender: Sender<Message>,
    },
    WorkspaceSymbols {
        id: RequestId,
        query: String,
        sender: Sender<Message>,
    },
    FoldingRanges {
        id: RequestId,
        path: PathBuf,
        text: String,
        sender: Sender<Message>,
    },
    SelectionRanges {
        id: RequestId,
        path: PathBuf,
        text: String,
        positions: Vec<Position>,
        sender: Sender<Message>,
    },
    SemanticTokensFull {
        id: RequestId,
        path: PathBuf,
        text: String,
        sender: Sender<Message>,
    },
    Completion {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: Sender<Message>,
    },
    CompletionResolve {
        id: RequestId,
        item: Box<CompletionItem>,
        sender: Sender<Message>,
    },
    Hover {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: Sender<Message>,
    },
    SignatureHelp {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: Sender<Message>,
    },
    Definition {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        position: Position,
        sender: Sender<Message>,
    },
    References {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        position: Position,
        include_declaration: bool,
        sender: Sender<Message>,
    },
    DocumentHighlight {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: Sender<Message>,
    },
    PrepareRename {
        id: RequestId,
        path: PathBuf,
        text: String,
        position: Position,
        sender: Sender<Message>,
    },
    Rename {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: String,
        position: Position,
        new_name: String,
        sender: Sender<Message>,
    },
}

impl ReadJob {
    /// Recover the request `id` and reply `sender` from an undeliverable job so
    /// the client still gets a (null) response instead of hanging.
    pub(crate) fn into_reply_parts(self) -> (RequestId, Sender<Message>) {
        match self {
            ReadJob::CodeAction { id, sender, .. } => (id, sender),
            ReadJob::DocumentDiagnostic { id, sender, .. } => (id, sender),
            ReadJob::Format { id, sender, .. } => (id, sender),
            ReadJob::FormatRange { id, sender, .. } => (id, sender),
            ReadJob::DocumentSymbols { id, sender, .. } => (id, sender),
            ReadJob::WorkspaceSymbols { id, sender, .. } => (id, sender),
            ReadJob::FoldingRanges { id, sender, .. } => (id, sender),
            ReadJob::SelectionRanges { id, sender, .. } => (id, sender),
            ReadJob::SemanticTokensFull { id, sender, .. } => (id, sender),
            ReadJob::Completion { id, sender, .. } => (id, sender),
            ReadJob::CompletionResolve { id, sender, .. } => (id, sender),
            ReadJob::Hover { id, sender, .. } => (id, sender),
            ReadJob::SignatureHelp { id, sender, .. } => (id, sender),
            ReadJob::Definition { id, sender, .. } => (id, sender),
            ReadJob::References { id, sender, .. } => (id, sender),
            ReadJob::DocumentHighlight { id, sender, .. } => (id, sender),
            ReadJob::PrepareRename { id, sender, .. } => (id, sender),
            ReadJob::Rename { id, sender, .. } => (id, sender),
        }
    }
}

/// The `DocumentDiagnosticReportResult` shape for a full (non-cached) report.
/// `result_id`-based `unchanged` responses are deferred; every pull is
/// answered in full.
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
            sender,
        } => {
            let actions: Vec<CodeActionOrCommand> =
                code_actions_via_db(&snapshot, &uri, &path, &text, range, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, actions)));
        }
        ReadJob::DocumentDiagnostic {
            id,
            path,
            text,
            sender,
        } => {
            let items = document_diagnostics_via_db(&snapshot, &path, &text, encoding);
            let result = full_report(items);
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
            let location = definition_via_db(&snapshot, &uri, &path, &text, position, encoding);
            let result = location.map(GotoDefinitionResponse::Scalar);
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
    }
}
