//! CLI-level tests for `fatou format --check` output, and for the global
//! `--quiet` that gates it.
//!
//! `--check` writes nothing, so its output is the only account of what would
//! change — hence the diff by default, and hence `--quiet` still printing the
//! file list rather than going silent. These run the real binary
//! (`CARGO_BIN_EXE_fatou`) with the config-discovery environment sandboxed
//! inside the temp dir, so a developer's own `fatou.toml` cannot leak in.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// Extra spaces the formatter collapses, so `--check` always has something to
/// report.
const UNFORMATTED: &str = "y  =  2\n";

fn sandbox() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("input.jl"), UNFORMATTED).unwrap();
    dir
}

/// Run the binary with `input` on stdin (`None` closes it: `/dev/null` is not a
/// terminal, so the interactive-input gate never fires and the run is the same
/// whether or not `cargo test` was started from a terminal).
fn run_stdin(dir: &Path, args: &[&str], input: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fatou"));
    cmd.args(args)
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", dir.join("xdg-config"))
        .env("HOME", dir)
        .env("APPDATA", dir)
        .env_remove("FATOU_CONFIG")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("run fatou");
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    child.wait_with_output().expect("wait for fatou")
}

fn run(dir: &Path, args: &[&str]) -> Output {
    run_stdin(dir, args, None)
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

/// The positional-input contract: `-` is the explicit stdin spelling, an
/// implicit (piped) stdin still works, and neither can be mixed with paths. The
/// gated case — no paths at an interactive terminal — is a usage error rather
/// than a silent wait; it needs a pty to reproduce, so the decision itself is
/// unit-tested in `main.rs` (`resolve_inputs`).
#[test]
fn dash_formats_stdin_to_stdout() {
    let dir = sandbox();

    let output = run_stdin(dir.path(), &["format", "-"], Some(UNFORMATTED));

    assert!(output.status.success(), "stdin should format cleanly");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "y = 2\n");
}

#[test]
fn piped_stdin_still_needs_no_dash() {
    // The pre-`-` spelling stays valid: a pipe is not a terminal, so nothing a
    // script or CI step does today changes behavior.
    let dir = sandbox();

    let output = run_stdin(dir.path(), &["format"], Some(UNFORMATTED));

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "y = 2\n");
}

#[test]
fn dash_cannot_be_mixed_with_paths() {
    let dir = sandbox();

    let output = run(dir.path(), &["format", "-", "input.jl"]);

    // Clap's own usage-error exit code, so the message reads like any other
    // argument mistake.
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("cannot be combined with other paths"),
        "expected the conflict error, got:\n{stderr}"
    );
    // The named file must be left alone.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("input.jl")).unwrap(),
        UNFORMATTED
    );
}

#[test]
fn check_rejects_stdin() {
    // `--check` reports on files it leaves on disk; before, a piped `--check`
    // with no paths silently formatted to stdout and exited 0 instead.
    let dir = sandbox();

    for args in [&["format", "--check", "-"][..], &["format", "--check"][..]] {
        let output = run_stdin(dir.path(), args, Some(UNFORMATTED));
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("cannot read from stdin"),
            "args: {args:?}"
        );
    }
}

#[test]
fn lint_rejects_stdin_by_name() {
    // `lint` has no stdin pipeline, so `-` must not be walked as a file path.
    let dir = sandbox();

    let output = run(dir.path(), &["lint", "-"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cannot read from stdin")
    );
}
