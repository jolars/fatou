//! Incomplete analysis is reported without turning a notice into a lint failure.

use std::process::Command;

use fatou::config::LintConfig;
use fatou::linter::{LintStatus, Severity, check_source};

fn undefined_names() -> LintConfig {
    LintConfig {
        select: Some(vec!["undefined-name".into()]),
        severity: [("undefined-name".into(), Severity::Error)].into(),
        ..Default::default()
    }
}

#[test]
fn include_without_project_context_explains_skipped_undefined_names() {
    for include in ["include()", "include(\"helper.jl\")", "include(path)"] {
        let source = format!("{include}\ninclude(\"other.jl\")\nf() = typo\n");
        let report = check_source(None, &source, &undefined_names());
        assert_eq!(report.status, LintStatus::Clean);
        let [notice] = &report.diagnostics[..] else {
            panic!("expected one analysis notice: {:?}", report.diagnostics);
        };
        assert_eq!(notice.rule, "analysis-incomplete");
        assert_eq!(notice.severity, Severity::Info);
        assert_eq!(&source[notice.range], include);
        assert!(notice.message.body.contains("undefined-name"));
        assert!(notice.message.body.contains("entry-points"));
        assert!(notice.fixes.is_empty());
    }
}

#[test]
fn skipped_analysis_notices_require_an_enabled_unsuppressed_rule() {
    let source = "include()\nf() = typo\n";
    let mut ignored = undefined_names();
    ignored.ignore.push("undefined-name".into());
    for config in [LintConfig::default(), ignored] {
        assert!(check_source(None, source, &config).diagnostics.is_empty());
    }
    for prefix in [
        "# fatou-ignore-file undefined-name: supplied by the caller\n",
        "# fatou-ignore-file: supplied by the caller\n",
    ] {
        let source = format!("{prefix}{source}");
        assert!(
            check_source(None, &source, &undefined_names())
                .diagnostics
                .is_empty()
        );
    }
    let report = check_source(None, "include()\nfunction f(\n", &undefined_names());
    assert!(matches!(report.status, LintStatus::ParseDiagnostics { .. }));
    assert!(report.diagnostics.iter().all(|d| d.rule == "parse-error"));
}

#[test]
fn other_resolution_limits_do_not_produce_include_context_notices() {
    for source in [
        "eval(code)\nf() = typo\n",
        "@eval x = 1\nf() = typo\n",
        "Core.eval(code)\nf() = typo\n",
        "using Unavailable\nf() = typo\n",
    ] {
        assert!(
            check_source(None, source, &undefined_names())
                .diagnostics
                .is_empty(),
            "{source}"
        );
    }
}

#[test]
fn analysis_notices_do_not_change_the_count_of_lint_findings() {
    let mut config = undefined_names();
    config.extend_select.push("unused-binding".into());
    config
        .severity
        .insert("unused-binding".into(), Severity::Info);
    let report = check_source(None, "include()\nf() = begin\n    a = typo\nend\n", &config);
    assert_eq!(report.status, LintStatus::Findings { count: 1 });
    assert_eq!(report.diagnostics.len(), 2);
    assert!(
        report
            .diagnostics
            .iter()
            .all(|d| d.severity == Severity::Info)
    );
}

#[test]
fn cli_analysis_notices_preserve_output_formats_and_success_with_fix() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    std::fs::write(
        dir.path().join("fatou.toml"),
        "[lint]\nselect = [\"undefined-name\"]\n[lint.severity]\nundefined-name = \"error\"\n",
    )
    .unwrap();
    let source = "include(\"helper.jl\")\nf() = typo\n";
    std::fs::write(dir.path().join("main.jl"), source).unwrap();
    std::fs::write(dir.path().join("helper.jl"), "provided = 1\n").unwrap();
    for mode in ["pretty", "concise", "json"] {
        for fix in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_fatou"));
            command
                .current_dir(dir.path())
                .env_remove("FATOU_CONFIG")
                .env("JULIA_PROJECT", dir.path())
                .env("JULIA_DEPOT_PATH", dir.path().join("empty-depot"))
                .args(["lint", "--output", mode]);
            if fix {
                command.arg("--fix");
            }
            let output = command.arg("main.jl").output().unwrap();
            assert!(output.status.success(), "{mode}, fix={fix}: {output:?}");
            let stdout = String::from_utf8(output.stdout).unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            let rendered = if mode == "json" { &stdout } else { &stderr };
            assert!(rendered.contains("analysis-incomplete"), "{rendered}");
            assert!(rendered.contains("entry-points"), "{rendered}");
            if mode == "json" {
                let diagnostics: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(diagnostics[0]["severity"], "info");
            }
            assert!(!stderr.contains(": clean"));
            assert_eq!(
                std::fs::read_to_string(dir.path().join("main.jl")).unwrap(),
                source
            );
        }
    }
}
