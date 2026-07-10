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
    ClientCapabilities, CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentFormattingParams, DocumentHighlight, DocumentHighlightKind, DocumentHighlightParams,
    DocumentRangeFormattingParams, DocumentSymbol, DocumentSymbolParams, FoldingRange,
    FoldingRangeKind, FoldingRangeParams, FormattingOptions, GeneralClientCapabilities,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    InitializeParams, Location, PartialResultParams, Position, PositionEncodingKind,
    PublishDiagnosticsParams, Range, ReferenceContext, ReferenceParams, RenameParams,
    SelectionRange, SelectionRangeParams, SemanticTokens, SemanticTokensParams, SignatureHelp,
    SignatureHelpParams, SymbolKind, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, TextEdit, Uri, VersionedTextDocumentIdentifier,
    WorkDoneProgressParams, WorkspaceEdit, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
                serde_json::from_value(resp.result.unwrap()).unwrap();
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
                serde_json::from_value(resp.result.unwrap()).unwrap();
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
                serde_json::from_value(resp.result.unwrap()).unwrap();
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
                resp.result.unwrap()["capabilities"]["hoverProvider"],
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
            let hover: Hover = serde_json::from_value(resp.result.unwrap()).unwrap();
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
            let triggers =
                &resp.result.unwrap()["capabilities"]["signatureHelpProvider"]["triggerCharacters"];
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
            let help: SignatureHelp = serde_json::from_value(resp.result.unwrap()).unwrap();
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
                resp.result.unwrap()["capabilities"]["definitionProvider"],
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
                serde_json::from_value(resp.result.unwrap()).unwrap();
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
                resp.result.unwrap()["capabilities"]["referencesProvider"],
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
            let locations: Vec<Location> = serde_json::from_value(resp.result.unwrap()).unwrap();
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
                resp.result.unwrap()["capabilities"]["documentHighlightProvider"],
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
                serde_json::from_value(resp.result.unwrap()).unwrap();
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
            let caps = resp.result.unwrap();
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
            let range: Range = serde_json::from_value(resp.result.unwrap()).unwrap();
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
            let edit: WorkspaceEdit = serde_json::from_value(resp.result.unwrap()).unwrap();
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
            let result = resp.result.unwrap();
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
            let result = resp.result.unwrap();
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
                serde_json::from_value(resp.result.unwrap()).unwrap();
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
            let result = resp.result.unwrap();
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
                serde_json::from_value(resp.result.unwrap()).unwrap();
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
            assert_eq!(resp.result, Some(serde_json::Value::Null));
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
                resp.result.unwrap()["capabilities"]["workspaceSymbolProvider"],
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
                serde_json::from_value(resp.result.unwrap()).unwrap();
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
            let result = resp.result.unwrap();
            assert_eq!(
                result["capabilities"]["completionProvider"]["resolveProvider"],
                serde_json::json!(true),
            );
            assert_eq!(
                result["capabilities"]["completionProvider"]["triggerCharacters"],
                serde_json::json!([".", "@"]),
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
            let items = match serde_json::from_value(resp.result.unwrap()).unwrap() {
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

    // --- resolve round-trips an item (no library loaded, so unchanged) ---
    let item = CompletionItem {
        label: "alpha".to_string(),
        ..Default::default()
    };
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "completionItem/resolve".to_string(),
            params: serde_json::to_value(&item).unwrap(),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let resolved: CompletionItem = serde_json::from_value(resp.result.unwrap()).unwrap();
            assert_eq!(resolved.label, "alpha");
        }
        other => panic!("expected a resolve response, got {other:?}"),
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
            let result = resp.result.unwrap();
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
            let folds: Vec<FoldingRange> = serde_json::from_value(resp.result.unwrap()).unwrap();
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
            assert_eq!(resp.result, Some(serde_json::Value::Null));
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
            let result = resp.result.unwrap();
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
            let chains: Vec<SelectionRange> = serde_json::from_value(resp.result.unwrap()).unwrap();
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
            assert_eq!(resp.result, Some(serde_json::Value::Null));
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

/// The decoded semantic tokens for `text` under UTF-8.
fn toks(text: &str) -> Vec<(u32, u32, u32, u32)> {
    decode(&compute_semantic_tokens(text, PositionEncoding::Utf8))
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
    // Trailing sigil: only `@time` paints, the module path stays plain
    // until name resolution can classify it (Phase 6).
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
        decode(&compute_semantic_tokens(text, PositionEncoding::Utf8)),
        vec![
            (0, 0, 6, STRING),
            (0, 8, 2, KEYWORD),
            (0, 11, 4, KEYWORD),
            (0, 16, 3, KEYWORD),
        ],
    );
    assert_eq!(
        decode(&compute_semantic_tokens(text, PositionEncoding::Utf16)),
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
            let provider = &resp.result.unwrap()["capabilities"]["semanticTokensProvider"];
            assert_eq!(
                provider["legend"]["tokenTypes"],
                serde_json::json!(["keyword", "macro", "string", "number"]),
            );
            assert_eq!(provider["full"], serde_json::json!(true));
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
                    text: "@show 1 + true\n".to_string(),
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
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            let tokens: SemanticTokens = serde_json::from_value(resp.result.unwrap()).unwrap();
            assert_eq!(
                decode(&tokens),
                vec![(0, 0, 5, MACRO), (0, 6, 1, NUMBER), (0, 10, 4, KEYWORD)],
            );
        }
        other => panic!("expected a semanticTokens response, got {other:?}"),
    }

    // --- an unknown document answers null ---
    let unknown = Uri::from_str("file:///work/never-opened.jl").unwrap();
    client
        .sender
        .send(Message::Request(Request {
            id: RequestId::from(3),
            method: "textDocument/semanticTokens/full".to_string(),
            params: semantic_params(&unknown),
        }))
        .unwrap();
    match client.receiver.recv().unwrap() {
        Message::Response(resp) => {
            assert_eq!(resp.result, Some(serde_json::Value::Null));
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
/// round-trips to the exact path the server tracks.
fn file_uri(path: &Path) -> Uri {
    Uri::from_str(&format!("file://{}", path.to_str().unwrap())).unwrap()
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
/// `a.jl`) until the harvest has landed and the reverse index is seeded — i.e.
/// the response spans both member files — or the deadline elapses. Before the
/// harvest, the intra-file fallback returns only the two `a.jl` sites; after it,
/// three sites across `a.jl` and `b.jl`.
fn poll_cross_file_references(client: &Connection, uri: &Uri, deadline: Duration) -> Vec<Location> {
    let start = Instant::now();
    let mut id = 100;
    loop {
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
        let locations: Vec<Location> = serde_json::from_value(resp.result.unwrap()).unwrap();
        let files: std::collections::HashSet<&str> =
            locations.iter().map(|l| l.uri.as_str()).collect();
        if files.len() >= 2 {
            return locations;
        }
        if start.elapsed() >= deadline {
            panic!(
                "cross-file references never populated within {deadline:?}: got \
                 {} location(s) across {} file(s) — the harvest or member seeding \
                 likely failed",
                locations.len(),
                files.len()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
        id += 1;
    }
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
    let locations = poll_cross_file_references(&client, &a_uri, Duration::from_secs(10));
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
    let range: Range = serde_json::from_value(resp.result.unwrap()).unwrap();
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
    let edit: WorkspaceEdit = serde_json::from_value(resp.result.unwrap()).unwrap();
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
