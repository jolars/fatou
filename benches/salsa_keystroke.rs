//! What one keystroke costs through the salsa pipeline, end to end.
//!
//! `benches/line_index.rs` times the pieces — the didChange splice, the
//! position queries, the raw reparse — but nothing there walks the path a real
//! keystroke takes: `apply_content_changes` on the live buffer, `upsert_file`
//! handing the text to salsa, `reparse_stage_edits` staging the transform, and
//! `parsed_document` consuming it. That path is where the per-keystroke text
//! copies live (or don't), so it is the one row on which the text-storage
//! designs — `String`, `Arc<str>`, a rope — are actually comparable.
//!
//! Three rows per size:
//!
//! 1. `upsert, text unchanged` — the staleness guard alone: what a no-op
//!    upsert costs to prove there is nothing to do.
//! 2. `splice + upsert + stage` — the write phase: the didChange splice plus
//!    handing the changed text to salsa, no parse demanded.
//! 3. `keystroke end-to-end` — the same plus `parsed_tree`, so the reparse the
//!    keystroke triggers is included; row 3 minus row 2 cross-checks against
//!    `line_index`'s token-tier row.
//!
//! Each timed iteration alternates inserting and deleting one character, so
//! the text genuinely changes every round: salsa sees a fresh revision, the
//! reparse base advances, and the printed number is one keystroke.
//!
//! The one line that differs per branch is [`handoff`], which converts the
//! live buffer into what `upsert_file` takes on that branch.
//!
//! Plain `main` (`harness = false`), same style as `line_index`:
//!
//! ```sh
//! bash bench/corpus/download.sh   # once
//! cargo bench --bench salsa_keystroke
//! ```

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use fatou::incremental::{IncrementalDatabase, IncrementalDb};
use fatou::text::{PositionEncoding, TextBuffer, apply_content_changes};
use lsp_types::{Position, Range, TextDocumentContentChangeEvent};

const UTF16: PositionEncoding = PositionEncoding::Utf16;

/// The live buffer, as `upsert_file` takes it on this branch.
///
/// - `main`:               `live.text().to_string()` (an O(N) copy)
/// - `experiment/arc-str`: `live.text_arc()` (a refcount bump)
/// - `refactor/rope`:      `live.clone()` (an O(1) rope clone)
fn handoff(live: &TextBuffer) -> Arc<str> {
    live.text_arc()
}

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

fn bench_size(label: &str, text: &str, iters: [usize; 3]) {
    println!("\n=== {label} ({} KB) ===", text.len() / 1024);

    // The edit site: ~80% of the way through the buffer, on a char boundary.
    let mut at = text.len() * 4 / 5;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    let mut live = TextBuffer::from(text);
    let position = live.line_index().byte_to_position(at, UTF16);
    let insert = vec![TextDocumentContentChangeEvent {
        range: Some(Range::new(position, position)),
        range_length: None,
        text: "z".to_string(),
    }];
    let after_z = Position::new(position.line, position.character + 1);
    let delete = vec![TextDocumentContentChangeEvent {
        range: Some(Range::new(position, after_z)),
        range_length: None,
        text: String::new(),
    }];

    let mut db = IncrementalDatabase::new();
    let path = PathBuf::from("/bench/keystroke.jl");
    let file = db.upsert_file(&path, handoff(&live));
    black_box(db.parsed_tree(file));

    row(
        "upsert, text unchanged",
        time(iters[0], || {
            black_box(db.upsert_file(&path, handoff(&live)))
        }),
    );

    // Alternate an insert and a delete so every iteration is a genuine text
    // change: a fresh salsa revision, never a memoized no-op.
    let mut flip = false;
    row(
        "splice + upsert + stage (write phase)",
        time(iters[1], || {
            flip = !flip;
            let batch = if flip { insert.clone() } else { delete.clone() };
            let edits = apply_content_changes(&mut live, batch, UTF16);
            let file = db.upsert_file(&path, handoff(&live));
            db.reparse_stage_edits(file, edits);
            file
        }),
    );

    // Row 2 left a long staged chain with no parse consuming it; drop it and
    // resync the base so row 3 starts from a clean, current parse.
    db.reparse_stage_edits(file, None);
    black_box(db.parsed_tree(file));

    let mut flip = false;
    row(
        "keystroke end-to-end (parse included)",
        time(iters[2], || {
            flip = !flip;
            let batch = if flip { insert.clone() } else { delete.clone() };
            let edits = apply_content_changes(&mut live, batch, UTF16);
            let file = db.upsert_file(&path, handoff(&live));
            db.reparse_stage_edits(file, edits);
            black_box(db.parsed_tree(file))
        }),
    );
}

fn main() {
    let Some(src) = corpus() else {
        eprintln!("corpus missing; run `bash bench/corpus/download.sh` first");
        return;
    };
    bench_size("JuliaSyntax/src/parser.jl", &src, [10_000, 2_000, 500]);

    // The same file over and over: not a plausible source file, but it shows
    // how each row scales with buffer size, which is the whole question.
    let big: String = std::iter::repeat_n(src.as_str(), 8).collect();
    bench_size("the same file x8", &big, [2_000, 500, 200]);
}
