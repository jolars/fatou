//! Read-only jobs serviced off the analysis thread's cached state.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use lsp_server::{Message, RequestId, Response};

use lsp_types::{CompletionItem, CompletionResponse, DocumentSymbolResponse, Position, Range};

use crate::formatter::FormatStyle;
use crate::incremental::Analysis;
use crate::text::PositionEncoding;

use super::completion::{completion_via_db, resolve_completion};
use super::folding::folding_ranges_via_db;
use super::format::{format_edits_via_db, format_range_edits_via_db};
use super::hover::hover_via_db;
use super::selection::selection_ranges_via_db;
use super::semantic_tokens::semantic_tokens_via_db;
use super::signature_help::signature_help_via_db;
use super::symbols::document_symbols_via_db;

/// A read-only request the analysis thread services by cloning its salsa db
/// and running the work off-thread on the read pool. Each variant carries the
/// live buffer `text` and the client `sender` so the worker can reply
/// directly; the analysis thread only adds the db snapshot. See [`run_read`].
pub(crate) enum ReadJob {
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
}

impl ReadJob {
    /// Recover the request `id` and reply `sender` from an undeliverable job so
    /// the client still gets a (null) response instead of hanging.
    pub(crate) fn into_reply_parts(self) -> (RequestId, Sender<Message>) {
        match self {
            ReadJob::Format { id, sender, .. } => (id, sender),
            ReadJob::FormatRange { id, sender, .. } => (id, sender),
            ReadJob::DocumentSymbols { id, sender, .. } => (id, sender),
            ReadJob::FoldingRanges { id, sender, .. } => (id, sender),
            ReadJob::SelectionRanges { id, sender, .. } => (id, sender),
            ReadJob::SemanticTokensFull { id, sender, .. } => (id, sender),
            ReadJob::Completion { id, sender, .. } => (id, sender),
            ReadJob::CompletionResolve { id, sender, .. } => (id, sender),
            ReadJob::Hover { id, sender, .. } => (id, sender),
            ReadJob::SignatureHelp { id, sender, .. } => (id, sender),
        }
    }
}

/// Service a read-only job against a db `snapshot`, replying to the client.
/// Runs on a read-pool worker; the `snapshot` is dropped on return so it never
/// blocks the analysis thread's next write longer than the job itself.
pub(crate) fn run_read(snapshot: Analysis, job: ReadJob, encoding: PositionEncoding) {
    match job {
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
    }
}
