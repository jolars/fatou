//! Drive the language server over an in-memory connection: initialize, open a
//! document, request formatting, edit through parse errors, and shut down
//! cleanly. Exercises the threaded pipeline end-to-end: main loop → analysis
//! thread (write-phase) → read pool (read-phase) → version-gated publish.

use fatou::lsp::{
    compute_document_symbols, compute_folding_ranges, compute_selection_ranges,
    compute_semantic_tokens,
};
use fatou::text::PositionEncoding;
use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    ClientCapabilities, CodeActionContext, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, Diagnostic,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidChangeWatchedFilesClientCapabilities,
    DidChangeWatchedFilesParams, DidChangeWatchedFilesRegistrationOptions,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentFormattingParams,
    DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams, DocumentLink,
    DocumentLinkParams, DocumentRangeFormattingParams, DocumentSymbol, DocumentSymbolParams,
    FileChangeType, FileEvent, FileRename, FoldingRange, FoldingRangeKind, FoldingRangeParams,
    FormattingOptions, GeneralClientCapabilities, GlobPattern, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, InitializeParams, Location,
    NumberOrString, PartialResultParams, Position, PositionEncodingKind, ProgressParams,
    ProgressParamsValue, PublishDiagnosticsParams, Range, ReferenceContext, ReferenceParams,
    RegistrationParams, RenameFilesParams, RenameParams, SelectionRange, SelectionRangeParams,
    SemanticTokens, SemanticTokensDeltaParams, SemanticTokensFullDeltaResult, SemanticTokensParams,
    SignatureHelp, SignatureHelpParams, SymbolKind, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, TextEdit,
    TypeHierarchyItem, TypeHierarchyPrepareParams, TypeHierarchySubtypesParams,
    TypeHierarchySupertypesParams, Uri, VersionedTextDocumentIdentifier, WindowClientCapabilities,
    WorkDoneProgress, WorkDoneProgressParams, WorkspaceClientCapabilities, WorkspaceEdit,
    WorkspaceFolder, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Bridge lsp-server 0.10's `response_result` field back to the flat `result`
/// accessor these tests were written against: `Some(value)` for an `Ok`
/// response, `None` for an error (matching the pre-0.9 `Option` field).
trait ResponseResultExt {
    fn result(&self) -> Option<serde_json::Value>;
}

impl ResponseResultExt for lsp_server::Response {
    fn result(&self) -> Option<serde_json::Value> {
        self.response_result.as_ref().ok().cloned()
    }
}

/// A `file:` URI on a platform-native absolute path: `native_file_uri("/work/a.jl")`
/// is `file:///work/a.jl` on Unix and `file:///C:/work/a.jl` on Windows. A
/// driveless `/work` path is not absolute on Windows, so the server's lexical
/// normalization would graft the CWD's drive onto it, skewing any assertion on
/// a server-produced URI (see AGENTS.md, "Testing layout").
fn native_file_uri(path: &str) -> Uri {
    let drive = if cfg!(windows) { "/C:" } else { "" };
    Uri::from_str(&format!("file://{drive}{path}")).unwrap()
}

#[test]
fn initialize_format_and_shutdown() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init_response = client.receiver.recv().unwrap();
    assert!(
        matches!(init_response, Message::Response(_)),
        "expected an InitializeResult, got {init_response:?}"
    );
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document; expect pushed (empty) diagnostics ---
    let uri = Uri::from_str("file:///work/a.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "x = 1\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag_note = client.receiver.recv().unwrap();
    match diag_note {
        Message::Notification(note) => {
            assert_eq!(note.method, "textDocument/publishDiagnostics");
        }
        other => panic!("expected publishDiagnostics, got {other:?}"),
    }

    // --- request formatting; identity formatter returns no edits ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/formatting".to_string(),
            params: serde_json::to_value(DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                options: FormattingOptions {
                    tab_size: 4,
                    insert_spaces: true,
                    ..Default::default()
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let format_response = client.receiver.recv().unwrap();
    match format_response {
        Message::Response(resp) => {
            let edits: Option<Vec<TextEdit>> =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            assert_eq!(edits.unwrap_or_default(), Vec::new());
        }
        other => panic!("expected a formatting response, got {other:?}"),
    }

    // --- open an unformatted document; formatting returns a real edit ---
    let messy = Uri::from_str("file:///work/b.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: messy.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "x=1\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let _messy_diag = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/formatting".to_string(),
            params: serde_json::to_value(DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri: messy.clone() },
                options: FormattingOptions {
                    tab_size: 4,
                    insert_spaces: true,
                    ..Default::default()
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let messy_response = client.receiver.recv().unwrap();
    match messy_response {
        Message::Response(resp) => {
            let edits: Option<Vec<TextEdit>> =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            let edits = edits.expect("formatting edits");
            assert_eq!(edits.len(), 1, "expected a single whole-document edit");
            assert_eq!(edits[0].new_text, "x = 1\n");
        }
        other => panic!("expected a formatting response, got {other:?}"),
    }

    // --- range formatting: the edit is scoped to the selected statement ---
    let scoped = Uri::from_str("file:///work/c.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: scoped.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "a=1\nb =2\nc= 3\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let _scoped_diag = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "textDocument/rangeFormatting".to_string(),
            params: serde_json::to_value(DocumentRangeFormattingParams {
                text_document: TextDocumentIdentifier {
                    uri: scoped.clone(),
                },
                range: Range::new(Position::new(1, 1), Position::new(1, 1)),
                options: FormattingOptions {
                    tab_size: 4,
                    insert_spaces: true,
                    ..Default::default()
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let scoped_response = client.receiver.recv().unwrap();
    match scoped_response {
        Message::Response(resp) => {
            let edits: Option<Vec<TextEdit>> =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            let edits = edits.expect("range formatting edits");
            assert_eq!(edits.len(), 1, "expected a single scoped edit");
            assert_eq!(edits[0].new_text, "b = 2");
            assert_eq!(
                edits[0].range,
                Range::new(Position::new(1, 0), Position::new(1, 4)),
                "the edit must cover exactly the widened statement"
            );
        }
        other => panic!("expected a rangeFormatting response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(5),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// The server advertises hover and answers `textDocument/hover` for a local
/// definition with its signature line and binding kind, rendered as markdown.
#[test]
fn hovers_a_local_definition() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capabilities announce hover support ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init_response = client.receiver.recv().unwrap();
    match init_response {
        Message::Response(resp) => {
            assert_eq!(
                resp.result().unwrap()["capabilities"]["hoverProvider"],
                serde_json::json!(true),
                "expected hover to be advertised"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document defining a local function ---
    let uri = Uri::from_str("file:///work/h.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "greet(name) = name\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag_note = client.receiver.recv().unwrap();
    assert!(matches!(diag_note, Message::Notification(_)));

    // --- hover the function name at (0, 0) ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/hover".to_string(),
            params: serde_json::to_value(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(0, 0),
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let hover_response = client.receiver.recv().unwrap();
    match hover_response {
        Message::Response(resp) => {
            let hover: Hover = serde_json::from_value(resp.result().unwrap()).unwrap();
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markup hover contents");
            };
            assert!(
                markup.value.contains("greet(name) = name"),
                "hover should show the definition line, got {:?}",
                markup.value
            );
            assert!(
                markup.value.contains("*function*"),
                "hover should tag the binding kind, got {:?}",
                markup.value
            );
        }
        other => panic!("expected a hover response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// A `$/cancelRequest` for an in-flight read is answered with `RequestCancelled`
/// (-32800), exactly once. This is the "arity pattern" anchor from TODO.md: it
/// began life as `cancel_request_is_currently_a_noop` documenting the pre-feature
/// behavior (the cancel was ignored and the real result returned) and flipped
/// here once cancellation landed.
#[test]
fn cancel_request_yields_request_cancelled() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_options(&client, serde_json::json!({}));

    let uri = Uri::from_str("file:///work/cancel.jl").unwrap();
    open_document(&client, &uri, "greet(name) = name\n");
    let _ = recv_publish_for(&client, &uri);

    // Fire a hover, then immediately cancel it. The read pipeline (analysis
    // thread → read pool) is many thread-hops slower than delivering the cancel,
    // and the main loop drains client input ahead of the pending read reply, so
    // the cancel reliably wins.
    let hover_id = RequestId::from(2);
    client
        .sender
        .send(Message::Request(Request {
            id: hover_id.clone(),
            method: "textDocument/hover".to_string(),
            params: serde_json::to_value(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(0, 0),
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "$/cancelRequest".to_string(),
            params: serde_json::json!({ "id": 2 }),
        }))
        .unwrap();

    let resp = recv_response(&client, hover_id);
    match resp.response_result {
        Err(err) => assert_eq!(err.code, -32800, "expected RequestCancelled"),
        Ok(value) => panic!("expected a RequestCancelled error, got {value:?}"),
    }

    drop(client);
    server_thread.join().unwrap();
}

/// A read computed against a buffer that a newer edit superseded is answered
/// with `ContentModified` (-32801), so the client re-requests against the
/// current text instead of applying a stale result.
#[test]
fn stale_read_yields_content_modified() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_options(&client, serde_json::json!({}));

    let uri = Uri::from_str("file:///work/stale.jl").unwrap();
    open_document(&client, &uri, "greet(name) = name\n");
    let _ = recv_publish_for(&client, &uri);

    // Hover against v1, then immediately land a v2 edit. The edit is processed
    // (main loop drains client input first) before the slower hover reply, so
    // the reply is gated as stale.
    let hover_id = RequestId::from(2);
    client
        .sender
        .send(Message::Request(Request {
            id: hover_id.clone(),
            method: "textDocument/hover".to_string(),
            params: serde_json::to_value(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(0, 0),
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "x = 1\n".to_string(),
                }],
            })
            .unwrap(),
        }))
        .unwrap();

    let resp = recv_response(&client, hover_id);
    match resp.response_result {
        Err(err) => assert_eq!(err.code, -32801, "expected ContentModified"),
        Ok(value) => panic!("expected a ContentModified error, got {value:?}"),
    }

    drop(client);
    server_thread.join().unwrap();
}

/// The server advertises signature help and answers `textDocument/signatureHelp`
/// for a call to an intra-file function, highlighting the argument the cursor is
/// on.
#[test]
fn serves_signature_help() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capabilities announce signature help ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init_response = client.receiver.recv().unwrap();
    match init_response {
        Message::Response(resp) => {
            let triggers = &resp.result().unwrap()["capabilities"]["signatureHelpProvider"]["triggerCharacters"];
            assert_eq!(
                *triggers,
                serde_json::json!(["(", ","]),
                "expected signature help trigger characters to be advertised"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document defining and calling a local function ---
    let uri = Uri::from_str("file:///work/s.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "greet(a, b) = a\ngreet(1, 2)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag_note = client.receiver.recv().unwrap();
    assert!(matches!(diag_note, Message::Notification(_)));

    // --- signature help on the second argument of `greet(1, 2)` at (1, 9) ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/signatureHelp".to_string(),
            params: serde_json::to_value(SignatureHelpParams {
                context: None,
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(1, 9),
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let help_response = client.receiver.recv().unwrap();
    match help_response {
        Message::Response(resp) => {
            let help: SignatureHelp = serde_json::from_value(resp.result().unwrap()).unwrap();
            assert_eq!(
                help.signatures
                    .iter()
                    .map(|s| s.label.as_str())
                    .collect::<Vec<_>>(),
                ["greet(a, b)"],
                "expected the local function signature"
            );
            assert_eq!(
                help.active_parameter,
                Some(1),
                "cursor is in the second argument"
            );
        }
        other => panic!("expected a signatureHelp response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

#[test]
fn serves_goto_definition() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capabilities announce the definition provider ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init_response = client.receiver.recv().unwrap();
    match init_response {
        Message::Response(resp) => {
            assert_eq!(
                resp.result().unwrap()["capabilities"]["definitionProvider"],
                serde_json::json!(true),
                "expected the definition provider to be advertised"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document defining and calling a local function ---
    let uri = Uri::from_str("file:///work/s.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "greet(a) = a\ngreet(1)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag_note = client.receiver.recv().unwrap();
    assert!(matches!(diag_note, Message::Notification(_)));

    // --- go-to-definition on the call `greet` at (1, 2) ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/definition".to_string(),
            params: serde_json::to_value(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(1, 2),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let def_response = client.receiver.recv().unwrap();
    match def_response {
        Message::Response(resp) => {
            let response: GotoDefinitionResponse =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            let GotoDefinitionResponse::Scalar(Location { uri: target, range }) = response else {
                panic!("expected a scalar location, got {response:?}");
            };
            assert_eq!(target, uri, "definition is in the same document");
            // The `greet` in the definition on line 0, columns 0..5.
            assert_eq!(range, Range::new(Position::new(0, 0), Position::new(0, 5)));
        }
        other => panic!("expected a definition response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// End-to-end multiple dispatch: go-to-definition on a call of a two-method
/// function returns an array with both method definition sites.
#[test]
fn serves_goto_definition_with_multiple_methods() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let _init_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document defining two methods of `greet` and calling it ---
    let uri = Uri::from_str("file:///work/s.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "greet(a) = a\ngreet(a, b) = a\ngreet(1)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag_note = client.receiver.recv().unwrap();
    assert!(matches!(diag_note, Message::Notification(_)));

    // --- go-to-definition on the call `greet` at (2, 2) ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/definition".to_string(),
            params: serde_json::to_value(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(2, 2),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let def_response = client.receiver.recv().unwrap();
    match def_response {
        Message::Response(resp) => {
            let response: GotoDefinitionResponse =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            let GotoDefinitionResponse::Array(locations) = response else {
                panic!("expected an array of locations, got {response:?}");
            };
            assert_eq!(locations.len(), 2, "{locations:?}");
            assert!(locations.iter().all(|l| l.uri == uri));
            // The two `greet` method names on lines 0 and 1, columns 0..5.
            assert_eq!(
                locations[0].range,
                Range::new(Position::new(0, 0), Position::new(0, 5))
            );
            assert_eq!(
                locations[1].range,
                Range::new(Position::new(1, 0), Position::new(1, 5))
            );
        }
        other => panic!("expected a definition response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// End-to-end references: the server advertises the provider, and a request on
/// a local variable returns every use plus the declaration in the same
/// document.
#[test]
fn serves_references() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init_response = client.receiver.recv().unwrap();
    match init_response {
        Message::Response(resp) => {
            assert_eq!(
                resp.result().unwrap()["capabilities"]["referencesProvider"],
                serde_json::json!(true),
                "expected the references provider to be advertised"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let uri = Uri::from_str("file:///work/s.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f()\n    x = 1\n    x + x\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag_note = client.receiver.recv().unwrap();
    assert!(matches!(diag_note, Message::Notification(_)));

    // References on the use `x` at (2, 4), including the declaration.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/references".to_string(),
            params: serde_json::to_value(ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(2, 4),
                },
                context: ReferenceContext {
                    include_declaration: true,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let response = client.receiver.recv().unwrap();
    match response {
        Message::Response(resp) => {
            let locations: Vec<Location> = serde_json::from_value(resp.result().unwrap()).unwrap();
            let ranges: Vec<_> = locations
                .iter()
                .map(|l| {
                    assert_eq!(l.uri, uri, "references are in the same document");
                    (l.range.start.line, l.range.start.character)
                })
                .collect();
            // `x = 1` on line 1, then the two uses on line 2.
            assert_eq!(ranges, vec![(1, 4), (2, 4), (2, 8)]);
        }
        other => panic!("expected a references response, got {other:?}"),
    }

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// End-to-end document highlight: the provider is advertised, and a request on
/// a variable returns each occurrence tagged read or write.
#[test]
fn serves_document_highlight() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init_response = client.receiver.recv().unwrap();
    match init_response {
        Message::Response(resp) => {
            assert_eq!(
                resp.result().unwrap()["capabilities"]["documentHighlightProvider"],
                serde_json::json!(true),
                "expected the document highlight provider to be advertised"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let uri = Uri::from_str("file:///work/s.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f()\n    x = 1\n    x = 2\n    x\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag_note = client.receiver.recv().unwrap();
    assert!(matches!(diag_note, Message::Notification(_)));

    // Highlight from the read `x` at (3, 4).
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/documentHighlight".to_string(),
            params: serde_json::to_value(DocumentHighlightParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(3, 4),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let response = client.receiver.recv().unwrap();
    match response {
        Message::Response(resp) => {
            let highlights: Vec<DocumentHighlight> =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            let tagged: Vec<_> = highlights
                .iter()
                .map(|h| (h.range.start.line, h.kind.unwrap()))
                .collect();
            // Two assignments write; the trailing use reads.
            assert_eq!(
                tagged,
                vec![
                    (1, DocumentHighlightKind::WRITE),
                    (2, DocumentHighlightKind::WRITE),
                    (3, DocumentHighlightKind::READ),
                ]
            );
        }
        other => panic!("expected a document highlight response, got {other:?}"),
    }

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// End-to-end rename: the provider (with prepare support) is advertised,
/// `prepareRename` reports the identifier range, and `rename` returns a
/// workspace edit touching every occurrence of the binding.
// `WorkspaceEdit::changes` is keyed by `Uri`, which clippy flags as a mutable
// key type (a false positive: the interior mutability is never used for hashing).
#[allow(clippy::mutable_key_type)]
#[test]
fn serves_rename() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init_response = client.receiver.recv().unwrap();
    match init_response {
        Message::Response(resp) => {
            let caps = resp.result().unwrap();
            assert_eq!(
                caps["capabilities"]["renameProvider"]["prepareProvider"],
                serde_json::json!(true),
                "expected the rename provider to advertise prepare support"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let uri = Uri::from_str("file:///work/s.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f()\n    x = 1\n    x + x\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag_note = client.receiver.recv().unwrap();
    assert!(matches!(diag_note, Message::Notification(_)));

    // prepareRename on the use `x` at (2, 4) reports the identifier's range.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/prepareRename".to_string(),
            params: serde_json::to_value(TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position::new(2, 4),
            })
            .unwrap(),
        }))
        .unwrap();
    let response = client.receiver.recv().unwrap();
    match response {
        Message::Response(resp) => {
            let range: Range = serde_json::from_value(resp.result().unwrap()).unwrap();
            assert_eq!(range.start, Position::new(2, 4));
            assert_eq!(range.end, Position::new(2, 5));
        }
        other => panic!("expected a prepareRename response, got {other:?}"),
    }

    // rename that `x` to `total`.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/rename".to_string(),
            params: serde_json::to_value(RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(2, 4),
                },
                new_name: "total".to_string(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let response = client.receiver.recv().unwrap();
    match response {
        Message::Response(resp) => {
            let edit: WorkspaceEdit = serde_json::from_value(resp.result().unwrap()).unwrap();
            let changes = edit.changes.expect("intra-file changes");
            let edits = changes.get(&uri).expect("edits for the document");
            let sites: Vec<_> = edits
                .iter()
                .map(|e| {
                    assert_eq!(e.new_text, "total");
                    (e.range.start.line, e.range.start.character)
                })
                .collect();
            // `x = 1` on line 1, then the two uses on line 2.
            assert_eq!(sites, vec![(1, 4), (2, 4), (2, 8)]);
        }
        other => panic!("expected a rename response, got {other:?}"),
    }

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// The server advertises incremental sync and splices range edits into the
/// live buffer: a batch of two range edits (the second positioned against the
/// text after the first) fixes a parse error, a later range edit reintroduces
/// one, and a `didChange` for a never-opened document is ignored.
#[test]
fn applies_incremental_range_edits() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capabilities announce incremental sync ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init_response = client.receiver.recv().unwrap();
    match init_response {
        Message::Response(resp) => {
            let result = resp.result().unwrap();
            assert_eq!(
                result["capabilities"]["textDocumentSync"]["change"],
                serde_json::json!(2),
                "expected TextDocumentSyncKind::INCREMENTAL"
            );
            assert_eq!(
                result["capabilities"]["textDocumentSync"]["save"],
                serde_json::json!(true),
                "save notifications drive the workspace re-harvest"
            );
            assert_eq!(
                result["capabilities"]["positionEncoding"],
                serde_json::json!("utf-16"),
                "a client offering no encodings gets the LSP default"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let recv_diagnostics = |client: &Connection| -> PublishDiagnosticsParams {
        loop {
            match client.receiver.recv().unwrap() {
                Message::Notification(note) if note.method == "textDocument/publishDiagnostics" => {
                    return serde_json::from_value(note.params).unwrap();
                }
                _ => {}
            }
        }
    };
    let range_edit =
        |start: (u32, u32), end: (u32, u32), text: &str| TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_string(),
        };
    let did_change = |uri: &Uri, version: i32, changes: Vec<TextDocumentContentChangeEvent>| {
        Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: changes,
            })
            .unwrap(),
        })
    };

    // --- open a document with a parse error; expect an error diagnostic @v1 ---
    let uri = Uri::from_str("file:///work/ranged.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f(x)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(1));
    assert!(!diag.diagnostics.is_empty());

    // --- fix it with a batch of two range edits; the second edit's position
    // is only valid against the text after the first, pinning sequential
    // application. Buffer becomes "function f(x)\n    x\nend\n". ---
    client
        .sender
        .send(did_change(
            &uri,
            2,
            vec![
                range_edit((1, 0), (1, 0), "    x\n"),
                range_edit((2, 0), (2, 0), "end\n"),
            ],
        ))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(2));
    assert_eq!(diag.diagnostics, Vec::new());

    // --- delete the `end` line with a range edit; the error returns @v3 ---
    client
        .sender
        .send(did_change(&uri, 3, vec![range_edit((2, 0), (3, 0), "")]))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(3));
    assert!(
        !diag.diagnostics.is_empty(),
        "expected the parse error back after deleting `end`"
    );

    // --- a change for a never-opened document is dropped: the next publish
    // observed is the clearing one for the real document's close ---
    let unknown = Uri::from_str("file:///work/never-opened.jl").unwrap();
    client
        .sender
        .send(did_change(
            &unknown,
            1,
            vec![range_edit((0, 0), (0, 0), "x")],
        ))
        .unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didClose".to_string(),
            params: serde_json::to_value(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.uri, uri);
    assert_eq!(diag.version, None);
    assert_eq!(diag.diagnostics, Vec::new());

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// Diagnostics carry the buffer version they were computed against, a fixing
/// edit yields a fresh (empty) report for the new version, and closing the
/// document clears diagnostics. Publishes for superseded versions are dropped
/// by the main loop's version gate, so the reports observed here are
/// unambiguous.
#[test]
fn publishes_versioned_diagnostics_across_edits() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let _init_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let recv_diagnostics = |client: &Connection| -> PublishDiagnosticsParams {
        loop {
            match client.receiver.recv().unwrap() {
                Message::Notification(note) if note.method == "textDocument/publishDiagnostics" => {
                    return serde_json::from_value(note.params).unwrap();
                }
                _ => {}
            }
        }
    };

    // --- open a document with a parse error; expect an error diagnostic @v1 ---
    let uri = Uri::from_str("file:///work/broken.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f(x)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(1));
    assert!(
        !diag.diagnostics.is_empty(),
        "expected a parse diagnostic for the unterminated function"
    );
    assert!(diag.diagnostics[0].message.contains("expected `end`"));

    // --- fix the document; expect an empty report @v2 ---
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "function f(x)\n    x\nend\n".to_string(),
                }],
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(2));
    assert_eq!(diag.diagnostics, Vec::new());

    // --- close the document; expect a clearing (empty, versionless) publish ---
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didClose".to_string(),
            params: serde_json::to_value(DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, None);
    assert_eq!(diag.diagnostics, Vec::new());

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// A burst of `didChange`s is coalesced by the analysis scheduler and gated by
/// the main loop's version check: intermediate versions are dropped (fewer
/// publishes than edits), no stale version is ever published (versions arrive
/// monotonically), and the client's final diagnostics reflect the last edit,
/// tagged with its version. This drives the cancellation/coalescing signal flow
/// end to end, complementing the pure-decision `decide` unit tests.
#[test]
fn coalesces_rapid_didchanges_and_gates_stale_versions() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake (push-diagnostics client) ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let _init_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let uri = Uri::from_str("file:///work/rapid.jl").unwrap();

    // --- open clean @v1 ---
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "x = 1\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();

    // --- fire a burst of full-document edits v2..=FINAL, back to back ---
    // Intermediate versions are clean; only the final one carries a parse error,
    // so the terminal diagnostic set uniquely identifies the last edit.
    const FINAL: i32 = 13;
    for version in 2..=FINAL {
        let text = if version == FINAL {
            // Unterminated `function` → a parse error mentioning `end`.
            "function f(x)\n".to_string()
        } else {
            format!("x = {version}\n")
        };
        client
            .sender
            .send(Message::Notification(Notification {
                method: "textDocument/didChange".to_string(),
                params: serde_json::to_value(DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier {
                        uri: uri.clone(),
                        version,
                    },
                    content_changes: vec![TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text,
                    }],
                })
                .unwrap(),
            }))
            .unwrap();
    }

    // Drain publishes until the final version lands, recording every version
    // seen; then drain any stragglers with a short timeout.
    let mut versions: Vec<i32> = Vec::new();
    let final_diags: Vec<Diagnostic> = loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
                if let Some(v) = params.version {
                    versions.push(v);
                    if v == FINAL {
                        break params.diagnostics;
                    }
                }
            }
            _ => {}
        }
    };
    // Collect stragglers (there should be none after the final version, but
    // draining guards against an off-by-one in the assertions below).
    while let Ok(msg) = client.receiver.recv_timeout(Duration::from_millis(500)) {
        if let Message::Notification(n) = msg
            && n.method == "textDocument/publishDiagnostics"
        {
            let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
            versions.extend(params.version);
        }
    }

    // The final publish reflects the last edit's content.
    assert!(
        final_diags
            .iter()
            .any(|d| d.message.contains("expected `end`")),
        "final diagnostics should carry the unterminated-function parse error, got {final_diags:?}"
    );

    // Version gate: nothing stale or from the future is published, and versions
    // arrive monotonically (a superseded version never appears after a newer one).
    assert!(
        versions.windows(2).all(|w| w[0] <= w[1]),
        "published versions must be monotonically non-decreasing, got {versions:?}"
    );
    assert!(
        versions.iter().all(|&v| v <= FINAL),
        "no version beyond the last edit may be published, got {versions:?}"
    );
    assert_eq!(
        *versions.last().unwrap(),
        FINAL,
        "the last publish must be the final version"
    );

    // Coalescing: the burst of 12 edits (v2..=13) plus the open collapse into far
    // fewer publishes than versions sent — stale intermediates are dropped.
    let edits_sent = FINAL; // v1 open + 12 changes = 13 buffer versions total.
    assert!(
        (versions.len() as i32) < edits_sent,
        "expected coalescing to drop intermediate versions: saw {} publishes for {edits_sent} edits ({versions:?})",
        versions.len()
    );

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// A client offering `utf-8` in `general.positionEncodings` gets it advertised
/// back, and the server then reads incoming range positions as byte offsets:
/// an edit deleting the 2-byte `é` (1 UTF-16 unit) is specified as 2 character
/// units, and the resulting buffer is pinned exactly through a formatting
/// round trip.
#[test]
fn negotiates_utf8_position_encoding() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake offering utf-8 ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams {
                capabilities: ClientCapabilities {
                    general: Some(GeneralClientCapabilities {
                        position_encodings: Some(vec![
                            PositionEncodingKind::UTF8,
                            PositionEncodingKind::UTF16,
                        ]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let result = resp.result().unwrap();
            assert_eq!(
                result["capabilities"]["positionEncoding"],
                serde_json::json!("utf-8"),
                "expected the offered utf-8 encoding to be picked"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let recv_diagnostics = |client: &Connection| -> PublishDiagnosticsParams {
        loop {
            match client.receiver.recv().unwrap() {
                Message::Notification(note) if note.method == "textDocument/publishDiagnostics" => {
                    return serde_json::from_value(note.params).unwrap();
                }
                _ => {}
            }
        }
    };

    // --- open "éy=1\n" (unformatted, but valid: no diagnostics) ---
    let uri = Uri::from_str("file:///work/utf8.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "\u{00E9}y=1\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(1));
    assert_eq!(diag.diagnostics, Vec::new());

    // --- delete the leading `é` with a byte-offset range: (0,0)..(0,2).
    // Misread as UTF-16 units this would delete `éy`, leaving the parse
    // error `=1`; read as bytes it leaves the valid `y=1`. ---
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: Some(Range::new(Position::new(0, 0), Position::new(0, 2))),
                    range_length: None,
                    text: String::new(),
                }],
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(2));
    assert_eq!(diag.diagnostics, Vec::new());

    // --- formatting the still-unformatted buffer reveals its exact content ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/formatting".to_string(),
            params: serde_json::to_value(DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                options: FormattingOptions {
                    tab_size: 4,
                    insert_spaces: true,
                    ..Default::default()
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let edits: Option<Vec<TextEdit>> =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            let edits = edits.expect("formatting edits");
            assert_eq!(edits.len(), 1, "expected a single whole-document edit");
            assert_eq!(
                edits[0].new_text, "y = 1\n",
                "the edit must have deleted exactly the 2-byte `é`"
            );
        }
        other => panic!("expected a formatting response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

// --- document symbols: unit tests on the pure compute function ---

fn symbols(text: &str) -> Vec<DocumentSymbol> {
    compute_document_symbols(text, PositionEncoding::Utf16)
}

fn names_and_kinds(symbols: &[DocumentSymbol]) -> Vec<(&str, SymbolKind)> {
    symbols.iter().map(|s| (s.name.as_str(), s.kind)).collect()
}

fn assert_selection_within_range(symbols: &[DocumentSymbol]) {
    fn contains(outer: &Range, inner: &Range) -> bool {
        outer.start <= inner.start && inner.end <= outer.end
    }
    for symbol in symbols {
        assert!(
            contains(&symbol.range, &symbol.selection_range),
            "selection range of `{}` escapes its full range",
            symbol.name
        );
        assert_selection_within_range(symbol.children.as_deref().unwrap_or_default());
    }
}

#[test]
fn document_symbols_cover_every_top_level_definition_kind() {
    let text = "\
module M
end

function f(x)
    return x
end

macro m(ex)
end

struct S
    a
    b::Int
end

abstract type A end

primitive type P 8 end

const C = 1
";
    let symbols = symbols(text);
    assert_eq!(
        names_and_kinds(&symbols),
        vec![
            ("M", SymbolKind::MODULE),
            ("f", SymbolKind::FUNCTION),
            ("@m", SymbolKind::FUNCTION),
            ("S", SymbolKind::STRUCT),
            ("A", SymbolKind::INTERFACE),
            ("P", SymbolKind::STRUCT),
            ("C", SymbolKind::CONSTANT),
        ]
    );
    let s = &symbols[3];
    assert_eq!(
        names_and_kinds(s.children.as_deref().unwrap_or_default()),
        vec![("a", SymbolKind::FIELD), ("b", SymbolKind::FIELD)]
    );
    assert_selection_within_range(&symbols);
}

#[test]
fn document_symbols_nest_definitions_inside_modules_and_functions() {
    let text = "\
module Outer
module Inner
g() = 1
end
function f()
    helper(x) = x
    return helper
end
end
";
    let symbols = symbols(text);
    assert_eq!(
        names_and_kinds(&symbols),
        vec![("Outer", SymbolKind::MODULE)]
    );
    let outer = symbols[0].children.as_deref().unwrap_or_default();
    assert_eq!(
        names_and_kinds(outer),
        vec![("Inner", SymbolKind::MODULE), ("f", SymbolKind::FUNCTION)]
    );
    let inner = outer[0].children.as_deref().unwrap_or_default();
    assert_eq!(names_and_kinds(inner), vec![("g", SymbolKind::FUNCTION)]);
    let f = outer[1].children.as_deref().unwrap_or_default();
    assert_eq!(names_and_kinds(f), vec![("helper", SymbolKind::FUNCTION)]);
}

#[test]
fn document_symbols_carry_the_signature_as_detail() {
    let text = "\
f(x::Int, y) = x
g(x::T) where T = x
h(x)::Int = x
function Base.show(io, x)
end
function +(a, b)
    a
end
function forward end
";
    let symbols = symbols(text);
    let details: Vec<(&str, Option<&str>)> = symbols
        .iter()
        .map(|s| (s.name.as_str(), s.detail.as_deref()))
        .collect();
    assert_eq!(
        details,
        vec![
            ("f", Some("(x::Int, y)")),
            ("g", Some("(x::T) where T")),
            ("h", Some("(x)::Int")),
            ("Base.show", Some("(io, x)")),
            ("+", Some("(a, b)")),
            ("forward", None),
        ]
    );
    assert!(symbols.iter().all(|s| s.kind == SymbolKind::FUNCTION));
}

#[test]
fn document_symbols_split_a_multi_name_const() {
    let symbols = symbols("const a, b = 1, 2\n");
    assert_eq!(
        names_and_kinds(&symbols),
        vec![("a", SymbolKind::CONSTANT), ("b", SymbolKind::CONSTANT)]
    );
}

#[test]
fn document_symbols_surface_a_definition_inside_control_flow() {
    let text = "\
if flag
    helper(x) = x
end
let
    inner() = 2
end
";
    assert_eq!(
        names_and_kinds(&symbols(text)),
        vec![
            ("helper", SymbolKind::FUNCTION),
            ("inner", SymbolKind::FUNCTION),
        ]
    );
}

#[test]
fn document_symbols_include_inner_constructors() {
    let text = "\
struct S
    x::Int
    S(x) = new(x)
end
";
    let symbols = symbols(text);
    let members = symbols[0].children.as_deref().unwrap_or_default();
    assert_eq!(
        names_and_kinds(members),
        vec![("x", SymbolKind::FIELD), ("S", SymbolKind::FUNCTION)]
    );
}

#[test]
fn document_symbols_skip_non_definitions() {
    assert!(symbols("x = 1\nprint(1)\nx, y = 1, 2\n").is_empty());
    assert!(symbols("").is_empty());
    // An interpolated definition name is not statically known.
    assert!(symbols("function $f end\n").is_empty());
}

#[test]
fn document_symbols_are_best_effort_on_broken_input() {
    // Unterminated function: the parse error must not hide the symbol.
    let symbols = symbols("function f(x)\n");
    assert_eq!(names_and_kinds(&symbols), vec![("f", SymbolKind::FUNCTION)]);
}

#[test]
fn document_symbol_positions_follow_the_encoding() {
    // U+1F600 is 4 bytes in UTF-8, 2 UTF-16 units; the symbol's end position
    // on the same line differs accordingly.
    let text = "f(x) = \"\u{1F600}\"\n";
    let utf16 = compute_document_symbols(text, PositionEncoding::Utf16);
    let utf8 = compute_document_symbols(text, PositionEncoding::Utf8);
    assert_eq!(utf16[0].range.end, Position::new(0, 11));
    assert_eq!(utf8[0].range.end, Position::new(0, 13));
}

/// End-to-end: the capability is advertised, an open document returns its
/// nested outline, and an unknown document returns null.
#[test]
fn serves_document_symbols() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capability advertised ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let result = resp.result().unwrap();
            assert_eq!(
                result["capabilities"]["documentSymbolProvider"],
                serde_json::json!(true),
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document; drain its diagnostics publish ---
    let uri = Uri::from_str("file:///work/symbols.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "module M\nf(x) = x\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let _diag = client.receiver.recv().unwrap();

    // --- request document symbols; expect the nested outline ---
    let symbol_params = |uri: &Uri| {
        serde_json::to_value(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/documentSymbol".to_string(),
            params: symbol_params(&uri),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let symbols: Vec<DocumentSymbol> =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            assert_eq!(names_and_kinds(&symbols), vec![("M", SymbolKind::MODULE)]);
            let children = symbols[0].children.as_deref().unwrap_or_default();
            assert_eq!(names_and_kinds(children), vec![("f", SymbolKind::FUNCTION)]);
        }
        other => panic!("expected a documentSymbol response, got {other:?}"),
    }

    // --- an unknown document answers null ---
    let unknown = Uri::from_str("file:///work/never-opened.jl").unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/documentSymbol".to_string(),
            params: symbol_params(&unknown),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            assert_eq!(resp.result(), Some(serde_json::Value::Null));
        }
        other => panic!("expected a null response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// End-to-end workspace symbols: the server advertises the provider, and a
/// `workspace/symbol` query with no Julia environment loaded returns an empty
/// list (the in-memory connection stands up no depot, so the depot-resolution
/// path is covered by the unit tests, as go-to-definition's is).
#[test]
fn serves_workspace_symbols() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capability advertised ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            assert_eq!(
                resp.result().unwrap()["capabilities"]["workspaceSymbolProvider"],
                serde_json::json!(true),
                "expected the workspace symbol provider to be advertised"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- query with no workspace package loaded → an empty symbol list ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "workspace/symbol".to_string(),
            params: serde_json::to_value(WorkspaceSymbolParams {
                query: "foo".to_string(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let response: WorkspaceSymbolResponse =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            match response {
                WorkspaceSymbolResponse::Nested(symbols) => assert!(
                    symbols.is_empty(),
                    "no environment loaded, so no workspace symbols"
                ),
                WorkspaceSymbolResponse::Flat(symbols) => assert!(symbols.is_empty()),
            }
        }
        other => panic!("expected a workspaceSymbol response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

#[test]
fn serves_completion_and_resolve() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capability advertised ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let result = resp.result().unwrap();
            assert_eq!(
                result["capabilities"]["completionProvider"]["resolveProvider"],
                serde_json::json!(true),
            );
            assert_eq!(
                result["capabilities"]["completionProvider"]["triggerCharacters"],
                serde_json::json!([".", "@", "\\"]),
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document with a local, then request completion in its body ---
    let uri = Uri::from_str("file:///work/complete.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f(alpha)\n    \nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let _diag = client.receiver.recv().unwrap();

    // Cursor on the blank body line (line 1, after its indentation).
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/completion".to_string(),
            params: serde_json::to_value(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    position: Position::new(1, 4),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let items = match serde_json::from_value(resp.result().unwrap()).unwrap() {
                CompletionResponse::Array(items) => items,
                CompletionResponse::List(list) => list.items,
            };
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            // The parameter is in scope, the function name is in scope, and
            // keywords are always offered.
            assert!(labels.contains(&"alpha"), "missing local in {labels:?}");
            assert!(labels.contains(&"f"), "missing function name in {labels:?}");
            let function_kw = items.iter().find(|i| i.label == "function").unwrap();
            assert_eq!(function_kw.kind, Some(CompletionItemKind::KEYWORD));
        }
        other => panic!("expected a completion response, got {other:?}"),
    }

    // --- a LaTeX sequence completes through the same live-buffer path ---
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": "function f(alpha)\n    x = \\alph\nend\n" }],
            }),
        }))
        .unwrap();
    let _diag = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/completion".to_string(),
            params: serde_json::to_value(CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    // Just past `\alph` on the body line.
                    position: Position::new(1, 13),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let items = match serde_json::from_value(resp.result().unwrap()).unwrap() {
                CompletionResponse::Array(items) => items,
                CompletionResponse::List(list) => list.items,
            };
            let alpha = items
                .iter()
                .find(|i| i.label == "\\alpha")
                .unwrap_or_else(|| panic!("no LaTeX sequence in {items:?}"));
            assert_eq!(alpha.detail.as_deref(), Some("α"));
            // The edit replaces the backslash too, so `x = \alph` becomes `x = α`.
            match alpha.text_edit.as_ref().expect("a text edit") {
                lsp_types::CompletionTextEdit::Edit(edit) => {
                    assert_eq!(edit.new_text, "α");
                    assert_eq!(edit.range.start, Position::new(1, 8));
                    assert_eq!(edit.range.end, Position::new(1, 13));
                }
                other => panic!("expected a plain edit, got {other:?}"),
            }
        }
        other => panic!("expected a completion response, got {other:?}"),
    }

    // --- resolve round-trips an item (no library loaded, so unchanged) ---
    let item = CompletionItem {
        label: "alpha".to_string(),
        ..Default::default()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "completionItem/resolve".to_string(),
            params: serde_json::to_value(&item).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let resolved: CompletionItem = serde_json::from_value(resp.result().unwrap()).unwrap();
            assert_eq!(resolved.label, "alpha");
        }
        other => panic!("expected a resolve response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(5),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

// --- folding ranges: unit tests on the pure compute function ---

/// Folds as `(start_line, end_line, kind)` triples, asserting the line-only
/// convention (no character offsets) along the way.
fn folds(text: &str) -> Vec<(u32, u32, Option<FoldingRangeKind>)> {
    compute_folding_ranges(text)
        .into_iter()
        .map(|fold| {
            assert_eq!(fold.start_character, None, "folds must be line-only");
            assert_eq!(fold.end_character, None, "folds must be line-only");
            (fold.start_line, fold.end_line, fold.kind)
        })
        .collect()
}

#[test]
fn folding_covers_nested_definition_and_loop_blocks() {
    let text = "\
module M
function f(x)
    for i in xs
        x
    end
end
end
";
    assert_eq!(
        folds(text),
        vec![(0, 6, None), (1, 5, None), (2, 4, None)],
        "module, function, and loop each fold through their `end`"
    );
}

#[test]
fn folding_covers_every_expression_block_kind() {
    let text = "\
struct S
    a
end
while c
    x
end
begin
    x
end
quote
    x
end
let y = 1
    y
end
map(xs) do x
    x
end
";
    assert_eq!(
        folds(text),
        vec![
            (0, 2, None),
            (3, 5, None),
            (6, 8, None),
            (9, 11, None),
            (12, 14, None),
            (15, 17, None),
        ],
    );
}

#[test]
fn folding_makes_if_and_try_arms_collapse_individually() {
    let text = "\
if a
    x
elseif b
    y
else
    z
end
";
    assert_eq!(
        folds(text),
        vec![(0, 6, None), (2, 3, None), (4, 5, None)],
        "the whole `if` folds, and so does each later arm"
    );

    let text = "\
try
    x
catch err
    y
finally
    z
end
";
    assert_eq!(
        folds(text),
        vec![(0, 6, None), (2, 3, None), (4, 5, None)],
        "the whole `try` folds, and so do `catch` and `finally`"
    );
}

#[test]
fn folding_skips_single_line_constructs() {
    let text = "\
begin; x; end
f(x) = x
using A
# lone comment
";
    assert_eq!(folds(text), vec![]);
}

#[test]
fn folding_groups_comment_runs() {
    let text = "\
# a
# b
x = 1
";
    assert_eq!(folds(text), vec![(0, 1, Some(FoldingRangeKind::Comment))]);
}

#[test]
fn folding_ignores_trailing_comments_in_runs() {
    let text = "\
# lead
x = 1  # tail
# c
# d
";
    assert_eq!(
        folds(text),
        vec![(2, 3, Some(FoldingRangeKind::Comment))],
        "a trailing comment neither starts nor joins a run"
    );
}

#[test]
fn folding_covers_comment_runs_inside_blocks() {
    let text = "\
function f(x)
    # a
    # b
    x
end
";
    assert_eq!(
        folds(text),
        vec![(0, 4, None), (1, 2, Some(FoldingRangeKind::Comment))],
    );
}

#[test]
fn folding_covers_multi_line_block_comments() {
    let text = "\
#=
body
=#
x = 1
";
    assert_eq!(folds(text), vec![(0, 2, Some(FoldingRangeKind::Comment))]);
}

#[test]
fn folding_groups_consecutive_imports() {
    let text = "\
using A
import B
using C

x = 1
";
    assert_eq!(folds(text), vec![(0, 2, Some(FoldingRangeKind::Imports))]);
}

#[test]
fn folding_splits_import_groups_on_blank_lines() {
    let text = "\
using A

using B
using C
";
    assert_eq!(folds(text), vec![(2, 3, Some(FoldingRangeKind::Imports))]);
}

#[test]
fn folding_covers_a_multi_line_import_on_its_own() {
    let text = "\
using Foo:
    a,
    b
using Bar
";
    assert_eq!(
        folds(text),
        vec![
            (0, 3, Some(FoldingRangeKind::Imports)),
            (0, 2, Some(FoldingRangeKind::Imports)),
        ],
        "the group folds as a whole and the multi-line statement by itself"
    );
}

#[test]
fn folding_is_best_effort_on_broken_input() {
    // A missing `end`: whatever partial folds come out must not panic.
    let _ = folds("function f(x)\n    if x\n        x\n    end\n");
}

#[test]
fn serves_folding_ranges() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capability advertised ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let result = resp.result().unwrap();
            assert_eq!(
                result["capabilities"]["foldingRangeProvider"],
                serde_json::json!(true),
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document; drain its diagnostics publish ---
    let uri = Uri::from_str("file:///work/folding.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f(x)\n    x\nend\n# a\n# b\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let _diag = client.receiver.recv().unwrap();

    // --- request folding ranges ---
    let folding_params = |uri: &Uri| {
        serde_json::to_value(FoldingRangeParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/foldingRange".to_string(),
            params: folding_params(&uri),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let folds: Vec<FoldingRange> = serde_json::from_value(resp.result().unwrap()).unwrap();
            let triples: Vec<_> = folds
                .into_iter()
                .map(|f| (f.start_line, f.end_line, f.kind))
                .collect();
            assert_eq!(
                triples,
                vec![(0, 2, None), (3, 4, Some(FoldingRangeKind::Comment))],
            );
        }
        other => panic!("expected a foldingRange response, got {other:?}"),
    }

    // --- an unknown document answers null ---
    let unknown = Uri::from_str("file:///work/never-opened.jl").unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/foldingRange".to_string(),
            params: folding_params(&unknown),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            assert_eq!(resp.result(), Some(serde_json::Value::Null));
        }
        other => panic!("expected a null response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// A document's static `include("path")` strings come back as document links
/// targeting the resolved file; an unknown document answers null.
#[test]
fn serves_document_links() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capability advertised ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let result = resp.result().unwrap();
            assert!(
                result["capabilities"]["documentLinkProvider"].is_object(),
                "documentLinkProvider must be advertised"
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document; drain its diagnostics publish ---
    let uri = native_file_uri("/work/src/Pkg.jl");
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "include(\"sub/a.jl\")\ninclude(dynamic)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let _diag = client.receiver.recv().unwrap();

    // --- request document links ---
    let link_params = |uri: &Uri| {
        serde_json::to_value(DocumentLinkParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/documentLink".to_string(),
            params: link_params(&uri),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let links: Vec<DocumentLink> = serde_json::from_value(resp.result().unwrap()).unwrap();
            assert_eq!(links.len(), 1, "only the static include links");
            assert_eq!(
                links[0].target.as_ref().map(|t| t.as_str()),
                Some(native_file_uri("/work/src/sub/a.jl").as_str()),
            );
            // The link covers exactly `sub/a.jl`, inside the quotes.
            assert_eq!(
                links[0].range,
                Range::new(Position::new(0, 9), Position::new(0, 17)),
            );
        }
        other => panic!("expected a documentLink response, got {other:?}"),
    }

    // --- an unknown document answers null ---
    let unknown = Uri::from_str("file:///work/never-opened.jl").unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/documentLink".to_string(),
            params: link_params(&unknown),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            assert_eq!(resp.result(), Some(serde_json::Value::Null));
        }
        other => panic!("expected a null response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

// --- selection ranges: unit tests on the pure compute function ---

/// Flatten a linked chain into its ranges, innermost first, asserting each
/// step strictly contains the one below it (containment is what makes the
/// client's repeated "expand" well-defined).
fn flatten_chain(selection: SelectionRange) -> Vec<Range> {
    let mut out = vec![selection.range];
    let mut current = selection;
    while let Some(parent) = current.parent {
        current = *parent;
        let (inner, outer) = (*out.last().unwrap(), current.range);
        assert!(
            outer.start <= inner.start && inner.end <= outer.end && inner != outer,
            "each step must strictly widen: {inner:?} -> {outer:?}"
        );
        out.push(current.range);
    }
    out
}

/// The chain for a single position under UTF-8, flattened innermost-first.
fn chain(text: &str, line: u32, character: u32) -> Vec<Range> {
    let mut chains = compute_selection_ranges(
        text,
        &[Position::new(line, character)],
        PositionEncoding::Utf8,
    );
    assert_eq!(chains.len(), 1, "one chain per requested position");
    flatten_chain(chains.remove(0))
}

fn sel(start_line: u32, start_char: u32, end_line: u32, end_char: u32) -> Range {
    Range::new(
        Position::new(start_line, start_char),
        Position::new(end_line, end_char),
    )
}

#[test]
fn selection_expands_from_identifier_through_enclosing_nodes() {
    // Cursor on the `x` of `x + 1`: identifier, binary expression, body
    // block, whole definition, whole file.
    assert_eq!(
        chain("function f(x)\n    x + 1\nend\n", 1, 4),
        vec![
            sel(1, 4, 1, 5),
            sel(1, 4, 1, 9),
            sel(0, 13, 2, 0),
            sel(0, 0, 2, 3),
            sel(0, 0, 3, 0),
        ],
    );
}

#[test]
fn selection_widens_stepwise_through_nested_calls() {
    // Cursor on the inner `x`: every nesting level is its own step, and
    // same-extent wrapper nodes contribute no zero-growth steps (the
    // strict-widening assertion in `flatten_chain` backstops that).
    assert_eq!(
        chain("f(g(x), y)\n", 0, 4),
        vec![
            sel(0, 4, 0, 5),
            sel(0, 3, 0, 6),
            sel(0, 2, 0, 6),
            sel(0, 1, 0, 10),
            sel(0, 0, 0, 10),
            sel(0, 0, 1, 0),
        ],
    );
}

#[test]
fn selection_returns_one_chain_per_position_in_order() {
    let text = "function f(x)\n    x + 1\nend\n";
    let chains = compute_selection_ranges(
        text,
        &[Position::new(1, 4), Position::new(0, 9)],
        PositionEncoding::Utf8,
    );
    let innermost: Vec<Range> = chains
        .into_iter()
        .map(|chain| flatten_chain(chain)[0])
        .collect();
    assert_eq!(
        innermost,
        vec![sel(1, 4, 1, 5), sel(0, 9, 0, 10)],
        "chains answer the requested positions in request order"
    );
}

#[test]
fn selection_prefers_identifier_at_token_boundary() {
    // Cursor between `f` and `(`: expansion starts from the identifier, not
    // the parenthesis.
    assert_eq!(chain("f(x)\n", 0, 1)[0], sel(0, 0, 0, 1));
}

#[test]
fn selection_in_whitespace_starts_at_the_enclosing_node() {
    // Cursor in the body's leading indentation: whitespace itself is not a
    // selection step, so the chain starts at the enclosing block.
    assert_eq!(
        chain("function f(x)\n    x + 1\nend\n", 1, 0),
        vec![sel(0, 13, 2, 0), sel(0, 0, 2, 3), sel(0, 0, 3, 0)],
    );
}

#[test]
fn selection_in_a_comment_starts_at_the_comment() {
    assert_eq!(chain("# hi\nx = 1\n", 0, 2)[0], sel(0, 0, 0, 4));
}

#[test]
fn selection_respects_the_negotiated_encoding() {
    // `α` is two UTF-8 bytes but one UTF-16 unit, shifting every column on
    // the line: the same cursor-on-`1` request differs in both the position
    // decoded and the ranges encoded.
    let text = "α + 1\n";
    let utf8 = compute_selection_ranges(text, &[Position::new(0, 5)], PositionEncoding::Utf8);
    assert_eq!(
        flatten_chain(utf8.into_iter().next().unwrap())[0],
        sel(0, 5, 0, 6)
    );
    let utf16 = compute_selection_ranges(text, &[Position::new(0, 4)], PositionEncoding::Utf16);
    assert_eq!(
        flatten_chain(utf16.into_iter().next().unwrap())[0],
        sel(0, 4, 0, 5)
    );
}

#[test]
fn selection_clamps_out_of_bounds_and_handles_empty_input() {
    // An empty file yields a single parentless (empty) range.
    assert_eq!(chain("", 0, 0), vec![sel(0, 0, 0, 0)]);
    // A position past the end of the buffer clamps instead of panicking.
    assert_eq!(chain("x\n", 5, 0), vec![sel(0, 0, 1, 0)]);
}

#[test]
fn selection_is_best_effort_on_broken_input() {
    // A missing `end`: whatever partial chain comes out must not panic.
    let _ = chain("function f(x)\n    if x\n        x\n", 1, 7);
}

#[test]
fn serves_selection_ranges() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capability advertised ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let result = resp.result().unwrap();
            assert_eq!(
                result["capabilities"]["selectionRangeProvider"],
                serde_json::json!(true),
            );
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document; drain its diagnostics publish ---
    let uri = Uri::from_str("file:///work/selection.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f(x)\n    x + 1\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let _diag = client.receiver.recv().unwrap();

    // --- request selection ranges for two positions ---
    let selection_params = |uri: &Uri, positions: Vec<Position>| {
        serde_json::to_value(SelectionRangeParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            positions,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/selectionRange".to_string(),
            params: selection_params(&uri, vec![Position::new(1, 4), Position::new(0, 9)]),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let chains: Vec<SelectionRange> =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            let innermost: Vec<Range> = chains
                .into_iter()
                .map(|chain| flatten_chain(chain)[0])
                .collect();
            assert_eq!(innermost, vec![sel(1, 4, 1, 5), sel(0, 9, 0, 10)]);
        }
        other => panic!("expected a selectionRange response, got {other:?}"),
    }

    // --- an unknown document answers null ---
    let unknown = Uri::from_str("file:///work/never-opened.jl").unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/selectionRange".to_string(),
            params: selection_params(&unknown, vec![Position::new(0, 0)]),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            assert_eq!(resp.result(), Some(serde_json::Value::Null));
        }
        other => panic!("expected a null response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

// ---------------------------------------------------------------------------
// Semantic tokens
// ---------------------------------------------------------------------------

/// Legend indices as advertised by the server (see `semantic_tokens::legend`).
const KEYWORD: u32 = 0;
const MACRO: u32 = 1;
const STRING: u32 = 2;
const NUMBER: u32 = 3;
const FUNCTION: u32 = 4;

/// Fold the relative encoding back into absolute
/// `(line, character, length, legend index)` tuples.
fn decode(tokens: &SemanticTokens) -> Vec<(u32, u32, u32, u32)> {
    let mut out = Vec::new();
    let (mut line, mut character) = (0, 0);
    for token in &tokens.data {
        if token.delta_line > 0 {
            line += token.delta_line;
            character = 0;
        }
        character += token.delta_start;
        out.push((line, character, token.length, token.token_type));
        assert_eq!(token.token_modifiers_bitset, 0, "no modifiers are emitted");
    }
    out
}

/// The decoded semantic tokens for `text` under UTF-8, with no library (free
/// reads stay unresolved, so only syntax and file-local names paint).
fn toks(text: &str) -> Vec<(u32, u32, u32, u32)> {
    decode(&compute_semantic_tokens(
        text,
        PositionEncoding::Utf8,
        &no_library(),
    ))
}

/// An empty harvested library for the pure-path helpers.
fn no_library() -> std::collections::BTreeMap<String, std::sync::Arc<fatou::index::PackageIndex>> {
    std::collections::BTreeMap::new()
}

#[test]
fn semantic_tokens_paint_keywords_and_bool_literals() {
    // `true`/`false` count as keywords: the standard legend has no boolean
    // type, and it matches the lexer's classification.
    assert_eq!(
        toks("if true\nelse\nend\n"),
        vec![
            (0, 0, 2, KEYWORD),
            (0, 3, 4, KEYWORD),
            (1, 0, 4, KEYWORD),
            (2, 0, 3, KEYWORD),
        ],
    );
}

#[test]
fn semantic_tokens_paint_a_macro_call_as_one_token() {
    // Sigil and name coalesce; the argument stays plain.
    assert_eq!(toks("@show x\n"), vec![(0, 0, 5, MACRO)]);
}

#[test]
fn semantic_tokens_leave_macro_qualifiers_plain() {
    // Trailing sigil: only `@time` paints; with no library harvested the
    // module path cannot resolve, so it stays plain.
    assert_eq!(toks("Base.@time f()\n"), vec![(0, 5, 5, MACRO)]);
    // Leading sigil: the sigil and the final component paint.
    assert_eq!(
        toks("@Base.time x\n"),
        vec![(0, 0, 1, MACRO), (0, 6, 4, MACRO)],
    );
}

#[test]
fn semantic_tokens_paint_a_keyword_named_macro_as_a_macro() {
    assert_eq!(toks("@macro a\n"), vec![(0, 0, 6, MACRO)]);
}

#[test]
fn semantic_tokens_leave_nonstandard_identifiers_plain() {
    // The `var"..."` body is an identifier spelled with quotes, not a
    // string; only the sigil paints.
    assert_eq!(toks("@var\"#\" a\n"), vec![(0, 0, 1, MACRO)]);
}

#[test]
fn semantic_tokens_paint_string_macro_prefix_and_suffix_as_macros() {
    // `r"ab"i` calls `@r_str` with flag `i`: the prefix and suffix are the
    // macro parts, the body is a string.
    assert_eq!(
        toks("r\"ab\"i\n"),
        vec![(0, 0, 1, MACRO), (0, 1, 4, STRING), (0, 5, 1, MACRO)],
    );
}

#[test]
fn semantic_tokens_paint_command_literals_as_strings() {
    assert_eq!(toks("`ls -l`\n"), vec![(0, 0, 7, STRING)]);
}

#[test]
fn semantic_tokens_never_span_line_breaks() {
    // Triple-quoted content splits into one token per line: most clients
    // reject multiline semantic tokens.
    assert_eq!(
        toks("s = \"\"\"\na b\nc\"\"\"\n"),
        vec![(0, 4, 3, STRING), (1, 0, 3, STRING), (2, 0, 4, STRING)],
    );
}

#[test]
fn semantic_tokens_leave_string_interpolation_unpainted() {
    // The interpolation renders as code, so the string paints around it.
    assert_eq!(
        toks("\"a $x b\"\n"),
        vec![(0, 0, 3, STRING), (0, 5, 3, STRING)],
    );
}

#[test]
fn semantic_tokens_paint_number_and_char_literals() {
    assert_eq!(
        toks("0x1f + 0b10 + 0o7 + 1.5 + 2f0 + 42\n"),
        vec![
            (0, 0, 4, NUMBER),
            (0, 7, 4, NUMBER),
            (0, 14, 3, NUMBER),
            (0, 20, 3, NUMBER),
            (0, 26, 3, NUMBER),
            (0, 32, 2, NUMBER),
        ],
    );
    assert_eq!(toks("'a'\n"), vec![(0, 0, 3, STRING)]);
}

#[test]
fn semantic_tokens_respect_the_negotiated_encoding() {
    // `α`/`β` are two UTF-8 bytes but one UTF-16 unit each, changing both
    // the string token's length and every later start on the line.
    let text = "\"αβ\"; if true end\n";
    assert_eq!(
        decode(&compute_semantic_tokens(
            text,
            PositionEncoding::Utf8,
            &no_library()
        )),
        vec![
            (0, 0, 6, STRING),
            (0, 8, 2, KEYWORD),
            (0, 11, 4, KEYWORD),
            (0, 16, 3, KEYWORD),
        ],
    );
    assert_eq!(
        decode(&compute_semantic_tokens(
            text,
            PositionEncoding::Utf16,
            &no_library()
        )),
        vec![
            (0, 0, 4, STRING),
            (0, 6, 2, KEYWORD),
            (0, 9, 4, KEYWORD),
            (0, 14, 3, KEYWORD),
        ],
    );
}

#[test]
fn semantic_tokens_is_best_effort_on_broken_input() {
    // A missing `end` and an unterminated string must not panic.
    let _ = toks("function f(x)\n    if x\n        \"a\n");
}

#[test]
fn serves_semantic_tokens() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    // --- initialize handshake; capability and legend advertised ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let provider = &resp.result().unwrap()["capabilities"]["semanticTokensProvider"];
            assert_eq!(
                provider["legend"]["tokenTypes"],
                serde_json::json!([
                    "keyword",
                    "macro",
                    "string",
                    "number",
                    "function",
                    "type",
                    "namespace"
                ]),
            );
            // Delta advertised so `full/delta` re-pulls can answer an empty
            // edit list for unchanged highlighting.
            assert_eq!(provider["full"], serde_json::json!({ "delta": true }));
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document; drain its diagnostics publish ---
    let uri = Uri::from_str("file:///work/semantic.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "@show f(1)\nf(x) = x\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let _diag = client.receiver.recv().unwrap();

    // --- request the full document's tokens ---
    let semantic_params = |uri: &Uri| {
        serde_json::to_value(SemanticTokensParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .unwrap()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/semanticTokens/full".to_string(),
            params: semantic_params(&uri),
        }))
        .unwrap();
    let result_id = match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let tokens: SemanticTokens = serde_json::from_value(resp.result().unwrap()).unwrap();
            // `@show` paints syntactically; both `f`s classify as function
            // through the semantic model (the call resolves to the file-local
            // definition below it).
            assert_eq!(
                decode(&tokens),
                vec![
                    (0, 0, 5, MACRO),
                    (0, 6, 1, FUNCTION),
                    (0, 8, 1, NUMBER),
                    (1, 0, 1, FUNCTION),
                ],
            );
            // The full response carries a content-derived id for the delta pull.
            tokens.result_id.expect("full tokens carry a resultId")
        }
        other => panic!("expected a semanticTokens response, got {other:?}"),
    };

    // --- a `full/delta` re-pull of the unchanged buffer answers empty edits ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/semanticTokens/full/delta".to_string(),
            params: serde_json::to_value(SemanticTokensDeltaParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                previous_result_id: result_id.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let result: SemanticTokensFullDeltaResult =
                serde_json::from_value(resp.result().unwrap()).unwrap();
            let SemanticTokensFullDeltaResult::TokensDelta(delta) = result else {
                panic!("an unchanged buffer must answer a token delta, got {result:?}");
            };
            assert_eq!(delta.result_id, Some(result_id), "the id is unchanged");
            assert!(
                delta.edits.is_empty(),
                "unchanged highlighting has no edits"
            );
        }
        other => panic!("expected a semanticTokens delta response, got {other:?}"),
    }

    // --- an unknown document answers null ---
    let unknown = Uri::from_str("file:///work/never-opened.jl").unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "textDocument/semanticTokens/full".to_string(),
            params: semantic_params(&unknown),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            assert_eq!(resp.result(), Some(serde_json::Value::Null));
        }
        other => panic!("expected a null response, got {other:?}"),
    }

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(5),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

// --- cross-file references and rename, end to end ---------------------------
//
// Unlike every other test in this file, this one opens a real workspace root so
// the server spawns its library harvester, resolves a temp package under
// development, harvests it, and seeds its member files into the reverse-
// occurrence index — the live path cross-file references and rename escalate
// onto. The harvest runs on a detached thread with no client-visible readiness
// signal (the analysis thread swaps the library in without re-publishing
// diagnostics), so we poll a real `references` request until the index is
// populated (the result spans both member files). See the plan for why a poll,
// not a signal, is the synchronization mechanism.

/// Serialize env-touching setup: `JULIA_*` is process-global and read
/// asynchronously by the detached harvester, so only one such test may run at a
/// time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A unique temp directory removed on drop. Avoids a `tempfile` dev-dependency
/// (mirrors the pattern in `tests/environment.rs`).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{}", std::process::id(), n));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

/// Build a `file:` URI for an absolute temp path. The temp paths here contain
/// only unreserved characters, so no percent-encoding is needed and the URI
/// round-trips to the exact path the server tracks. Windows drive-rooted paths
/// (`C:\...`) need a leading slash and forward slashes.
fn file_uri(path: &Path) -> Uri {
    let text = path.to_str().unwrap().replace('\\', "/");
    let slash = if text.starts_with('/') { "" } else { "/" };
    Uri::from_str(&format!("file://{slash}{text}")).unwrap()
}

/// Set env vars for the duration of a test, restoring their prior values on
/// drop. `set_var`/`remove_var` are `unsafe` in edition 2024; safe here because
/// this runs under `ENV_LOCK` and before the harvester thread (the sole reader
/// of these vars) is spawned, so there is no concurrent read.
struct EnvGuard {
    prev: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&str, &str)]) -> Self {
        let mut prev = Vec::new();
        for (key, value) in vars {
            prev.push(((*key).to_string(), std::env::var(key).ok()));
            unsafe { std::env::set_var(key, value) };
        }
        Self { prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in &self.prev {
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

/// The two-message initialize handshake with a workspace `root_uri` set (the
/// existing tests inline `InitializeParams::default()`, which opens no folder).
fn initialize_with_root(client: &Connection, root_uri: &Uri) {
    #[allow(deprecated)]
    let params = InitializeParams {
        root_uri: Some(root_uri.clone()),
        ..Default::default()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(params).unwrap(),
        }))
        .unwrap();
    let init = client.receiver.recv().unwrap();
    assert!(
        matches!(init, Message::Response(_)),
        "expected an InitializeResult, got {init:?}"
    );
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();
}

/// Like [`initialize_with_root`], but advertising `window.workDoneProgress` so
/// the server reports its harvest via `$/progress`.
fn initialize_with_work_done_progress(client: &Connection, root_uri: &Uri) {
    #[allow(deprecated)]
    let params = InitializeParams {
        root_uri: Some(root_uri.clone()),
        capabilities: ClientCapabilities {
            window: Some(WindowClientCapabilities {
                work_done_progress: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(params).unwrap(),
        }))
        .unwrap();
    let init = client.receiver.recv().unwrap();
    assert!(
        matches!(init, Message::Response(_)),
        "expected an InitializeResult, got {init:?}"
    );
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();
}

/// The two-message initialize handshake with several workspace folders open.
/// Returns the raw `InitializeResult` so callers can assert on the advertised
/// capabilities.
fn initialize_with_folders(client: &Connection, roots: &[&Uri]) -> serde_json::Value {
    let params = InitializeParams {
        workspace_folders: Some(
            roots
                .iter()
                .map(|uri| WorkspaceFolder {
                    uri: (*uri).clone(),
                    name: String::new(),
                })
                .collect(),
        ),
        ..Default::default()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(params).unwrap(),
        }))
        .unwrap();
    let result = match client.receiver.recv().unwrap() {
        Message::Response(resp) => resp.result().expect("an InitializeResult"),
        other => panic!("expected an InitializeResult, got {other:?}"),
    };
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();
    result
}

/// Receive messages until the response with `id` arrives, skipping unrelated
/// notifications and stale responses.
fn recv_response(client: &Connection, id: RequestId) -> lsp_server::Response {
    loop {
        match client.receiver.recv().unwrap() {
            Message::Response(resp) if resp.id == id => return resp,
            Message::Response(_) | Message::Notification(_) => continue,
            other => panic!("unexpected message: {other:?}"),
        }
    }
}

/// Resend `textDocument/references` at `greet`'s definition (position 0,0 of
/// `a.jl`) until the response spans exactly `spanning` files — i.e. a harvest
/// and member seeding have landed and settled on that membership — or the
/// deadline elapses. Before a harvest, the intra-file fallback returns only
/// the `a.jl` sites (one file); each (re-)harvest grows or shrinks the span.
fn poll_references_spanning(
    client: &Connection,
    uri: &Uri,
    spanning: usize,
    deadline: Duration,
) -> Vec<Location> {
    static POLL_ID: AtomicU64 = AtomicU64::new(100);
    let start = Instant::now();
    loop {
        let id = i32::try_from(POLL_ID.fetch_add(1, Ordering::Relaxed)).unwrap();
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: "textDocument/references".to_string(),
                params: serde_json::to_value(ReferenceParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position: Position::new(0, 0),
                    },
                    context: ReferenceContext {
                        include_declaration: true,
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                })
                .unwrap(),
            }))
            .unwrap();
        let resp = recv_response(client, RequestId::from(id));
        let locations: Vec<Location> = serde_json::from_value(resp.result().unwrap()).unwrap();
        let files: std::collections::HashSet<&str> =
            locations.iter().map(|l| l.uri.as_str()).collect();
        if files.len() == spanning {
            return locations;
        }
        if start.elapsed() >= deadline {
            panic!(
                "references never spanned {spanning} file(s) within {deadline:?}: got \
                 {} location(s) across {} file(s) — the harvest or member seeding \
                 likely failed",
                locations.len(),
                files.len()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Resend `workspace/willRenameFiles` until it answers with edits — before the
/// harvest lands, no file is a seeded member and the honest answer is `null` —
/// and while it is landing, only the members seeded so far can contribute, so
/// this waits for the edit to span exactly `spanning` files rather than taking
/// the first non-null answer.
fn poll_will_rename(
    client: &Connection,
    files: Vec<FileRename>,
    spanning: usize,
    deadline: Duration,
) -> WorkspaceEdit {
    static POLL_ID: AtomicU64 = AtomicU64::new(400);
    let start = Instant::now();
    loop {
        let id = i32::try_from(POLL_ID.fetch_add(1, Ordering::Relaxed)).unwrap();
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: "workspace/willRenameFiles".to_string(),
                params: serde_json::to_value(RenameFilesParams {
                    files: files.clone(),
                })
                .unwrap(),
            }))
            .unwrap();
        let value = recv_response(client, RequestId::from(id)).result().unwrap();
        let edit: Option<WorkspaceEdit> = serde_json::from_value(value).unwrap();
        let covered = edit
            .as_ref()
            .and_then(|edit| edit.changes.as_ref())
            .map_or(0, |changes| changes.len());
        if covered == spanning {
            return edit.expect("a non-empty edit spans at least one file");
        }
        assert!(
            start.elapsed() < deadline,
            "willRenameFiles never spanned {spanning} file(s) within {deadline:?}: \
             got {covered} — the harvest or member seeding likely failed"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The replacement texts `edit` carries for `path`, in position order.
fn new_texts(edit: &WorkspaceEdit, path: &Path) -> Vec<String> {
    let uri = file_uri(path);
    edit.changes
        .as_ref()
        .expect("changes")
        .get(&uri)
        .map(|edits| edits.iter().map(|e| e.new_text.clone()).collect())
        .unwrap_or_default()
}

fn file_rename(old: &Path, new: &Path) -> FileRename {
    FileRename {
        old_uri: file_uri(old).as_str().to_string(),
        new_uri: file_uri(new).as_str().to_string(),
    }
}

/// The rename file operations reach the wire, so an explorer move actually
/// asks the server for edits. Both filters must survive serialization: a client
/// only sends folder renames if the `folder` pattern kind is advertised.
#[test]
fn advertises_rename_file_operations() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let operations =
                resp.result().unwrap()["capabilities"]["workspace"]["fileOperations"].clone();
            for kind in ["willRename", "didRename"] {
                let kinds: Vec<serde_json::Value> = operations[kind]["filters"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{kind} filters"))
                    .iter()
                    .map(|filter| filter["pattern"]["matches"].clone())
                    .collect();
                assert_eq!(
                    kinds,
                    [serde_json::json!("file"), serde_json::json!("folder")],
                    "{kind}"
                );
            }
        }
        other => panic!("expected an InitializeResult, got {other:?}"),
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(2));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Moving a member file, then a whole folder, rewrites the `include` literals
/// that name what moved *and* rebases the moved files' own relative includes.
/// One server for both phases: the `JULIA_*` env is process-global, so a second
/// env-setting test would race this one's detached harvester.
#[test]
fn serves_will_rename_files() {
    let _env = ENV_LOCK.lock().unwrap();

    let pkg = TempDir::new("fatou-lsp-will-rename");
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ninclude(\"a.jl\")\ninclude(\"sub/c.jl\")\nend\n",
    );
    write_file(&pkg.path.join("src/a.jl"), "greet(a) = a\n");
    // `c.jl` spells its own include with an escape (`\x2f` is `/`): the rewrite
    // resolves the literal's *decoded* path, so an escaped literal is rewritten
    // like any other rather than skipped.
    write_file(&pkg.path.join("src/sub/c.jl"), "include(\"..\\x2fa.jl\")\n");

    let depot = TempDir::new("fatou-lsp-will-rename-depot");
    let _guard = isolate_env(&pkg, &depot);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_root(&client, &file_uri(&pkg.path));

    let entry = pkg.path.join("src/MyPkg.jl");
    let nested = pkg.path.join("src/sub/c.jl");

    // Renaming `src/a.jl` re-spells it for both of its includers, each against
    // its own directory.
    let edit = poll_will_rename(
        &client,
        vec![file_rename(
            &pkg.path.join("src/a.jl"),
            &pkg.path.join("src/moved.jl"),
        )],
        2,
        Duration::from_secs(10),
    );
    assert_eq!(new_texts(&edit, &entry), ["moved.jl"]);
    assert_eq!(new_texts(&edit, &nested), ["../moved.jl"]);

    // Renaming the `src/sub` *folder* moves `c.jl` with it: its includer's
    // literal grows a segment, and its own include walks up one more level.
    let edit = poll_will_rename(
        &client,
        vec![file_rename(
            &pkg.path.join("src/sub"),
            &pkg.path.join("src/deep/sub"),
        )],
        2,
        Duration::from_secs(10),
    );
    assert_eq!(new_texts(&edit, &entry), ["deep/sub/c.jl"]);
    assert_eq!(new_texts(&edit, &nested), ["../../a.jl"]);

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(90),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(90));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// A single test covers both features against one initialized, harvested server:
/// the `JULIA_*` env is process-global and read asynchronously by the detached
/// harvester, so two parallel env-setting tests would race. One env setup, one
/// harvest, both assertions once the index is warm.
#[test]
fn serves_cross_file_references_and_rename() {
    let _env = ENV_LOCK.lock().unwrap();

    // A real package under development: a named `Project.toml` with a matching
    // `src/MyPkg.jl`, `greet` defined in `a.jl` and called in `b.jl` (the same
    // shape as the db-level `cross_file` tests, driven end-to-end here).
    let pkg = TempDir::new("fatou-lsp-xfile");
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write_file(&pkg.path.join("src/a.jl"), "greet(a) = a\ngreet(1)\n");
    write_file(&pkg.path.join("src/b.jl"), "callit() = greet(2)\n");

    // Isolate the environment so the harvest is fast and hermetic:
    // - `JULIA_PROJECT` points resolution at this package (it is consulted
    //   before the workspace-root walk-up, so a stray dev-shell value would
    //   otherwise hijack it);
    // - an empty `JULIA_DEPOT_PATH` and empty `JULIA_BINDIR` skip install
    //   discovery via juliaup and the bindir override;
    // - an empty `PATH` stops the last install-discovery probe (`julia` on
    //   `PATH`), so `locate_install` returns `None` and the harvester uses the
    //   embedded minimal-Base fallback instead of parsing all of Base (~1ms vs
    //   tens of seconds). The cross-file symbols are workspace-local, so the
    //   real Base index is not needed here.
    let depot = TempDir::new("fatou-lsp-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    initialize_with_root(&client, &root_uri);

    // Open `a.jl` (the cursor file must be open; its member siblings are served
    // from disk-seeded text).
    let a_uri = file_uri(&pkg.path.join("src/a.jl"));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: a_uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "greet(a) = a\ngreet(1)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Notification(n) => assert_eq!(n.method, "textDocument/publishDiagnostics"),
        other => panic!("expected publishDiagnostics, got {other:?}"),
    }

    // References at `greet`'s definition, once the harvest lands: def + call in
    // `a.jl`, call in `b.jl`.
    let locations = poll_references_spanning(&client, &a_uri, 2, Duration::from_secs(10));
    assert_eq!(locations.len(), 3, "def + call in a.jl, call in b.jl");
    let a_count = locations
        .iter()
        .filter(|l| l.uri.as_str().ends_with("a.jl"))
        .count();
    let b_count = locations
        .iter()
        .filter(|l| l.uri.as_str().ends_with("b.jl"))
        .count();
    assert_eq!((a_count, b_count), (2, 1), "two sites in a.jl, one in b.jl");

    // prepareRename reports the `greet` identifier's range.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(200),
            method: "textDocument/prepareRename".to_string(),
            params: serde_json::to_value(TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: a_uri.clone() },
                position: Position::new(0, 0),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(200));
    let range: Range = serde_json::from_value(resp.result().unwrap()).unwrap();
    assert_eq!(range.start, Position::new(0, 0));
    assert_eq!(range.end, Position::new(0, 5));

    // Rename `greet` -> `hello` across both member files.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(201),
            method: "textDocument/rename".to_string(),
            params: serde_json::to_value(RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: a_uri.clone() },
                    position: Position::new(0, 0),
                },
                new_name: "hello".to_string(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(201));
    let edit: WorkspaceEdit = serde_json::from_value(resp.result().unwrap()).unwrap();
    #[allow(clippy::mutable_key_type)]
    let changes = edit.changes.expect("multi-file changes");
    for edits in changes.values() {
        for e in edits {
            assert_eq!(e.new_text, "hello");
        }
    }
    let a_edits = changes
        .iter()
        .find(|(u, _)| u.as_str().ends_with("a.jl"))
        .map(|(_, e)| e.len());
    let b_edits = changes
        .iter()
        .find(|(u, _)| u.as_str().ends_with("b.jl"))
        .map(|(_, e)| e.len());
    assert_eq!(a_edits, Some(2), "a.jl: def + call rewritten");
    assert_eq!(b_edits, Some(1), "b.jl: call rewritten");

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(202),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(202));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Write a minimal harvestable package (`Project.toml` + `src/MyPkg.jl`) into a
/// fresh temp dir and return it. Shared by the progress tests.
fn write_harvestable_pkg(prefix: &str) -> TempDir {
    let pkg = TempDir::new(prefix);
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ngreet(a) = a\nend\n",
    );
    pkg
}

/// The env isolation from `serves_cross_file_references_and_rename`: resolve
/// against `pkg` with an empty depot and `PATH` so the harvest uses the embedded
/// minimal-Base fallback (fast and hermetic) instead of parsing all of Base.
fn isolate_env(pkg: &TempDir, depot: &TempDir) -> EnvGuard {
    EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
    ])
}

/// A client advertising `window.workDoneProgress` gets a create request plus a
/// `$/progress` begin/report/end cycle around the harvest, all sharing one
/// token — the harvester's indexing pass surfaced to the editor.
#[test]
fn reports_harvest_progress_when_the_client_supports_it() {
    let _env = ENV_LOCK.lock().unwrap();
    let pkg = write_harvestable_pkg("fatou-lsp-progress");
    let depot = TempDir::new("fatou-lsp-progress-depot");
    let _guard = isolate_env(&pkg, &depot);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    initialize_with_work_done_progress(&client, &root_uri);

    // Collect the create request and the $/progress cycle. The harvester fires
    // these right after the handshake; drain until `end` or the deadline.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut create_request_token = None;
    let mut kinds: Vec<&'static str> = Vec::new();
    let mut progress_tokens = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("harvest progress never completed within the deadline");
        match client.receiver.recv_timeout(remaining).unwrap() {
            Message::Request(req) if req.method == "window/workDoneProgress/create" => {
                let params: lsp_types::WorkDoneProgressCreateParams =
                    serde_json::from_value(req.params).unwrap();
                create_request_token = Some(params.token);
            }
            Message::Notification(n) if n.method == "$/progress" => {
                let params: ProgressParams = serde_json::from_value(n.params).unwrap();
                progress_tokens.push(params.token);
                let ProgressParamsValue::WorkDone(value) = params.value;
                match value {
                    WorkDoneProgress::Begin(_) => kinds.push("begin"),
                    WorkDoneProgress::Report(_) => kinds.push("report"),
                    WorkDoneProgress::End(_) => {
                        kinds.push("end");
                        break;
                    }
                }
            }
            // publishDiagnostics and other notifications are irrelevant here.
            Message::Notification(_) | Message::Response(_) | Message::Request(_) => {}
        }
    }

    assert_eq!(
        kinds,
        vec!["begin", "report", "end"],
        "a full begin/report/end cycle in order"
    );
    let create_request_token = create_request_token.expect("a create request preceded $/progress");
    assert!(
        progress_tokens.iter().all(|t| *t == create_request_token),
        "every $/progress carries the created token"
    );

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(900),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(900));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// A client that does not advertise `window.workDoneProgress` gets no create
/// request and no `$/progress` — even though the harvest runs (proven by
/// `greet` resolving across the package once the index lands).
#[test]
fn suppresses_harvest_progress_without_client_support() {
    let _env = ENV_LOCK.lock().unwrap();
    let pkg = write_harvestable_pkg("fatou-lsp-noprogress");
    let depot = TempDir::new("fatou-lsp-noprogress-depot");
    let _guard = isolate_env(&pkg, &depot);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    initialize_with_root(&client, &root_uri); // default caps: no progress support

    // Open the source so document-symbol requests have something to answer, and
    // so a harvest-completion probe has a live buffer.
    let src_uri = file_uri(&pkg.path.join("src/MyPkg.jl"));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: src_uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "module MyPkg\ngreet(a) = a\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();

    // Drive to harvest completion by polling workspace/symbol for `greet` (empty
    // until the index lands), asserting no progress message slips through. Any
    // server-initiated request would be a leak: with these caps the server has
    // no reason to send one.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut poll_id = 400;
    loop {
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(poll_id),
                method: "workspace/symbol".to_string(),
                params: serde_json::to_value(WorkspaceSymbolParams {
                    query: "greet".to_string(),
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                })
                .unwrap(),
            }))
            .unwrap();
        let symbols = loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("harvest never landed within the deadline");
            match client.receiver.recv_timeout(remaining).unwrap() {
                Message::Notification(n) => assert_ne!(
                    n.method, "$/progress",
                    "server must not send $/progress without client support"
                ),
                Message::Request(req) => {
                    panic!("unexpected server-initiated request: {}", req.method)
                }
                Message::Response(resp) if resp.id == RequestId::from(poll_id) => {
                    let response: Option<WorkspaceSymbolResponse> =
                        serde_json::from_value(resp.result().unwrap_or(serde_json::Value::Null))
                            .unwrap();
                    break match response {
                        Some(WorkspaceSymbolResponse::Flat(items)) => items.len(),
                        Some(WorkspaceSymbolResponse::Nested(items)) => items.len(),
                        None => 0,
                    };
                }
                Message::Response(_) => {}
            }
        };
        if symbols > 0 {
            break; // The harvest landed; no progress was seen along the way.
        }
        poll_id += 1;
        std::thread::sleep(Duration::from_millis(25));
    }

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(901),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(901));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Call hierarchy end-to-end over a harvested workspace package (the
/// `serves_cross_file_references_and_rename` scaffolding): prepare on a
/// cross-file call resolves the item into the defining file, incoming calls
/// reach both a closed sibling member and a synthesized top-level caller, and
/// outgoing calls on a caller jump back — exercising the document-less
/// incoming/outgoing path against a file with no open buffer.
#[test]
fn serves_call_hierarchy() {
    let _env = ENV_LOCK.lock().unwrap();

    // The same hermetic package + env isolation as
    // `serves_cross_file_references_and_rename`.
    let pkg = TempDir::new("fatou-lsp-callhier");
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write_file(&pkg.path.join("src/a.jl"), "greet(a) = a\ngreet(1)\n");
    write_file(&pkg.path.join("src/b.jl"), "callit() = greet(2)\n");

    let depot = TempDir::new("fatou-lsp-callhier-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    let init = initialize_with_folders(&client, &[&root_uri]);
    assert_eq!(
        init["capabilities"]["callHierarchyProvider"],
        serde_json::json!(true),
        "the capability must be advertised"
    );

    // Open `b.jl` only: `a.jl` stays a closed, disk-seeded member.
    let b_uri = file_uri(&pkg.path.join("src/b.jl"));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: b_uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "callit() = greet(2)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Notification(n) => assert_eq!(n.method, "textDocument/publishDiagnostics"),
        other => panic!("expected publishDiagnostics, got {other:?}"),
    }

    // Poll prepare at the `greet` call in b.jl until the harvest lands: before
    // it, `greet` is an unresolvable free read and prepare returns null; after,
    // the item points into the defining file a.jl.
    static POLL_ID: AtomicU64 = AtomicU64::new(300);
    let deadline = Duration::from_secs(10);
    let start = Instant::now();
    let item: CallHierarchyItem = loop {
        let id = i32::try_from(POLL_ID.fetch_add(1, Ordering::Relaxed)).unwrap();
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: "textDocument/prepareCallHierarchy".to_string(),
                params: serde_json::to_value(CallHierarchyPrepareParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: b_uri.clone() },
                        position: Position::new(0, 11),
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                })
                .unwrap(),
            }))
            .unwrap();
        let resp = recv_response(&client, RequestId::from(id));
        let items: Option<Vec<CallHierarchyItem>> =
            serde_json::from_value(resp.result().unwrap()).unwrap();
        if let Some(items) = items
            && let Some(item) = items.into_iter().next()
            && item.uri.as_str().ends_with("a.jl")
        {
            break item;
        }
        if start.elapsed() >= deadline {
            panic!("prepare never resolved `greet` into a.jl within {deadline:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(item.name, "greet");
    assert_eq!(item.kind, SymbolKind::FUNCTION);
    assert_eq!(item.selection_range.start, Position::new(0, 0));

    // Incoming calls to `greet`: the top-level `greet(1)` in the *closed* a.jl
    // (synthesized file caller) and `callit` in b.jl, in file order.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(400),
            method: "callHierarchy/incomingCalls".to_string(),
            params: serde_json::to_value(CallHierarchyIncomingCallsParams {
                item: item.clone(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(400));
    let incoming: Vec<CallHierarchyIncomingCall> =
        serde_json::from_value(resp.result().unwrap()).unwrap();
    assert_eq!(incoming.len(), 2, "{incoming:#?}");
    assert_eq!(incoming[0].from.kind, SymbolKind::FILE);
    assert_eq!(incoming[0].from.name, "a.jl");
    assert_eq!(
        incoming[0].from_ranges,
        vec![Range::new(Position::new(1, 0), Position::new(1, 5))]
    );
    assert_eq!(incoming[1].from.name, "callit");
    assert!(incoming[1].from.uri.as_str().ends_with("b.jl"));
    assert_eq!(incoming[1].from_ranges[0].start, Position::new(0, 11));

    // Outgoing calls from the `callit` caller: back to `greet` in a.jl, the
    // call site reported in callit's own document.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(401),
            method: "callHierarchy/outgoingCalls".to_string(),
            params: serde_json::to_value(CallHierarchyOutgoingCallsParams {
                item: incoming[1].from.clone(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(401));
    let outgoing: Vec<CallHierarchyOutgoingCall> =
        serde_json::from_value(resp.result().unwrap()).unwrap();
    assert_eq!(outgoing.len(), 1, "{outgoing:#?}");
    assert_eq!(outgoing[0].to.name, "greet");
    assert!(outgoing[0].to.uri.as_str().ends_with("a.jl"));
    assert_eq!(outgoing[0].from_ranges[0].start, Position::new(0, 11));

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(402),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(402));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Type hierarchy end-to-end over a harvested workspace package (the
/// `serves_call_hierarchy` scaffolding): the capability rides the serialized
/// JSON (lsp-types 0.97 has no struct field for it), prepare on a cross-file
/// supertype use resolves the item into the defining file, subtypes reach the
/// declaration in a sibling member, and supertypes on that subtype jump back —
/// exercising the document-less supertypes/subtypes path against a file with
/// no open buffer.
#[test]
fn serves_type_hierarchy() {
    let _env = ENV_LOCK.lock().unwrap();

    let pkg = TempDir::new("fatou-lsp-typehier");
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000003\"\n",
    );
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write_file(&pkg.path.join("src/a.jl"), "abstract type Animal end\n");
    write_file(
        &pkg.path.join("src/b.jl"),
        "struct Dog <: Animal\n    name\nend\n",
    );

    let depot = TempDir::new("fatou-lsp-typehier-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    let init = initialize_with_folders(&client, &[&root_uri]);
    assert_eq!(
        init["capabilities"]["typeHierarchyProvider"],
        serde_json::json!(true),
        "the capability must be advertised (it is injected into the JSON)"
    );

    // Open `b.jl` only: `a.jl` stays a closed, disk-seeded member.
    let b_uri = file_uri(&pkg.path.join("src/b.jl"));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: b_uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "struct Dog <: Animal\n    name\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Notification(n) => assert_eq!(n.method, "textDocument/publishDiagnostics"),
        other => panic!("expected publishDiagnostics, got {other:?}"),
    }

    // Poll prepare at the `Animal` supertype use in b.jl until the harvest
    // lands: before it, `Animal` is an unresolvable free read and prepare
    // returns null; after, the item points into the defining file a.jl.
    static POLL_ID: AtomicU64 = AtomicU64::new(500);
    let deadline = Duration::from_secs(10);
    let start = Instant::now();
    let animal: TypeHierarchyItem = loop {
        let id = i32::try_from(POLL_ID.fetch_add(1, Ordering::Relaxed)).unwrap();
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: "textDocument/prepareTypeHierarchy".to_string(),
                params: serde_json::to_value(TypeHierarchyPrepareParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: b_uri.clone() },
                        position: Position::new(0, 14),
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                })
                .unwrap(),
            }))
            .unwrap();
        let resp = recv_response(&client, RequestId::from(id));
        let items: Option<Vec<TypeHierarchyItem>> =
            serde_json::from_value(resp.result().unwrap()).unwrap();
        if let Some(items) = items
            && let Some(item) = items.into_iter().next()
            && item.uri.as_str().ends_with("a.jl")
        {
            break item;
        }
        if start.elapsed() >= deadline {
            panic!("prepare never resolved `Animal` into a.jl within {deadline:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(animal.name, "Animal");
    assert_eq!(animal.kind, SymbolKind::INTERFACE);
    assert_eq!(animal.selection_range.start, Position::new(0, 14));

    // Subtypes of `Animal`: the `Dog` declaration in b.jl.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(600),
            method: "typeHierarchy/subtypes".to_string(),
            params: serde_json::to_value(TypeHierarchySubtypesParams {
                item: animal.clone(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(600));
    let subtypes: Vec<TypeHierarchyItem> = serde_json::from_value(resp.result().unwrap()).unwrap();
    assert_eq!(subtypes.len(), 1, "{subtypes:#?}");
    assert_eq!(subtypes[0].name, "Dog");
    assert!(subtypes[0].uri.as_str().ends_with("b.jl"));
    assert_eq!(subtypes[0].kind, SymbolKind::STRUCT);
    assert_eq!(subtypes[0].detail.as_deref(), Some("<: Animal"));
    assert_eq!(subtypes[0].selection_range.start, Position::new(0, 7));

    // Supertypes of that `Dog` item: back to `Animal` in the *closed* a.jl.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(601),
            method: "typeHierarchy/supertypes".to_string(),
            params: serde_json::to_value(TypeHierarchySupertypesParams {
                item: subtypes[0].clone(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(601));
    let supertypes: Vec<TypeHierarchyItem> =
        serde_json::from_value(resp.result().unwrap()).unwrap();
    assert_eq!(supertypes.len(), 1, "{supertypes:#?}");
    assert_eq!(supertypes[0].name, "Animal");
    assert!(supertypes[0].uri.as_str().ends_with("a.jl"));
    assert_eq!(supertypes[0].selection_range.start, Position::new(0, 14));

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(602),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(602));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Cross-file references across a *nested* `module`, driven end-to-end. `greet`
/// lives in `MyPkg.Sub`, split across two included files; the whole harvest →
/// host-module → reverse-index pipeline must attribute both sites to `Sub` and
/// stitch them, exactly as it does for a root-module symbol. Guards the
/// nested-`module` file-membership behavior at the server level.
#[test]
fn serves_cross_file_references_in_a_nested_module() {
    let _env = ENV_LOCK.lock().unwrap();

    let pkg = TempDir::new("fatou-lsp-nested");
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000002\"\n",
    );
    // The `Sub` module wrapper is in the parent; `a.jl`/`b.jl` splice into it, so
    // both files' host module is `Sub`.
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\nmodule Sub\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\nend\n",
    );
    write_file(&pkg.path.join("src/a.jl"), "greet(a) = a\ngreet(1)\n");
    write_file(&pkg.path.join("src/b.jl"), "callit() = greet(2)\n");

    let depot = TempDir::new("fatou-lsp-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    initialize_with_root(&client, &root_uri);

    let a_uri = file_uri(&pkg.path.join("src/a.jl"));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: a_uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "greet(a) = a\ngreet(1)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Notification(n) => assert_eq!(n.method, "textDocument/publishDiagnostics"),
        other => panic!("expected publishDiagnostics, got {other:?}"),
    }

    let locations = poll_references_spanning(&client, &a_uri, 2, Duration::from_secs(10));
    assert_eq!(
        locations.len(),
        3,
        "def + call in a.jl, call in b.jl — all attributed to Sub"
    );
    let a_count = locations
        .iter()
        .filter(|l| l.uri.as_str().ends_with("a.jl"))
        .count();
    let b_count = locations
        .iter()
        .filter(|l| l.uri.as_str().ends_with("b.jl"))
        .count();
    assert_eq!((a_count, b_count), (2, 1), "two sites in a.jl, one in b.jl");

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(210),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(210));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Drain server notifications until a `publishDiagnostics` for a URI ending in
/// `uri_suffix` carries at least one diagnostic, returning them. Panics on
/// timeout — the harvest or graph-diagnostics publish never landed.
fn poll_publish_diagnostics(
    client: &Connection,
    uri_suffix: &str,
    deadline: Duration,
) -> Vec<Diagnostic> {
    poll_publish_where(client, uri_suffix, deadline, |diags| !diags.is_empty())
}

/// [`poll_publish_diagnostics`], but for any predicate over the published set —
/// notably "empty", which is how a cleared file announces itself and which the
/// non-empty default cannot express.
fn poll_publish_where(
    client: &Connection,
    uri_suffix: &str,
    deadline: Duration,
    accept: impl Fn(&[Diagnostic]) -> bool,
) -> Vec<Diagnostic> {
    let start = Instant::now();
    loop {
        let remaining = deadline
            .checked_sub(start.elapsed())
            .filter(|d| !d.is_zero())
            .unwrap_or_else(|| panic!("no diagnostics for {uri_suffix} within {deadline:?}"));
        match client.receiver.recv_timeout(remaining) {
            Ok(Message::Notification(n)) if n.method == "textDocument/publishDiagnostics" => {
                let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
                if params.uri.as_str().ends_with(uri_suffix) && accept(&params.diagnostics) {
                    return params.diagnostics;
                }
            }
            Ok(_) => {}
            Err(_) => panic!("no diagnostics for {uri_suffix} within {deadline:?}"),
        }
    }
}

/// A static `include("missing.jl")` to a nonexistent file surfaces as an
/// include-graph diagnostic on the entry file, published end-to-end after the
/// harvest lands. Guards the whole `project_graph` -> graph-diagnostics ->
/// `publishDiagnostics` pipeline at the server level.
#[test]
fn publishes_unresolved_include_diagnostic() {
    let _env = ENV_LOCK.lock().unwrap();

    let pkg = TempDir::new("fatou-lsp-badinclude");
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    // The entry includes an existing `a.jl` and a nonexistent `missing.jl`.
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ninclude(\"a.jl\")\ninclude(\"missing.jl\")\nend\n",
    );
    write_file(&pkg.path.join("src/a.jl"), "f() = 1\n");

    let depot = TempDir::new("fatou-lsp-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    initialize_with_root(&client, &root_uri);

    // The diagnostic attaches to the entry file that holds the bad `include`.
    let diags = poll_publish_diagnostics(&client, "src/MyPkg.jl", Duration::from_secs(10));
    let unresolved: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("cannot resolve include"))
        .collect();
    assert_eq!(unresolved.len(), 1, "one unresolved include is reported");
    let diag = unresolved[0];
    assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
    assert!(
        diag.message.contains("missing.jl"),
        "names the missing file"
    );
    // The `include("missing.jl")` call is on the third line (0-based line 2).
    assert_eq!(diag.range.start.line, 2);

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(300),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(300));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// A `Project.toml` that does not parse reports on itself, and the report
/// clears once the file is fixed. Guards the whole environment-diagnostics
/// pipeline at the server level: the resolve failure the harvester used to
/// swallow, `Outbound::EnvironmentDiagnostics`, the three-way merge in
/// `publish_merged`, and the clear-on-fix set.
///
/// The file is never opened — these attach to a file on disk, and the push is
/// their only route, since a pull report is served only for an open document.
#[test]
fn publishes_and_clears_project_file_syntax_diagnostics() {
    let _env = ENV_LOCK.lock().unwrap();

    let pkg = TempDir::new("fatou-lsp-badproject");
    // `uuid` with no value: valid TOML up to the newline, then a syntax error.
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \n",
    );
    write_file(&pkg.path.join("src/MyPkg.jl"), "module MyPkg\nend\n");

    let depot = TempDir::new("fatou-lsp-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
        ("HOME", depot.path.to_str().unwrap()),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    initialize_with_root(&client, &root_uri);

    let diags = poll_publish_diagnostics(&client, "Project.toml", Duration::from_secs(10));
    let [diag] = &diags[..] else {
        panic!("expected exactly one diagnostic, got {diags:?}");
    };
    assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(
        diag.code,
        Some(NumberOrString::String("toml-syntax".to_string()))
    );
    assert_eq!(diag.source.as_deref(), Some("fatou"));
    // The `uuid = ` line, second in the file.
    assert_eq!(diag.range.start.line, 1);

    // The syntax error fixed, but the package still declares no Julia support
    // range: the environment now resolves, so the *semantic* checks take over
    // on the same file. This is the harvester's success branch, which the
    // failure branch above does not reach.
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    did_change_watched_files(
        &client,
        vec![FileEvent::new(
            file_uri(&pkg.path.join("Project.toml")),
            FileChangeType::CHANGED,
        )],
    );

    let diags = poll_publish_where(&client, "Project.toml", Duration::from_secs(10), |diags| {
        diags
            .iter()
            .any(|d| d.code == Some(NumberOrString::String("missing-julia-compat".to_string())))
    });
    let compat = diags
        .iter()
        .find(|d| d.code == Some(NumberOrString::String("missing-julia-compat".to_string())))
        .expect("polled for it");
    assert_eq!(compat.severity, Some(DiagnosticSeverity::WARNING));
    assert!(
        !diags
            .iter()
            .any(|d| d.code == Some(NumberOrString::String("toml-syntax".to_string()))),
        "the syntax finding is gone: {diags:?}"
    );

    // Both satisfied: the file clears — exactly once, which the unit test in
    // `server.rs` pins separately.
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n\n\
         [compat]\njulia = \"1.10\"\n",
    );
    did_change_watched_files(
        &client,
        vec![FileEvent::new(
            file_uri(&pkg.path.join("Project.toml")),
            FileChangeType::CHANGED,
        )],
    );

    let cleared = poll_publish_where(
        &client,
        "Project.toml",
        Duration::from_secs(10),
        <[_]>::is_empty,
    );
    assert!(cleared.is_empty());

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(310),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(310));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// An open `Project.toml` reports its own TOML errors from the *buffer*, at
/// edit cadence: the error appears on the keystroke that introduces it and
/// clears on the one that fixes it, with no save, no watched-file event, and —
/// no workspace folder here — no harvester at all. The second half is the
/// other side of making it a real document: a language feature must answer
/// nothing for it, rather than parsing TOML as Julia.
#[test]
fn publishes_project_file_diagnostics_from_the_open_buffer() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(1));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let project = native_file_uri("/work/Project.toml");
    open_document(&client, &project, "name = \"MyPkg\"\n");
    // Broken by an edit that is never saved.
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: project.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "name = \"MyPkg\"\nuuid = \n".to_string(),
                }],
            })
            .unwrap(),
        }))
        .unwrap();

    let published = recv_publish_for(&client, &project);
    let [diag] = &published.diagnostics[..] else {
        panic!("expected one diagnostic, got {published:?}");
    };
    assert_eq!(
        diag.code,
        Some(NumberOrString::String("toml-syntax".to_string()))
    );
    assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(diag.range.start.line, 1, "the `uuid = ` line");
    assert_eq!(published.version, Some(2));

    // A language feature answers nothing for it: a formatting request would
    // otherwise reformat TOML as Julia.
    assert_eq!(format_document(&client, &project, 2), None);

    // Fixed, still unsaved: the file clears.
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: project.clone(),
                    version: 3,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n"
                        .to_string(),
                }],
            })
            .unwrap(),
        }))
        .unwrap();
    let cleared = recv_publish_for(&client, &project);
    assert!(cleared.diagnostics.is_empty(), "{cleared:?}");

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(3));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// The prize of making the project file a live input: adding a name to
/// `[deps]` in an editor buffer clears `unresolved-import` across the package
/// with **no save and no watched-file event**. Nothing here writes the project
/// file back to disk — only `didOpen`/`didChange` carry the new text — so a
/// pass proves the buffer, not the disk copy, is what the linter reads.
///
/// It doubles as the end-to-end check that a push client is refreshed at all:
/// the finding itself only appears once the harvest lands, which is a moment
/// no edit of the Julia buffer marks.
#[test]
fn an_unsaved_project_file_edit_clears_unresolved_import() {
    let _env = ENV_LOCK.lock().unwrap();

    let pkg = TempDir::new("fatou-lsp-livedeps");
    let project = pkg.path.join("Project.toml");
    let on_disk = "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n\n[compat]\njulia = \"1.10\"\n";
    write_file(&project, on_disk);
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\nusing Foo\nend\n",
    );

    let depot = TempDir::new("fatou-lsp-livedeps-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
        ("HOME", depot.path.to_str().unwrap()),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_root(&client, &file_uri(&pkg.path));

    let source_uri = file_uri(&pkg.path.join("src/MyPkg.jl"));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: source_uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "module MyPkg\nusing Foo\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();

    // `Foo` is neither declared nor installed. The finding needs the harvest,
    // which lands well after the buffer's own analysis.
    let diags = poll_publish_where(&client, "MyPkg.jl", Duration::from_secs(10), |diags| {
        diags
            .iter()
            .any(|d| d.code == Some(NumberOrString::String("unresolved-import".to_string())))
    });
    assert!(diags.iter().any(|d| d.message.contains("Foo")), "{diags:?}");

    // The project file opened and edited, never saved.
    let project_uri = file_uri(&project);
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: project_uri.clone(),
                    language_id: "toml".to_string(),
                    version: 1,
                    text: on_disk.to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: project_uri,
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: format!(
                        "{on_disk}\n[deps]\nFoo = \"00000000-0000-0000-0000-0000000000f0\"\n"
                    ),
                }],
            })
            .unwrap(),
        }))
        .unwrap();

    let cleared = poll_publish_where(
        &client,
        "MyPkg.jl",
        Duration::from_secs(10),
        <[_]>::is_empty,
    );
    assert!(cleared.is_empty());
    assert_eq!(
        std::fs::read_to_string(&project).unwrap(),
        on_disk,
        "witness: the file on disk still declares nothing"
    );

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(311),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(311));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Write a workspace holding `MyPkg` and a dependency it can resolve with no
/// depot at all: the manifest pins `Greetings` by `path`, so the harvester
/// indexes it straight off disk. Returns the temp root; the project directory
/// is `<root>/MyPkg`.
fn write_pkg_with_path_dep(prefix: &str) -> TempDir {
    const GREETINGS: &str = "00000000-0000-0000-0000-0000000000e0";
    let root = TempDir::new(prefix);
    write_file(
        &root.path.join("Greetings/Project.toml"),
        &format!("name = \"Greetings\"\nuuid = \"{GREETINGS}\"\n"),
    );
    write_file(
        &root.path.join("Greetings/src/Greetings.jl"),
        "module Greetings\ngreet(a) = a\nend\n",
    );
    write_file(
        &root.path.join("MyPkg/Project.toml"),
        &format!(
            "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n\n\
             [deps]\nGreetings = \"{GREETINGS}\"\n\n[compat]\njulia = \"1.10\"\nGreetings = \"1\"\n"
        ),
    );
    write_file(
        &root.path.join("MyPkg/Manifest.toml"),
        &format!(
            "manifest_format = \"2.0\"\n\n[[deps.Greetings]]\n\
             uuid = \"{GREETINGS}\"\npath = \"../Greetings\"\n"
        ),
    );
    write_file(&root.path.join("MyPkg/src/MyPkg.jl"), "module MyPkg\nend\n");
    root
}

/// Poll `textDocument/definition` at `position` until it answers something, or
/// the deadline passes. The library index arrives with the harvest, well after
/// the handshake, so the first request legitimately answers `null`.
fn poll_definition(
    client: &Connection,
    uri: &Uri,
    position: Position,
    deadline: Duration,
) -> Location {
    static POLL_ID: AtomicU64 = AtomicU64::new(600);
    let start = Instant::now();
    loop {
        let id = i32::try_from(POLL_ID.fetch_add(1, Ordering::Relaxed)).unwrap();
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: "textDocument/definition".to_string(),
                params: serde_json::to_value(GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        position,
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                })
                .unwrap(),
            }))
            .unwrap();
        let resp = recv_response(client, RequestId::from(id));
        let result = resp.result().expect("a definition result");
        if !result.is_null() {
            return serde_json::from_value(result).expect("a single location");
        }
        assert!(
            start.elapsed() < deadline,
            "definition never resolved within {deadline:?} — the harvest likely failed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Go-to-definition and hover on a `[deps]` name in an open `Project.toml`:
/// the jump lands on the dependency's entry file, the hover reports what the
/// environment resolved it to. Both ride the same route, so one initialized
/// server covers them — and the `JULIA_*` env is process-global anyway.
#[test]
fn serves_navigation_on_a_dependency_name() {
    let _env = ENV_LOCK.lock().unwrap();

    let root = write_pkg_with_path_dep("fatou-lsp-depdef");
    let pkg_dir = root.path.join("MyPkg");
    let depot = TempDir::new("fatou-lsp-depdef-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg_dir.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
        ("HOME", depot.path.to_str().unwrap()),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_root(&client, &file_uri(&pkg_dir));

    let project = pkg_dir.join("Project.toml");
    let project_uri = file_uri(&project);
    let text = std::fs::read_to_string(&project).unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: project_uri.clone(),
                    language_id: "toml".to_string(),
                    version: 1,
                    text: text.clone(),
                },
            })
            .unwrap(),
        }))
        .unwrap();

    // The `Greetings` key of `[deps]`, wherever the fixture happens to put it.
    let offset = text.find("Greetings = ").unwrap();
    let line = u32::try_from(text[..offset].matches('\n').count()).unwrap();
    let location = poll_definition(
        &client,
        &project_uri,
        Position::new(line, 2),
        Duration::from_secs(10),
    );

    let entry = root.path.join("Greetings/src/Greetings.jl");
    assert_eq!(location.uri.as_str(), file_uri(&entry).as_str());
    // The `Greetings` of `module Greetings`, line 0, columns 7..16.
    assert_eq!(
        location.range,
        Range::new(Position::new(0, 7), Position::new(0, 16)),
    );

    // Hover on the same name reports what the environment resolved it to. It
    // rides the same route, so one initialized server covers both.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(619),
            method: "textDocument/hover".to_string(),
            params: serde_json::to_value(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: project_uri.clone(),
                    },
                    position: Position::new(line, 2),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(619));
    let hover: Hover = serde_json::from_value(resp.result().unwrap()).unwrap();
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markdown hover contents");
    };
    let dep_root = root.path.join("Greetings");
    assert_eq!(
        markup.value,
        format!(
            "**Greetings**\n\nDevelopment dependency\n\n`{}`",
            dep_root.display()
        ),
    );

    // And the name is a document link to the same file, so a client underlines
    // it without a cursor ever landing there.
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(618),
            method: "textDocument/documentLink".to_string(),
            params: serde_json::to_value(DocumentLinkParams {
                text_document: TextDocumentIdentifier {
                    uri: project_uri.clone(),
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(618));
    let links: Vec<DocumentLink> = serde_json::from_value(resp.result().unwrap()).unwrap();
    assert_eq!(
        links
            .iter()
            .map(|link| (link.range, link.target.as_ref().map(|uri| uri.as_str())))
            .collect::<Vec<_>>(),
        vec![(
            Range::new(Position::new(line, 0), Position::new(line, 9)),
            Some(file_uri(&entry).as_str()),
        )],
        "only the [deps] name links; [compat] names no package to link to"
    );

    // Nothing else in the file jumps: a `[compat]` key names a dependency too,
    // but resolving it is not this feature.
    let compat = text.rfind("Greetings = ").unwrap();
    let compat_line = u32::try_from(text[..compat].matches('\n').count()).unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(620),
            method: "textDocument/definition".to_string(),
            params: serde_json::to_value(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: project_uri.clone(),
                    },
                    position: Position::new(compat_line, 2),
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(620));
    assert_eq!(resp.result(), Some(serde_json::Value::Null));

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(621),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(621));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Two workspace folders, each its own package project, driven end-to-end:
/// both harvest, cross-file references stay inside the folder that owns the
/// cursor (the other folder defines a same-named `greet`), and workspace
/// symbols span both packages. One test for the whole multi-folder behavior —
/// the `JULIA_*` env is process-global (see `serves_cross_file_references_and_
/// rename`), so the assertions share one initialized server.
#[test]
fn serves_multi_folder_workspaces() {
    let _env = ENV_LOCK.lock().unwrap();

    let pkg_a = TempDir::new("fatou-lsp-multi-a");
    write_file(
        &pkg_a.path.join("Project.toml"),
        "name = \"PkgA\"\nuuid = \"00000000-0000-0000-0000-00000000000a\"\n",
    );
    write_file(
        &pkg_a.path.join("src/PkgA.jl"),
        "module PkgA\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write_file(&pkg_a.path.join("src/a.jl"), "greet(a) = a\ngreet(1)\n");
    write_file(&pkg_a.path.join("src/b.jl"), "callit() = greet(2)\n");

    // The second folder defines its *own* `greet`: references in PkgA must not
    // leak here.
    let pkg_b = TempDir::new("fatou-lsp-multi-b");
    write_file(
        &pkg_b.path.join("Project.toml"),
        "name = \"PkgB\"\nuuid = \"00000000-0000-0000-0000-00000000000b\"\n",
    );
    write_file(
        &pkg_b.path.join("src/PkgB.jl"),
        "module PkgB\ngreet(b) = b\nother() = greet(3)\nend\n",
    );

    // An *empty* `JULIA_PROJECT` is trimmed and skipped by env resolution, so
    // each folder resolves via walk-up to its own `Project.toml` (a set value
    // would win over both walk-ups and collapse the folders into one env). The
    // rest isolates install discovery exactly as the single-folder e2e does.
    let depot = TempDir::new("fatou-lsp-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", ""),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let a_root = file_uri(&pkg_a.path);
    let b_root = file_uri(&pkg_b.path);
    let result = initialize_with_folders(&client, &[&a_root, &b_root]);
    assert_eq!(
        result["capabilities"]["workspace"]["workspaceFolders"]["supported"],
        serde_json::Value::Bool(true),
        "multi-folder support is advertised"
    );

    let a_uri = file_uri(&pkg_a.path.join("src/a.jl"));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: a_uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "greet(a) = a\ngreet(1)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Notification(n) => assert_eq!(n.method, "textDocument/publishDiagnostics"),
        other => panic!("expected publishDiagnostics, got {other:?}"),
    }

    // References on PkgA's `greet`: def + call in a.jl, call in b.jl — and
    // nothing from PkgB, whose same-named `greet` lives in another package.
    let locations = poll_references_spanning(&client, &a_uri, 2, Duration::from_secs(10));
    assert_eq!(locations.len(), 3, "def + call in a.jl, call in b.jl");
    let b_prefix = b_root.as_str().to_string();
    assert!(
        locations
            .iter()
            .all(|l| !l.uri.as_str().starts_with(&b_prefix)),
        "PkgB's same-named `greet` never leaks into PkgA references: {locations:?}"
    );

    // Workspace symbols with an empty query span both folders' packages (the
    // harvest has landed — the references poll above proved it).
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(400),
            method: "workspace/symbol".to_string(),
            params: serde_json::to_value(WorkspaceSymbolParams {
                query: String::new(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(400));
    let response: WorkspaceSymbolResponse = serde_json::from_value(resp.result().unwrap()).unwrap();
    // The untagged response enum parses as either shape; keep just the names.
    let names: Vec<String> = match response {
        WorkspaceSymbolResponse::Flat(symbols) => symbols.into_iter().map(|s| s.name).collect(),
        WorkspaceSymbolResponse::Nested(symbols) => symbols.into_iter().map(|s| s.name).collect(),
    };
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    for expected in ["PkgA", "PkgB", "callit", "other"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    assert_eq!(
        names.iter().filter(|n| **n == "greet").count(),
        2,
        "each package contributes its own greet"
    );

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(401),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(401));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Send a `workspace/didChangeWatchedFiles` batch.
fn did_change_watched_files(client: &Connection, changes: Vec<FileEvent>) {
    client
        .sender
        .send(Message::Notification(Notification {
            method: "workspace/didChangeWatchedFiles".to_string(),
            params: serde_json::to_value(DidChangeWatchedFilesParams { changes }).unwrap(),
        }))
        .unwrap();
}

/// A client that supports dynamic `didChangeWatchedFiles` registration and
/// opens a workspace folder gets asked, right after `initialized`, to watch
/// `.jl` sources and the project/manifest flavors.
#[test]
fn registers_file_watchers_when_the_client_supports_it() {
    let _env = ENV_LOCK.lock().unwrap();

    let ws = TempDir::new("fatou-lsp-watch-reg");
    let depot = TempDir::new("fatou-lsp-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", ws.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
        ("HOME", depot.path.to_str().unwrap()),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    #[allow(deprecated)]
    let params = InitializeParams {
        root_uri: Some(file_uri(&ws.path)),
        capabilities: ClientCapabilities {
            workspace: Some(WorkspaceClientCapabilities {
                did_change_watched_files: Some(DidChangeWatchedFilesClientCapabilities {
                    dynamic_registration: Some(true),
                    relative_pattern_support: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(params).unwrap(),
        }))
        .unwrap();
    assert!(matches!(
        client.receiver.recv().unwrap(),
        Message::Response(_)
    ));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // The registration request arrives after `initialized`; skip any
    // notifications published in between. Bounded so a missing registration
    // fails instead of hanging.
    let request = loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Message::Request(req) => break req,
            Message::Notification(_) => continue,
            other => panic!("unexpected message: {other:?}"),
        }
    };
    assert_eq!(request.method, "client/registerCapability");
    let params: RegistrationParams = serde_json::from_value(request.params).unwrap();
    assert_eq!(params.registrations.len(), 1);
    let registration = &params.registrations[0];
    assert_eq!(registration.method, "workspace/didChangeWatchedFiles");
    let options: DidChangeWatchedFilesRegistrationOptions =
        serde_json::from_value(registration.register_options.clone().unwrap()).unwrap();
    let globs: Vec<&str> = options
        .watchers
        .iter()
        .map(|watcher| match &watcher.glob_pattern {
            GlobPattern::String(glob) => glob.as_str(),
            other => panic!("expected a string glob, got {other:?}"),
        })
        .collect();
    for expected in [
        "**/*.jl",
        "**/Project.toml",
        "**/JuliaProject.toml",
        "**/Manifest.toml",
        "**/JuliaManifest.toml",
        "**/Manifest-v*.toml",
        "**/fatou.toml",
    ] {
        assert!(globs.contains(&expected), "missing {expected} in {globs:?}");
    }
    // Acknowledge the registration; the server ignores the response.
    client
        .sender
        .send(Message::Response(lsp_server::Response::new_ok(
            request.id,
            serde_json::Value::Null,
        )))
        .unwrap();

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(2));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Watched-file events drive the workspace index without any editor saves,
/// end-to-end: a created `Project.toml` re-resolves the environment (cross-file
/// references appear), a created-and-included member file joins the membership
/// (references span it), and deleting it shrinks the membership back. The
/// notifications are handled without dynamic registration — a client may watch
/// on its own initiative — so the handshake needs no watcher capability. One
/// test for the whole lifecycle: the `JULIA_*` env is process-global (see
/// `serves_cross_file_references_and_rename`), so the phases share one
/// initialized server.
#[test]
fn watched_file_events_refresh_environment_and_membership() {
    let _env = ENV_LOCK.lock().unwrap();

    // The package files exist up front, but *no* `Project.toml`: nothing
    // resolves, so nothing harvests.
    let pkg = TempDir::new("fatou-lsp-watch");
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    write_file(&pkg.path.join("src/a.jl"), "greet(a) = a\ngreet(1)\n");
    write_file(&pkg.path.join("src/b.jl"), "callit() = greet(2)\n");

    // The usual hermetic env (see `serves_cross_file_references_and_rename`),
    // plus `HOME` pointed into the temp depot: with no `Project.toml` yet,
    // resolution would otherwise fall through to the machine's real
    // `~/.julia/environments` default env.
    let depot = TempDir::new("fatou-lsp-depot");
    let _guard = EnvGuard::set(&[
        ("JULIA_PROJECT", pkg.path.to_str().unwrap()),
        ("JULIA_DEPOT_PATH", depot.path.to_str().unwrap()),
        ("JULIA_BINDIR", ""),
        ("PATH", ""),
        ("HOME", depot.path.to_str().unwrap()),
    ]);

    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let root_uri = file_uri(&pkg.path);
    initialize_with_root(&client, &root_uri);

    let a_uri = file_uri(&pkg.path.join("src/a.jl"));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: a_uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "greet(a) = a\ngreet(1)\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Notification(n) => assert_eq!(n.method, "textDocument/publishDiagnostics"),
        other => panic!("expected publishDiagnostics, got {other:?}"),
    }

    // Without an environment, references stay on the intra-file fallback.
    let locations = poll_references_spanning(&client, &a_uri, 1, Duration::from_secs(10));
    assert_eq!(locations.len(), 2, "def + call in a.jl only");

    // Phase 1: `Project.toml` appears (a `Pkg.generate`, a `git checkout`) —
    // the environment re-resolves and the package harvests, so references now
    // span the members.
    write_file(
        &pkg.path.join("Project.toml"),
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    did_change_watched_files(
        &client,
        vec![FileEvent::new(
            file_uri(&pkg.path.join("Project.toml")),
            FileChangeType::CREATED,
        )],
    );
    let locations = poll_references_spanning(&client, &a_uri, 2, Duration::from_secs(10));
    assert_eq!(locations.len(), 3, "def + call in a.jl, call in b.jl");

    // Phase 2: a new member file is created and included, both outside the
    // editor — membership refreshes and references span three files.
    write_file(&pkg.path.join("src/c.jl"), "alsocall() = greet(3)\n");
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ninclude(\"a.jl\")\ninclude(\"b.jl\")\ninclude(\"c.jl\")\nend\n",
    );
    did_change_watched_files(
        &client,
        vec![
            FileEvent::new(
                file_uri(&pkg.path.join("src/c.jl")),
                FileChangeType::CREATED,
            ),
            FileEvent::new(
                file_uri(&pkg.path.join("src/MyPkg.jl")),
                FileChangeType::CHANGED,
            ),
        ],
    );
    let locations = poll_references_spanning(&client, &a_uri, 3, Duration::from_secs(10));
    assert_eq!(locations.len(), 4, "a new call site in c.jl");

    // Phase 3: the member file is deleted (and no longer included) —
    // membership shrinks back.
    fs::remove_file(pkg.path.join("src/c.jl")).unwrap();
    write_file(
        &pkg.path.join("src/MyPkg.jl"),
        "module MyPkg\ninclude(\"a.jl\")\ninclude(\"b.jl\")\nend\n",
    );
    did_change_watched_files(
        &client,
        vec![
            FileEvent::new(
                file_uri(&pkg.path.join("src/c.jl")),
                FileChangeType::DELETED,
            ),
            FileEvent::new(
                file_uri(&pkg.path.join("src/MyPkg.jl")),
                FileChangeType::CHANGED,
            ),
        ],
    );
    let locations = poll_references_spanning(&client, &a_uri, 2, Duration::from_secs(10));
    assert_eq!(locations.len(), 3, "c.jl's call site is gone");

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(300),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(300));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    server_thread.join().unwrap();
}

/// Lint findings publish alongside parse diagnostics in the push pipeline: an
/// unused local yields a tagged `unused-binding` warning, a parse-broken edit
/// suppresses lint findings (rules need a clean tree) in favor of the parse
/// errors, and a fixing edit clears the report.
#[test]
fn publishes_lint_findings_as_diagnostics() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let _init_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let recv_diagnostics = |client: &Connection| -> PublishDiagnosticsParams {
        loop {
            match client.receiver.recv().unwrap() {
                Message::Notification(note) if note.method == "textDocument/publishDiagnostics" => {
                    return serde_json::from_value(note.params).unwrap();
                }
                _ => {}
            }
        }
    };

    // --- open a document with an unused local; expect a lint warning @v1 ---
    let uri = Uri::from_str("file:///work/lint.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f(x)\n    tmp = x + 1\n    return x\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(1));
    assert_eq!(diag.diagnostics.len(), 1);
    let finding = &diag.diagnostics[0];
    assert_eq!(
        finding.code,
        Some(lsp_types::NumberOrString::String(
            "unused-binding".to_string()
        ))
    );
    assert_eq!(finding.severity, Some(DiagnosticSeverity::WARNING));
    assert_eq!(finding.source.as_deref(), Some("fatou"));
    assert_eq!(
        finding.range,
        Range::new(Position::new(1, 4), Position::new(1, 7)),
        "the finding must cover `tmp`"
    );
    assert!(finding.message.contains("tmp"));

    // --- break the parse; the lint finding yields to the parse error @v2 ---
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "function f(x)\n    tmp = x + 1\n    return x\n".to_string(),
                }],
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(2));
    assert!(
        !diag.diagnostics.is_empty(),
        "expected a parse diagnostic for the unterminated function"
    );
    assert!(
        diag.diagnostics
            .iter()
            .all(|d| d.severity == Some(DiagnosticSeverity::ERROR) && d.code.is_none()),
        "a parse-broken buffer must carry parse errors only, got {:?}",
        diag.diagnostics
    );

    // --- fix the code entirely; expect an empty report @v3 ---
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 3,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "function f(x)\n    tmp = x + 1\n    return tmp\nend\n".to_string(),
                }],
            })
            .unwrap(),
        }))
        .unwrap();
    let diag = recv_diagnostics(&client);
    assert_eq!(diag.version, Some(3));
    assert_eq!(diag.diagnostics, Vec::new());

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _shutdown_response = client.receiver.recv().unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// `textDocument/codeAction` over a lint finding returns a preferred quick fix
/// whose edit resolves it: the `nothing-comparison` on the cursor line offers
/// `===`, carrying the diagnostic it fixes and a single-document edit.
#[test]
fn serves_quick_fix_code_actions() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(InitializeParams::default()).unwrap(),
        }))
        .unwrap();
    let init = recv_response(&client, RequestId::from(1));
    let capabilities = init.result().unwrap()["capabilities"].clone();
    assert_eq!(
        capabilities["codeActionProvider"]["codeActionKinds"],
        serde_json::json!(["quickfix"]),
        "the server must advertise quick-fix code actions"
    );
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    let text = "check(x) = x == nothing\n";
    let uri = Uri::from_str("file:///work/fixme.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: text.to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();

    // --- request code actions with the cursor inside the comparison ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(2),
            method: "textDocument/codeAction".to_string(),
            params: serde_json::to_value(CodeActionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                range: Range::new(Position::new(0, 14), Position::new(0, 14)),
                context: CodeActionContext::default(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let response = recv_response(&client, RequestId::from(2));
    let actions: Vec<CodeActionOrCommand> =
        serde_json::from_value(response.result().unwrap()).unwrap();
    assert_eq!(actions.len(), 1);
    let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
        panic!("expected a code action, got {:?}", actions[0]);
    };
    assert_eq!(action.title, "Replace `==` with `===`");
    assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
    assert_eq!(action.is_preferred, Some(true));
    let attached = action.diagnostics.as_ref().expect("attached diagnostics");
    assert_eq!(
        attached[0].code,
        Some(lsp_types::NumberOrString::String(
            "nothing-comparison".to_string()
        ))
    );

    // --- the edit rewrites exactly the operator ---
    // `Uri`-keyed maps trip `mutable_key_type` (see the allow in `src/lsp.rs`).
    #[allow(clippy::mutable_key_type)]
    let changes = action
        .edit
        .as_ref()
        .and_then(|edit| edit.changes.as_ref())
        .expect("single-document changes");
    let edits = &changes[&uri];
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "===");
    assert_eq!(
        edits[0].range,
        Range::new(Position::new(0, 13), Position::new(0, 15))
    );

    // --- a cursor outside any finding yields no actions ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/codeAction".to_string(),
            params: serde_json::to_value(CodeActionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                range: Range::new(Position::new(0, 2), Position::new(0, 4)),
                context: CodeActionContext::default(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let response = recv_response(&client, RequestId::from(3));
    let actions: Vec<CodeActionOrCommand> =
        serde_json::from_value(response.result().unwrap()).unwrap();
    assert_eq!(actions, Vec::new());

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(4));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

/// A client advertising `textDocument.diagnostic` gets the pull model: the
/// server advertises a diagnostic provider, answers `textDocument/diagnostic`
/// with a full report (parse errors, or lint findings on a clean tree), and
/// publishes nothing for open documents — while a client without the
/// capability (every other test here) keeps the push fallback.
#[test]
fn serves_pull_diagnostics() {
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });

    let params = InitializeParams {
        capabilities: ClientCapabilities {
            text_document: Some(lsp_types::TextDocumentClientCapabilities {
                diagnostic: Some(lsp_types::DiagnosticClientCapabilities::default()),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(params).unwrap(),
        }))
        .unwrap();
    let init = recv_response(&client, RequestId::from(1));
    let capabilities = init.result().unwrap()["capabilities"].clone();
    assert_eq!(
        capabilities["diagnosticProvider"]["identifier"],
        serde_json::json!("fatou"),
        "a pulling client must be offered the diagnostic provider"
    );
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();

    // --- open a document with an unused local; no publish must arrive ---
    let uri = Uri::from_str("file:///work/pull.jl").unwrap();
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: "function f(x)\n    tmp = x + 1\n    return x\nend\n".to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();

    // --- pull; the report carries the lint finding ---
    let pull = |id: i32, previous_result_id: Option<String>| {
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(id),
                method: "textDocument/diagnostic".to_string(),
                params: serde_json::to_value(lsp_types::DocumentDiagnosticParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    identifier: Some("fatou".to_string()),
                    previous_result_id,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .unwrap(),
            }))
            .unwrap();
    };
    // Any push for the opened document would arrive before the pull response;
    // fail on it instead of skipping past.
    let recv_result = |id: i32| -> lsp_types::DocumentDiagnosticReportResult {
        loop {
            match client.receiver.recv().unwrap() {
                Message::Response(resp) if resp.id == RequestId::from(id) => {
                    return serde_json::from_value(resp.result().unwrap()).unwrap();
                }
                Message::Notification(note) if note.method == "textDocument/publishDiagnostics" => {
                    panic!("a pull client must not receive pushes for open documents: {note:?}");
                }
                _ => {}
            }
        }
    };
    // Assert a full report and return its items paired with the content id the
    // client echoes back to detect an unchanged re-pull.
    let recv_full = |id: i32| -> (Vec<Diagnostic>, String) {
        let lsp_types::DocumentDiagnosticReportResult::Report(
            lsp_types::DocumentDiagnosticReport::Full(full),
        ) = recv_result(id)
        else {
            panic!("expected a full document diagnostic report");
        };
        let report = full.full_document_diagnostic_report;
        let result_id = report.result_id.expect("a full report carries a resultId");
        (report.items, result_id)
    };

    pull(2, None);
    let (items, result_id) = recv_full(2);
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].code,
        Some(lsp_types::NumberOrString::String(
            "unused-binding".to_string()
        ))
    );

    // --- re-pull the unchanged buffer with its id; the report collapses to
    // `Unchanged` (id only, no items resent) ---
    pull(100, Some(result_id.clone()));
    let lsp_types::DocumentDiagnosticReportResult::Report(
        lsp_types::DocumentDiagnosticReport::Unchanged(unchanged),
    ) = recv_result(100)
    else {
        panic!("a matching previousResultId must answer Unchanged");
    };
    assert_eq!(
        unchanged.unchanged_document_diagnostic_report.result_id, result_id,
        "the Unchanged report echoes the same content id"
    );

    // --- break the parse; the next pull reports the parse error only ---
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_string(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "function f(x)\n    tmp = x + 1\n    return x\n".to_string(),
                }],
            })
            .unwrap(),
        }))
        .unwrap();
    // Re-pull with the stale id: the findings changed, so it must not collapse
    // to `Unchanged` but resend a full report (carrying a fresh id).
    pull(3, Some(result_id.clone()));
    let (items, changed_id) = recv_full(3);
    assert_ne!(changed_id, result_id, "changed findings mint a new id");
    assert!(!items.is_empty(), "expected the parse error in the report");
    assert!(
        items
            .iter()
            .all(|d| d.severity == Some(DiagnosticSeverity::ERROR) && d.code.is_none()),
        "a parse-broken buffer must report parse errors only, got {items:?}"
    );

    // --- a pull for a never-opened document answers an empty report ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(4),
            method: "textDocument/diagnostic".to_string(),
            params: serde_json::to_value(lsp_types::DocumentDiagnosticParams {
                text_document: TextDocumentIdentifier {
                    uri: Uri::from_str("file:///work/never-opened.jl").unwrap(),
                },
                identifier: Some("fatou".to_string()),
                previous_result_id: None,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(&client, RequestId::from(4));
    let report: lsp_types::DocumentDiagnosticReportResult =
        serde_json::from_value(resp.result().unwrap()).unwrap();
    let lsp_types::DocumentDiagnosticReportResult::Report(
        lsp_types::DocumentDiagnosticReport::Full(full),
    ) = report
    else {
        panic!("expected a full document diagnostic report");
    };
    assert_eq!(full.full_document_diagnostic_report.items, Vec::new());

    // --- shutdown / exit ---
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(5),
            method: "shutdown".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    let _ = recv_response(&client, RequestId::from(5));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();

    server_thread.join().unwrap();
}

// --- configuration: initializationOptions, didChangeConfiguration, and
// `fatou.toml` discovery (file config shadows client settings) ---------------

/// The two-message initialize handshake with `initializationOptions` set (no
/// workspace folder, so no harvester spawns and discovery anchors on the
/// opened documents alone).
fn initialize_with_options(client: &Connection, options: serde_json::Value) {
    let params = InitializeParams {
        initialization_options: Some(options),
        ..Default::default()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(1),
            method: "initialize".to_string(),
            params: serde_json::to_value(params).unwrap(),
        }))
        .unwrap();
    assert!(matches!(
        client.receiver.recv().unwrap(),
        Message::Response(_)
    ));
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_string(),
            params: serde_json::json!({}),
        }))
        .unwrap();
}

/// Drain messages until a `publishDiagnostics` for `uri` arrives.
fn recv_publish_for(client: &Connection, uri: &Uri) -> PublishDiagnosticsParams {
    loop {
        match client
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Message::Notification(n) if n.method == "textDocument/publishDiagnostics" => {
                let params: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
                if params.uri == *uri {
                    return params;
                }
            }
            _ => {}
        }
    }
}

fn open_document(client: &Connection, uri: &Uri, text: &str) {
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_string(),
            params: serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "julia".to_string(),
                    version: 1,
                    text: text.to_string(),
                },
            })
            .unwrap(),
        }))
        .unwrap();
}

/// Request full-document formatting and return the single whole-document
/// edit's new text (`None` when formatting returns no edits).
fn format_document(client: &Connection, uri: &Uri, id: i32) -> Option<String> {
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(id),
            method: "textDocument/formatting".to_string(),
            params: serde_json::to_value(DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                options: FormattingOptions {
                    tab_size: 4,
                    insert_spaces: true,
                    ..Default::default()
                },
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        }))
        .unwrap();
    let resp = recv_response(client, RequestId::from(id));
    let edits: Option<Vec<TextEdit>> = serde_json::from_value(resp.result().unwrap()).unwrap();
    let mut edits = edits.unwrap_or_default();
    match edits.len() {
        0 => None,
        1 => Some(edits.remove(0).new_text),
        n => panic!("expected a single whole-document edit, got {n}"),
    }
}

const UNUSED_BINDING_SOURCE: &str = "function f(x)\n    tmp = x + 1\n    return x\nend\n";
const UNFORMATTED_FUNCTION: &str = "function f(x)\nreturn x\nend\n";

/// `initializationOptions` reconfigure the formatter: an `indent-width` of 2
/// shows up in the formatted output.
#[test]
fn initialization_options_set_format_style() {
    let dir = TempDir::new("fatou-lsp-init-options");
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_options(&client, serde_json::json!({"format": {"indent-width": 2}}));

    let uri = file_uri(&dir.path.join("a.jl"));
    open_document(&client, &uri, UNFORMATTED_FUNCTION);
    assert_eq!(
        format_document(&client, &uri, 2).as_deref(),
        Some("function f(x)\n  return x\nend\n")
    );

    drop(client);
    server_thread.join().unwrap();
}

/// `workspace/didChangeConfiguration` re-resolves the lint rules and
/// re-publishes for open documents: ignoring `unused-binding` clears the
/// finding without any edit.
#[test]
fn did_change_configuration_reconfigures_lint() {
    let dir = TempDir::new("fatou-lsp-config-change");
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_options(&client, serde_json::Value::Null);

    let uri = file_uri(&dir.path.join("lint.jl"));
    open_document(&client, &uri, UNUSED_BINDING_SOURCE);
    let published = recv_publish_for(&client, &uri);
    assert_eq!(published.diagnostics.len(), 1);
    assert_eq!(
        published.diagnostics[0].code,
        Some(lsp_types::NumberOrString::String(
            "unused-binding".to_string()
        ))
    );

    client
        .sender
        .send(Message::Notification(Notification {
            method: "workspace/didChangeConfiguration".to_string(),
            params: serde_json::json!({
                "settings": {"fatou": {"lint": {"ignore": ["unused-binding"]}}}
            }),
        }))
        .unwrap();
    let republished = recv_publish_for(&client, &uri);
    assert_eq!(
        republished.diagnostics,
        Vec::new(),
        "the ignored rule's finding must clear on the re-publish"
    );

    drop(client);
    server_thread.join().unwrap();
}

/// Per-rule option tables reach the server the same way the rest of the
/// configuration does, so a `[lint.rules.<id>]` table pushed as client settings
/// changes what the server publishes. This is what catches the rule config
/// failing to be forwarded onto the server's own rule sets.
#[test]
fn client_settings_carry_per_rule_options() {
    let dir = TempDir::new("fatou-lsp-rule-options");
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_options(&client, serde_json::Value::Null);

    let uri = file_uri(&dir.path.join("lint.jl"));
    open_document(&client, &uri, "function f()\n    sleep(1)\nend\n");
    let published = recv_publish_for(&client, &uri);
    assert_eq!(
        published.diagnostics,
        Vec::new(),
        "`sleep` is not on the built-in deny-list"
    );

    client
        .sender
        .send(Message::Notification(Notification {
            method: "workspace/didChangeConfiguration".to_string(),
            params: serde_json::json!({
                "settings": {"fatou": {"lint": {"rules": {
                    "discouraged-function": {"extend-functions": {"sleep": "use a timer"}}
                }}}}
            }),
        }))
        .unwrap();
    let republished = recv_publish_for(&client, &uri);
    assert_eq!(republished.diagnostics.len(), 1);
    assert_eq!(
        republished.diagnostics[0].code,
        Some(lsp_types::NumberOrString::String(
            "discouraged-function".to_string()
        ))
    );

    drop(client);
    server_thread.join().unwrap();
}

/// A discovered `fatou.toml` fully shadows conflicting client settings
/// (arity's rule): the file's `indent-width` wins over the
/// `initializationOptions` one.
#[test]
fn fatou_toml_wins_over_client_settings() {
    let dir = TempDir::new("fatou-lsp-toml-wins");
    write_file(&dir.path.join("fatou.toml"), "[format]\nindent-width = 2\n");
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_options(&client, serde_json::json!({"format": {"indent-width": 8}}));

    let uri = file_uri(&dir.path.join("a.jl"));
    open_document(&client, &uri, UNFORMATTED_FUNCTION);
    assert_eq!(
        format_document(&client, &uri, 2).as_deref(),
        Some("function f(x)\n  return x\nend\n")
    );

    drop(client);
    server_thread.join().unwrap();
}

/// A watched `fatou.toml` change invalidates the discovery cache and
/// re-publishes: dropping an `ignore` brings the finding back with no edit to
/// the document.
#[test]
fn fatou_toml_edit_invalidates_config() {
    let dir = TempDir::new("fatou-lsp-toml-edit");
    let toml_path = dir.path.join("fatou.toml");
    write_file(&toml_path, "[lint]\nignore = [\"unused-binding\"]\n");
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_options(&client, serde_json::Value::Null);

    let uri = file_uri(&dir.path.join("lint.jl"));
    open_document(&client, &uri, UNUSED_BINDING_SOURCE);
    let published = recv_publish_for(&client, &uri);
    assert_eq!(
        published.diagnostics,
        Vec::new(),
        "the fatou.toml ignore must suppress the finding on open"
    );

    write_file(&toml_path, "[lint]\n");
    did_change_watched_files(
        &client,
        vec![FileEvent {
            uri: file_uri(&toml_path),
            typ: FileChangeType::CHANGED,
        }],
    );
    let republished = recv_publish_for(&client, &uri);
    assert_eq!(republished.diagnostics.len(), 1, "{republished:?}");
    assert_eq!(
        republished.diagnostics[0].code,
        Some(lsp_types::NumberOrString::String(
            "unused-binding".to_string()
        ))
    );

    drop(client);
    server_thread.join().unwrap();
}

/// A `[lint.severity]` override in the client settings escalates the
/// published diagnostic's severity.
#[test]
fn severity_override_via_settings() {
    let dir = TempDir::new("fatou-lsp-severity");
    let (server, client) = Connection::memory();
    let server_thread = std::thread::spawn(move || {
        fatou::lsp::serve(&server).expect("server loop");
    });
    initialize_with_options(
        &client,
        serde_json::json!({"lint": {"severity": {"unused-binding": "error"}}}),
    );

    let uri = file_uri(&dir.path.join("lint.jl"));
    open_document(&client, &uri, UNUSED_BINDING_SOURCE);
    let published = recv_publish_for(&client, &uri);
    assert_eq!(published.diagnostics.len(), 1);
    assert_eq!(
        published.diagnostics[0].severity,
        Some(DiagnosticSeverity::ERROR)
    );

    drop(client);
    server_thread.join().unwrap();
}
