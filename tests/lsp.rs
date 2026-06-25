//! Drive the language server over an in-memory connection: initialize, open a
//! document, request formatting, and shut down cleanly.

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidOpenTextDocumentParams, DocumentFormattingParams, FormattingOptions, InitializeParams,
    TextDocumentIdentifier, TextDocumentItem, TextEdit, Uri,
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
