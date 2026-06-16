//! Formatter fixtures: `format(input) == expected`, and idempotence.
//!
//! In the groundwork phase the formatter is a lossless passthrough, so each
//! fixture's `expected.jl` equals its `input.jl`. As real rules land, the
//! fixtures gain genuinely reformatted expectations.

use std::fs;
use std::path::{Path, PathBuf};

use fatou::formatter::format;

fn fixture_cases() -> Vec<PathBuf> {
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

#[test]
fn formatter_fixtures() {
    for case in fixture_cases() {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");
        let expected = fs::read_to_string(case.join("expected.jl")).expect("read expected.jl");

        let formatted = format(&input).expect("format input");
        assert_eq!(formatted, expected, "format mismatch for `{name}`");

        let again = format(&formatted).expect("format formatted");
        assert_eq!(again, formatted, "format is not idempotent for `{name}`");
    }
}
