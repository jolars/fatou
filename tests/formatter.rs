//! Formatter fixtures.
//!
//! Two invariants over `tests/fixtures/formatter/<slug>/`:
//!
//! - **Gate** (`formatter_fixtures_match_expected`): every fixture that has a
//!   hand-authored `expected.jl` must format to it exactly
//!   (`format(input.jl) == expected.jl`). A fixture without `expected.jl` is not
//!   yet in the gate — its construct is still being authored. Presence of
//!   `expected.jl` *is* gate membership; there is no allowlist.
//! - **Stability** (`formatter_is_idempotent_and_stable`): over every fixture's
//!   `input.jl`, formatting is idempotent (`format(format(x)) == format(x)`) and
//!   its output parses cleanly (no parse diagnostics). This holds for *all*
//!   inputs, gated or not, so it guards against mangling any curated input as
//!   rules land.
//!
//! `expected.jl` is hand-authored under Tenet 1 (deterministic full reflow),
//! never captured from an external formatter. Grow the corpus with the
//! `formatter` skill.

use std::fs;
use std::path::{Path, PathBuf};

use fatou::formatter::format;
use fatou::parser::parse;

fn fixture_dirs() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/formatter");
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read formatter fixtures dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    cases
}

fn slug(case: &Path) -> String {
    case.file_name().unwrap().to_string_lossy().to_string()
}

/// Gate: every fixture with an `expected.jl` must format to it exactly.
#[test]
fn formatter_fixtures_match_expected() {
    let mut failures = Vec::new();
    for case in fixture_dirs() {
        let expected_path = case.join("expected.jl");
        if !expected_path.is_file() {
            continue; // not yet in the gate
        }
        let name = slug(&case);
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");
        let expected = fs::read_to_string(&expected_path).expect("read expected.jl");

        match format(&input) {
            Ok(formatted) if formatted == expected => {}
            Ok(_) => failures.push(name),
            Err(_) => failures.push(format!("{name} (format error)")),
        }
    }
    assert!(
        failures.is_empty(),
        "formatter fixtures diverge from expected.jl: {failures:?}"
    );
}

/// Stability: formatting is idempotent and its output parses cleanly, over every
/// `input.jl` (gated or not).
#[test]
fn formatter_is_idempotent_and_stable() {
    for case in fixture_dirs() {
        let name = slug(&case);
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");

        let once = format(&input).expect("format input");
        let twice = format(&once).expect("format formatted");
        assert_eq!(twice, once, "format is not idempotent for `{name}`");

        let reparsed = parse(&once);
        assert!(
            reparsed.diagnostics.is_empty(),
            "formatted output of `{name}` does not parse cleanly: {:?}",
            reparsed.diagnostics
        );
    }
}
