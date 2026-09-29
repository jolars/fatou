//! Script programs have explicit roots and independent module contexts.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn project(entries: &[&str]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    write(
        dir.path(),
        "fatou.toml",
        &format!("[project]\nentry-points = {entries:?}\n[lint]\nselect = [\"undefined-name\"]\n"),
    );
    dir
}

fn lint(dir: &Path, args: &[&str]) -> Vec<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_fatou"))
        .current_dir(dir)
        .env_remove("FATOU_CONFIG")
        .env("JULIA_DEPOT_PATH", dir.join("empty-depot"))
        .env("JULIA_PROJECT", dir)
        .args(["lint", "--output", "json"])
        .args(args)
        .output()
        .unwrap();
    let findings: Vec<Value> = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("{err}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(
        output.status.success(),
        findings.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    findings
}

#[test]
fn script_globals_and_transitive_includes_resolve() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "main.jl",
        "include(\"settings.jl\")\ninclude(\"worker.jl\")\n",
    );
    write(
        dir.path(),
        "settings.jl",
        "threshold = 0.5\ninclude(\"more.jl\")\n",
    );
    write(dir.path(), "more.jl", "offset = 2\n");
    write(
        dir.path(),
        "worker.jl",
        "f(x) = x + threshold + offset + typo\n",
    );
    let findings = lint(dir.path(), &["worker.jl"]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert!(findings[0].to_string().contains("`typo`"));
    assert!(findings[0].to_string().contains("main.jl"));
    assert!(lint(dir.path(), &["main.jl"]).is_empty());
}

#[test]
fn script_entries_do_not_share_globals() {
    let dir = project(&["a.jl", "b.jl"]);
    write(dir.path(), "a.jl", "supplied = 1\ninclude(\"shared.jl\")\n");
    write(dir.path(), "b.jl", "include(\"shared.jl\")\n");
    write(dir.path(), "shared.jl", "f() = supplied\n");
    let findings = lint(dir.path(), &["shared.jl"]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    let finding = findings[0].to_string();
    assert!(finding.contains("b.jl"), "{finding}");
    assert!(!finding.contains("a.jl"), "{finding}");
}

#[test]
fn script_include_hosts_do_not_share_globals() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "main.jl",
        "module A\nsupplied = 1\ninclude(\"shared.jl\")\nend\nmodule B\ninclude(\"shared.jl\")\nend\n",
    );
    write(dir.path(), "shared.jl", "f() = supplied\n");
    let findings = lint(dir.path(), &["shared.jl"]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert!(findings[0].to_string().contains("Main.B"));
}

#[test]
fn script_uncertainty_in_a_sibling_suppresses_undefined_names() {
    for uncertain in [
        "eval(code)\n",
        "include(path)\n",
        "using Unavailable\n",
        "include(\"missing.jl\")\n",
        "function broken(\n",
    ] {
        let dir = project(&["main.jl"]);
        write(
            dir.path(),
            "main.jl",
            "include(\"uncertain.jl\")\ninclude(\"worker.jl\")\n",
        );
        write(dir.path(), "uncertain.jl", uncertain);
        write(dir.path(), "worker.jl", "f() = maybe_defined\n");
        assert!(lint(dir.path(), &["worker.jl"]).is_empty(), "{uncertain}");
    }
}

#[test]
fn script_suppressions_are_applied_after_contexts_are_combined() {
    let dir = project(&["a.jl", "b.jl"]);
    for entry in ["a.jl", "b.jl"] {
        write(dir.path(), entry, "include(\"shared.jl\")\n");
    }
    write(
        dir.path(),
        "shared.jl",
        "# fatou-ignore undefined-name: external input\nf() = supplied\n",
    );
    assert!(lint(dir.path(), &["shared.jl"]).is_empty());
}

#[test]
fn script_macros_and_module_names_resolve_in_their_namespace() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "main.jl",
        "macro known(x)\n    x\nend\ninclude(\"worker.jl\")\n",
    );
    write(
        dir.path(),
        "worker.jl",
        "@known arbitrary\nf() = known\ng() = Main\nmodule Child\nh() = Child\nend\n",
    );
    let findings = lint(dir.path(), &["worker.jl"]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert!(findings[0].to_string().contains("`known`"));
}

#[test]
fn script_fix_respects_cross_file_base_shadows() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "fatou.toml",
        "[project]\nentry-points = [\"main.jl\", \"other.jl\"]\n[lint]\nselect = [\"undefined-name\", \"length-zero\"]\n",
    );
    write(
        dir.path(),
        "main.jl",
        "length(x) = 0\ninclude(\"worker.jl\")\n",
    );
    write(dir.path(), "other.jl", "include(\"worker.jl\")\n");
    let source = "f(x) = length(x) == 0\ng() = typo\n";
    write(dir.path(), "worker.jl", source);
    let findings = lint(dir.path(), &["--fix", "worker.jl"]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert!(findings[0].to_string().contains("`typo`"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("worker.jl")).unwrap(),
        source
    );
}

#[test]
fn script_globals_shadow_package_names_for_qualified_checks() {
    for entries in [vec!["main.jl"], vec!["main.jl", "other.jl"]] {
        let dir = project(&entries);
        write(
            dir.path(),
            "fatou.toml",
            &format!(
                "[project]\nentry-points = {entries:?}\n[lint]\nselect = [\"non-public-access\", \"unresolved-docstring-reference\"]\n"
            ),
        );
        write(
            dir.path(),
            "main.jl",
            "Base = nothing\ninclude(\"worker.jl\")\n",
        );
        write(dir.path(), "other.jl", "include(\"worker.jl\")\n");
        write(
            dir.path(),
            "worker.jl",
            "\"\"\"See [`Base.script_member`](@ref Base.script_member).\"\"\"\nf() = Base.script_member()\n",
        );
        let findings = lint(dir.path(), &["worker.jl"]);
        assert!(findings.is_empty(), "{entries:?}: {findings:#?}");
    }
}

#[test]
fn script_entries_preserve_selection_excludes_and_severity() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "main.jl",
        "include(\"settings.jl\")\ninclude(\"worker.jl\")\n",
    );
    write(
        dir.path(),
        "settings.jl",
        "provided = 1\ng() = excluded_typo\n",
    );
    write(dir.path(), "worker.jl", "f() = provided + typo\n");
    write(
        dir.path(),
        "fatou.toml",
        "exclude = [\"settings.jl\"]\n[project]\nentry-points = [\"main.jl\"]\n[lint]\nselect = []\nextend-select = [\"undefined-name\"]\n[lint.severity]\nundefined-name = \"hint\"\n",
    );
    let findings = lint(dir.path(), &["."]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0]["severity"], "hint");
    assert!(findings[0].to_string().contains("`typo`"));
    for selection in [
        "select = []",
        "extend-select = [\"undefined-name\"]\nignore = [\"undefined-name\"]",
    ] {
        write(
            dir.path(),
            "fatou.toml",
            &format!("[project]\nentry-points = [\"main.jl\"]\n[lint]\n{selection}\n"),
        );
        assert!(lint(dir.path(), &["worker.jl"]).is_empty());
    }
}

#[test]
fn script_cycles_do_not_disable_independent_entries() {
    let dir = project(&["cycle.jl", "good.jl"]);
    write(
        dir.path(),
        "cycle.jl",
        "include(\"cycle.jl\")\nf() = uncertain\n",
    );
    write(dir.path(), "good.jl", "f() = typo\n");
    let findings = lint(dir.path(), &["cycle.jl", "good.jl"]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert!(findings[0].to_string().contains("`typo`"));
}
