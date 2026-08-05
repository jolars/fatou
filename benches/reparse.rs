//! What incremental reparse actually buys, measured against a full parse.
//!
//! Seven scenarios over one ~100 KB corpus file, all sharing a single base
//! parse done in setup:
//!
//! | bench                     | what it costs                                  |
//! |---------------------------|------------------------------------------------|
//! | `full_parse`              | the baseline every other row is judged by      |
//! | `token_keystroke`         | one char typed into an identifier (token tier) |
//! | `docstring_keystroke`     | one char typed into a docstring (token tier)   |
//! | `statement_edit`          | a statement added at the end (top-level tier)  |
//! | `precise_chain`           | 3 scattered edits replayed one at a time       |
//! | `scattered_via_diff_edit` | the same net change as one collapsed edit      |
//! | `rejected_attempt`        | the wasted work when no tier can splice        |
//!
//! Measured on ~131 KB of JuliaSyntax (2026-08-05, release):
//!
//! ```text
//! full_parse                6.34 ms
//! token_keystroke          15.5  us     410x
//! docstring_keystroke      18.2  us     349x
//! statement_edit          548    us      12x
//! precise_chain            59.7  us     106x
//! scattered_via_diff_edit  18.2  ms     0.35x   -- slower than full_parse
//! rejected_attempt          1.05 ms
//! ```
//!
//! `docstring_keystroke` is what the `STRING_CONTENT` path bought: the same
//! edit cost 548 us before it, because `fold_docstrings` makes the docstring
//! and the definition it documents one `ROOT` child, so the statement tier
//! reparsed both. The small gap to `token_keystroke` is the whole-literal
//! relex, which scans a docstring rather than an identifier.
//!
//! The last three rows are the ones that shaped stage 4.
//!
//! `precise_chain` vs `scattered_via_diff_edit` is the argument for keeping
//! the client's precise edits, and it is stronger than it first looks. A
//! collapsed diff of scattered edits spans everything between the first and
//! the last, and the top-level tier does not answer a wide span cheaply — it
//! fragment-parses the region *and* both boundary guards, so the attempt costs
//! well over the full parse it then falls back to. A wide diff is not merely a
//! miss; it is the most expensive thing in this file. That is why
//! `parsed_document` offers the chain first and only then falls back to
//! `diff_edit` (`src/incremental.rs`).
//!
//! `rejected_attempt` is the tax on an edit no tier can splice: ~16% of a full
//! parse here, paid on top of it. Tolerable, and it bounds how much a new
//! guard may cost before rejecting — but it is not free, so a guard should
//! bail on cheap evidence rather than after a fragment parse.
//!
//! `cargo bench` builds with the release profile, so `debug_assertions` is off
//! and the Tenet-4 check inside `reparse` (a whole extra parse plus two
//! fingerprint builds per call) does not run. Do not read anything into a
//! `--profile dev` run; it measures that assert, not the reparse.
//!
//! The corpus is `bench/corpus/JuliaSyntax`, git-ignored and fetched by
//! `bench/corpus/download.sh`. Without it this bench prints a hint and exits.

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};

use fatou::parser::{Edit, ParseDiagnostic, ReparseTier, diff_edit, parse, reparse, reparse_edits};
use rowan::GreenNode;

/// Roughly the size the TODO calls for: big enough that a full parse is a
/// visible cost, small enough to stay one plausible source file.
const TARGET_BYTES: usize = 100 * 1024;

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/corpus/JuliaSyntax/src")
}

/// The corpus as one buffer: `parser.jl` first (the largest single file, and
/// the one Fatou is best equipped to handle), then further sources in name
/// order until [`TARGET_BYTES`].
fn load_corpus() -> Option<String> {
    let dir = corpus_dir();
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "jl").then_some(path)
        })
        .collect();
    paths.sort();
    // `parser.jl` leads so the bench's edit sites are stable at low offsets
    // even if the checkout gains or loses other files.
    let lead = dir.join("parser.jl");
    paths.retain(|p| *p != lead);
    paths.insert(0, lead);

    let mut src = String::with_capacity(TARGET_BYTES);
    for path in paths {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        src.push_str(&text);
        src.push('\n');
        if src.len() >= TARGET_BYTES {
            break;
        }
    }
    (!src.is_empty()).then_some(src)
}

/// The byte offset just past the occurrence of `needle` that sits `fraction`
/// of the way through all of them — so a site at 0.1 and one at 0.9 really are
/// at opposite ends of the file, however many occurrences the checkout has.
///
/// `needle` must be (a prefix of) an identifier for the token tier to have a
/// chance: an insert just past a keyword is a different scenario entirely.
fn ident_site(src: &str, needle: &str, fraction: f64) -> usize {
    let sites: Vec<usize> = src.match_indices(needle).map(|(at, _)| at).collect();
    assert!(
        sites.len() > 50,
        "corpus has only {} `{needle}`; has the checkout changed?",
        sites.len()
    );
    sites[((sites.len() - 1) as f64 * fraction) as usize] + needle.len()
}

/// A byte offset inside a triple-quoted string's *body*: just past the first
/// letter following the `fraction`-th opening `"""`. Occurrences alternate
/// open/close, so only the even ones are openers.
///
/// This is the site the `STRING_CONTENT` path exists for. `fold_docstrings`
/// folds a docstring with the definition it documents into one `ROOT` child,
/// so without that path a keystroke here reparses the docstring *and* the
/// whole definition under it at the statement tier.
fn docstring_site(src: &str, fraction: f64) -> usize {
    let sites: Vec<usize> = src.match_indices("\"\"\"").map(|(at, _)| at).collect();
    assert!(
        sites.len() > 50,
        "corpus has only {} `\"\"\"`; has the checkout changed?",
        sites.len()
    );
    let openers = sites.len() / 2;
    let open = sites[2 * (((openers - 1) as f64 * fraction) as usize)] + 3;
    let rel = src[open..]
        .find(|c: char| c.is_ascii_alphabetic())
        .expect("a letter in the docstring body");
    open + rel + 1
}

fn insert(at: usize, text: &str) -> Edit {
    Edit {
        range: at..at,
        insert: text.to_string(),
    }
}

/// A base parse plus the pieces every scenario needs to splice against it.
struct Base {
    src: String,
    green: GreenNode,
    diagnostics: Vec<ParseDiagnostic>,
}

impl Base {
    fn new(src: String) -> Self {
        let parsed = parse(&src);
        Self {
            green: parsed.cst.green().into_owned(),
            diagnostics: parsed.diagnostics,
            src,
        }
    }

    /// Run one edit through `reparse`, asserting which tier answers so a
    /// grammar change that silently downgrades a scenario is caught here
    /// rather than quietly showing up as a slower number.
    fn expect_tier(&self, edit: &Edit, tier: ReparseTier) -> impl Fn() + '_ {
        let new_text = edit.apply(&self.src);
        let edit = edit.clone();
        let got = reparse(&self.src, &self.green, &self.diagnostics, &edit, &new_text)
            .unwrap_or_else(|| panic!("expected the {tier:?} tier to handle {edit:?}"));
        assert_eq!(got.tier, tier, "wrong tier for {edit:?}");
        move || {
            black_box(
                reparse(
                    &self.src,
                    &self.green,
                    &self.diagnostics,
                    &edit,
                    black_box(&new_text),
                )
                .expect("tier should still fire"),
            );
        }
    }
}

fn bench_reparse(c: &mut Criterion) {
    let Some(src) = load_corpus() else {
        eprintln!(
            "reparse bench: corpus missing at {}\n\
             run `bash bench/corpus/download.sh` first",
            corpus_dir().display()
        );
        return;
    };
    let base = Base::new(src);
    let mut group = c.benchmark_group("reparse");
    group.throughput(criterion::Throughput::Bytes(base.src.len() as u64));

    group.bench_function("full_parse", |b| {
        b.iter(|| black_box(parse(black_box(&base.src))));
    });

    // A char typed into an identifier deep in the file.
    let keystroke = insert(ident_site(&base.src, "bump", 0.5), "z");
    let run = base.expect_tier(&keystroke, ReparseTier::Token);
    group.bench_function("token_keystroke", |b| b.iter(&run));

    // A char typed into a docstring body. Same tier as `token_keystroke`, but
    // it is the case with the most to lose: the statement tier would answer it
    // by reparsing the whole documented definition.
    let prose = insert(docstring_site(&base.src, 0.5), "z");
    let run = base.expect_tier(&prose, ReparseTier::Token);
    group.bench_function("docstring_keystroke", |b| b.iter(&run));

    // A whole statement appended at the end of the buffer: a new top-level
    // item, which is the statement tier's bread and butter.
    let statement = insert(base.src.len(), "\nbench_probe_fn(x) = x + 1\n");
    let run = base.expect_tier(&statement, ReparseTier::TopLevel);
    group.bench_function("statement_edit", |b| b.iter(&run));

    // Three identifier edits at opposite ends of the file — a multi-cursor
    // rename, or a code action rewriting several call sites. Offsets are
    // against the text each predecessor produced, so later ones shift by the
    // inserts already applied: the shape a `didChange` batch arrives in.
    let scattered = vec![
        insert(ident_site(&base.src, "bump", 0.1), "z"),
        insert(ident_site(&base.src, "bump", 0.5) + 1, "z"),
        insert(ident_site(&base.src, "bump", 0.9) + 2, "z"),
    ];
    let scattered_text = fatou::parser::apply_edits(&base.src, &scattered);
    assert!(
        reparse_edits(
            &base.src,
            &base.green,
            &base.diagnostics,
            &scattered,
            &scattered_text,
        )
        .is_some(),
        "the scattered chain should splice"
    );
    group.bench_function("precise_chain", |b| {
        b.iter(|| {
            black_box(reparse_edits(
                &base.src,
                &base.green,
                &base.diagnostics,
                black_box(&scattered),
                black_box(&scattered_text),
            ))
        });
    });

    // The same net change seen only as two whole texts. The collapsed edit
    // spans from the first change to the last, so this is expected to miss and
    // cost a full parse — that gap is what `precise_chain` closes.
    let collapsed = diff_edit(&base.src, &scattered_text);
    group.bench_function("scattered_via_diff_edit", |b| {
        b.iter(|| {
            let spliced = reparse(
                &base.src,
                &base.green,
                &base.diagnostics,
                black_box(&collapsed),
                black_box(&scattered_text),
            );
            match spliced {
                Some(r) => black_box(r.green),
                None => black_box(parse(&scattered_text).cst.green().into_owned()),
            }
        });
    });

    // The cost of a miss. An unbalanced quote relexes the rest of the file as
    // string content, so no tier can prove a splice sound and the caller has
    // to full-parse after all.
    //
    // Only the rejected attempt is timed, not the parse that follows it: the
    // caller was going to pay that parse regardless, and a text with an
    // unterminated string is not the same parse workload as the original
    // (most of the file becomes one string token, so it parses several times
    // faster — timing the pair would flatter the fallback, not stress it).
    // What matters is that this row is a rounding error next to `full_parse`.
    let hazard = insert(ident_site(&base.src, "bump", 0.5), "\"");
    let hazard_text = hazard.apply(&base.src);
    assert!(
        reparse(
            &base.src,
            &base.green,
            &base.diagnostics,
            &hazard,
            &hazard_text
        )
        .is_none(),
        "an unbalanced quote should reject"
    );
    group.bench_function("rejected_attempt", |b| {
        b.iter(|| {
            black_box(reparse(
                &base.src,
                &base.green,
                &base.diagnostics,
                black_box(&hazard),
                black_box(&hazard_text),
            ))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_reparse);
criterion_main!(benches);
