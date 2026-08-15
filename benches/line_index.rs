//! What the maintained line table costs, against what rescanning costs.
//!
//! The language server resolves LSP positions against a document many times per
//! keystroke: once to splice the `didChange` in, then again in every read
//! handler answering against that buffer. [`TextBuffer`] stores the text as a
//! `ropey::Rope`, whose line metrics answer those queries in O(log n) — there
//! is no separate table to rebuild (`src/text/buffer.rs`).
//!
//! This bench is the evidence for that shape, and the guard against a change
//! that quietly reintroduces a rescan on the hot path. Measured on 2026-08-15
//! (release, otherwise-idle machine — every row here scales with load, so read
//! the ratios, not the absolutes):
//!
//! ```text
//!                                   134 KB     1073 KB
//! rescan (TextBuffer::new)            23 us      185 us
//! reuse the buffer's rope             0 ns        0 ns
//! didChange (edit plus undo)        0.75 us     0.77 us
//! flatten the whole buffer (text())  3.4 us       25 us
//! slice a 64-byte span                34 ns        28 ns
//! reparse, token tier                 26 us      214 us
//! ```
//!
//! The `didChange` row applies a keystroke and then undoes it, so one
//! keystroke is about half of what it prints.
//!
//! A keystroke used to pay that rescan on the main loop before dispatching
//! anything, and pay it again in every handler that answered against the
//! buffer. Position conversion against the live buffer is now O(log n) on the
//! rope; salsa stores that same rope (`SourceFile.text` is a `TextBuffer`), so
//! the write-phase clones it O(1). The parser consumes that rope directly
//! (`parse_rope`, `reparse_*_rope`, … — issue #76), so `parsed_document` no
//! longer flattens at all: a keystroke is O(log n + region), not O(N).
//!
//! The two "bounded read" rows are the read-handler path: a warm handler
//! answers against a small span — a signature, a token, a line. Flattening the
//! whole buffer per request (`text()`) is O(N); slicing the span off the rope
//! (`rope().slice(..)`) is O(1) plus the span's bytes. The ratio is the
//! per-request win, and it is independent of the span size.
//!
//! Plain `main` (`harness = false`), same style as `format_compare`: no
//! criterion dependency in the root crate, just a warm loop and a table.
//!
//! ```sh
//! bash bench/corpus/download.sh   # once
//! cargo bench --bench line_index
//! ```

use std::borrow::Cow;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fatou::parser::{Edit, parse, reparse};
use fatou::text::{PositionEncoding, TextBuffer, apply_content_changes};
use lsp_types::{Range, TextDocumentContentChangeEvent};

const UTF16: PositionEncoding = PositionEncoding::Utf16;

fn corpus() -> Option<String> {
    let dir: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/corpus/JuliaSyntax/src");
    fs::read_to_string(dir.join("parser.jl")).ok()
}

/// Time `f` in a warm loop, returning nanoseconds per iteration.
fn time<T>(iters: usize, mut f: impl FnMut() -> T) -> f64 {
    for _ in 0..(iters / 10).max(1) {
        black_box(f());
    }
    let start = Instant::now();
    for _ in 0..iters {
        black_box(f());
    }
    start.elapsed().as_nanos() as f64 / iters as f64
}

fn row(name: &str, ns: f64) {
    let time = if ns < 1_000.0 {
        format!("{ns:>8.0} ns")
    } else if ns < 1_000_000.0 {
        format!("{:>8.2} us", ns / 1_000.0)
    } else {
        format!("{:>8.3} ms", ns / 1_000_000.0)
    };
    println!("{name:<40}{time}");
}

fn bench_size(label: &str, text: &str) {
    println!(
        "\n=== {label} ({} KB, {} lines) ===",
        text.len() / 1024,
        TextBuffer::new(text).line_count()
    );

    // The edit site: ~80% of the way through the buffer, on a char boundary.
    let mut at = text.len() * 4 / 5;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    let buffer = TextBuffer::from(text);
    let position = buffer.line_index().byte_to_position(at, UTF16);
    let keystroke = vec![TextDocumentContentChangeEvent {
        range: Some(Range::new(position, position)),
        range_length: None,
        text: "z".to_string(),
    }];

    println!("-- getting an index to resolve a position with --");
    row(
        "rescan (TextBuffer::new)",
        time(2_000, || TextBuffer::new(text)),
    );
    row(
        "reuse the maintained table",
        time(1_000_000, || buffer.line_index()),
    );

    println!("-- one keystroke, the didChange path --");
    // A live document's buffer has slack capacity, so hand the timed loop a
    // buffer it can splice into repeatedly rather than one rebuilt per
    // iteration: rebuilding would time a 1 MB alloc, not the edit.
    let mut live = TextBuffer::from(text);
    row(
        "apply_content_changes",
        time(20_000, || {
            let edits = apply_content_changes(&mut live, keystroke.clone(), UTF16);
            // Undo it, so the loop measures a steady-state buffer rather than
            // one growing by a byte per iteration. Charged to the row, so the
            // true cost of one keystroke is about half of what it prints.
            live.replace_range(at..at + 1, "");
            black_box(edits)
        }),
    );

    println!("-- one position query, index in hand --");
    let index = buffer.line_index();
    row(
        "TextBuffer::position_to_byte",
        time(200_000, || index.position_to_byte(position, UTF16)),
    );
    row(
        "TextBuffer::byte_to_position",
        time(200_000, || index.byte_to_position(at, UTF16)),
    );

    println!("-- one bounded read, the handler path --");
    // A warm read handler (hover/completion/semantic-tokens/…) answers against
    // a small span — a signature, a token, a line. It used to flatten the whole
    // buffer per request (`text()`); it now slices that span off the rope. The
    // ratio below is the per-request win, and it is independent of the span
    // size: the flatten is O(N), the slice is O(1) plus the span's bytes.
    row(
        "flatten the whole buffer (text())",
        time(2_000, || buffer.text()),
    );
    row(
        "slice a 64-byte signature span",
        time(200_000, || {
            Cow::<str>::from(buffer.rope().slice(at..at + 64))
        }),
    );

    println!("-- for scale: what the keystroke triggers --");
    let parsed = parse(text);
    let green = parsed.cst.green().to_owned();
    let diags = parsed.diagnostics;
    let edit = Edit {
        range: at..at,
        insert: "z".to_string(),
    };
    let new_text = edit.apply(text);
    row("parse (full)", time(20, || parse(text)));
    match reparse(text, &green, &diags, &edit, &new_text) {
        Some(_) => row(
            "reparse (token tier)",
            time(2_000, || reparse(text, &green, &diags, &edit, &new_text)),
        ),
        None => println!("reparse: declined at this site (would full-parse)"),
    }
}

fn main() {
    let Some(src) = corpus() else {
        eprintln!("corpus missing; run `bash bench/corpus/download.sh` first");
        return;
    };
    bench_size("JuliaSyntax/src/parser.jl", &src);

    // The same file over and over: not a plausible source file, but it shows
    // how each row scales with buffer size, which is the whole question.
    let big: String = std::iter::repeat_n(src.as_str(), 8).collect();
    bench_size("the same file x8", &big);
}
