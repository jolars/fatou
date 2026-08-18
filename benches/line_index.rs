//! What the maintained line table costs, against what rescanning costs.
//!
//! The language server resolves LSP positions against a document many times per
//! keystroke: once to splice the `didChange` in, then again in every read
//! handler answering against that buffer. Rescanning the text for its line
//! starts each time is linear in the *buffer* rather than in the edit, so
//! [`TextBuffer`] keeps the table beside the text and patches it across each
//! edit (`src/text/buffer.rs`).
//!
//! This bench guards against reintroducing a rescan on the hot path. Measured on 2026-08-16
//! (release, otherwise-idle machine — every row here scales with load, so read
//! the ratios, not the absolutes):
//!
//! ```text
//!                                   134 KB     1073 KB
//! rescan (LineIndex::new)            14 us      117 us
//! reuse the maintained table          1 ns        1 ns
//! didChange (edit plus undo)        7.0 us       67 us
//! reparse, token tier                19 us      154 us
//! ```
//!
//! The `didChange` row applies a keystroke and then undoes it, so one
//! keystroke is about half of what it prints.
//!
//! The `didChange` row grew (it was 0.9/8.8 us against a `String` spliced in
//! place) when the text became a shared `Arc<str>`: an edit rebuilds the
//! string rather than mutating it, which is what makes every *handoff* of the
//! text free. That trade is priced in `benches/salsa_keystroke.rs`, which
//! times the whole keystroke rather than this one step — read the two
//! together, and prefer the pipeline bench when judging a text-storage
//! change.
//!
//! Ropey wins this bench's didChange row outright
//! (~0.7 us flat at 1 MB) and loses the point-query rows ~3-9x, but the
//! pipeline benchmark is authoritative for text-storage changes.
//!
//! Plain `main` (`harness = false`), same style as `format_compare`: no
//! criterion dependency in the root crate, just a warm loop and a table.
//!
//! ```sh
//! bash bench/corpus/download.sh   # once
//! cargo bench --bench line_index
//! ```

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fatou::parser::{Edit, parse, reparse};
use fatou::text::{LineIndex, LineStarts, PositionEncoding, TextBuffer, apply_content_changes};
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
        LineStarts::new(text).len()
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
        "rescan (LineIndex::new)",
        time(2_000, || LineIndex::new(text)),
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
        "LineIndex::position_to_byte",
        time(200_000, || index.position_to_byte(position, UTF16)),
    );
    row(
        "LineIndex::byte_to_position",
        time(200_000, || index.byte_to_position(at, UTF16)),
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
