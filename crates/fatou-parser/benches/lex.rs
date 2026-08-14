//! Lexer throughput, the check on the operator table's scan cost.
//!
//! `lex_corpus` is the honest workload: ~100 KB of real Julia, where operators
//! are a minority of the tokens. `lex_operators` is the adversarial one: a
//! stream that is *nothing but* operator tokens, so a regression in
//! `lex_operator_or_unknown` shows up undiluted. `lex_field_access` is the
//! case the operator table's first-byte grouping most has to earn — `a.b`,
//! where a `.` must fall through the whole broadcast-operator group to the
//! lone `Dot`.
//!
//! Measured on ~101 KB of JuliaSyntax (2026-08-13, release), before and after
//! the arm ladder in `lex_operator_or_unknown` became the `OPS` table:
//!
//! ```text
//!                    ladder     table
//! lex_corpus         1.016 ms   0.994 ms
//! lex_operators      1.148 ms   1.321 ms
//! lex_field_access     661 us     686 us
//! ```
//!
//! The table costs a linear scan of one first-byte group where the ladder
//! walked a decision tree, and `lex_operators` — 100% operator tokens, a
//! shape no real source has — is where that shows, at +15%. Real code does not
//! pay it: the group is skipped outright unless the *next* byte can continue a
//! multi-byte spelling, which is why the corpus row does not move.
//!
//! Run it with `task bench-lex`, or:
//!
//! ```sh
//! cargo bench -p fatou-parser --features bench --bench lex
//! ```
//!
//! See `benches/reparse.rs` for why the `bench` feature exists at all. The
//! corpus is `bench/corpus/JuliaSyntax`, git-ignored and fetched by
//! `bench/corpus/download.sh`; without it the corpus row is skipped.

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};

use fatou_parser::parser::{token_count, token_count_rope};

/// The same target the reparse bench uses, so the two rows are comparable.
const TARGET_BYTES: usize = 100 * 1024;

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/corpus/JuliaSyntax/src")
}

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

/// ~64 KB of operators and nothing else, spanning every spelling length the
/// table holds (1 to 5 bytes) and both the ASCII and the Unicode paths.
fn operator_soup() -> String {
    const SPELLINGS: &[&str] = &[
        ".>>>=", ".<-->", "<-->", ">>>=", ".===", ".//=", "-->", "===", "//=", "+%=", ".==", ".&&",
        ".<:", "==", "=>", "::", "<:", "|>", "++", "+%", ".+", ".|", "+", "-", "*", "(", ")", ",",
        ";", ".", ":", "[", "]", "÷=", "⊻", "×", "−", "√",
    ];
    let mut src = String::with_capacity(64 * 1024);
    let mut i = 0;
    while src.len() < 64 * 1024 {
        src.push_str(SPELLINGS[i % SPELLINGS.len()]);
        src.push(' ');
        i += 1;
    }
    src
}

/// ~64 KB of `a.b` field access: a `.` whose next byte is an identifier, the
/// shape that has to leave the `.` group without matching anything longer.
fn field_access() -> String {
    let mut src = String::with_capacity(64 * 1024);
    while src.len() < 64 * 1024 {
        src.push_str("value.field.inner\n");
    }
    src
}

fn bench_lex(c: &mut Criterion) {
    let mut group = c.benchmark_group("lex");

    match load_corpus() {
        Some(src) => {
            group.throughput(criterion::Throughput::Bytes(src.len() as u64));
            group.bench_function("lex_corpus", |b| {
                b.iter(|| black_box(token_count(black_box(&src))));
            });
            // The same tokenize over a multi-chunk rope (the LSP path), which the
            // flat `&str` row above cannot see.
            let rope = ropey::Rope::from_str(&src);
            assert!(rope.chunks().count() > 1, "corpus must be multi-chunk");
            group.bench_function("lex_corpus_rope", |b| {
                b.iter(|| black_box(token_count_rope(black_box(&rope))));
            });
        }
        None => eprintln!(
            "lex bench: corpus missing at {}\n\
             run `bash bench/corpus/download.sh` first; other rows still run",
            corpus_dir().display()
        ),
    }

    let soup = operator_soup();
    group.throughput(criterion::Throughput::Bytes(soup.len() as u64));
    group.bench_function("lex_operators", |b| {
        b.iter(|| black_box(token_count(black_box(&soup))));
    });

    let fields = field_access();
    group.throughput(criterion::Throughput::Bytes(fields.len() as u64));
    group.bench_function("lex_field_access", |b| {
        b.iter(|| black_box(token_count(black_box(&fields))));
    });

    group.finish();
}

criterion_group!(benches, bench_lex);
criterion_main!(benches);
