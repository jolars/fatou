use fatou::parse_context::{SourceRole, manifest_edition, project_edition};
use fatou::parser::SyntaxEdition;
use std::path::Path;

#[test]
fn project_selection_keeps_editions_separate_from_runtime_compatibility() {
    let path = Path::new("Project.toml");
    let explicit = "[compat]\njulia = \"1.6\"\n[syntax]\njulia_version = \"1.14.0\"\n";
    assert_eq!(
        project_edition(path, explicit, SourceRole::Package)
            .options
            .syntax_edition,
        SyntaxEdition::Julia114
    );
    assert_eq!(
        project_edition(path, "[compat]\njulia = \"1.6, 1.14\"", SourceRole::Package)
            .options
            .syntax_edition,
        SyntaxEdition::Julia113
    );
    assert_eq!(
        project_edition(
            path,
            "[compat]\njulia = \"1.14, 1.6 - 1.5\"",
            SourceRole::Package
        )
        .options
        .syntax_edition,
        SyntaxEdition::Julia114
    );
    assert_eq!(
        project_edition(path, "", SourceRole::Package)
            .options
            .syntax_edition,
        SyntaxEdition::Julia113
    );
    assert_eq!(
        project_edition(path, "", SourceRole::Script)
            .options
            .syntax_edition,
        SyntaxEdition::LATEST
    );
    assert_eq!(
        project_edition(
            path,
            "[syntax]\njulia_version = \"1.99\"",
            SourceRole::Package
        )
        .options
        .syntax_edition,
        SyntaxEdition::LATEST
    );
}

#[test]
fn invalid_metadata_uses_a_deterministic_uncertain_fallback() {
    for text in [
        "[syntax",
        "[syntax]\njulia_version = 14",
        "[syntax]\njulia_version = \"oops\"",
        "[syntax]",
        "[compat]\njulia = \"^\"",
    ] {
        for role in [SourceRole::Package, SourceRole::Script] {
            let selection = project_edition(Path::new("Project.toml"), text, role);
            assert!(!selection.certain, "{text:?}");
            assert!(selection.issue.is_some());
            assert_eq!(selection.options.syntax_edition, role.default_edition());
        }
    }
}

#[test]
fn syntax_edition_validates_the_entire_julia_version() {
    for (version, valid) in [
        ("1.14-rc1", true),
        ("1.14+test", true),
        ("v1.14.0-DEV.100+build-1", true),
        ("1.14rc1", true),
        ("1.14.0-invalid suffix", false),
        ("1.14.0-rc1+bad.", false),
        ("1.14.0.1", false),
        ("vv1.14", false),
        ("1.14-+build", false),
    ] {
        let project = format!("[syntax]\njulia_version = '{version}'");
        let selected = project_edition(Path::new("Project.toml"), &project, SourceRole::Package);
        assert_eq!(selected.certain, valid, "{version}");
        if valid {
            assert_eq!(selected.options.syntax_edition, SyntaxEdition::Julia114);
        }
    }
}

#[test]
fn manifest_dependencies_use_their_own_recorded_editions() {
    let uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c".parse().unwrap();
    for prefix in ["", "deps."] {
        let base = format!("julia_version = \"1.15\"\n[[{prefix}Foo]]\nuuid = \"{uuid}\"\n");
        let old = manifest_edition(Path::new("Manifest.toml"), &base, "Foo", uuid);
        assert!(old.certain);
        assert_eq!(old.options.syntax_edition, SyntaxEdition::Julia113);
        let new = manifest_edition(
            Path::new("Manifest.toml"),
            &(base + "syntax.julia_version = \"1.14\"\n"),
            "Foo",
            uuid,
        );
        assert!(new.certain);
        assert_eq!(new.options.syntax_edition, SyntaxEdition::Julia114);
    }
}

#[test]
fn live_project_edits_reparse_unchanged_julia_and_uncertain_editions_block_writes() {
    use fatou::incremental::{IncrementalDatabase, parsed_document};
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("Project.toml");
    std::fs::write(&project, "[syntax]\njulia_version = \"1.13\"\n").unwrap();
    // Edition discovery must not need a successful environment resolve.
    std::fs::write(dir.path().join("Manifest.toml"), "[broken").unwrap();
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(&dir.path().join("src/Foo.jl"), "typegroup = 1\n");
    assert_eq!(
        db.parse_options(file).syntax_edition,
        SyntaxEdition::Julia113
    );
    assert!(parsed_document(&db, file).diagnostics.is_empty());
    db.upsert_file(&project, "[syntax]\njulia_version = \"1.14\"\n");
    assert!(!parsed_document(&db, file).diagnostics.is_empty());
    assert_eq!(db.file_text(file), "typegroup = 1\n");
    db.upsert_file(&project, "[syntax");
    let selection = &db.source_editions(file)[0].1;
    assert_eq!(selection.options.syntax_edition, SyntaxEdition::LATEST);
    assert!(selection.require_certain().is_err());
    db.revert_file_to_disk(&project);
    assert!(db.source_editions(file)[0].1.certain);
    assert!(parsed_document(&db, file).diagnostics.is_empty());
}

#[test]
fn a_source_keeps_distinct_edition_parses_and_semantic_models() {
    use fatou::incremental::{
        IncrementalDatabase, parsed_document_with_options, semantic_model_with_options,
    };
    use fatou::parser::ParseOptions;
    let db = IncrementalDatabase::default();
    let file = db.add_file("typegroup = 1\n");
    let old = ParseOptions {
        syntax_edition: SyntaxEdition::Julia113,
    };
    let new = ParseOptions {
        syntax_edition: SyntaxEdition::Julia114,
    };
    assert!(
        parsed_document_with_options(&db, file, old)
            .diagnostics
            .is_empty()
    );
    assert!(
        !parsed_document_with_options(&db, file, new)
            .diagnostics
            .is_empty()
    );
    assert!(
        semantic_model_with_options(&db, file, old)
            .bindings()
            .iter()
            .any(|binding| binding.name == "typegroup")
    );
    assert!(
        !semantic_model_with_options(&db, file, new)
            .bindings()
            .iter()
            .any(|binding| binding.name == "typegroup")
    );
    assert!(
        parsed_document_with_options(&db, file, old)
            .diagnostics
            .is_empty()
    );
}

#[test]
fn unsaved_manifests_change_only_their_dependency_edition() {
    use fatou::incremental::IncrementalDatabase;
    use fatou::parse_context::{EditionSource, SourceContext};
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("Manifest.toml");
    let uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c".parse().unwrap();
    let base = format!("[[deps.Foo]]\nuuid = \"{uuid}\"\n");
    std::fs::write(&manifest, &base).unwrap();
    let path = dir.path().join("Foo.jl");
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(&path, "typegroup = 1\n");
    db.set_source_contexts(
        &path,
        vec![SourceContext {
            entry: path.clone(),
            source: EditionSource::Manifest {
                path: manifest.clone(),
                name: "Foo".into(),
                uuid,
            },
        }],
    );
    assert_eq!(
        db.parse_options(file).syntax_edition,
        SyntaxEdition::Julia113
    );
    db.upsert_file(&manifest, base + "syntax.julia_version = \"1.14\"\n");
    assert_eq!(
        db.parse_options(file).syntax_edition,
        SyntaxEdition::Julia114
    );
    db.revert_file_to_disk(&manifest);
    assert_eq!(
        db.parse_options(file).syntax_edition,
        SyntaxEdition::Julia113
    );
}

#[test]
fn includes_keep_each_entry_edition_across_project_boundaries() {
    use fatou::parse_context::{ContextCatalog, EditionSource, SourceContext, format_source};
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old");
    let new = dir.path().join("new");
    for (root, edition) in [(&old, "1.13"), (&new, "1.14")] {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            root.join("Project.toml"),
            format!("[syntax]\njulia_version = \"{edition}\"\n"),
        )
        .unwrap();
        std::fs::write(root.join("main.jl"), "include(\"../shared.jl\")\n").unwrap();
    }
    let shared = dir.path().join("shared.jl");
    std::fs::write(&shared, "typegroup  =  1\n").unwrap();
    let roots: Vec<_> = [&old, &new]
        .into_iter()
        .map(|root| SourceContext {
            entry: root.join("main.jl"),
            source: EditionSource::Project {
                path: root.join("Project.toml"),
                role: SourceRole::Script,
            },
        })
        .collect();
    let catalog = ContextCatalog::load(&roots, &BTreeMap::new(), None);
    let contexts = catalog.selections(&shared);
    assert_eq!(contexts.len(), 2);
    assert_eq!(
        contexts[0].1.options.syntax_edition,
        SyntaxEdition::Julia113
    );
    assert_eq!(
        contexts[1].1.options.syntax_edition,
        SyntaxEdition::Julia114
    );
    assert_eq!(
        catalog.sources.len(),
        3,
        "source text is shared across edition views"
    );
    assert!(format_source("typegroup  =  1\n", Default::default(), false, contexts).is_err());
    let overridden = ContextCatalog::load(&roots, &BTreeMap::new(), Some(SyntaxEdition::Julia113));
    assert_eq!(
        overridden.selections(&shared)[0].0.source,
        roots[0].source,
        "a grammar override preserves the loading environment"
    );
    assert_eq!(
        format_source(
            "typegroup  =  1\n",
            Default::default(),
            false,
            overridden.selections(&shared)
        )
        .unwrap(),
        "typegroup = 1\n"
    );
    assert_eq!(
        format_source("x  =  1\n", Default::default(), true, contexts).unwrap(),
        "x = 1\n"
    );
}

#[test]
fn missing_live_metadata_does_not_reuse_the_previous_edition() {
    use fatou::incremental::IncrementalDatabase;
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("Project.toml");
    std::fs::write(&project, "[syntax]\njulia_version = \"1.14\"\n").unwrap();
    let mut db = IncrementalDatabase::default();
    let file = db.upsert_file(&dir.path().join("src/Foo.jl"), "typegroup = 1\n");
    assert_eq!(
        db.parse_options(file).syntax_edition,
        SyntaxEdition::Julia114
    );
    std::fs::remove_file(&project).unwrap();
    db.revert_file_to_disk(&project);
    assert_eq!(db.parse_options(file).syntax_edition, SyntaxEdition::LATEST);
    assert!(!db.source_editions(file)[0].1.certain);
}

#[test]
fn invalid_manifest_editions_report_the_value_span() {
    let uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c".parse().unwrap();
    for prefix in ["", "deps."] {
        let text = format!(
            "julia_version = \"1.15\"\n[[{prefix}Foo]]\nuuid = \"{uuid}\"\nsyntax.julia_version = \"bad\"\n"
        );
        let selected = manifest_edition(Path::new("Manifest.toml"), &text, "Foo", uuid);
        let issue = selected.issue.unwrap();
        assert_eq!(&text[issue.range], "\"bad\"");
    }
}

#[test]
fn manifest_diagnostics_keep_spans_for_each_invalid_entry() {
    use fatou::project_files::syntax_findings;
    for prefix in ["", "deps."] {
        let text = format!(
            "manifest_format = '2.0'\n\
             [[{prefix}Foo]]\nuuid = '1520ce14-60c1-5f80-bbc7-55ef81b5835c'\nsyntax.julia_version = 'bad'\n\
             [[{prefix}Foo]]\nuuid = '1520ce14-60c1-5f80-bbc7-55ef81b5835d'\nsyntax = {{ julia_version = 14 }}\n\
             [[{prefix}Bar]]\nuuid = '1520ce14-60c1-5f80-bbc7-55ef81b5835e'\n"
        );
        let findings = syntax_findings(Path::new("Manifest.toml"), &text);
        assert!(
            findings
                .iter()
                .all(|finding| finding.check == "syntax-edition")
        );
        let mut values: Vec<_> = findings
            .iter()
            .map(|finding| &text[finding.range])
            .collect();
        values.sort_unstable();
        assert_eq!(values, ["'bad'", "14"]);
    }
}

#[test]
fn metadata_identity_ignores_comments_and_patch_editions() {
    use fatou::parse_context::metadata_signature;
    let path = Path::new("Project.toml");
    assert_eq!(
        metadata_signature(path, Some("[syntax]\njulia_version = \"1.14.0\"\n")),
        metadata_signature(
            path,
            Some("# edited\n[syntax]\njulia_version = \"1.14.9\"\n")
        )
    );
    assert_ne!(
        metadata_signature(path, Some("[syntax]\njulia_version = \"1.13\"\n")),
        metadata_signature(path, Some("[syntax]\njulia_version = \"1.14\"\n"))
    );
}

fn cli(dir: &Path, args: &[&str], input: Option<&str>) -> std::process::Output {
    cli_with_project(dir, args, input, None)
}

fn cli_with_project(
    dir: &Path,
    args: &[&str],
    input: Option<&str>,
    project: Option<&Path>,
) -> std::process::Output {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_fatou"))
        .args(args)
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("HOME", dir)
        .env("APPDATA", dir)
        .env("JULIA_PROJECT", project.unwrap_or_else(|| Path::new("")))
        .env("JULIA_DEPOT_PATH", dir.join("depot"))
        .env("JULIA_BINDIR", "")
        .env("PATH", "")
        .env_remove("FATOU_CONFIG")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    }
    child.wait_with_output().unwrap()
}

#[test]
fn cli_uses_only_the_selected_project_in_the_package_directory() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("Project.toml");
    for (path, edition) in [
        (project.clone(), "1.13"),
        (dir.path().join("JuliaProject.toml"), "1.14"),
    ] {
        std::fs::write(
            path,
            format!("name = 'Foo'\n[syntax]\njulia_version = '{edition}'\n"),
        )
        .unwrap();
    }
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/Foo.jl"), "typegroup() = 1\n").unwrap();

    for args in [
        &["parse", "src/Foo.jl"][..],
        &["format", "--check", "src/Foo.jl"],
    ] {
        let output = cli_with_project(dir.path(), args, None, Some(&project));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn cli_uses_the_active_manifest_edition_for_dependency_targets() {
    let dir = tempfile::tempdir().unwrap();
    let active = dir.path().join("active");
    let dependency = dir.path().join("Foo");
    std::fs::create_dir(&active).unwrap();
    std::fs::create_dir_all(dependency.join("src")).unwrap();
    let uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c";
    std::fs::write(
        active.join("Project.toml"),
        format!("[deps]\nFoo = '{uuid}'\n"),
    )
    .unwrap();
    std::fs::write(
        dependency.join("Project.toml"),
        format!("name = 'Foo'\nuuid = '{uuid}'\n[syntax]\njulia_version = '1.13'\n"),
    )
    .unwrap();
    let input = "module Foo\ntypegroup  =  1\nend\n";
    let path = dependency.join("src/Foo.jl");
    std::fs::write(&path, input).unwrap();
    let manifest = format!("[[deps.Foo]]\nuuid = '{uuid}'\npath = '../Foo'\n");
    std::fs::write(
        active.join("Manifest.toml"),
        format!("{manifest}syntax.julia_version = '1.14'\n"),
    )
    .unwrap();
    for args in [
        &["format", "--safe", "Foo/src/Foo.jl"][..],
        &["lint", "--output", "json", "Foo/src/Foo.jl"][..],
    ] {
        let output = cli_with_project(dir.path(), args, None, Some(&active));
        assert!(!output.status.success(), "{args:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), input);
    }
    std::fs::write(active.join("Manifest.toml"), format!("{manifest}[broken")).unwrap();
    let output = cli_with_project(
        dir.path(),
        &["format", "--safe", "Foo/src/Foo.jl"],
        None,
        Some(&active),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("uncertain"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), input);
    // The active manifest remains authoritative even if the dependency's own
    // checkout has since changed its project edition.
    std::fs::write(
        dependency.join("Project.toml"),
        "name = 'Foo'\n[syntax]\njulia_version = '1.14'\n",
    )
    .unwrap();
    std::fs::write(active.join("Manifest.toml"), manifest).unwrap();
    let output = cli_with_project(
        dir.path(),
        &["format", "--safe", "Foo/src/Foo.jl"],
        None,
        Some(&active),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_inherits_the_parent_manifest_edition_for_dependency_extensions() {
    let dir = tempfile::tempdir().unwrap();
    let active = dir.path().join("active");
    let dep = dir.path().join("Foo");
    std::fs::create_dir(&active).unwrap();
    std::fs::create_dir_all(dep.join("src")).unwrap();
    std::fs::create_dir_all(dep.join("ext/NestedExt")).unwrap();
    std::fs::create_dir_all(dep.join("lib")).unwrap();
    let uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c";
    std::fs::write(active.join("Project.toml"), "[deps]\n").unwrap();
    std::fs::write(
        active.join("Manifest.toml"),
        format!("[[deps.Foo]]\nuuid = '{uuid}'\npath = '../Foo'\nsyntax.julia_version = '1.14'\n[deps.Foo.extensions]\nFlatExt = 'Trigger'\nNestedExt = ['Trigger']\n"),
    )
    .unwrap();
    std::fs::write(
        dep.join("Project.toml"),
        "name = 'Foo'\n[syntax]\njulia_version = '1.13'\n",
    )
    .unwrap();
    std::fs::write(dep.join("src/Foo.jl"), "module Foo end\n").unwrap();
    std::fs::write(
        dep.join("ext/FlatExt.jl"),
        "include(\"../lib/shared.jl\")\nflat()=1+%2\n",
    )
    .unwrap();
    std::fs::write(dep.join("ext/NestedExt/NestedExt.jl"), "nested()=3+%4\n").unwrap();
    std::fs::write(dep.join("lib/shared.jl"), "shared()=5+%6\n").unwrap();
    let output = cli_with_project(
        dir.path(),
        &["format", "--safe", "Foo/ext", "Foo/lib/shared.jl"],
        None,
        Some(Path::new("active")),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_discovers_package_includes_outside_src_and_ext() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::create_dir(dir.path().join("lib")).unwrap();
    std::fs::write(dir.path().join("Project.toml"), "name = 'Foo'\n").unwrap();
    std::fs::write(
        dir.path().join("src/Foo.jl"),
        "module Foo\ninclude(\"../lib/impl.jl\")\nend\n",
    )
    .unwrap();
    let path = dir.path().join("lib/impl.jl");
    std::fs::write(&path, "typegroup  =  1\n").unwrap();
    let output = cli(dir.path(), &["format", "--safe", "lib/impl.jl"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "typegroup = 1\n");
}

#[test]
fn cli_loads_external_includes_from_the_active_package() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("package");
    std::fs::create_dir_all(package.join("src")).unwrap();
    std::fs::write(package.join("Project.toml"), "name = 'Foo'\n").unwrap();
    std::fs::write(
        package.join("src/Foo.jl"),
        "module Foo\ninclude(\"../../shared.jl\")\nend\n",
    )
    .unwrap();
    let helper = dir.path().join("shared.jl");
    for (cwd, path, project) in [
        (package.as_path(), "../shared.jl", None),
        (dir.path(), "shared.jl", Some(package.as_path())),
    ] {
        std::fs::write(&helper, "typegroup  =  1\n").unwrap();
        let output = cli_with_project(cwd, &["format", "--safe", path], None, project);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_to_string(&helper).unwrap(), "typegroup = 1\n");
    }
}

#[test]
fn safe_formatting_rejects_the_recovered_edition_errors() {
    let dir = tempfile::tempdir().unwrap();
    for (edition, source) in [
        ("1.14", "function typegroup(x) end end\n"),
        ("1.13", "@m typegroup \"text\"\n"),
        ("1.13", "a[return begin, end]\n"),
    ] {
        let output = cli(
            dir.path(),
            &["format", "--safe", "--syntax-edition", edition],
            Some(source),
        );
        assert!(!output.status.success(), "{edition}: {source:?}");
        assert!(output.stdout.is_empty());
    }
    let result = cli(
        dir.path(),
        &["format", "--safe", "--syntax-edition", "1.14"],
        Some("a[return begin, end]\n"),
    );
    assert!(result.status.success(), "{result:?}");
}

#[test]
fn cli_loads_includes_from_declared_workspace_extensions() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("package");
    for subdir in ["src", "lib", "ext/NestedExt"] {
        std::fs::create_dir_all(package.join(subdir)).unwrap();
    }
    std::fs::write(
        package.join("Project.toml"),
        "name = 'Foo'\n[extensions]\nFlatExt = 'Trigger'\nNestedExt = ['Trigger']\n",
    )
    .unwrap();
    std::fs::write(package.join("src/Foo.jl"), "module Foo end\n").unwrap();
    for (entry, helper, include) in [
        ("ext/FlatExt.jl", "flat.jl", "../lib/flat.jl"),
        (
            "ext/NestedExt/NestedExt.jl",
            "nested.jl",
            "../../lib/nested.jl",
        ),
    ] {
        std::fs::write(package.join(entry), format!("include(\"{include}\")\n")).unwrap();
        let path = package.join("lib").join(helper);
        std::fs::write(&path, "typegroup  =  1\n").unwrap();
        let output = cli(
            dir.path(),
            &["format", "--safe", path.to_str().unwrap()],
            None,
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "typegroup = 1\n");
    }
}

#[test]
fn cli_lint_uses_package_and_script_defaults_for_shared_sources() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    for (path, text) in [
        (
            "Project.toml",
            "name = 'Foo'\nuuid = '00000000-0000-0000-0000-000000000001'\n",
        ),
        ("fatou.toml", "[project]\nentry-points = ['main.jl']\n"),
        (
            "main.jl",
            "include(\"src/shared.jl\")\ninclude(\"script_only.jl\")\n",
        ),
        ("src/Foo.jl", "module Foo\ninclude(\"shared.jl\")\nend\n"),
        (
            "src/shared.jl",
            "using NeverDeclared\nf(x) = x == nothing\n",
        ),
        ("script_only.jl", "using ScriptDependency\n"),
    ] {
        std::fs::write(dir.path().join(path), text).unwrap();
    }
    for fix in [false, true] {
        let mut args = vec!["lint", "--output", "json"];
        if fix {
            args.push("--fix");
        }
        args.extend(["src/shared.jl", "script_only.jl"]);
        let output = cli(dir.path(), &args, None);
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(!output.status.success(), "{report}");
        assert!(
            report.contains("unresolved-import") && report.contains("NeverDeclared"),
            "{report}"
        );
        assert!(
            report.contains("syntax contexts:") && report.contains("Foo.jl"),
            "{report}"
        );
        assert!(!report.contains("ScriptDependency"), "{report}");
    }
    assert!(
        std::fs::read_to_string(dir.path().join("src/shared.jl"))
            .unwrap()
            .contains("x === nothing")
    );
}

#[test]
fn shadowed_manifest_pins_still_select_their_own_checkout_editions() {
    use fatou::environment::{self, EnvContext};
    use fatou::incremental::IncrementalDatabase;
    use fatou::index::harvest_libraries;

    let dir = tempfile::tempdir().unwrap();
    let mut environments = Vec::new();
    let mut helpers = Vec::new();
    let uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c";
    for (name, edition, source) in [
        ("old", SyntaxEdition::Julia113, "typegroup = 1\n"),
        ("new", SyntaxEdition::Julia114, "x = 1 +% 2\n"),
    ] {
        let workspace = dir.path().join(name);
        let checkout = workspace.join("checkout");
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        std::fs::write(
            workspace.join("Project.toml"),
            format!("[deps]\nFoo = '{uuid}'\n"),
        )
        .unwrap();
        std::fs::write(
            workspace.join("Manifest.toml"),
            format!("[[deps.Foo]]\nuuid = '{uuid}'\npath = 'checkout'\nsyntax.julia_version = '{edition}'\n"),
        )
        .unwrap();
        std::fs::write(checkout.join("Project.toml"), "name = 'Foo'\n").unwrap();
        std::fs::write(
            checkout.join("src/Foo.jl"),
            "module Foo\ninclude(\"../helper.jl\")\nend\n",
        )
        .unwrap();
        let helper = checkout.join("helper.jl");
        std::fs::write(&helper, source).unwrap();
        helpers.push((helper, edition, source));
        environments.push(
            environment::resolve(&EnvContext {
                workspace_root: workspace,
                julia_project: None,
                julia_depot_path: None,
                home: None,
                julia_bindir: None,
                path: None,
            })
            .unwrap()
            .unwrap(),
        );
    }
    for _ in 0..2 {
        let library = harvest_libraries(&environments);
        assert_eq!(
            library.roots["Foo"],
            environments[0].packages[0].source.clone().unwrap()
        );
        let mut db = IncrementalDatabase::default();
        db.set_library_syntax(library.syntax);
        for (path, edition, source) in &helpers {
            let file = db.upsert_file(path, *source);
            let contexts = db.source_editions(file);
            assert_eq!(contexts.len(), 1, "{path:?}: {contexts:?}");
            assert_eq!(contexts[0].1.options.syntax_edition, *edition, "{path:?}");
            assert!(db.snapshot().parse_diagnostics(file).is_empty(), "{path:?}");
        }
        environments.reverse();
    }
}

#[test]
fn parse_stdin_skips_automatic_config_discovery() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("fatou.toml"), "[").unwrap();
    let result = cli(
        dir.path(),
        &["parse", "--verify", "--quiet", "-"],
        Some("x = 1\n"),
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&result.stderr).trim_end(),
        "losslessness OK"
    );
}

#[cfg(unix)]
#[test]
fn context_discovery_does_not_open_fifo_include_targets() {
    use fatou::parse_context::ContextCatalog;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(dir.path().join("pipe.jl"))
            .status()
            .unwrap()
            .success()
    );
    let path = dir.path().join("main.jl");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let buffers = [(path.clone(), Arc::from("include(\"pipe.jl\")\n"))].into();
        let catalog = ContextCatalog::discover(std::slice::from_ref(&path), &[], &buffers, None);
        tx.send(catalog.sources[&path.with_file_name("pipe.jl")].is_err())
            .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap());
}

#[test]
fn nested_package_is_the_primary_context_in_the_catalog_and_live_database() {
    use fatou::incremental::IncrementalDatabase;
    use fatou::parse_context::{ContextCatalog, EditionSource, SourceContext};
    use std::collections::BTreeMap;

    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path();
    let inner = outer.join("Nested");
    let shared = inner.join("shared.jl");
    let source = "typegroup() = 1\n";
    let mut contexts = Vec::new();
    let mut buffers = BTreeMap::from([(shared.clone(), source.into())]);
    for (root, edition, include) in [
        (outer, "1.14", "Nested/shared.jl"),
        (inner.as_path(), "1.13", "shared.jl"),
    ] {
        let project = root.join("Project.toml");
        let entry = root.join("entry.jl");
        contexts.push(SourceContext {
            entry: entry.clone(),
            source: EditionSource::Project {
                path: project.clone(),
                role: SourceRole::Package,
            },
        });
        buffers.insert(
            project,
            format!("[syntax]\njulia_version = '{edition}'\n").into(),
        );
        buffers.insert(entry, format!("include(\"{include}\")\n").into());
    }
    let catalog = ContextCatalog::load(&contexts, &buffers, None);
    assert_eq!(
        catalog.options(&shared)[0].syntax_edition,
        SyntaxEdition::Julia113
    );
    assert_eq!(catalog.options(&shared).len(), 2);

    let mut db = IncrementalDatabase::default();
    for (path, text) in buffers {
        db.upsert_file(&path, text);
    }
    db.set_source_contexts(&shared, contexts);
    assert_eq!(
        db.parse_options(db.lookup_file(&shared).unwrap())
            .syntax_edition,
        SyntaxEdition::Julia113
    );
}

#[test]
fn cli_include_contexts_do_not_depend_on_target_sort_order() {
    for included in ["alpha", "zulu"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("scripts")).unwrap();
        std::fs::create_dir(dir.path().join(included)).unwrap();
        std::fs::write(
            dir.path().join("scripts/Project.toml"),
            "[syntax]\njulia_version = '1.13'\n",
        )
        .unwrap();
        let shared = format!("{included}/shared.jl");
        std::fs::write(dir.path().join(&shared), "typegroup  =  1\n").unwrap();
        std::fs::write(
            dir.path().join("scripts/main.jl"),
            format!("include(\"../{shared}\")\n"),
        )
        .unwrap();
        let output = cli(
            dir.path(),
            &["format", "--safe", "scripts/main.jl", &shared],
            None,
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(&shared)).unwrap(),
            "typegroup = 1\n"
        );
    }
}

#[test]
fn inferred_roots_follow_the_includers_edition_and_keep_recursive_groups() {
    use fatou::parse_context::ContextCatalog;

    let dir = tempfile::tempdir().unwrap();
    for (name, edition) in [("Old", "1.13"), ("New", "1.14")] {
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Project.toml"),
            format!("name = '{name}'\n[syntax]\njulia_version = '{edition}'\n"),
        )
        .unwrap();
        std::fs::write(
            root.join(format!("src/{name}.jl")),
            format!("module {name} end\n"),
        )
        .unwrap();
    }
    let paths: Vec<_> = [
        (
            "Old/src/root.jl",
            "include(\"../../New/src/bridge.jl\")\ninclude(\"../../common.jl\")\n",
        ),
        ("New/src/bridge.jl", "break label include(\"child.jl\")\n"),
        (
            "New/src/child.jl",
            "include(\"child.jl\")\ninclude(\"../../common.jl\")\n",
        ),
        ("New/src/a.jl", "include(\"b.jl\")\n"),
        ("New/src/b.jl", "include(\"a.jl\")\n"),
    ]
    .into_iter()
    .map(|(path, text)| {
        let path = dir.path().join(path);
        std::fs::write(&path, text).unwrap();
        path
    })
    .collect();
    let common = dir.path().join("common.jl");
    std::fs::write(&common, "x = 1\n").unwrap();

    let targets = [2, 1, 0, 3, 4].map(|i| paths[i].clone());
    let catalog = ContextCatalog::discover(&targets, &[], &Default::default(), None);
    let roots = |path: &Path| {
        catalog
            .selections(path)
            .iter()
            .map(|(context, _)| &context.entry)
            .collect::<Vec<_>>()
    };
    assert_eq!(roots(&paths[0]), [&paths[0]]);
    assert_eq!(roots(&paths[1]), [&paths[0]]);
    assert_eq!(roots(&paths[2]), [&paths[2]]);
    assert_eq!(roots(&paths[3]), [&paths[3], &paths[4]]);
    assert_eq!(roots(&paths[4]), [&paths[3], &paths[4]]);
    assert_eq!(roots(&common), [&paths[2], &paths[0]]);
}

#[test]
fn cli_scripts_in_src_of_a_nameless_environment_use_the_script_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("Project.toml"), "[deps]\n").unwrap();
    std::fs::write(dir.path().join("src/main.jl"), "x  =  2 +% 1\n").unwrap();
    let output = cli(dir.path(), &["format", "--safe", "src/main.jl"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/main.jl")).unwrap(),
        "x = 2 +% 1\n"
    );
}

#[test]
fn cli_configured_program_in_package_src_uses_script_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    for (path, text) in [
        ("Project.toml", "name = 'Foo'\n"),
        (
            "fatou.toml",
            "[project]\nentry-points = ['src/program.jl']\n",
        ),
        ("src/Foo.jl", "module Foo end\n"),
        ("src/program.jl", "x  =  2 +% 1\n"),
    ] {
        std::fs::write(dir.path().join(path), text).unwrap();
    }
    let output = cli(dir.path(), &["format", "--safe", "src/program.jl"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/program.jl")).unwrap(),
        "x = 2 +% 1\n"
    );
}

#[test]
fn lint_fix_checks_every_package_in_the_batch_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("fatou.toml"),
        "[lint]\nselect = ['nothing-comparison']\n",
    )
    .unwrap();
    for (package, edition) in [("Old", "1.13"), ("New", "1.14")] {
        let root = dir.path().join(package);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Project.toml"),
            format!("name = '{package}'\n[syntax]\njulia_version = '{edition}'\n"),
        )
        .unwrap();
        std::fs::write(
            root.join(format!("src/{package}.jl")),
            format!("module {package}\ninclude(\"../../Old/src/shared.jl\")\nend\n"),
        )
        .unwrap();
    }
    let path = dir.path().join("Old/src/shared.jl");
    let input = "typegroup = x == nothing\n";
    std::fs::write(&path, input).unwrap();
    let output = cli(
        dir.path(),
        &[
            "lint",
            "--fix",
            "Old/src/Old.jl",
            "New/src/New.jl",
            "Old/src/shared.jl",
        ],
        None,
    );
    assert!(!output.status.success());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), input);
}

#[test]
fn unused_import_projects_package_siblings_in_the_overridden_edition() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    for (file, text) in [
        (
            "Project.toml",
            "name = 'Foo'\n[syntax]\njulia_version = '1.13'\n",
        ),
        ("fatou.toml", "[lint]\nselect = ['unused-import']\n"),
        (
            "src/Foo.jl",
            "module Foo\ninclude(\"imports.jl\")\ninclude(\"arith.jl\")\nend\n",
        ),
        ("src/imports.jl", "using Base: sin\n"),
        ("src/arith.jl", "f(x) = x +% 1\n"),
    ] {
        std::fs::write(dir.path().join(file), text).unwrap();
    }
    let output = cli(
        dir.path(),
        &[
            "lint",
            "--syntax-edition",
            "1.14",
            "--output",
            "json",
            "src",
        ],
        None,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success());
    assert!(stdout.contains("unused-import"), "{stdout}");
    assert!(!stdout.contains("parse-error"), "{stdout}");
}

#[test]
fn cli_discovers_package_and_script_defaults_and_honors_the_override() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Project.toml"),
        "name = \"Foo\"\nuuid = \"1520ce14-60c1-5f80-bbc7-55ef81b5835c\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/Foo.jl"), "typegroup  =  1\n").unwrap();
    let output = cli(dir.path(), &["format", "--safe", "src/Foo.jl"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/Foo.jl")).unwrap(),
        "typegroup = 1\n"
    );
    let output = cli(dir.path(), &["format", "--safe"], Some("typegroup = 1\n"));
    assert!(!output.status.success(), "stdin defaults to latest");
    let output = cli(
        dir.path(),
        &["format", "--safe", "--syntax-edition", "1.13"],
        Some("typegroup  =  1\n"),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "typegroup = 1\n");
    let output = cli(
        dir.path(),
        &[
            "lint",
            "--output",
            "json",
            "--syntax-edition",
            "1.14",
            "src/Foo.jl",
        ],
        None,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("parse-error"));
    let output = cli(
        dir.path(),
        &[
            "parse",
            "--syntax-edition",
            "1.13",
            "--to",
            "sexpr",
            "src/Foo.jl",
        ],
        None,
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("(= typegroup 1)"));
    let output = cli(
        dir.path(),
        &["debug", "format", "--syntax-edition", "1.13", "src/Foo.jl"],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_withholds_writes_for_invalid_metadata_and_conflicting_include_contexts() {
    let dir = tempfile::tempdir().unwrap();
    for (name, edition) in [("old", "1.13"), ("new", "1.14")] {
        let root = dir.path().join(name);
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("Project.toml"),
            format!("[syntax]\njulia_version = \"{edition}\"\n"),
        )
        .unwrap();
        std::fs::write(root.join("main.jl"), "include(\"../shared.jl\")\n").unwrap();
    }
    std::fs::write(
        dir.path().join("fatou.toml"),
        "[project]\nentry-points = [\"old/main.jl\", \"new/main.jl\"]\n",
    )
    .unwrap();
    let input = "typegroup  =  1\n";
    std::fs::write(dir.path().join("shared.jl"), input).unwrap();
    let output = cli(dir.path(), &["format", "shared.jl"], None);
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("shared.jl")).unwrap(),
        input
    );
    let output = cli(
        dir.path(),
        &["format", "--syntax-edition", "1.13", "shared.jl"],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(
        dir.path().join("old/Project.toml"),
        "[syntax]\njulia_version = \"oops\"\n",
    )
    .unwrap();
    let output = cli(dir.path(), &["format", "old/main.jl"], None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("uncertain"));
}

#[test]
fn changed_editions_exclude_stale_indexes_until_the_matching_variant_arrives() {
    use fatou::incremental::IncrementalDatabase;
    use fatou::index::{LibrarySyntax, PackageIndex, PackageSyntax};
    use fatou::parse_context::EditionSource;
    use fatou::parser::ParseOptions;
    use std::{collections::BTreeMap, sync::Arc};
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("Project.toml");
    std::fs::write(&project, "[syntax]\njulia_version = \"1.13\"\n").unwrap();
    let old = ParseOptions {
        syntax_edition: SyntaxEdition::Julia113,
    };
    let new = ParseOptions {
        syntax_edition: SyntaxEdition::Julia114,
    };
    let index = Arc::new(PackageIndex {
        name: "Foo".into(),
        root: fatou::index::harvest_tree(&fatou::parser::parse("").cst),
        members: vec![],
        member_modules: Default::default(),
        diagnostics: vec![],
    });
    let syntax = LibrarySyntax {
        packages: BTreeMap::from([(
            "Foo".into(),
            PackageSyntax {
                root: dir.path().to_path_buf(),
                source: EditionSource::Project {
                    path: project.clone(),
                    role: SourceRole::Package,
                },
                options: old,
            },
        )]),
        variants: BTreeMap::from([(("Foo".into(), old), Arc::clone(&index))]),
        ..Default::default()
    };
    let mut db = IncrementalDatabase::default();
    db.set_library_syntax(syntax.clone());
    db.set_library(
        BTreeMap::from([("Foo".into(), Arc::clone(&index))]),
        BTreeMap::from([("Foo".into(), dir.path().to_path_buf())]),
        vec!["Foo".into()],
    );
    assert!(db.library_package("Foo").is_some());
    db.upsert_file(&project, "[syntax]\njulia_version = \"1.14\"\n");
    assert!(
        db.library_package("Foo").is_none(),
        "old grammar must not leak into resolution"
    );
    assert_eq!(
        db.pending_syntax_harvests(),
        vec![("Foo".into(), dir.path().to_path_buf(), new)]
    );
    db.install_syntax_variant("Foo".into(), new, index);
    assert!(db.library_package("Foo").is_some());
    db.set_library_syntax(syntax);
    assert!(
        db.library_package("Foo").is_none(),
        "a disk harvest cannot overwrite the live project edition"
    );
    db.revert_file_to_disk(&project);
    assert!(db.library_package("Foo").is_some());
}

#[test]
fn system_harvests_use_install_and_stdlib_editions_in_their_cache_keys() {
    use fatou::environment::JuliaInstall;
    use fatou::index::{IndexCache, build_system_library_cached};
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("base");
    let stdlib = dir.path().join("stdlib");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(stdlib.join("Example/src")).unwrap();
    std::fs::write(base.join("Base.jl"), "module Base\ntypegroup = 1\nend\n").unwrap();
    std::fs::write(base.join("boot.jl"), "module Core\nend\n").unwrap();
    std::fs::write(
        stdlib.join("Example/src/Example.jl"),
        "module Example\ntypegroup = 1\nend\n",
    )
    .unwrap();
    let project = stdlib.join("Example/Project.toml");
    std::fs::write(&project, "[syntax]\njulia_version = \"1.13\"\n").unwrap();
    let mut install = JuliaInstall {
        prefix: dir.path().to_path_buf(),
        share: dir.path().to_path_buf(),
        base_dir: base,
        stdlib_dir: stdlib,
        version: "1.13".into(),
    };
    let cache = IndexCache::open_at(dir.path().join("cache"));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    let old = build_system_library_cached(Some(&install), Some(&cache), &pool);
    assert!(old.packages["Base"].diagnostics.is_empty());
    assert!(old.packages["Example"].diagnostics.is_empty());
    install.version = "1.14".into();
    let new = build_system_library_cached(Some(&install), Some(&cache), &pool);
    assert!(!new.packages["Base"].diagnostics.is_empty());
    assert!(
        new.packages["Example"].diagnostics.is_empty(),
        "stdlib owns its package edition"
    );
    std::fs::write(project, "[syntax]\njulia_version = \"1.14\"\n").unwrap();
    let changed = build_system_library_cached(Some(&install), Some(&cache), &pool);
    assert!(
        !changed.packages["Example"].diagnostics.is_empty(),
        "the cache key includes the stdlib edition"
    );
}

#[test]
fn embedded_docstring_julia_inherits_the_containing_edition() {
    use fatou::linter::{ProjectContext, ResolvedRules};
    use fatou::parse_context::{EditionSelection, EditionSource, SourceContext};
    let (rules, _) = ResolvedRules::resolve(&fatou::config::LintConfig::default());
    let contexts: Vec<_> = [SyntaxEdition::Julia113, SyntaxEdition::Julia114]
        .into_iter()
        .map(|edition| {
            (
                SourceContext {
                    entry: Default::default(),
                    source: EditionSource::Fixed(edition),
                },
                EditionSelection::known(edition),
            )
        })
        .collect();
    let report = fatou::linter::check::check_text_in_contexts(
        None,
        "\"\"\"\n```julia\ntypegroup = 1\n```\n\"\"\"\nf() = 1\n",
        &rules,
        ProjectContext::SystemOnly,
        &contexts,
    );
    assert!(!report.diagnostics.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.message.body.contains("1.14"))
    );
}

#[test]
fn cached_include_membership_follows_each_environment_when_editions_change() {
    use fatou::incremental::IncrementalDatabase;
    use fatou::index::{LibrarySyntax, PackageIndex, PackageSyntax};
    use fatou::parse_context::{EditionSource, SourceContext};
    use fatou::parser::ParseOptions;
    use std::{collections::BTreeMap, sync::Arc};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let old = ParseOptions {
        syntax_edition: SyntaxEdition::Julia113,
    };
    let new = ParseOptions {
        syntax_edition: SyntaxEdition::Julia114,
    };
    let uuid = "1520ce14-60c1-5f80-bbc7-55ef81b5835c".parse().unwrap();
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::create_dir(root.join("b")).unwrap();
    let a = root.join("a/Manifest.toml");
    let b = root.join("b/Manifest.toml");
    let metadata = format!("[[deps.Foo]]\nuuid = \"{uuid}\"\n");
    std::fs::write(&a, &metadata).unwrap();
    std::fs::write(&b, format!("{metadata}syntax.julia_version = \"1.14\"\n")).unwrap();
    let source_a = EditionSource::Manifest {
        path: a.clone(),
        name: "Foo".into(),
        uuid,
    };
    let source_b = EditionSource::Manifest {
        path: b.clone(),
        name: "Foo".into(),
        uuid,
    };
    let index = |member: &str| {
        Arc::new(PackageIndex {
            name: "Foo".into(),
            root: fatou::index::harvest_tree(&fatou::parser::parse("").cst),
            members: vec!["src/Foo.jl".into(), member.into()],
            member_modules: Default::default(),
            diagnostics: vec![],
        })
    };
    let old_index = index("old.jl");
    let mut db = IncrementalDatabase::default();
    db.set_library_syntax(LibrarySyntax {
        packages: BTreeMap::from([(
            "Foo".into(),
            PackageSyntax {
                root: root.to_path_buf(),
                source: source_a.clone(),
                options: old,
            },
        )]),
        variants: BTreeMap::from([(("Foo".into(), old), old_index)]),
        environments: BTreeMap::from([(b, BTreeMap::from([("Foo".into(), source_b.clone())]))]),
        ..Default::default()
    });
    db.install_syntax_variant("Foo".into(), new, index("new.jl"));
    let file = db.upsert_file(&root.join("new.jl"), "x = 1\n");
    assert!(
        db.source_editions(file)
            .iter()
            .any(|(context, selected)| context.source == source_b && selected.options == new)
    );
    assert!(
        !db.source_editions(file)
            .iter()
            .any(|(context, _)| context.source == source_a)
    );
    db.upsert_file(&a, format!("{metadata}syntax.julia_version = \"1.14\"\n"));
    assert!(
        db.source_editions(file)
            .iter()
            .any(|(context, selected)| *context
                == SourceContext {
                    entry: root.join("src/Foo.jl"),
                    source: source_a.clone()
                }
                && selected.options == new)
    );
    assert!(
        db.pending_syntax_harvests().is_empty(),
        "switching to a cached variant needs no new harvest"
    );
}

#[test]
fn an_include_cycle_does_not_promote_its_downstream_helper() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("old")).unwrap();
    std::fs::create_dir(dir.path().join("new")).unwrap();
    for (path, text) in [
        ("old/Project.toml", "[syntax]\njulia_version = '1.13'\n"),
        ("new/Project.toml", "[syntax]\njulia_version = '1.14'\n"),
        ("old/a.jl", "include(\"b.jl\")\n"),
        (
            "old/b.jl",
            "include(\"a.jl\")\ninclude(\"../new/helper.jl\")\n",
        ),
        ("new/helper.jl", "typegroup  =  1\n"),
    ] {
        std::fs::write(dir.path().join(path), text).unwrap();
    }
    let output = cli(
        dir.path(),
        &["format", "--safe", "old/a.jl", "old/b.jl", "new/helper.jl"],
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new/helper.jl")).unwrap(),
        "typegroup = 1\n"
    );
}

#[test]
fn workspace_harvest_uses_the_explicitly_selected_project_file() {
    use fatou::environment::{self, EnvContext};
    use fatou::index::{harvest_libraries, harvest_libraries_parallel};

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    for (path, text) in [
        (
            "Project.toml",
            "name = 'Foo'\n[syntax]\njulia_version = '1.13'\n",
        ),
        (
            "JuliaProject.toml",
            "name = 'Foo'\n[syntax]\njulia_version = '1.14'\n",
        ),
        ("src/Foo.jl", "module Foo\ninclude(\"sibling.jl\")\nend\n"),
        ("src/sibling.jl", "typegroup() = 1\n"),
    ] {
        std::fs::write(dir.path().join(path), text).unwrap();
    }
    let env = environment::resolve(&EnvContext {
        workspace_root: dir.path().to_path_buf(),
        julia_project: Some(dir.path().join("Project.toml").display().to_string()),
        julia_depot_path: None,
        home: None,
        julia_bindir: None,
        path: None,
    })
    .unwrap()
    .unwrap();
    let envs = &[env];
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    for library in [
        harvest_libraries(envs),
        harvest_libraries_parallel(envs, None, &pool),
    ] {
        assert!(
            library.packages["Foo"]
                .root
                .functions
                .iter()
                .any(|function| function.name == "typegroup")
        );
    }
}
