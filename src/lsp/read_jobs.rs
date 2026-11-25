//! Read-only jobs serviced off the analysis thread's cached state.

use std::path::PathBuf;

use crossbeam_channel::{SendError, Sender};
use lsp_server::{ErrorCode, Message, RequestId, Response};

use lsp_types::{
    CallHierarchyItem, CodeActionOrCommand, CompletionItem, CompletionResponse,
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, DocumentSymbolResponse, FileRename,
    FullDocumentDiagnosticReport, GotoDefinitionResponse, Location, Position, Range,
    RelatedFullDocumentDiagnosticReport, RelatedUnchangedDocumentDiagnosticReport, TextEdit,
    TypeHierarchyItem, UnchangedDocumentDiagnosticReport, Uri, WorkspaceSymbolResponse,
};

use std::sync::{Arc, Mutex};

use crate::formatter::{FormatStyle, VerifiedFormatError};
use crate::incremental::Analysis;
use crate::text::edit::apply_text_edits;
use crate::text::{PositionEncoding, TextBuffer};

use super::call_hierarchy::{
    incoming_calls_via_db, outgoing_calls_via_db, prepare_call_hierarchy_via_db,
};
use super::code_action::{code_actions_via_db, qualify_name_via_db};
use super::completion::{completion_via_db, resolve_completion};
use super::definition::definition_via_db;
use super::document_link::document_links_via_db;
use super::folding::folding_ranges_via_db;
use super::format::{format_edits_via_db, format_range_edits_via_db};
use super::hover::hover_via_db;
use super::lint::ServerRules;
use super::project_navigation::{
    manifest_document_links, project_definition_via_db, project_document_links_via_db,
    project_hover_via_db, project_inlay_hints_via_db,
};
use super::pull_diagnostics::document_diagnostics_via_db;
use super::references::{document_highlights_via_db, references_via_db};
use super::rename::{RenameError, prepare_rename_via_db, rename_via_db};
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
#[derive(Clone)]
pub(crate) struct ReadReply {
    out_tx: Sender<Outbound>,
    captured: Option<Arc<Mutex<Vec<Message>>>>,
    membership_generation: Option<u64>,
}

impl ReadReply {
    pub(crate) fn new(out_tx: Sender<Outbound>) -> Self {
        Self {
            out_tx,
            captured: None,
            membership_generation: None,
        }
    }

    /// Route `message` (always a `Message::Response`) to the main loop for
    /// gating. Errors only if the main loop is gone (shutdown), like the old
    /// direct send.
    pub(crate) fn send(self, message: Message) -> Result<(), SendError<Outbound>> {
        if let Some(captured) = self.captured {
            captured
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(message);
            Ok(())
        } else {
            self.out_tx.send(Outbound::ReadReply {
                message,
                membership_generation: self.membership_generation,
            })
        }
    }
}

/// A read-only request the analysis thread services by cloning its salsa db
/// and running the work off-thread on the read pool. Each variant carries the
/// live buffer `text` and the [`ReadReply`] channel so the worker can reply;
/// the analysis thread only adds the db snapshot. See [`run_read`].
///
/// `text` is the shared [`TextBuffer`], not a copy: the buffer carries the
/// line-start table the main loop already maintains, so a handler resolves
/// positions against it instead of rescanning the document (`src/text/buffer.rs`).
#[derive(Clone)]
pub(crate) enum ReadJob {
    CodeAction {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: Arc<TextBuffer>,
        range: Range,
        rules: Option<Arc<ServerRules>>,
        qualify: bool,
        sender: ReadReply,
    },
    DocumentDiagnostic {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
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
        text: Arc<TextBuffer>,
        style: FormatStyle,
        safe: bool,
        sender: ReadReply,
    },
    FormatRange {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        range: Range,
        style: FormatStyle,
        safe: bool,
        sender: ReadReply,
    },
    DocumentSymbols {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
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
        text: Arc<TextBuffer>,
        sender: ReadReply,
    },
    DocumentLinks {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        sender: ReadReply,
    },
    SelectionRanges {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        positions: Vec<Position>,
        sender: ReadReply,
    },
    SemanticTokensFull {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        sender: ReadReply,
    },
    SemanticTokensDelta {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        /// The `resultId` from the client's last full or delta response; a
        /// match against the freshly computed id answers an empty delta.
        previous_result_id: String,
        sender: ReadReply,
    },
    Completion {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
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
        text: Arc<TextBuffer>,
        position: Position,
        sender: ReadReply,
    },
    SignatureHelp {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        position: Position,
        sender: ReadReply,
    },
    Definition {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: Arc<TextBuffer>,
        position: Position,
        sender: ReadReply,
    },
    /// Go-to-definition in an open *project file*: a `[deps]` name resolves to
    /// its package's entry file. No `path`, unlike its Julia twin — the buffer
    /// is parsed as TOML rather than looked up in the database, and the answer
    /// points at another file entirely.
    ProjectDefinition {
        id: RequestId,
        text: Arc<TextBuffer>,
        position: Position,
        sender: ReadReply,
    },
    /// Hover in an open *project file*: a `[deps]` name reports the version,
    /// kind, and resolved path the environment gave it. Pathless for the same
    /// reason as [`ProjectDefinition`](Self::ProjectDefinition).
    ProjectHover {
        id: RequestId,
        text: Arc<TextBuffer>,
        position: Position,
        sender: ReadReply,
    },
    /// Document links in an open *project file*: each dependency name links to
    /// its package's entry file, the same target
    /// [`ProjectDefinition`](Self::ProjectDefinition) jumps to.
    ProjectDocumentLinks {
        id: RequestId,
        text: Arc<TextBuffer>,
        sender: ReadReply,
    },
    /// Document links in an open *manifest*: each `path` entry links to the
    /// `dev`'d package it pins. The one feature a manifest answers, and the one
    /// project-file job that carries a `path`: the entry is a path itself, and
    /// it resolves against the manifest's own directory.
    ManifestDocumentLinks {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        sender: ReadReply,
    },
    /// Inlay hints in an open *project file*: each dependency's resolved
    /// version, followed by registry versions beside compat bounds. `range` is
    /// the client's viewport, which it re-sends on every scroll. The registry
    /// worker finishes the reply after this job releases its salsa snapshot.
    ProjectInlayHints {
        updates: super::project_updates::ProjectUpdates,
        id: RequestId,
        text: Arc<TextBuffer>,
        range: Range,
        sender: ReadReply,
    },
    References {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: Arc<TextBuffer>,
        position: Position,
        include_declaration: bool,
        sender: ReadReply,
    },
    DocumentHighlight {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        position: Position,
        sender: ReadReply,
    },
    PrepareRename {
        id: RequestId,
        path: PathBuf,
        text: Arc<TextBuffer>,
        position: Position,
        sender: ReadReply,
    },
    Rename {
        id: RequestId,
        uri: Uri,
        path: PathBuf,
        text: Arc<TextBuffer>,
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
        text: Arc<TextBuffer>,
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
        text: Arc<TextBuffer>,
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
    pub(crate) fn produces_edits(&self) -> bool {
        matches!(
            self,
            Self::Format { .. }
                | Self::FormatRange { .. }
                | Self::Rename { .. }
                | Self::WillRenameFiles { .. }
                | Self::CodeAction { .. }
        )
    }

    pub(crate) fn with_membership_generation(mut self, generation: u64) -> Self {
        if self.produces_edits() {
            self.sender_mut().membership_generation = Some(generation);
        }
        self
    }

    fn sender_mut(&mut self) -> &mut ReadReply {
        match self {
            Self::CodeAction { sender, .. }
            | Self::DocumentDiagnostic { sender, .. }
            | Self::Format { sender, .. }
            | Self::FormatRange { sender, .. }
            | Self::DocumentSymbols { sender, .. }
            | Self::WorkspaceSymbols { sender, .. }
            | Self::FoldingRanges { sender, .. }
            | Self::DocumentLinks { sender, .. }
            | Self::SelectionRanges { sender, .. }
            | Self::SemanticTokensFull { sender, .. }
            | Self::SemanticTokensDelta { sender, .. }
            | Self::Completion { sender, .. }
            | Self::CompletionResolve { sender, .. }
            | Self::Hover { sender, .. }
            | Self::SignatureHelp { sender, .. }
            | Self::Definition { sender, .. }
            | Self::ProjectDefinition { sender, .. }
            | Self::ProjectHover { sender, .. }
            | Self::ProjectDocumentLinks { sender, .. }
            | Self::ManifestDocumentLinks { sender, .. }
            | Self::ProjectInlayHints { sender, .. }
            | Self::References { sender, .. }
            | Self::DocumentHighlight { sender, .. }
            | Self::PrepareRename { sender, .. }
            | Self::Rename { sender, .. }
            | Self::WillRenameFiles { sender, .. }
            | Self::PrepareCallHierarchy { sender, .. }
            | Self::CallHierarchyIncoming { sender, .. }
            | Self::CallHierarchyOutgoing { sender, .. }
            | Self::PrepareTypeHierarchy { sender, .. }
            | Self::TypeHierarchySupertypes { sender, .. }
            | Self::TypeHierarchySubtypes { sender, .. } => sender,
        }
    }

    /// Capture metadata selections before the read enters the worker pool so
    /// canceled cold paths retain the grammar selected at dispatch.
    pub(crate) fn contexts(
        &self,
        snapshot: &Analysis,
    ) -> Vec<(
        crate::parse_context::SourceContext,
        crate::parse_context::EditionSelection,
    )> {
        self.context_path()
            .map(|path| snapshot.capture_source_editions(&path))
            .unwrap_or_default()
    }

    pub(crate) fn context_path(&self) -> Option<PathBuf> {
        if let Some((path, _)) = self.source() {
            return Some(path.to_path_buf());
        }
        match self {
            Self::CompletionResolve { item, .. } => item
                .data
                .as_ref()?
                .get("fatouSourcePath")?
                .as_str()
                .map(PathBuf::from),
            Self::CallHierarchyIncoming { item, .. } | Self::CallHierarchyOutgoing { item, .. } => {
                super::uri::to_path(&item.uri)
            }
            Self::TypeHierarchySupertypes { item, .. }
            | Self::TypeHierarchySubtypes { item, .. } => super::uri::to_path(&item.uri),
            _ => None,
        }
    }

    pub(crate) fn source(&self) -> Option<(&std::path::Path, &Arc<TextBuffer>)> {
        match self {
            Self::CodeAction { path, text, .. }
            | Self::DocumentDiagnostic { path, text, .. }
            | Self::Format { path, text, .. }
            | Self::FormatRange { path, text, .. }
            | Self::DocumentSymbols { path, text, .. }
            | Self::FoldingRanges { path, text, .. }
            | Self::DocumentLinks { path, text, .. }
            | Self::SelectionRanges { path, text, .. }
            | Self::SemanticTokensFull { path, text, .. }
            | Self::SemanticTokensDelta { path, text, .. }
            | Self::Completion { path, text, .. }
            | Self::Hover { path, text, .. }
            | Self::SignatureHelp { path, text, .. }
            | Self::Definition { path, text, .. }
            | Self::References { path, text, .. }
            | Self::DocumentHighlight { path, text, .. }
            | Self::PrepareRename { path, text, .. }
            | Self::Rename { path, text, .. }
            | Self::PrepareCallHierarchy { path, text, .. }
            | Self::PrepareTypeHierarchy { path, text, .. } => Some((path, text)),
            _ => None,
        }
    }

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
            ReadJob::ProjectDefinition { id, sender, .. } => (id, sender),
            ReadJob::ProjectHover { id, sender, .. } => (id, sender),
            ReadJob::ProjectDocumentLinks { id, sender, .. } => (id, sender),
            ReadJob::ManifestDocumentLinks { id, sender, .. } => (id, sender),
            ReadJob::ProjectInlayHints { id, sender, .. } => (id, sender),
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

fn format_response(
    id: RequestId,
    text: &TextBuffer,
    edits: Option<Vec<TextEdit>>,
    safe: bool,
    encoding: PositionEncoding,
    options: crate::parser::ParseOptions,
) -> Response {
    if safe {
        let verified = edits
            .as_ref()
            .ok_or_else(|| "safe formatting failed: formatter rejected the input".to_string())
            .and_then(|edits| {
                super::format::verify_format_edits_with_options(text, edits, encoding, options)
                    .map_err(|error| VerifiedFormatError::from(error).to_string())
            });
        if let Err(message) = verified {
            return Response::new_err(id, ErrorCode::RequestFailed as i32, message);
        }
    }
    Response::new_ok(id, edits)
}

/// Service a read-only job against a db `snapshot`, replying to the client.
/// Runs on a read-pool worker; the `snapshot` is dropped on return so it never
/// blocks the analysis thread's next write longer than the job itself.
/// Evaluate semantic requests in each loading context. Structural requests use
/// the stable primary context; edits require agreement and a clean reparse in
/// every affected context before they can leave the server.
pub(crate) fn run_read(
    snapshot: Analysis,
    mut job: ReadJob,
    encoding: PositionEncoding,
    contexts: Vec<(
        crate::parse_context::SourceContext,
        crate::parse_context::EditionSelection,
    )>,
) {
    let primary_only = matches!(
        job,
        ReadJob::DocumentSymbols { .. }
            | ReadJob::FoldingRanges { .. }
            | ReadJob::SelectionRanges { .. }
            | ReadJob::SemanticTokensFull { .. }
            | ReadJob::SemanticTokensDelta { .. }
            | ReadJob::DocumentDiagnostic { .. }
    );
    let actions = matches!(job, ReadJob::CodeAction { .. });
    let rewrite = job.produces_edits() && !actions;
    if (rewrite || actions) && contexts.iter().any(|(_, selection)| !selection.certain) {
        let (id, sender) = job.into_reply_parts();
        let _ = sender.send(Message::Response(Response::new_ok(
            id,
            serde_json::Value::Null,
        )));
        return;
    }
    if primary_only || (contexts.len() <= 1 && !rewrite && !actions) {
        let view = contexts.first().map_or_else(
            || snapshot.clone(),
            |(context, selected)| snapshot.with_context(context.clone(), selected.options),
        );
        run_read_single(view, job, encoding, &contexts);
        return;
    }
    let captured = Arc::new(Mutex::new(Vec::new()));
    let reply = job.sender_mut().clone();
    job.sender_mut().captured = Some(Arc::clone(&captured));
    if contexts.is_empty() {
        run_read_single(snapshot.clone(), job.clone(), encoding, &[]);
    } else {
        for (context, selected) in &contexts {
            run_read_single(
                snapshot.with_context(context.clone(), selected.options),
                job.clone(),
                encoding,
                &[],
            );
        }
    }
    let messages = std::mem::take(
        &mut *captured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let mut responses: Vec<_> = messages
        .into_iter()
        .filter_map(|message| {
            if let Message::Response(response) = message {
                Some(response)
            } else {
                None
            }
        })
        .collect();
    if responses.is_empty() {
        return;
    }
    let mut response = responses.remove(0);
    let values: Vec<_> = std::iter::once(&response)
        .chain(&responses)
        .map(|response| {
            response
                .response_result
                .as_ref()
                .ok()
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        })
        .collect();
    let has_error = response.response_result.is_err()
        || responses
            .iter()
            .any(|response| response.response_result.is_err());
    if rewrite {
        if has_error {
            if response.response_result.is_ok() {
                response = responses
                    .into_iter()
                    .find(|other| other.response_result.is_err())
                    .expect("a syntax context returned an error");
            }
        } else if values.iter().any(|value| value != &values[0])
            || !valid_rewrite(&snapshot, &job, &values[0], encoding, &contexts)
        {
            response = Response::new_err(
                response.id,
                ErrorCode::RequestFailed as i32,
                "source edits do not agree or parse cleanly in every syntax context".into(),
            );
        }
    } else if actions {
        let mut common = values[0].as_array().cloned().unwrap_or_default();
        common.retain(|action| {
            values.iter().all(|value| {
                value
                    .as_array()
                    .is_some_and(|actions| actions.contains(action))
            }) && action
                .get("edit")
                .is_none_or(|edit| valid_workspace_edit(&snapshot, &job, edit, encoding))
        });
        response = Response::new_ok(response.id, common);
    } else if !has_error {
        let definitions = matches!(
            job,
            ReadJob::Definition { .. } | ReadJob::ProjectDefinition { .. }
        );
        let merged = if definitions {
            merge_definitions(&values)
        } else if let ReadJob::CompletionResolve { item, .. } = &job {
            let merged = merge_results(&values);
            if merged.is_null() {
                serde_json::to_value(item).expect("completion item serializes")
            } else {
                merged
            }
        } else {
            merge_results(&values)
        };
        response = Response::new_ok(response.id, merged);
    }
    let _ = reply.send(Message::Response(response));
}

fn merge_results(values: &[serde_json::Value]) -> serde_json::Value {
    use serde_json::Value;
    if values.iter().all(|value| value == &values[0]) {
        return values[0].clone();
    }
    if values
        .iter()
        .all(|value| value.is_array() || value.is_null())
    {
        let mut merged = Vec::new();
        for item in values.iter().filter_map(Value::as_array).flatten() {
            if !merged.contains(item) {
                merged.push(item.clone());
            }
        }
        return Value::Array(merged);
    }
    // Scalar hovers, signatures, and rename preparations must describe the
    // same symbol in every context. A disagreement has no unambiguous answer.
    Value::Null
}

fn merge_definitions(values: &[serde_json::Value]) -> serde_json::Value {
    use serde_json::Value;
    let mut locations = Vec::new();
    for value in values.iter().filter(|value| !value.is_null()) {
        let sites = value
            .as_array()
            .map_or_else(|| std::slice::from_ref(value), Vec::as_slice);
        for site in sites {
            if !locations.contains(site) {
                locations.push(site.clone());
            }
        }
    }
    match locations.len() {
        0 => Value::Null,
        1 => locations.pop().unwrap(),
        _ => Value::Array(locations),
    }
}

fn valid_rewrite(
    snapshot: &Analysis,
    job: &ReadJob,
    value: &serde_json::Value,
    encoding: PositionEncoding,
    contexts: &[(
        crate::parse_context::SourceContext,
        crate::parse_context::EditionSelection,
    )],
) -> bool {
    if value.is_null() {
        return true;
    }
    if let ReadJob::Format { text, safe, .. } | ReadJob::FormatRange { text, safe, .. } = job {
        let fallback = crate::parser::ParseOptions::default();
        let primary = contexts
            .first()
            .map_or(fallback, |(_, selected)| selected.options);
        if !safe && contexts.len() <= 1 && primary == fallback {
            return true;
        }
        let Ok(edits) = serde_json::from_value::<Vec<TextEdit>>(value.clone()) else {
            return false;
        };
        return apply_text_edits(text, edits, encoding).is_some_and(|(next, _)| {
            if contexts.is_empty() {
                parses_cleanly(&next, fallback)
            } else {
                contexts
                    .iter()
                    .all(|(_, selected)| parses_cleanly(&next, selected.options))
            }
        });
    }
    valid_workspace_edit(snapshot, job, value, encoding)
}

fn valid_workspace_edit(
    snapshot: &Analysis,
    job: &ReadJob,
    value: &serde_json::Value,
    encoding: PositionEncoding,
) -> bool {
    let Ok(edit) = serde_json::from_value::<lsp_types::WorkspaceEdit>(value.clone()) else {
        return false;
    };
    let mut changes = edit.changes.unwrap_or_default();
    if let Some(document_changes) = edit.document_changes {
        let edits = match document_changes {
            lsp_types::DocumentChanges::Edits(edits) => edits,
            lsp_types::DocumentChanges::Operations(operations) => operations
                .into_iter()
                .filter_map(|operation| match operation {
                    lsp_types::DocumentChangeOperation::Edit(edit) => Some(edit),
                    _ => None,
                })
                .collect(),
        };
        for edit in edits {
            changes
                .entry(edit.text_document.uri)
                .or_default()
                .extend(edit.edits.into_iter().map(|edit| match edit {
                    lsp_types::OneOf::Left(edit) => edit,
                    lsp_types::OneOf::Right(edit) => edit.text_edit,
                }));
        }
    }
    changes.into_iter().all(|(uri, edits)| {
        let path = super::uri::to_path_or_synthetic(&uri);
        if let Some((source, text)) = job.source()
            && source == path
        {
            return valid_text_edits(snapshot, &path, text, edits, encoding);
        }
        let text = salsa::Cancelled::catch(std::panic::AssertUnwindSafe(|| {
            snapshot
                .lookup_file(&path)
                .map(|file| snapshot.file_text_arc(file))
        }))
        .ok()
        .flatten()
        .or_else(|| std::fs::read_to_string(&path).ok().map(Arc::from));
        text.is_some_and(|text| {
            valid_text_edits(snapshot, &path, &TextBuffer::new(text), edits, encoding)
        })
    })
}

fn valid_text_edits(
    snapshot: &Analysis,
    path: &std::path::Path,
    text: &TextBuffer,
    edits: Vec<TextEdit>,
    encoding: PositionEncoding,
) -> bool {
    let Some((next, edits)) = apply_text_edits(text, edits, encoding) else {
        return false;
    };
    if crate::environment::is_environment_file(path) {
        return toml::from_str::<toml::Table>(&next).is_ok();
    }
    salsa::Cancelled::catch(std::panic::AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path);
        let original = text.text_arc();
        let valid = |options| {
            let reparsed = file.and_then(|file| {
                snapshot.reparse_proposed_edits(file, &original, &edits, &next, options)
            });
            reparsed.map_or_else(
                || parses_cleanly(&next, options),
                |parsed| parsed.diagnostics.is_empty(),
            )
        };
        let contexts = file.map_or(&[][..], |file| snapshot.source_editions(file).as_slice());
        if contexts.is_empty() {
            return valid(snapshot.options_for_path(path));
        }
        contexts
            .iter()
            .all(|(_, selected)| selected.certain && valid(selected.options))
    }))
    .unwrap_or(false)
}

fn parses_cleanly(text: &str, options: crate::parser::ParseOptions) -> bool {
    crate::parser::parse_with_options(text, options)
        .diagnostics
        .is_empty()
}

fn run_read_single(
    snapshot: Analysis,
    job: ReadJob,
    encoding: PositionEncoding,
    contexts: &[(
        crate::parse_context::SourceContext,
        crate::parse_context::EditionSelection,
    )],
) {
    match job {
        ReadJob::CodeAction {
            id,
            uri,
            path,
            text,
            range,
            rules,
            qualify,
            sender,
        } => {
            let mut actions: Vec<CodeActionOrCommand> = rules.map_or_else(Vec::new, |rules| {
                code_actions_via_db(&snapshot, &uri, &path, &text, range, encoding, &rules)
            });
            if qualify {
                actions.extend(qualify_name_via_db(
                    &snapshot, &uri, &path, &text, range, encoding,
                ));
            }
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
            let items =
                document_diagnostics_via_db(&snapshot, &path, &text, encoding, &rules, contexts);
            let result = diagnostic_report(items, previous_result_id.as_deref());
            let _ = sender.send(Message::Response(Response::new_ok(id, result)));
        }
        ReadJob::Format {
            id,
            path,
            text,
            style,
            safe,
            sender,
        } => {
            let options = snapshot.options_for_path(&path);
            let result = format_edits_via_db(&snapshot, &path, &text, style, encoding);
            let _ = sender.send(Message::Response(format_response(
                id, &text, result, safe, encoding, options,
            )));
        }
        ReadJob::FormatRange {
            id,
            path,
            text,
            range,
            style,
            safe,
            sender,
        } => {
            let options = snapshot.options_for_path(&path);
            let result = format_range_edits_via_db(&snapshot, &path, &text, range, style, encoding);
            let _ = sender.send(Message::Response(format_response(
                id, &text, result, safe, encoding, options,
            )));
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
            let locations = definition_via_db(&snapshot, &uri, &path, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(
                id,
                goto_response(locations),
            )));
        }
        ReadJob::ProjectDefinition {
            id,
            text,
            position,
            sender,
        } => {
            let locations = project_definition_via_db(&snapshot, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(
                id,
                goto_response(locations),
            )));
        }
        ReadJob::ProjectHover {
            id,
            text,
            position,
            sender,
        } => {
            let hover = project_hover_via_db(&snapshot, &text, position, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, hover)));
        }
        ReadJob::ProjectDocumentLinks { id, text, sender } => {
            let links = project_document_links_via_db(&snapshot, &text, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, links)));
        }
        // No snapshot in sight: a manifest's links are decided by its buffer
        // and the filesystem, and its text never reaches the database.
        ReadJob::ManifestDocumentLinks {
            id,
            path,
            text,
            sender,
        } => {
            let links = manifest_document_links(&text, &path, encoding);
            let _ = sender.send(Message::Response(Response::new_ok(id, links)));
        }
        ReadJob::ProjectInlayHints {
            updates,
            id,
            text,
            range,
            sender,
        } => {
            let hints = project_inlay_hints_via_db(&snapshot, &text, range, encoding);
            drop(snapshot);
            updates.inlay_hints(id, text, range, encoding, hints, sender);
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
                    Err(RenameError::InvalidName(message)) => {
                        Response::new_err(id, ErrorCode::InvalidParams as i32, message)
                    }
                    Err(RenameError::BindingConflict) => Response::new_err(
                        id,
                        ErrorCode::RequestFailed as i32,
                        "rename would change different bindings in the source's loading contexts"
                            .into(),
                    ),
                };
            let _ = sender.send(Message::Response(response));
        }
        ReadJob::WillRenameFiles {
            id,
            files,
            open_docs,
            sender,
        } => {
            let response = match will_rename_files_via_db(&snapshot, &files, &open_docs, encoding) {
                Ok(edit) => Response::new_ok(id, edit),
                Err(message) => Response::new_err(id, ErrorCode::RequestFailed as i32, message),
            };
            let _ = sender.send(Message::Response(response));
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

/// Collapse definition sites into a `textDocument/definition` result: none is
/// `null`, one is a plain jump, several are an array the client offers as a
/// picker (the methods of a function, say). Shared so the Julia and project-file
/// routes cannot drift on the shape they answer with.
fn goto_response(mut locations: Vec<Location>) -> Option<GotoDefinitionResponse> {
    match locations.len() {
        0 => None,
        1 => Some(GotoDefinitionResponse::Scalar(locations.remove(0))),
        _ => Some(GotoDefinitionResponse::Array(locations)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    use crate::parse_context::{EditionSource, SourceContext};
    use crate::parser::SyntaxEdition;

    fn contexts(db: &mut IncrementalDatabase, path: &std::path::Path) {
        db.set_source_contexts(
            path,
            [SyntaxEdition::Julia113, SyntaxEdition::Julia114]
                .into_iter()
                .map(|edition| SourceContext {
                    entry: path.to_path_buf(),
                    source: EditionSource::Fixed(edition),
                })
                .collect(),
        );
    }

    fn response(
        db: &IncrementalDatabase,
        job: ReadJob,
        receiver: crossbeam_channel::Receiver<Outbound>,
    ) -> Response {
        let contexts = job
            .context_path()
            .and_then(|path| db.lookup_file(&path))
            .map(|file| db.source_editions(file).as_ref().clone())
            .unwrap_or_default();
        run_read(db.snapshot(), job, PositionEncoding::Utf16, contexts);
        let Outbound::ReadReply {
            message: Message::Response(response),
            ..
        } = receiver.try_recv().unwrap()
        else {
            panic!("expected response");
        };
        response
    }

    #[test]
    fn conflicting_editions_block_formatting_and_preserve_the_primary_outline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.jl");
        let text = Arc::new(TextBuffer::new("typegroup  =  1\n"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&path, text.text_arc());
        contexts(&mut db, &path);
        for safe in [false, true] {
            let (sender, receiver) = crossbeam_channel::unbounded();
            let result = response(
                &db,
                ReadJob::Format {
                    id: 1.into(),
                    path: path.clone(),
                    text: Arc::clone(&text),
                    style: Default::default(),
                    safe,
                    sender: ReadReply::new(sender),
                },
                receiver,
            );
            assert!(result.response_result.is_err(), "safe = {safe}");
        }
        let (sender, receiver) = crossbeam_channel::unbounded();
        let result = response(
            &db,
            ReadJob::DocumentSymbols {
                id: 2.into(),
                path,
                text: Arc::clone(&text),
                sender: ReadReply::new(sender),
            },
            receiver,
        );
        let expected = super::super::symbols::compute_document_symbols_with_options(
            &text,
            PositionEncoding::Utf16,
            crate::parser::ParseOptions {
                syntax_edition: SyntaxEdition::Julia113,
            },
        );
        assert_eq!(
            result.response_result.unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }

    #[test]
    fn code_actions_must_agree_and_reparse_in_all_editions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.jl");
        let text = Arc::new(TextBuffer::new("f(x) = x == nothing\n"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&path, text.text_arc());
        contexts(&mut db, &path);
        let (sender, receiver) = crossbeam_channel::unbounded();
        let result = response(
            &db,
            ReadJob::CodeAction {
                id: 1.into(),
                uri: super::super::uri::from_path(&path).unwrap(),
                path,
                text,
                range: Range::new(Position::new(0, 0), Position::new(1, 0)),
                rules: Some(ServerRules::defaults()),
                qualify: true,
                sender: ReadReply::new(sender),
            },
            receiver,
        );
        assert_eq!(result.response_result.unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn agreeing_actions_can_edit_an_untitled_buffer() {
        let uri: Uri = "untitled:edition-actions.jl".parse().unwrap();
        let path = super::super::uri::to_path_or_synthetic(&uri);
        let text = Arc::new(TextBuffer::new("f(x) = x == nothing\n"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&path, text.text_arc());
        contexts(&mut db, &path);
        let (sender, receiver) = crossbeam_channel::unbounded();
        let result = response(
            &db,
            ReadJob::CodeAction {
                id: 1.into(),
                uri,
                path,
                text,
                range: Range::new(Position::new(0, 0), Position::new(1, 0)),
                rules: Some(ServerRules::defaults()),
                qualify: true,
                sender: ReadReply::new(sender),
            },
            receiver,
        );
        assert_eq!(result.response_result.unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn rename_validates_utf16_edits_in_every_loading_edition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.jl");
        let uri = super::super::uri::from_path(&path).unwrap();
        let text = Arc::new(TextBuffer::new("value = 1\n(\"😀\", value, value)\n"));
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&path, text.text_arc());
        contexts(&mut db, &path);

        let rename = |new_name: &str| {
            let (sender, receiver) = crossbeam_channel::unbounded();
            response(
                &db,
                ReadJob::Rename {
                    id: 1.into(),
                    uri: uri.clone(),
                    path: path.clone(),
                    text: Arc::clone(&text),
                    position: Position::new(1, 8),
                    new_name: new_name.into(),
                    sender: ReadReply::new(sender),
                },
                receiver,
            )
            .response_result
        };

        assert_eq!(
            rename("typegroup").unwrap_err().code,
            ErrorCode::RequestFailed as i32
        );
        let edit: lsp_types::WorkspaceEdit =
            serde_json::from_value(rename("renamed").unwrap()).unwrap();
        assert_eq!(
            edit.changes.unwrap()[&uri],
            [(0, 0), (1, 7), (1, 14)].map(|(line, start)| TextEdit {
                range: Range::new(Position::new(line, start), Position::new(line, start + 5)),
                new_text: "renamed".into(),
            })
        );
    }

    #[test]
    fn semantic_lists_are_unioned_and_differing_scalar_answers_are_withheld() {
        let first = serde_json::json!([{"name":"x"}, {"name":"common"}]);
        let second = serde_json::json!([{"name":"common"}, {"name":"y"}]);
        assert_eq!(
            merge_results(&[first, second]),
            serde_json::json!([{"name":"x"}, {"name":"common"}, {"name":"y"}])
        );
        assert_eq!(
            merge_results(&[
                serde_json::json!({"contents":"old"}),
                serde_json::json!({"contents":"new"})
            ]),
            serde_json::Value::Null
        );
    }

    #[test]
    fn pull_diagnostics_keep_all_captured_editions_after_cancellation() {
        use salsa::Database;
        use std::panic::AssertUnwindSafe;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.jl");
        let text = Arc::new(TextBuffer::new("typegroup = 1\n"));
        let mut db = IncrementalDatabase::default();
        let file = db.upsert_file(&path, text.text_arc());
        contexts(&mut db, &path);
        let (sender, receiver) = crossbeam_channel::unbounded();
        let job = ReadJob::DocumentDiagnostic {
            id: 1.into(),
            path,
            text,
            rules: ServerRules::defaults(),
            previous_result_id: None,
            sender: ReadReply::new(sender),
        };
        let expected = response(&db, job.clone(), receiver.clone());
        let snapshot = db.snapshot();
        let contexts = job.contexts(&snapshot);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                while salsa::Cancelled::catch(AssertUnwindSafe(|| snapshot.source_editions(file)))
                    .is_ok()
                {
                    std::thread::yield_now();
                }
                run_read(snapshot, job, PositionEncoding::Utf16, contexts);
            });
            db.trigger_cancellation();
        });
        let Outbound::ReadReply {
            message: Message::Response(response),
            ..
        } = receiver.try_recv().unwrap()
        else {
            panic!("expected response");
        };
        let result = response.response_result.unwrap();
        assert_eq!(result, expected.response_result.unwrap());
        let items = result["items"].as_array().unwrap();
        assert!(!items.is_empty(), "{result}");
        let message = items[0]["message"].as_str().unwrap();
        assert!(
            message.contains("1.14") && message.contains("syntax contexts:"),
            "{message}"
        );
    }

    #[test]
    fn formatting_validates_with_the_captured_editions_after_cancellation() {
        use salsa::Database;
        use std::panic::AssertUnwindSafe;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.jl");
        let text = Arc::new(TextBuffer::new("x=1\n"));
        let mut db = IncrementalDatabase::default();
        let file = db.upsert_file(&path, text.text_arc());
        contexts(&mut db, &path);
        let (sender, receiver) = crossbeam_channel::unbounded();
        let job = ReadJob::Format {
            id: 1.into(),
            path,
            text,
            style: Default::default(),
            safe: true,
            sender: ReadReply::new(sender),
        };
        let snapshot = db.snapshot();
        let contexts = job.contexts(&snapshot);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                while salsa::Cancelled::catch(AssertUnwindSafe(|| snapshot.source_editions(file)))
                    .is_ok()
                {
                    std::thread::yield_now();
                }
                run_read(snapshot, job, PositionEncoding::Utf16, contexts);
            });
            db.trigger_cancellation();
        });
        let Outbound::ReadReply {
            message: Message::Response(response),
            ..
        } = receiver.try_recv().unwrap()
        else {
            panic!("expected response")
        };
        let edits: Vec<TextEdit> =
            serde_json::from_value(response.response_result.unwrap()).unwrap();
        assert!(!edits.is_empty());
    }

    #[test]
    fn file_rename_checks_uncertainty_only_where_it_needs_to_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let healthy = dir.path().join("healthy.jl");
        let uncertain = dir.path().join("uncertain.jl");
        let project = dir.path().join("Project.toml");
        let uri = |path: &std::path::Path| super::super::uri::from_path(path).unwrap();
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&healthy, "include(\"one.jl\")\n");
        db.upsert_file(&uncertain, "include(\"other.jl\")\n");
        db.upsert_file(&project, "[syntax]\njulia_version = 'bad'\n");
        db.set_source_contexts(
            &healthy,
            vec![SourceContext {
                entry: healthy.clone(),
                source: EditionSource::Fixed(SyntaxEdition::Julia114),
            }],
        );
        db.set_source_contexts(
            &uncertain,
            vec![SourceContext {
                entry: uncertain.clone(),
                source: EditionSource::Project {
                    path: project,
                    role: crate::parse_context::SourceRole::Package,
                },
            }],
        );
        for (old, succeeds) in [("one.jl", true), ("other.jl", false)] {
            let (sender, receiver) = crossbeam_channel::unbounded();
            let job = ReadJob::WillRenameFiles {
                id: 1.into(),
                files: vec![FileRename {
                    old_uri: uri(&dir.path().join(old)).to_string(),
                    new_uri: uri(&dir.path().join("moved.jl")).to_string(),
                }],
                open_docs: vec![healthy.clone(), uncertain.clone()],
                sender: ReadReply::new(sender),
            };
            let snapshot = db.snapshot();
            let contexts = job.contexts(&snapshot);
            run_read(snapshot, job, PositionEncoding::Utf16, contexts);
            let Outbound::ReadReply {
                message: Message::Response(response),
                ..
            } = receiver.try_recv().unwrap()
            else {
                panic!("expected response")
            };
            if succeeds {
                assert_eq!(
                    response.response_result.unwrap()["changes"][uri(&healthy).as_str()][0]["newText"],
                    "moved.jl"
                );
            } else {
                assert_eq!(
                    response.response_result.unwrap_err().code,
                    ErrorCode::RequestFailed as i32
                );
            }
        }
    }
}
