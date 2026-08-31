//! CLI-level tests for `fatou debug format`, the invariant-check command the
//! smoke-test workflow (`.github/workflows/smoke-test.yml`) drives per file.
//!
//! These pin the output contracts the workflow greps — the parenthesized
//! failure labels, the report header, and the sanitized dump-file names — by
//! running the real binary (`CARGO_BIN_EXE_fatou`). Every invocation passes
//! `--no-config` (except the config test) so a developer's own `fatou.toml`
//! cannot leak in, and points the home and config directories at the test's own
//! temp dir so the global user config cannot either.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const SMOKE_TEST_WORKFLOW: &str = include_str!("../.github/workflows/smoke-test.yml");

fn fatou(dir: &Path, args: &[&str]) -> Output {
    fatou_with_config(dir, &[&["--no-config"], args].concat())
}

/// Like [`fatou`] but without `--no-config`, for the config-discovery test.
fn fatou_with_config(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fatou"))
        .args(args)
        .current_dir(dir)
        // Sandbox every candidate global-config location inside the temp dir
        // (`$XDG_CONFIG_HOME`, `$HOME/.config`, and the platform config dir,
        // which derive from `$HOME`/`%APPDATA%`).
        .env("XDG_CONFIG_HOME", dir.join("xdg-config"))
        .env("HOME", dir)
        .env("APPDATA", dir)
        .env_remove("FATOU_CONFIG")
        .output()
        .expect("run fatou")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn passing_file_exits_zero() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.jl"), "x = 1\n").unwrap();

    let output = fatou(dir.path(), &["debug", "format", "--checks", "all", "ok.jl"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("All checks passed (checks: all, files: 1)"));
}

#[test]
fn report_on_passing_file_has_no_failure_headings() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.jl"), "x = 1\n").unwrap();

    let output = fatou(dir.path(), &["debug", "format", "--report", "ok.jl"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("# Debug-format regression report"));
    assert!(report.contains("All checks passed."));
    assert!(!report.contains("(idempotency)"));
    assert!(!report.contains("(losslessness)"));
}

#[test]
fn dump_passes_writes_sanitized_artifact_names() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("sub dir")).unwrap();
    let source = "f(x) = x + 1\n";
    std::fs::write(dir.path().join("sub dir/a b.jl"), source).unwrap();

    let output = fatou(
        dir.path(),
        &[
            "debug",
            "format",
            "--dump-dir",
            "dumps",
            "--dump-passes",
            "sub dir/a b.jl",
        ],
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    // The stem is the path as passed, sanitized exactly like the workflow's
    // `sed 's/[^[:alnum:]._-]/_/g'` — this is the artifact-lookup contract.
    let dumps = dir.path().join("dumps");
    let input = std::fs::read_to_string(dumps.join("sub_dir_a_b.jl.idempotency.input.txt"))
        .expect("input dump exists");
    let once = std::fs::read_to_string(dumps.join("sub_dir_a_b.jl.idempotency.once.txt"))
        .expect("once dump exists");
    let twice = std::fs::read_to_string(dumps.join("sub_dir_a_b.jl.idempotency.twice.txt"))
        .expect("twice dump exists");
    assert_eq!(input, source);
    assert_eq!(once, twice);
    assert!(dumps.join("sub_dir_a_b.jl.losslessness.input.txt").exists());
    assert!(
        dumps
            .join("sub_dir_a_b.jl.losslessness.parsed.txt")
            .exists()
    );
}

#[test]
fn dump_passes_requires_dump_dir() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.jl"), "x = 1\n").unwrap();

    let output = fatou(dir.path(), &["debug", "format", "--dump-passes", "ok.jl"]);

    assert_eq!(output.status.code(), Some(2), "clap usage error expected");
}

#[test]
fn format_error_never_reads_as_an_invariant_failure() {
    let dir = TempDir::new().unwrap();
    // An unclosed parenthesis parses with diagnostics, so the idempotency
    // invariant cannot be evaluated: a `format-error`, not an idempotency or
    // losslessness regression.
    std::fs::write(dir.path().join("bad.jl"), "f(x\n").unwrap();

    let output = fatou(dir.path(), &["debug", "format", "bad.jl"]);

    assert_eq!(output.status.code(), Some(1));
    let log = stderr(&output).to_lowercase();
    assert!(log.contains("(format-error)"), "log: {log}");
    assert!(!log.contains("idempot"), "log: {log}");
    assert!(!log.contains("lossless"), "log: {log}");

    let output = fatou(dir.path(), &["debug", "format", "--report", "bad.jl"]);
    assert_eq!(output.status.code(), Some(1));
    let report = stdout(&output);
    assert!(
        report.contains("### 1. `bad.jl` (format-error)"),
        "report: {report}"
    );
    assert!(report.contains("- Files checked: 1"));
}

#[test]
fn broken_config_errors_mention_fatou_toml_and_no_config_bypasses() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("ok.jl"), "x = 1\n").unwrap();
    std::fs::write(
        dir.path().join("fatou.toml"),
        "[format]\nline-width = \"wide\"\n",
    )
    .unwrap();

    // The smoke-test workflow retries with `--no-config` when a scanned repo's
    // own config is broken; its detection is `grep -Fq 'fatou.toml'` over the
    // log, so the error message must name the config path.
    let output = fatou_with_config(dir.path(), &["debug", "format", "ok.jl"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("fatou.toml"),
        "stderr: {}",
        stderr(&output)
    );

    let output = fatou_with_config(dir.path(), &["--no-config", "debug", "format", "ok.jl"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn smoke_artifact_excludes_upstream_clones() {
    assert!(SMOKE_TEST_WORKFLOW.contains("REPOS_DIR=\"$RUNNER_TEMP/fatou-debug-format-repos\""));
    assert!(!SMOKE_TEST_WORKFLOW.contains("REPOS_DIR=\"$RESULTS_DIR/repos\""));
}
