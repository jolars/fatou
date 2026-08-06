//! CLI-level tests for `fatou format --check` output, and for the global
//! `--quiet` that gates it.
//!
//! `--check` writes nothing, so its output is the only account of what would
//! change — hence the diff by default, and hence `--quiet` still printing the
//! file list rather than going silent. These run the real binary
//! (`CARGO_BIN_EXE_fatou`) with the config-discovery environment sandboxed
//! inside the temp dir, so a developer's own `fatou.toml` cannot leak in.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// Extra spaces the formatter collapses, so `--check` always has something to
/// report.
const UNFORMATTED: &str = "y  =  2\n";

fn sandbox() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("input.jl"), UNFORMATTED).unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fatou"))
        .args(args)
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", dir.join("xdg-config"))
        .env("HOME", dir)
        .env("APPDATA", dir)
        .env_remove("FATOU_CONFIG")
        .output()
        .expect("run fatou")
}

#[test]
fn check_prints_the_diff_by_default() {
    let dir = sandbox();

    let output = run(dir.path(), &["format", "--check", "input.jl"]);

    assert!(!output.status.success(), "unformatted file should exit 1");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("would reformat"),
        "expected the file list, got:\n{stdout}"
    );
    assert!(
        stdout.contains("-y  =  2") && stdout.contains("+y = 2"),
        "expected both diff sides, got:\n{stdout}"
    );
}

#[test]
fn quiet_check_lists_files_without_the_diff() {
    let dir = sandbox();

    let output = run(dir.path(), &["format", "--check", "--quiet", "input.jl"]);

    assert!(!output.status.success(), "unformatted file should exit 1");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("would reformat"),
        "quiet is not silent: the file list survives, got:\n{stdout}"
    );
    assert!(
        stdout.contains("1 of 1 file(s) would be reformatted"),
        "expected the summary, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("-y  =  2"),
        "the diff should be suppressed, got:\n{stdout}"
    );
}

#[test]
fn quiet_check_is_silent_when_formatted() {
    let dir = sandbox();
    std::fs::write(dir.path().join("input.jl"), "y = 2\n").unwrap();

    let output = run(dir.path(), &["format", "--check", "--quiet", "input.jl"]);

    assert!(output.status.success(), "formatted file should exit 0");
    // No changed files means no list and no summary — nothing to report.
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
}

/// `--quiet` moved from `parse`-local to global so `format` could share it.
/// Globals accept the flag after the subcommand too, so this spelling — the one
/// that already existed — must keep working and keep its meaning.
#[test]
fn parse_quiet_still_suppresses_the_cst() {
    let dir = sandbox();

    let output = run(dir.path(), &["parse", "--quiet", "input.jl"]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
}
