//! Snapshot the CST + diagnostics for each parser fixture, and assert the
//! losslessness invariant (`reconstruct(input) == input`) on every case.

use std::fs;
use std::path::{Path, PathBuf};

use fatou::parser::{parse, reconstruct};

fn fixture_cases() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parser");
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read parser fixtures dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    cases
}

#[test]
fn parser_fixtures() {
    for case in fixture_cases() {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let input = fs::read_to_string(case.join("input.jl")).expect("read input.jl");

        assert_eq!(
            reconstruct(&input),
            input,
            "losslessness failed for `{name}`"
        );

        let output = parse(&input);
        let mut rendered = format!("{:#?}", output.cst);
        for diag in &output.diagnostics {
            rendered.push_str(&format!(
                "\ndiagnostic [{}..{}]: {}",
                diag.start, diag.end, diag.message
            ));
        }
        insta::assert_snapshot!(name, rendered);
    }
}
