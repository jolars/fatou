//! `ast(x) == ast(format(x))` over the parser's corpora.
//!
//! [`fatou_formatter::verify::ast_shape`] says whether formatting preserved the
//! program. `crates/fatou-formatter/tests/formatter.rs` applies it to the
//! formatter's own 151 fixtures; this applies it to the ~1000 inputs the
//! *parser* curates, which the formatter otherwise never runs on:
//!
//! - `crates/fatou-parser/tests/fixtures/oracle/<slug>/input.jl` — the pinned
//!   JuliaSyntax corpus.
//! - `crates/fatou-parser/tests/fixtures/oracle/juliasyntax.jsonl` — the 756
//!   micro-cases harvested from JuliaSyntax's own `test/parser.jl`.
//!
//! Those corpora exist to pin *parser* shape, so they reach far more grammar
//! than hand-authored formatter fixtures do — which is exactly why the formatter
//! defects this check found live here. The check needs no `expected` file, so
//! pointing it at them costs nothing.
//!
//! Cases outside the invariant's domain (no clean parse, or a projector
//! sentinel) are skipped by `ast_shape` itself; the corpora are full of
//! deliberate error-recovery inputs, and formatting an error tree has no defined
//! equivalence. A case whose *formatted output* stops parsing is a failure, not
//! a skip.
//!
//! `tests/ast-equivalence/known-drift.txt` lists the slugs whose shape the
//! formatter is known to move, grouped under a rationale comment, in the style
//! of `crates/fatou-parser/tests/oracle/blocked.txt`. Entries are checked for
//! staleness: an entry fails until it is removed once its slug stops drifting,
//! and equally once its slug stops being checked at all. The corpora are
//! regenerated — `js-*` slugs are content-derived hashes — so an entry whose
//! case was renamed away would otherwise linger unexamined forever.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use fatou::parser::parse;
use fatou_formatter::format;
use fatou_formatter::verify::{ast_shape, verify_format};

const DIR_CORPUS: &str = "crates/fatou-parser/tests/fixtures/oracle";
const JS_CORPUS: &str = "crates/fatou-parser/tests/fixtures/oracle/juliasyntax.jsonl";
const KNOWN_DRIFT: &str = "tests/ast-equivalence/known-drift.txt";

fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn known_drift() -> BTreeSet<String> {
    let Ok(content) = fs::read_to_string(repo_path(KNOWN_DRIFT)) else {
        return BTreeSet::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// `(slug, source)` for every case in both corpora.
fn cases() -> Vec<(String, String)> {
    let mut cases = Vec::new();

    let dir = repo_path(DIR_CORPUS);
    for entry in fs::read_dir(&dir).expect("read oracle corpus dir") {
        let entry = entry.expect("read corpus entry");
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let input = entry.path().join("input.jl");
        if !input.is_file() {
            continue;
        }
        cases.push((
            entry.file_name().to_string_lossy().to_string(),
            fs::read_to_string(&input).expect("read input.jl"),
        ));
    }

    if let Ok(content) = fs::read_to_string(repo_path(JS_CORPUS)) {
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("parse juliasyntax.jsonl line");
            cases.push((
                v["slug"].as_str().expect("slug").to_string(),
                v["input"].as_str().expect("input").to_string(),
            ));
        }
    }

    cases.sort();
    cases
}

#[test]
fn formatting_preserves_ast_shape_over_parser_corpora() {
    let known = known_drift();
    let (mut drifted, mut unparsable, mut unprojectable) = (Vec::new(), Vec::new(), Vec::new());
    let mut verification_failed = Vec::new();
    let mut stale = Vec::new();
    let mut exercised = BTreeSet::new();
    let (mut checked, mut skipped) = (0usize, 0usize);

    for (slug, source) in cases() {
        let Some(before) = ast_shape(&source) else {
            skipped += 1;
            continue; // out of domain: no clean parse, or a projector sentinel
        };
        let Ok(formatted) = format(&source) else {
            unparsable.push(format!(
                "{slug}: format() errored on a cleanly-parsing input"
            ));
            continue;
        };
        checked += 1;
        exercised.insert(slug.clone());

        if let Err(error) = verify_format(&source, &formatted) {
            verification_failed.push(format!("{slug}: {error}"));
        }

        match ast_shape(&formatted) {
            // The source parsed cleanly and projected, so a formatted text with
            // no shape is the formatter's doing — but `ast_shape` folds two
            // causes into `None`, and they point at different code. Reparse to
            // tell them apart instead of naming the more likely one.
            None if !parse(&formatted).diagnostics.is_empty() => {
                unparsable.push(format!("{slug}: {source:?} -> {formatted:?}"));
            }
            None => unprojectable.push(format!("{slug}: {source:?} -> {formatted:?}")),
            Some(after) if after == before => {
                if known.contains(&slug) {
                    stale.push(format!("{slug} (no longer drifts)"));
                }
            }
            Some(after) => {
                if !known.contains(&slug) {
                    drifted.push(format!(
                        "{slug}: {source:?} -> {formatted:?}\n     before: {before}\n     after:  {after}"
                    ));
                }
            }
        }
    }

    // An entry no case reached is as stale as one that stopped drifting: the
    // slug was renamed or dropped, and the exemption now hides nothing.
    stale.extend(
        known
            .difference(&exercised)
            .map(|slug| format!("{slug} (matches no case in either corpus)")),
    );

    assert!(
        checked > 500,
        "only {checked} cases were in domain ({skipped} skipped) — the corpora look truncated"
    );
    assert!(
        unparsable.is_empty(),
        "formatted output no longer parses cleanly for:\n  - {}",
        unparsable.join("\n  - ")
    );
    assert!(
        unprojectable.is_empty(),
        "formatted output parses but no longer projects — the formatter produced \
         a construct the projector cannot render, so this case is unverifiable:\n  - {}",
        unprojectable.join("\n  - ")
    );
    assert!(
        stale.is_empty(),
        "known-drift.txt entries no longer apply — remove them:\n  - {}",
        stale.join("\n  - ")
    );
    assert!(
        drifted.is_empty(),
        "formatting changed the program shape for:\n  - {}",
        drifted.join("\n  - ")
    );
    assert!(
        verification_failed.is_empty(),
        "safe formatting rejected comparable corpus cases:\n  - {}",
        verification_failed.join("\n  - ")
    );
}
