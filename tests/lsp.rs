//! Drive the language server over an in-memory connection: initialize, open a
//! document, request formatting, edit through parse errors, and shut down
//! cleanly. Exercises the threaded pipeline end-to-end: main loop → analysis
//! thread (write-phase) → read pool (read-phase) → version-gated publish.

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    ClientCapabilities, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, FormattingOptions,
    GeneralClientCapabilities, InitializeParams, Position, PositionEncodingKind,
    PublishDiagnosticsParams, Range, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextEdit, Uri, VersionedTextDocumentIdentifier,
};
use std::str::FromStr;

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
                result["capabilities"]["textDocumentSync"],
                serde_json::json!(2),
                "expected TextDocumentSyncKind::INCREMENTAL"
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
