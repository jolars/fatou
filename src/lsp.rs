//! A minimal Julia language server over `lsp-server`'s stdio JSON-RPC transport.
//!
//! Groundwork phase: a **single-threaded** loop that advertises full-document
//! sync and document formatting, pushes parse diagnostics on open/change, and
//! formats on request. The dedicated-lint-thread + rayon read-pool model (which
//! Arity uses to keep the salsa single-writer database off the request path) is
//! a deliberate later step — see `TODO.md`. The server here parses on demand
//! rather than owning a persistent `IncrementalDatabase`.

use std::collections::HashMap;
use std::error::Error;

use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::Formatting;
use lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentFormattingParams, OneOf, Position,
    PublishDiagnosticsParams, Range, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Uri,
};

use crate::formatter::{FormatStyle, format_with_style};
use crate::parser::parse;
use crate::text::LineIndex;

type LspResult<T> = Result<T, Box<dyn Error + Sync + Send>>;

/// Run the language server on stdio until the client shuts it down.
pub fn run() -> LspResult<()> {
    let (connection, io_threads) = Connection::stdio();
    serve(&connection)?;
    io_threads.join()?;
    Ok(())
}

/// Perform the initialize handshake on `connection`, then run the message loop.
/// Split out from [`run`] so tests can drive it over an in-memory connection.
pub fn serve(connection: &Connection) -> LspResult<()> {
    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        document_formatting_provider: Some(OneOf::Left(true)),
        ..Default::default()
    };
    connection.initialize(serde_json::to_value(capabilities)?)?;
    main_loop(connection)
}

fn main_loop(connection: &Connection) -> LspResult<()> {
    // Open documents, keyed by URI string.
    let mut documents: HashMap<String, String> = HashMap::new();

    for message in &connection.receiver {
        match message {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                handle_request(connection, &documents, req)?;
            }
            Message::Notification(note) => {
                handle_notification(connection, &mut documents, note)?;
            }
            Message::Response(_) => {}
        }
    }

    Ok(())
}

fn handle_request(
    connection: &Connection,
    documents: &HashMap<String, String>,
    req: Request,
) -> LspResult<()> {
    match cast::<Formatting>(req) {
        Ok((id, params)) => {
            let edits = format_edits(documents, &params);
            respond(connection, id, &edits)?;
            Ok(())
        }
        Err(ExtractError::MethodMismatch(req)) => {
            // Unhandled method: reply with an empty result so the client is not
            // left waiting.
            respond::<Option<serde_json::Value>>(connection, req.id, &None)?;
            Ok(())
        }
        Err(ExtractError::JsonError { method, error }) => {
            Err(format!("malformed `{method}` request: {error}").into())
        }
    }
}

fn handle_notification(
    connection: &Connection,
    documents: &mut HashMap<String, String>,
    note: Notification,
) -> LspResult<()> {
    match note.method.as_str() {
        DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams = serde_json::from_value(note.params)?;
            let uri = params.text_document.uri.clone();
            let text = params.text_document.text;
            publish_diagnostics(connection, &uri, &text)?;
            documents.insert(uri_key(&uri), text);
        }
        DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(note.params)?;
            // Full sync: the last change carries the entire document.
            if let Some(change) = params.content_changes.into_iter().next_back() {
                let uri = params.text_document.uri.clone();
                publish_diagnostics(connection, &uri, &change.text)?;
                documents.insert(uri_key(&uri), change.text);
            }
        }
        DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams =
                serde_json::from_value(note.params)?;
            documents.remove(&uri_key(&params.text_document.uri));
        }
        _ => {}
    }
    Ok(())
}

/// Build the full-document formatting edits for a `textDocument/formatting`
/// request, or `None` if the document is unknown or formatting fails.
fn format_edits(
    documents: &HashMap<String, String>,
    params: &DocumentFormattingParams,
) -> Option<Vec<TextEdit>> {
    let text = documents.get(&uri_key(&params.text_document.uri))?;
    let style = FormatStyle::default();
    let formatted = format_with_style(text, style).ok()?;
    if formatted == *text {
        return Some(Vec::new());
    }
    let line_index = LineIndex::new(text);
    let end = line_index.byte_to_position(text.len());
    Some(vec![TextEdit {
        range: Range::new(Position::new(0, 0), end),
        new_text: formatted,
    }])
}

fn publish_diagnostics(connection: &Connection, uri: &Uri, text: &str) -> LspResult<()> {
    let parsed = parse(text);
    let line_index = LineIndex::new(text);
    let diagnostics: Vec<Diagnostic> = parsed
        .diagnostics
        .iter()
        .map(|diag| Diagnostic {
            range: Range::new(
                line_index.byte_to_position(diag.start),
                line_index.byte_to_position(diag.end),
            ),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("fatou".to_string()),
            message: diag.message.clone(),
            ..Default::default()
        })
        .collect();

    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics,
        version: None,
    };
    connection.sender.send(Message::Notification(Notification {
        method: PublishDiagnostics::METHOD.to_string(),
        params: serde_json::to_value(params)?,
    }))?;
    Ok(())
}

fn respond<T: serde::Serialize>(
    connection: &Connection,
    id: RequestId,
    result: &T,
) -> LspResult<()> {
    let response = Response {
        id,
        result: Some(serde_json::to_value(result)?),
        error: None,
    };
    connection.sender.send(Message::Response(response))?;
    Ok(())
}

fn cast<R>(req: Request) -> Result<(RequestId, R::Params), ExtractError<Request>>
where
    R: lsp_types::request::Request,
    R::Params: serde::de::DeserializeOwned,
{
    req.extract(R::METHOD)
}

fn uri_key(uri: &Uri) -> String {
    uri.as_str().to_string()
}
