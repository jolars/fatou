//! CLI-level tests for config resolution order: project `fatou.toml`, then
//! `$FATOU_CONFIG`, then the global user config, then built-in defaults.
//!
//! These run the real binary (`CARGO_BIN_EXE_fatou`) with `$HOME`,
//! `$XDG_CONFIG_HOME`, and `%APPDATA%` pointed inside the test's temp dir, so a
//! developer's own global config can neither leak in nor be clobbered.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// A source line that fits in the default width of 92 but not in 40, so the
/// resolved `line-width` decides whether `format --check` reports a diff.
const WIDE_CALL: &str = "result = some_function(alpha, beta, gamma, delta, epsilon, zeta)\n";

struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let sandbox = Self {
            dir: TempDir::new().unwrap(),
        };
        std::fs::write(sandbox.path().join("input.jl"), WIDE_CALL).unwrap();
        sandbox
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Write a config file at `relative`, creating parent directories.
    fn write_config(&self, relative: &str, line_width: u32) -> std::path::PathBuf {
        let path = self.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("[format]\nline-width = {line_width}\n")).unwrap();
        path
    }

    /// Create a git repository at `relative` holding its own `input.jl`, and
    /// return its path. Only the `.git` marker matters, so no real repository
    /// is initialized.
    fn write_repo(&self, relative: &str) -> std::path::PathBuf {
        let repo = self.path().join(relative);
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("input.jl"), WIDE_CALL).unwrap();
        repo
    }

    /// Run fatou in `cwd` with the sandboxed environment, plus any extra env
    /// vars, and the given arguments.
    fn run(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fatou"));
        command
            .args(args)
            .current_dir(cwd)
            // Sandbox every candidate global-config location inside the temp
            // dir (`$XDG_CONFIG_HOME`, `$HOME/.config`, and the platform config
            // dir, which derives from `$HOME`/`%APPDATA%`).
            .env("XDG_CONFIG_HOME", self.path().join("xdg-config"))
            .env("HOME", self.path())
            .env("APPDATA", self.path())
            .env_remove("FATOU_CONFIG");
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run fatou")
    }

    /// Run `fatou format --check input.jl` at the sandbox root, with `args`
    /// passed ahead of the subcommand as global flags.
    fn format_check(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.format_check_in(self.path(), env, args)
    }

    /// [`format_check`](Self::format_check) from a chosen working directory.
    fn format_check_in(&self, cwd: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.run(
            cwd,
            env,
            &[args, &["format", "--check", "input.jl"]].concat(),
        )
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `format --check` exits non-zero exactly when the file would be reformatted,
/// which is how these tests observe the `line-width` that took effect.
fn would_reformat(output: &Output) -> bool {
    !output.status.success()
}

#[test]
fn defaults_apply_without_any_config() {
    let sandbox = Sandbox::new();

    let output = sandbox.format_check(&[], &[]);

    assert!(
        !would_reformat(&output),
        "the line fits the default width of 92; stderr: {}",
        stderr(&output)
    );
}

#[test]
fn global_config_applies_without_a_project_config() {
    let sandbox = Sandbox::new();
    sandbox.write_config("xdg-config/fatou/fatou.toml", 40);

    let output = sandbox.format_check(&[], &[]);

    assert!(
        would_reformat(&output),
        "the global width of 40 should force a break; stderr: {}",
        stderr(&output)
    );
}

/// The `~/.config` candidate is checked on every platform, not just where XDG
/// is the native convention.
#[test]
fn global_config_is_found_under_dot_config() {
    let sandbox = Sandbox::new();
    sandbox.write_config(".config/fatou/fatou.toml", 40);

    let output = sandbox.format_check(&[], &[]);

    assert!(
        would_reformat(&output),
        "`~/.config/fatou/fatou.toml` should apply; stderr: {}",
        stderr(&output)
    );
}

#[test]
fn project_config_wins_over_global() {
    let sandbox = Sandbox::new();
    sandbox.write_config("xdg-config/fatou/fatou.toml", 40);
    sandbox.write_config("fatou.toml", 120);

    let output = sandbox.format_check(&[], &[]);

    assert!(
        !would_reformat(&output),
        "the project width of 120 should win; stderr: {}",
        stderr(&output)
    );
}

#[test]
fn env_config_wins_over_global() {
    let sandbox = Sandbox::new();
    sandbox.write_config("xdg-config/fatou/fatou.toml", 40);
    let env = sandbox.write_config("elsewhere/synced.toml", 120);

    let output = sandbox.format_check(&[("FATOU_CONFIG", env.to_str().unwrap())], &[]);

    assert!(
        !would_reformat(&output),
        "the `$FATOU_CONFIG` width of 120 should win; stderr: {}",
        stderr(&output)
    );
}

#[test]
fn no_config_ignores_the_global_config() {
    let sandbox = Sandbox::new();
    sandbox.write_config("xdg-config/fatou/fatou.toml", 40);

    let output = sandbox.format_check(&[], &["--no-config"]);

    assert!(
        !would_reformat(&output),
        "`--no-config` should fall back to the defaults; stderr: {}",
        stderr(&output)
    );
}

/// A dangling `$FATOU_CONFIG` is a hard error, not a silent fall-through that
/// would hide a typo'd path indefinitely.
#[test]
fn dangling_env_config_is_an_error() {
    let sandbox = Sandbox::new();
    let missing = sandbox.path().join("nope.toml");

    let output = sandbox.format_check(&[("FATOU_CONFIG", missing.to_str().unwrap())], &[]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("nope.toml"),
        "the error should name the file; stderr: {}",
        stderr(&output)
    );
}

/// A `fatou.toml` above a repository belongs to an unrelated tree, so the walk
/// stops at the repository root rather than inheriting it.
#[test]
fn discovery_stops_at_the_repository_root() {
    let sandbox = Sandbox::new();
    sandbox.write_config("fatou.toml", 40);
    let repo = sandbox.write_repo("repo");

    let output = sandbox.format_check_in(&repo, &[], &[]);

    assert!(
        !would_reformat(&output),
        "the config above the repo should not apply; stderr: {}",
        stderr(&output)
    );
}

/// The boundary directory is searched before the walk stops, so the usual
/// layout (`fatou.toml` beside `.git`) still resolves.
#[test]
fn config_at_the_repository_root_applies() {
    let sandbox = Sandbox::new();
    let repo = sandbox.write_repo("repo");
    sandbox.write_config("repo/fatou.toml", 40);

    let output = sandbox.format_check_in(&repo, &[], &[]);

    assert!(
        would_reformat(&output),
        "a config beside `.git` should apply; stderr: {}",
        stderr(&output)
    );
}

/// The boundary stops project discovery, not resolution: the global config is
/// still the fallback inside a repository. This is the intended replacement for
/// a config parked above the repo.
#[test]
fn global_config_applies_inside_a_repository() {
    let sandbox = Sandbox::new();
    sandbox.write_config("xdg-config/fatou/fatou.toml", 40);
    let repo = sandbox.write_repo("repo");

    let output = sandbox.format_check_in(&repo, &[], &[]);

    assert!(
        would_reformat(&output),
        "the global width of 40 should apply inside the repo; stderr: {}",
        stderr(&output)
    );
}

/// Relative excludes in a global config have no project directory to anchor at,
/// so they resolve against the working directory instead.
#[test]
fn global_config_excludes_resolve_against_the_working_directory() {
    let sandbox = Sandbox::new();
    let global = sandbox.path().join("xdg-config/fatou/fatou.toml");
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    std::fs::write(&global, "exclude = [\"vendor/\"]\n").unwrap();
    std::fs::create_dir_all(sandbox.path().join("vendor")).unwrap();
    std::fs::write(sandbox.path().join("vendor/bad.jl"), "x=1\n").unwrap();

    let output = sandbox.run(sandbox.path(), &[], &["format", "--check", "."]);

    assert!(
        output.status.success(),
        "`vendor/` should be pruned relative to the working directory; stderr: {}",
        stderr(&output)
    );
}

// --- per-rule config (`[lint.rules.<id>]`) ----------------------------------

/// Write `fatou.toml` and a Julia file at the sandbox root, then
/// `fatou lint` that file.
fn lint_with_config(sandbox: &Sandbox, config: &str, source: &str) -> Output {
    std::fs::write(sandbox.path().join("fatou.toml"), config).unwrap();
    std::fs::write(sandbox.path().join("lint.jl"), source).unwrap();
    sandbox.run(sandbox.path(), &[], &["lint", "lint.jl"])
}

const EXITS: &str = "function cleanup()\n    exit(1)\nend\n";

#[test]
fn cli_reports_a_builtin_discouraged_function_by_default() {
    let sandbox = Sandbox::new();

    let output = lint_with_config(&sandbox, "", EXITS);

    assert!(
        stderr(&output).contains("`exit` is discouraged"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn cli_lint_rules_table_extends_the_deny_list() {
    let sandbox = Sandbox::new();

    let output = lint_with_config(
        &sandbox,
        "[lint.rules.discouraged-function]\nextend-functions = { sleep = \"use a timer\" }\n",
        "function f()\n    sleep(1)\n    exit(1)\nend\n",
    );

    let stderr = stderr(&output);
    assert!(
        stderr.contains("`sleep` is discouraged: use a timer"),
        "{stderr}"
    );
    assert!(
        stderr.contains("`exit` is discouraged"),
        "extend keeps the built-ins: {stderr}"
    );
}

#[test]
fn cli_lint_rules_empty_functions_table_silences_the_rule() {
    let sandbox = Sandbox::new();

    let output = lint_with_config(
        &sandbox,
        "[lint.rules.discouraged-function]\nfunctions = {}\n",
        EXITS,
    );

    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn cli_unknown_rule_table_is_a_config_parse_error() {
    let sandbox = Sandbox::new();

    let output = lint_with_config(&sandbox, "[lint.rules.discouraged-funktion]\n", EXITS);

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(
        stderr.contains("discouraged-funktion"),
        "the typo should be named: {stderr}"
    );
}
