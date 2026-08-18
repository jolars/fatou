//! What line-scoped formatting edits cost, and what they save.
//!
//! The server line-diffs formatted output and returns one edit per changed run,
//! falling back to a whole-document edit when more than half the file differs.
//!
//! Two questions, and this bench is the evidence for both: what the diff adds
//! to a format request, and how much smaller the answer gets. `payload` is the
//! edits' `new_text` as a share of the document.
//!
//! Measured on 2026-08-13 (release, otherwise-idle machine). Absolute times
//! drift with CPU frequency across rows, so **the comparison that means
//! something is `format` against `+diff` within a row**:
//!
//! ```text
//! === JuliaSyntax/src/parser.jl (134 KB, 3611 lines) ===
//!                                 format        +diff   edits   payload
//! already formatted             11.95 ms     12.12 ms       0     0.0 %
//! one line dirtied              11.93 ms     12.75 ms       1     0.0 %
//! five lines dirtied            11.83 ms     12.46 ms       5     0.1 %
//! fifty lines dirtied           11.98 ms     12.58 ms      50     1.3 %
//! unformatted (as committed)    12.00 ms     13.83 ms     199    16.2 %
//! CRLF, line-ending forced LF   12.10 ms     12.95 ms       1    97.3 %
//! ```
//!
//! The diff costs about 5% of the format it follows on an ordinary edit, and
//! 15% in the worst row here, which is the price of turning a 134 KB payload
//! into a few hundred bytes. It is charged only when the document actually
//! changed: an already-formatted document short-circuits on the string
//! comparison before any diffing, which is the first row's ~0%.
//!
//! The last two rows are the interesting ones. Real Julia in the wild is
//! *close* to formatted, so even a file nobody has ever run through Fatou comes
//! back as scattered small hunks rather than a rewrite — the whole-document
//! fallback is for genuine wholesale changes, not for ordinary first contact.
//! Forcing a line-ending conversion is such a change: every line differs, the
//! diff covers the whole file, and the fallback correctly collapses it back to
//! one edit rather than shipping a hunk per line. (Its payload is the whole
//! document, under 100% only because the CRLF source is longer than the LF
//! output it is measured against.)
//!
//! Plain `main` (`harness = false`), same style as `line_index`: no criterion
//! dependency in the root crate, just a warm loop and a table.
//!
//! ```sh
//! bash bench/corpus/download.sh   # once
//! cargo bench --bench format_edits
//! ```

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fatou::formatter::{FormatStyle, LineEnding, format_with_style};
use fatou::lsp::compute_format_edits;
use fatou::text::PositionEncoding;

const UTF16: PositionEncoding = PositionEncoding::Utf16;

fn corpus(relative: &str) -> Option<String> {
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/corpus");
    fs::read_to_string(root.join(relative)).ok()
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

fn millis(ns: f64) -> String {
    if ns < 1_000_000.0 {
        format!("{:>7.2} us", ns / 1_000.0)
    } else {
        format!("{:>7.2} ms", ns / 1_000_000.0)
    }
}

/// Rewrite line `index` of `lines` with its first ` = ` closed up, the way a
/// buffer is dirty just before a format-on-save.
fn dirty_line(lines: &[&str], index: usize) -> String {
    let mut out = String::with_capacity(lines.iter().map(|line| line.len()).sum());
    for (at, line) in lines.iter().enumerate() {
        if at == index {
            out.push_str(&line.replacen(" = ", "=", 1));
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Lines whose ` = ` the formatter demonstrably puts back, spread through the
/// file. A ` = ` inside a docstring or a comment survives untouched, and a row
/// measuring zero edits measures nothing — so each pick is confirmed by
/// formatting the file with just that line dirtied.
fn dirty_targets(formatted: &str, style: FormatStyle, want: usize) -> Vec<usize> {
    let lines: Vec<&str> = formatted.split_inclusive('\n').collect();
    let candidates: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(" = "))
        .map(|(index, _)| index)
        .collect();
    let stride = (candidates.len() / want.max(1)).max(1);
    let mut picked = Vec::new();
    let mut at = 0;
    while picked.len() < want && at < candidates.len() {
        let target = candidates[at];
        let edits = compute_format_edits(&dirty_line(&lines, target), style, UTF16);
        if edits.is_some_and(|edits| edits.len() == 1) {
            picked.push(target);
            at += stride;
        } else {
            at += 1;
        }
    }
    picked
}

/// Dirty every line in `targets` at once.
fn dirty(formatted: &str, targets: &[usize]) -> String {
    let mut lines: Vec<String> = formatted
        .split_inclusive('\n')
        .map(|line| line.to_string())
        .collect();
    for &target in targets {
        lines[target] = lines[target].replacen(" = ", "=", 1);
    }
    lines.concat()
}

fn row(label: &str, text: &str, style: FormatStyle) {
    let format_ns = time(30, || format_with_style(text, style));
    let edits_ns = time(30, || compute_format_edits(text, style, UTF16));
    let edits = compute_format_edits(text, style, UTF16).expect("corpus must format");
    let payload: usize = edits.iter().map(|edit| edit.new_text.len()).sum();
    let share = if text.is_empty() {
        0.0
    } else {
        payload as f64 * 100.0 / text.len() as f64
    };
    println!(
        "{label:<28}{}   {}   {:>5}   {share:>5.1} %",
        millis(format_ns),
        millis(edits_ns),
        edits.len(),
    );
}

fn bench_file(label: &str, source: &str) {
    let style = FormatStyle::default();
    let formatted = format_with_style(source, style).expect("corpus must format");
    println!(
        "\n=== {label} ({} KB, {} lines) ===",
        source.len() / 1024,
        source.lines().count()
    );
    println!(
        "{:<28}{:>10}   {:>10}   {:>5}   {:>7}",
        "", "format", "+diff", "edits", "payload"
    );

    let targets = dirty_targets(&formatted, style, 50);
    row("already formatted", &formatted, style);
    row("one line dirtied", &dirty(&formatted, &targets[..1]), style);
    row(
        "five lines dirtied",
        &dirty(&formatted, &targets[..5]),
        style,
    );
    row("fifty lines dirtied", &dirty(&formatted, &targets), style);
    row("unformatted (as committed)", source, style);

    // The degenerate case the fallback exists for. `LineEnding::Auto` would
    // keep the CRLF and find nothing to do, so force LF: now every line differs
    // and the diff covers the whole file, buying nothing a single replacement
    // does not.
    let forced_lf = FormatStyle {
        line_ending: LineEnding::Lf,
        ..style
    };
    row(
        "CRLF, line-ending forced LF",
        &formatted.replace('\n', "\r\n"),
        forced_lf,
    );
}

fn main() {
    let files = [
        "JuliaSyntax/src/parser.jl",
        "DataFrames/src/abstractdataframe/abstractdataframe.jl",
    ];
    let mut found = false;
    for relative in files {
        match corpus(relative) {
            Some(source) => {
                found = true;
                bench_file(relative, &source);
            }
            None => eprintln!("missing {relative}"),
        }
    }
    if !found {
        eprintln!("corpus missing; run `bash bench/corpus/download.sh` first");
    }
}
