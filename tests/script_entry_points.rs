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
    lint_with_project(dir, dir, args)
}

fn lint_with_project(dir: &Path, project: &Path, args: &[&str]) -> Vec<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_fatou"))
        .current_dir(dir)
        .env_remove("FATOU_CONFIG")
        .env("JULIA_DEPOT_PATH", dir.join("empty-depot"))
        .env("JULIA_PROJECT", project)
        .env("JULIA_BINDIR", "")
        .env("PATH", "")
        .args(["lint", "--output", "json"])
        .args(args)
        .output()
        .unwrap();
    let findings: Vec<Value> = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("{err}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(
        output.status.success(),
        findings.iter().all(|d| d["rule"] == "analysis-incomplete"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    findings
}

#[test]
fn script_entries_enable_undefined_names_by_default() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "fatou.toml",
        "[project]\nentry-points = [\"main.jl\"]\n",
    );
    write(
        dir.path(),
        "main.jl",
        "provided = 1\ninclude(\"worker.jl\")\n",
    );
    write(dir.path(), "worker.jl", "f() = provided + typo\n");
    write(dir.path(), "loose.jl", "g() = unrelated_typo\n");

    for args in [vec!["."], vec!["--fix", "."]] {
        let findings = lint(dir.path(), &args);
        assert_eq!(findings.len(), 1, "{args:?}: {findings:#?}");
        assert_eq!(findings[0]["rule"], "undefined-name");
        assert!(findings[0].to_string().contains("`typo`"));
    }
}

#[test]
fn package_defaults_and_overrides_apply_to_cli_check_and_fix() {
    let dir = project(&[]);
    write(
        dir.path(),
        "Project.toml",
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    write(
        dir.path(),
        "src/MyPkg.jl",
        "module MyPkg\ninclude(\"member.jl\")\nend\n",
    );
    let text = "half(x) = x\nhalf(1, 2)\nfunction pending end\npending()\nf() = missing_name\n";
    write(dir.path(), "src/member.jl", text);
    write(dir.path(), "loose.jl", text);

    for (config, expected) in [
        (
            "",
            vec!["call-arity", "function-has-no-methods", "undefined-name"],
        ),
        (
            "[project]\nentry-points = [\"loose.jl\"]\n",
            vec![
                "call-arity",
                "function-has-no-methods",
                "undefined-name",
                "undefined-name",
            ],
        ),
        ("[lint]\nselect = []\n", vec![]),
        ("[lint]\nselect = [\"unused-binding\"]\n", vec![]),
        (
            "[lint]\nignore = [\"call-arity\", \"function-has-no-methods\", \"unused-import\"]\n",
            vec!["undefined-name"],
        ),
        (
            "[lint]\nselect = []\nextend-select = [\"undefined-name\", \"call-arity\"]\nignore = [\"call-arity\"]\n[lint.severity]\nundefined-name = \"hint\"\n",
            vec!["undefined-name", "undefined-name"],
        ),
    ] {
        write(dir.path(), "fatou.toml", config);
        for args in [vec!["."], vec!["--fix", "."]] {
            let findings = lint(dir.path(), &args);
            let mut ids: Vec<_> = findings
                .iter()
                .map(|d| d["rule"].as_str().unwrap())
                .collect();
            ids.sort_unstable();
            assert_eq!(ids, expected, "{config}: {args:?}: {findings:#?}");
            if config.contains("severity") {
                assert!(findings.iter().all(|d| d["severity"] == "hint"));
            }
        }
    }
}

#[test]
fn macro_implicit_arguments_are_local_to_the_macro() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "main.jl",
        concat!(
            "macro where_am_i()\n",
            "    f = string(__source__.file)\n",
            "    owner() = __module__\n",
            "    return (f, owner(), macro_typo)\n",
            "end\n",
            "outside() = (__source__, __module__)\n",
        ),
    );
    let findings = lint(dir.path(), &["main.jl"]);
    assert_eq!(findings.len(), 3, "{findings:#?}");
    for name in ["macro_typo", "__source__", "__module__"] {
        assert_eq!(
            findings
                .iter()
                .filter(|d| d.to_string().contains(&format!("`{name}`")))
                .count(),
            1,
            "{findings:#?}"
        );
    }
}

#[test]
fn script_failed_includes_report_errors_without_cascading_name_warnings() {
    let dir = project(&["main.jl", "good.jl"]);
    write(
        dir.path(),
        "fatou.toml",
        "[project]\nentry-points = [\"main.jl\", \"good.jl\"]\n",
    );
    write(dir.path(), "good.jl", "g() = independent_typo\n");
    std::fs::create_dir(dir.path().join("directory")).unwrap();
    std::fs::write(dir.path().join("unreadable.jl"), [0xff]).unwrap();
    for target in ["", "missing.jl", "directory", "unreadable.jl"] {
        write(
            dir.path(),
            "main.jl",
            &format!("include(\"{target}\")\nf() = uncertain\n"),
        );
        let findings = lint(dir.path(), &["main.jl", "good.jl"]);
        assert_eq!(findings.len(), 2, "{target:?}: {findings:#?}");
        assert!(
            findings
                .iter()
                .any(|d| d["rule"] == "missing-include-file" && d["severity"] == "error")
        );
        assert!(
            findings
                .iter()
                .any(|d| d.to_string().contains("`independent_typo`"))
        );
        assert!(
            !findings
                .iter()
                .any(|d| d.to_string().contains("`uncertain`"))
        );
    }
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
fn static_eval_definitions_do_not_hide_findings_in_included_files() {
    for definition in [
        "@eval g() = 1",
        "@eval function g()\n    1\nend",
        "Base.@eval g() = 1",
        "@Base.eval g() = 1",
        "@eval(g() = 1)",
    ] {
        let dir = project(&["src/Demo.jl"]);
        write(
            dir.path(),
            "src/Demo.jl",
            &format!("module Demo\ninclude(\"a.jl\")\n{definition}\nend\n"),
        );
        write(
            dir.path(),
            "src/a.jl",
            "f() = (g(), undefined_probe_name())\n",
        );
        for args in [vec!["src"], vec!["--fix", "src/a.jl"]] {
            let findings = lint(dir.path(), &args);
            assert_eq!(findings.len(), 1, "{definition}: {findings:#?}");
            assert_eq!(findings[0]["rule"], "undefined-name");
            assert!(findings[0].to_string().contains("`undefined_probe_name`"));
        }
    }
}

#[test]
fn dynamic_eval_explains_skipped_analysis_in_sibling_files() {
    let dir = project(&["main.jl", "good.jl"]);
    write(
        dir.path(),
        "main.jl",
        "include(\"dynamic.jl\")\ninclude(\"worker.jl\")\n",
    );
    write(dir.path(), "dynamic.jl", "@eval $name() = 1\n");
    write(dir.path(), "worker.jl", "f() = maybe_generated\n");
    write(dir.path(), "good.jl", "g() = independent_typo\n");
    let findings = lint(dir.path(), &["worker.jl", "good.jl"]);
    assert_eq!(findings.len(), 2, "{findings:#?}");
    let notice = findings
        .iter()
        .find(|d| d["rule"] == "analysis-incomplete")
        .unwrap();
    assert_eq!(notice["severity"], "info");
    let message = notice.to_string();
    assert!(message.contains("undefined-name"), "{message}");
    assert!(message.contains("dynamic.jl"), "{message}");
    assert!(message.contains("main.jl"), "{message}");
    assert!(
        findings
            .iter()
            .any(|d| d["rule"] == "undefined-name" && d.to_string().contains("`independent_typo`"))
    );
}

#[test]
fn shared_files_keep_findings_from_entries_without_dynamic_eval() {
    let dir = project(&["dynamic.jl", "static.jl"]);
    write(
        dir.path(),
        "dynamic.jl",
        "@eval $name() = 1\ninclude(\"worker.jl\")\n",
    );
    write(
        dir.path(),
        "static.jl",
        "@eval g() = 1\ninclude(\"worker.jl\")\n",
    );
    write(dir.path(), "worker.jl", "f() = (g(), typo)\n");
    let diagnostics = lint(dir.path(), &["worker.jl"]);
    assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
    let finding = diagnostics
        .iter()
        .find(|d| d["rule"] == "undefined-name")
        .unwrap()
        .to_string();
    assert!(finding.contains("`typo`"), "{finding}");
    assert!(finding.contains("static.jl"), "{finding}");
    assert!(!finding.contains("dynamic.jl"), "{finding}");
    let notice = diagnostics
        .iter()
        .find(|d| d["rule"] == "analysis-incomplete")
        .unwrap()
        .to_string();
    assert!(notice.contains("dynamic.jl"), "{notice}");
    assert!(!notice.contains("static.jl"), "{notice}");
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
        let diagnostics = lint(dir.path(), &["worker.jl"]);
        assert!(
            diagnostics
                .iter()
                .all(|d| d["rule"] == "analysis-incomplete"),
            "{uncertain}: {diagnostics:#?}"
        );
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
        "select = [\"unused-binding\"]",
        "ignore = [\"undefined-name\"]",
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

#[test]
fn unused_import_counts_uses_across_the_include_closure() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "fatou.toml",
        "[project]\nentry-points = [\"main.jl\"]\n[lint]\nselect = [\"unused-import\"]\n",
    );
    write(
        dir.path(),
        "main.jl",
        "module M\ninclude(\"imports.jl\")\ninclude(\"nested.jl\")\nend\n",
    );
    write(dir.path(), "imports.jl", "using Base: sin, cos\n");
    write(dir.path(), "nested.jl", "include(\"use.jl\")\n");
    write(dir.path(), "use.jl", "f(x) = sin(x)\n");
    for args in [vec!["imports.jl"], vec!["--fix", "imports.jl"], vec!["."]] {
        let findings = lint(dir.path(), &args);
        assert_eq!(findings.len(), 1, "{args:?}: {findings:#?}");
        assert!(findings[0].to_string().contains("`cos`"), "{findings:#?}");
    }
}

#[test]
fn unused_import_keeps_entry_points_modules_and_locals_separate() {
    let dir = project(&["main.jl", "other.jl"]);
    write(
        dir.path(),
        "fatou.toml",
        "[project]\nentry-points = [\"main.jl\", \"other.jl\"]\n[lint]\nselect = [\"unused-import\"]\n",
    );
    write(
        dir.path(),
        "main.jl",
        "module A\ninclude(\"imports.jl\")\ninclude(\"use.jl\")\nend\nmodule B\ninclude(\"imports.jl\")\ng(x) = cos(x)\nend\n",
    );
    write(dir.path(), "other.jl", "f(x) = tan(x)\n");
    write(dir.path(), "imports.jl", "using Base: sin, cos, tan\n");
    write(dir.path(), "use.jl", "f(sin, x) = sin(x)\n");
    let findings = lint(dir.path(), &["imports.jl"]);
    assert_eq!(findings.len(), 2, "{findings:#?}");
    assert!(findings.iter().any(|d| d.to_string().contains("`sin`")));
    assert!(findings.iter().any(|d| d.to_string().contains("`tan`")));
}

#[test]
fn unused_import_withholds_findings_for_incomplete_include_contexts() {
    for uncertain in [
        "include(path)\n",
        "include(\"missing.jl\")\n",
        "include(\"main.jl\")\n",
        "eval(code)\n",
        "function broken(\n",
    ] {
        let dir = project(&["main.jl"]);
        write(
            dir.path(),
            "fatou.toml",
            "[project]\nentry-points = [\"main.jl\"]\n[lint]\nselect = [\"unused-import\"]\n",
        );
        write(
            dir.path(),
            "main.jl",
            "include(\"imports.jl\")\ninclude(\"uncertain.jl\")\n",
        );
        write(dir.path(), "imports.jl", "using Base: sin\n");
        write(dir.path(), "uncertain.jl", uncertain);
        assert!(lint(dir.path(), &["imports.jl"]).is_empty(), "{uncertain}");
    }
}

#[test]
fn unused_import_counts_cross_file_import_forms_and_hygienic_uses() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "fatou.toml",
        "[project]\nentry-points = [\"main.jl\"]\n[lint]\nselect = [\"unused-import\"]\n",
    );
    write(
        dir.path(),
        "main.jl",
        "include(\"imports.jl\")\ninclude(\"use.jl\")\n",
    );
    for (imports, uses) in [
        ("import Base as B\n", "f(x) = B.sin(x)\n"),
        ("using Base: sin as s\n", "f(x) = s(x)\n"),
        ("using Base: sin\n", "export sin\n"),
        ("using Base: sin\n", "public sin\n"),
        ("import Base: sin\n", "sin(::MyType) = 1\n"),
        ("import Base: +\n", "f(x, y) = x + y\n"),
        ("import Base: +\n", "+(::MyType, ::MyType) = 1\n"),
        ("using Base: @time\n", "f(x) = @time x\n"),
        ("using Base: @time\n", "f(x) = :(@time x)\n"),
        ("using Unitful: @u_str\n", "f() = u\"ns\"\n"),
        ("using S: median\n", "f() = :(median(x))\n"),
        ("using S: median\n", "macro m(x)\n:(median($x))\nend\n"),
    ] {
        write(dir.path(), "imports.jl", imports);
        write(dir.path(), "use.jl", uses);
        assert!(
            lint(dir.path(), &["imports.jl"]).is_empty(),
            "{imports}{uses}"
        );
    }
}

#[test]
fn unused_import_preserves_suppressions_while_include_usage_is_unknown() {
    let dir = project(&["main.jl"]);
    write(
        dir.path(),
        "fatou.toml",
        "[project]\nentry-points = [\"main.jl\"]\n[lint]\nselect = [\"unused-import\", \"outdated-suppression\"]\n",
    );
    write(
        dir.path(),
        "main.jl",
        "include(\"imports.jl\")\ninclude(\"unknown.jl\")\n",
    );
    write(
        dir.path(),
        "imports.jl",
        "# fatou-ignore unused-import: consumed in included code\nusing Base: sin\n",
    );
    write(dir.path(), "unknown.jl", "include(path)\n");
    assert!(lint(dir.path(), &["imports.jl"]).is_empty());
    write(dir.path(), "unknown.jl", "f(x) = sin(x)\n");
    let findings = lint(dir.path(), &["imports.jl"]);
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0]["rule"], "outdated-suppression");
}

#[test]
fn package_unused_import_cli_and_fix_use_the_current_include_closure() {
    let dir = package_with_imports();
    for args in [vec!["src/imports.jl"], vec!["--fix", "src/imports.jl"]] {
        let findings = lint(dir.path(), &args);
        assert_eq!(findings.len(), 1, "{args:?}: {findings:#?}");
        assert!(findings[0].to_string().contains("`cos`"));
    }
}

#[cfg(unix)]
#[test]
fn package_unused_import_cli_and_fix_follow_a_symlinked_environment() {
    let dir = package_with_imports();
    let aliases = tempfile::tempdir().unwrap();
    let alias = aliases.path().join("project");
    std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
    for args in [vec!["src/imports.jl"], vec!["--fix", "src/imports.jl"]] {
        let findings = lint_with_project(dir.path(), &alias, &args);
        assert_eq!(findings.len(), 1, "{args:?}: {findings:#?}");
        assert!(findings[0].to_string().contains("`cos`"));
    }
    let target = alias.join("src/imports.jl");
    let target = target.to_str().unwrap();
    for args in [vec![target], vec!["--fix", target]] {
        let findings = lint(dir.path(), &args);
        assert_eq!(findings.len(), 1, "{args:?}: {findings:#?}");
        assert!(findings[0].to_string().contains("`cos`"));
    }
}

fn package_with_imports() -> TempDir {
    let dir = project(&[]);
    write(
        dir.path(),
        "fatou.toml",
        "[lint]\nselect = [\"unused-import\"]\n",
    );
    write(
        dir.path(),
        "Project.toml",
        "name = \"MyPkg\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
    );
    write(
        dir.path(),
        "src/MyPkg.jl",
        "module MyPkg\ninclude(\"imports.jl\")\ninclude(\"use.jl\")\nend\n",
    );
    write(dir.path(), "src/imports.jl", "using Base: sin, cos\n");
    write(dir.path(), "src/use.jl", "f(x) = sin(x)\n");
    dir
}
