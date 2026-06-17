//! JuliaSyntax.jl differential parser oracle.
//!
//! Projects each fixture's Fatou CST into a JuliaSyntax-native s-expression
//! (via [`fatou::parser::to_juliasyntax_sexpr`]) and diffs it, whitespace-
//! normalized, against the pinned `expected.sexpr` captured from JuliaSyntax.
//!
//! Layout:
//! - Corpus: `tests/fixtures/oracle/<slug>/` with `input.jl` + `expected.sexpr`
//!   (the latter pinned by `scripts/update-juliasyntax-corpus.sh`). The pinned
//!   tool versions live in `tests/fixtures/oracle/.juliasyntax-source`.
//! - Allowlist: `tests/oracle/allowlist.txt` — slugs guarded against regression.
//! - Blocked list: `tests/oracle/blocked.txt` — slugs deliberately deferred,
//!   each with a one-line rationale.
//!
//! Two test entry points:
//! - `oracle_allowlist`: fails if any allowlisted slug regresses. Runs with no
//!   Julia dependency (the corpus is pinned), so it is CI-safe.
//! - `oracle_full_report` (ignored by default): runs every case and writes a
//!   triage summary to `tests/oracle/report.txt`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use fatou::parser::{normalize_sexpr, parse, to_juliasyntax_sexpr};

const CORPUS_REL: &str = "tests/fixtures/oracle";
const ALLOWLIST_REL: &str = "tests/oracle/allowlist.txt";
const BLOCKED_REL: &str = "tests/oracle/blocked.txt";
const REPORT_REL: &str = "tests/oracle/report.txt";

/// The harvested JuliaSyntax sub-corpus (`scripts/harvest-juliasyntax-corpus.jl`):
/// ~575 micro-cases from JuliaSyntax's own `test/parser.jl`, one JSON object per
/// line. Gated **opt-in** (like the dir corpus): every allowlisted slug must
/// match Julia, guarding against regression. The full report's divergence and
/// unsupported buckets are the prioritizable backlog for growing parser parity.
const JS_CORPUS_REL: &str = "tests/fixtures/oracle/juliasyntax.jsonl";
const JS_ALLOWLIST_REL: &str = "tests/oracle/juliasyntax-allowlist.txt";
const JS_REPORT_REL: &str = "tests/oracle/juliasyntax-report.txt";

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
    for entry in fs::read_dir(&dir).expect("read oracle corpus dir") {
        let entry = entry.expect("read corpus entry");
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        let input_path = entry.path().join("input.jl");
        let expected_path = entry.path().join("expected.sexpr");
        if !input_path.is_file() || !expected_path.is_file() {
            continue;
        }
        cases.push(Case {
            slug,
            input: fs::read_to_string(&input_path).expect("read input.jl"),
            expected: fs::read_to_string(&expected_path).expect("read expected.sexpr"),
        });
    }
    cases.sort_by(|a, b| a.slug.cmp(&b.slug));
    cases
}

fn read_js_corpus() -> Vec<Case> {
    let path = manifest_path(JS_CORPUS_REL);
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let v: serde_json::Value =
                serde_json::from_str(line).expect("parse juliasyntax.jsonl line");
            Case {
                slug: v["slug"].as_str().expect("slug").to_string(),
                input: v["input"].as_str().expect("input").to_string(),
                expected: v["expected"].as_str().expect("expected").to_string(),
            }
        })
        .collect()
}

/// Outcome of projecting one case against its pinned expected s-expression.
#[derive(PartialEq, Eq)]
enum Outcome {
    /// Fatou parses cleanly and the projection matches Julia.
    Pass,
    /// Fatou parses cleanly but the projection diverges from Julia.
    Fail,
    /// Fatou reports parse diagnostics — unsupported syntax (the growth frontier).
    Unsupported,
}

fn outcome(case: &Case) -> Outcome {
    match render(case) {
        None => Outcome::Unsupported,
        Some(rendered) if rendered == normalize_sexpr(&case.expected) => Outcome::Pass,
        Some(_) => Outcome::Fail,
    }
}

/// Project the case's CST to a normalized s-expression. `None` if Fatou reports
/// parse diagnostics — those cases are deferred until error-shape parity exists.
fn render(case: &Case) -> Option<String> {
    let output = parse(&case.input);
    if !output.diagnostics.is_empty() {
        return None;
    }
    Some(normalize_sexpr(&to_juliasyntax_sexpr(&output.cst)))
}

fn matches(case: &Case) -> bool {
    match render(case) {
        Some(rendered) => rendered == normalize_sexpr(&case.expected),
        None => false,
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
        "oracle corpus is empty; check {CORPUS_REL} and run scripts/update-juliasyntax-corpus.sh"
    );
}

#[test]
fn allowlist_and_blocked_are_disjoint() {
    let allow = read_slug_file(ALLOWLIST_REL);
    let blocked = read_slug_file(BLOCKED_REL);
    let overlap: Vec<_> = allow.intersection(&blocked).collect();
    assert!(
        overlap.is_empty(),
        "slugs in both allowlist and blocked: {overlap:?}"
    );
}

/// Guard against regressions: every allowlisted slug must still match.
#[test]
fn oracle_allowlist() {
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
        "allowlisted oracle cases regressed: {regressions:?}\n\
         re-run `cargo test --test juliasyntax_oracle -- --ignored oracle_full_report` to triage"
    );
}

/// Opt-in parity gate over the harvested JuliaSyntax sub-corpus: every
/// allowlisted slug must still match Julia. Also guards the allowlist's own
/// hygiene (no slugs absent from the corpus). CI-safe — the corpus is pinned, so
/// no Julia is needed.
#[test]
fn oracle_juliasyntax() {
    let allowed = read_slug_file(JS_ALLOWLIST_REL);
    if allowed.is_empty() {
        return; // baseline still being seeded
    }
    let cases = read_js_corpus();
    let by_slug: std::collections::HashMap<&str, &Case> =
        cases.iter().map(|c| (c.slug.as_str(), c)).collect();

    let mut regressions = Vec::new();
    let mut missing = Vec::new();
    for slug in &allowed {
        match by_slug.get(slug.as_str()) {
            Some(case) if outcome(case) != Outcome::Pass => regressions.push(slug.clone()),
            Some(_) => {}
            None => missing.push(slug.clone()),
        }
    }

    let mut problems = String::new();
    if !regressions.is_empty() {
        problems.push_str(&format!(
            "\n  {} allowlisted case(s) regressed: {regressions:?}",
            regressions.len()
        ));
    }
    if !missing.is_empty() {
        problems.push_str(&format!(
            "\n  {} allowlisted slug(s) absent from the corpus: {missing:?}",
            missing.len()
        ));
    }
    assert!(
        problems.is_empty(),
        "JuliaSyntax parity gate:{problems}\n\
         re-run `cargo test --test juliasyntax_oracle -- --ignored juliasyntax_full_report` to triage"
    );
}

/// Triage run for the harvested JuliaSyntax sub-corpus (ignored by default):
/// writes a per-case report and a summary (pass / divergence / unsupported), so
/// divergences can be blocked and the unsupported frontier prioritized.
#[test]
#[ignore = "diagnostic/triage run; writes tests/oracle/juliasyntax-report.txt"]
fn juliasyntax_full_report() {
    let cases = read_js_corpus();
    let allowed = read_slug_file(JS_ALLOWLIST_REL);

    let (mut pass, mut fail, mut unsupported) = (0u32, 0u32, 0u32);
    let mut lines = Vec::new();
    for case in &cases {
        let status = match outcome(case) {
            Outcome::Pass => {
                pass += 1;
                "PASS"
            }
            Outcome::Fail => {
                fail += 1;
                "FAIL"
            }
            Outcome::Unsupported => {
                unsupported += 1;
                "UNSUPPORTED"
            }
        };
        let tag = if allowed.contains(&case.slug) {
            " [allow]"
        } else {
            ""
        };
        // Keep the input on the line (escaped) so divergences are diagnosable.
        lines.push(format!(
            "{status:<12} {}{tag}  {:?}",
            case.slug,
            case.input.replace('\n', "\\n")
        ));
    }
    lines.sort();

    let mut report = lines.join("\n");
    report.push_str(&format!(
        "\n\n{} cases: {pass} pass ({} allowlisted), {fail} divergence, {unsupported} unsupported (frontier)\n",
        cases.len(),
        allowed.len(),
    ));
    fs::write(manifest_path(JS_REPORT_REL), &report).expect("write juliasyntax-report.txt");
    eprint!("{report}");
}

/// Full triage run (ignored by default): renders every case, writes a report,
/// and prints a summary. Use it to seed `allowlist.txt` / `blocked.txt`.
#[test]
#[ignore = "diagnostic/triage run; writes tests/oracle/report.txt"]
fn oracle_full_report() {
    let cases = read_corpus();
    let allowed = read_slug_file(ALLOWLIST_REL);
    let blocked = read_slug_file(BLOCKED_REL);

    let mut report = String::new();
    let (mut pass, mut fail, mut skip) = (0u32, 0u32, 0u32);

    for case in &cases {
        let status = match render(case) {
            None => {
                skip += 1;
                "SKIP (parse diagnostics)"
            }
            Some(rendered) if rendered == normalize_sexpr(&case.expected) => {
                pass += 1;
                "PASS"
            }
            Some(_) => {
                fail += 1;
                "FAIL"
            }
        };
        let tag = if allowed.contains(&case.slug) {
            " [allow]"
        } else if blocked.contains(&case.slug) {
            " [blocked]"
        } else {
            " [untriaged]"
        };
        report.push_str(&format!("{status:<24} {}{tag}\n", case.slug));
    }

    let summary = format!(
        "\n{} cases: {pass} pass, {fail} fail, {skip} skipped\n",
        cases.len()
    );
    report.push_str(&summary);

    fs::write(manifest_path(REPORT_REL), &report).expect("write report.txt");
    eprint!("{report}");
}
