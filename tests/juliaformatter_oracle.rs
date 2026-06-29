//! JuliaFormatter.jl differential formatter oracle.
//!
//! Formats each fixture's `input.jl` with Fatou and diffs the result against the
//! pinned `expected.jl` captured from JuliaFormatter (`JuliaFormatter.format_text`,
//! DefaultStyle). This is a **direct-parity** gate:
//! `format(input) == juliaformatter(input)` (see `AGENTS.md`). Where Fatou
//! deliberately diverges (Tenet 1: deterministic layout vs constructs not yet at
//! parity), the case is recorded in `juliaformatter-blocked.txt` with a
//! rationale, never silently.
//!
//! Layout:
//! - Corpus: `tests/fixtures/formatter/<slug>/` with `input.jl` + `expected.jl`
//!   (the latter pinned by `scripts/update-juliaformatter-corpus.sh`). The pinned
//!   tool versions live in `tests/fixtures/formatter/.juliaformatter-source`.
//! - Allowlist: `tests/oracle/juliaformatter-allowlist.txt` — slugs at parity
//!   with JuliaFormatter, guarded against regression.
//! - Blocked list: `tests/oracle/juliaformatter-blocked.txt` — slugs deliberately
//!   diverging or not yet supported, each with a one-line rationale.
//!
//! `allowlist ∪ blocked` must cover the whole corpus (`juliaformatter_corpus_fully_triaged`).
//!
//! Two test entry points:
//! - `juliaformatter_allowlist`: fails if any allowlisted slug regresses. Runs
//!   with no Julia dependency (the corpus is pinned), so it is CI-safe.
//! - `juliaformatter_full_report` (ignored by default): runs every case and
//!   writes a triage summary to `tests/oracle/juliaformatter-report.txt`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use fatou::formatter::format;

const CORPUS_REL: &str = "tests/fixtures/formatter";
const ALLOWLIST_REL: &str = "tests/oracle/juliaformatter-allowlist.txt";
const BLOCKED_REL: &str = "tests/oracle/juliaformatter-blocked.txt";
const REPORT_REL: &str = "tests/oracle/juliaformatter-report.txt";

struct Case {
    slug: String,
    input: String,
    expected: String,
}

fn manifest_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read_corpus() -> Vec<Case> {
    let dir = manifest_path(CORPUS_REL);
    let mut cases = Vec::new();
    for entry in fs::read_dir(&dir).expect("read formatter corpus dir") {
        let entry = entry.expect("read corpus entry");
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        let input_path = entry.path().join("input.jl");
        let expected_path = entry.path().join("expected.jl");
        if !input_path.is_file() || !expected_path.is_file() {
            continue;
        }
        cases.push(Case {
            slug,
            input: fs::read_to_string(&input_path).expect("read input.jl"),
            expected: fs::read_to_string(&expected_path).expect("read expected.jl"),
        });
    }
    cases.sort_by(|a, b| a.slug.cmp(&b.slug));
    cases
}

/// Whether Fatou's formatting of this case matches JuliaFormatter's pinned output.
fn matches(case: &Case) -> bool {
    match format(&case.input) {
        Ok(formatted) => formatted == case.expected,
        Err(_) => false,
    }
}

fn read_slug_file(rel: &str) -> BTreeSet<String> {
    let path = manifest_path(rel);
    let Ok(content) = fs::read_to_string(&path) else {
        return BTreeSet::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[test]
fn corpus_is_present() {
    let cases = read_corpus();
    assert!(
        !cases.is_empty(),
        "formatter corpus is empty; check {CORPUS_REL} and run scripts/update-juliaformatter-corpus.sh"
    );
}

#[test]
fn juliaformatter_allowlist_and_blocked_are_disjoint() {
    let allow = read_slug_file(ALLOWLIST_REL);
    let blocked = read_slug_file(BLOCKED_REL);
    let overlap: Vec<_> = allow.intersection(&blocked).collect();
    assert!(
        overlap.is_empty(),
        "slugs in both allowlist and blocked: {overlap:?}"
    );
}

/// Every corpus slug must be classified as either allowlisted or blocked — no
/// case may sit untriaged (the dir-corpus opt-out discipline). A new fixture
/// forces a deliberate accept-or-record decision.
#[test]
fn juliaformatter_corpus_fully_triaged() {
    let allow = read_slug_file(ALLOWLIST_REL);
    let blocked = read_slug_file(BLOCKED_REL);
    let untriaged: Vec<String> = read_corpus()
        .into_iter()
        .map(|c| c.slug)
        .filter(|slug| !allow.contains(slug) && !blocked.contains(slug))
        .collect();
    assert!(
        untriaged.is_empty(),
        "untriaged formatter fixtures (add to juliaformatter-allowlist.txt or juliaformatter-blocked.txt): {untriaged:?}\n\
         re-run `cargo test --test juliaformatter_oracle -- --ignored juliaformatter_full_report` to triage"
    );
}

/// Guard against regressions: every allowlisted slug must still match JuliaFormatter.
#[test]
fn juliaformatter_allowlist() {
    let allowed = read_slug_file(ALLOWLIST_REL);
    if allowed.is_empty() {
        return; // baseline still being seeded
    }

    let cases = read_corpus();
    let by_slug: std::collections::HashMap<&str, &Case> =
        cases.iter().map(|c| (c.slug.as_str(), c)).collect();

    let mut regressions = Vec::new();
    for slug in &allowed {
        match by_slug.get(slug.as_str()) {
            Some(case) => {
                if !matches(case) {
                    regressions.push(slug.clone());
                }
            }
            None => panic!("allowlisted slug {slug:?} has no corpus case"),
        }
    }

    assert!(
        regressions.is_empty(),
        "allowlisted formatter cases regressed: {regressions:?}\n\
         re-run `cargo test --test juliaformatter_oracle -- --ignored juliaformatter_full_report` to triage"
    );
}

/// Full triage run (ignored by default): formats every case, writes a report,
/// and prints a summary. Use it to seed `juliaformatter-allowlist.txt` /
/// `juliaformatter-blocked.txt`.
#[test]
#[ignore = "diagnostic/triage run; writes tests/oracle/juliaformatter-report.txt"]
fn juliaformatter_full_report() {
    let cases = read_corpus();
    let allowed = read_slug_file(ALLOWLIST_REL);
    let blocked = read_slug_file(BLOCKED_REL);

    let mut report = String::new();
    let (mut pass, mut fail) = (0u32, 0u32);

    for case in &cases {
        let status = if matches(case) {
            pass += 1;
            "PASS"
        } else {
            fail += 1;
            "FAIL"
        };
        let tag = if allowed.contains(&case.slug) {
            " [allow]"
        } else if blocked.contains(&case.slug) {
            " [blocked]"
        } else {
            " [untriaged]"
        };
        report.push_str(&format!("{status:<5} {}{tag}\n", case.slug));
    }

    report.push_str(&format!(
        "\n{} cases: {pass} pass ({} allowlisted), {fail} divergence ({} blocked)\n",
        cases.len(),
        allowed.len(),
        blocked.len(),
    ));

    fs::write(manifest_path(REPORT_REL), &report).expect("write juliaformatter-report.txt");
    eprint!("{report}");
}
