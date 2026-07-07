//! Drive the language server over an in-memory connection: initialize, open a
//! document, request formatting, edit through parse errors, and shut down
//! cleanly. Exercises the threaded pipeline end-to-end: main loop → analysis
//! thread (write-phase) → read pool (read-phase) → version-gated publish.

use fatou::lsp::{compute_document_symbols, compute_folding_ranges};
use fatou::text::PositionEncoding;
use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    ClientCapabilities, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams,
    FoldingRange, FoldingRangeKind, FoldingRangeParams, FormattingOptions,
    GeneralClientCapabilities, InitializeParams, Position, PositionEncodingKind,
    PublishDiagnosticsParams, Range, SymbolKind, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextEdit, Uri, VersionedTextDocumentIdentifier,
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
