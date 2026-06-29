//! Formatter fixtures: the universal **idempotence** invariant over every
//! fixture (`format(format(x)) == format(x)`), regardless of whether the case is
//! at JuliaFormatter parity. Direct JuliaFormatter parity
//! (`format(input) == expected.jl`) is the oracle gate, owned by
//! `juliaformatter_oracle.rs` and partitioned by the allowlist/blocked files —
//! so a backlog fixture whose `expected.jl` Fatou cannot yet reproduce belongs
//! there, not here.

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
fn formatter_is_idempotent() {
    for case in fixture_cases() {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");

        let once = format(&input).expect("format input");
        let twice = format(&once).expect("format formatted");
        assert_eq!(twice, once, "format is not idempotent for `{name}`");
    }
}
