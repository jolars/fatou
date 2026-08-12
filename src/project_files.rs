//! Checks over Julia's project files themselves: `Project.toml` and
//! `Manifest.toml`.
//!
//! These are **not lint rules.** [`crate::linter::Rule`] dispatches on
//! `SyntaxKind` over a single walk of the Julia CST, and by tenet the linter is
//! purely semantic over Julia; a TOML check has no kind to register against.
//! Findings here carry no rule ID, appear in no rule reference, and are not
//! suppressible with `# fatou-ignore`.
//!
//! The module is deliberately **LSP-independent**: a [`ProjectFinding`] carries
//! a byte range and a [`Severity`], and the mapping to `lsp_types` happens at
//! the edge in `crate::lsp`, exactly as it already does for lint findings. The
//! cautionary precedent is the include-graph analysis, which was written
//! against `lsp_types` in `crate::lsp::graph_diagnostics` and then had to be
//! written a *second* time for the CLI in `crate::linter::include_graph`.
//!
//! Two entry points, because a syntax failure is precisely the case where there
//! is no [`Environment`] to check: [`syntax_findings`] reports on one file's
//! text, and [`semantic_findings`] reports on a resolved environment.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rowan::{TextRange, TextSize};
use toml::Spanned;

use crate::environment::{
    Environment, EnvironmentError, PackageKind, ProjectFile, Uuid, is_project_file,
    parse_project_text,
};
use crate::linter::Severity;

/// One project-file finding, anchored to a byte range in a named file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFinding {
    /// Stable check ID, e.g. `"toml-syntax"`. Rendered as the diagnostic's
    /// `code`. Not a lint rule ID: it names no registry entry and has no
    /// reference page to link to.
    pub check: &'static str,
    pub severity: Severity,
    /// The file whose text [`range`](Self::range) indexes.
    pub path: PathBuf,
    pub range: TextRange,
    pub message: String,
}

/// TOML syntax and schema findings for one already-read file. Empty when it
/// parses.
///
/// The file name picks the schema: a project file is read against the typed
/// `ProjectFile`, so a wrong-typed `name` is caught alongside a malformed
/// table; a manifest is read against a plain table, since nothing anchors
/// inside a manifest beyond its syntax.
pub fn syntax_findings(path: &Path, text: &str) -> Vec<ProjectFinding> {
    let parsed = if is_project_file(path) {
        parse_project_text(path, text).map(|_| ())
    } else {
        toml::from_str::<toml::Table>(text)
            .map(|_| ())
            .map_err(|err| EnvironmentError::Parse {
                path: path.to_path_buf(),
                message: err.message().to_string(),
                span: err.span(),
            })
    };
    let Err(error) = parsed else {
        return Vec::new();
    };
    vec![syntax_finding(path, text, &error)]
}

/// Turn a read or parse failure into the finding that reports it. Public so the
/// server can report the failure that [`crate::environment::resolve`] hands
/// back, which is the only place an unreadable environment is observed.
pub fn syntax_finding(path: &Path, text: &str, error: &EnvironmentError) -> ProjectFinding {
    let (message, span) = match error {
        EnvironmentError::Read { message, .. } => (message.clone(), None),
        EnvironmentError::Parse { message, span, .. } => (message.clone(), span.clone()),
    };
    ProjectFinding {
        check: "toml-syntax",
        severity: Severity::Error,
        path: path.to_path_buf(),
        range: span.map_or_else(|| first_line(text), |span| to_range(span.start, span.end)),
        message,
    }
}

/// The semantic checks over a resolved environment. Every returned range
/// indexes `project_text`, which must be the text of `env.project_file`.
/// Empty when that text does not parse — [`syntax_findings`] owns that case.
pub fn semantic_findings(env: &Environment, project_text: &str) -> Vec<ProjectFinding> {
    let Ok(project) = parse_project_text(&env.project_file, project_text) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    let mut warn = |check, range, message| {
        findings.push(ProjectFinding {
            check,
            severity: Severity::Warning,
            path: env.project_file.clone(),
            range,
            message,
        });
    };

    let is_package = project.name.is_some() && project.uuid.is_some();

    // A package whose entry file is missing silently stops being a package:
    // nothing harvests, and every symbol it exports goes unresolved.
    if is_package
        && let Some(entry) = env.entry_file()
        && !entry.is_file()
        && let (Some(name), Some(file_name)) = (&project.name, entry.file_name())
    {
        warn(
            "missing-entry-file",
            span(name),
            format!(
                "package `{}` has no entry file `src/{}`",
                name.as_ref(),
                file_name.to_string_lossy()
            ),
        );
    }

    // A package with no declared Julia support range: `julia-version-compat`
    // has nothing to check against, and the registry will not accept it.
    if is_package && !project.compat.contains_key("julia") {
        let anchor = project
            .compat
            .keys()
            .next()
            .or(project.name.as_ref())
            .map_or_else(|| first_line(project_text), span);
        warn(
            "missing-julia-compat",
            anchor,
            "no `[compat]` bound on `julia`".to_string(),
        );
    }

    // A compat entry naming nothing is dead weight at best and a typo for a
    // real dependency at worst. `[extras]` and `[weakdeps]` count: a test-only
    // or extension-triggering dependency is legitimately bounded.
    for name in project.compat.keys().filter(|name| {
        name.as_ref() != "julia"
            && ![&project.deps, &project.extras, &project.weakdeps]
                .iter()
                .any(|table| table.contains_key(name.as_ref().as_str()))
    }) {
        warn(
            "unknown-compat",
            span(name),
            format!(
                "compat entry `{}` names nothing in `[deps]`, `[extras]`, or `[weakdeps]`",
                name.as_ref()
            ),
        );
    }

    // A dependency with no compat bound: unbounded upgrades, and the General
    // registry rejects it. Skipped wholesale when `[compat]` is absent
    // entirely — a package with ten deps and no compat table should produce
    // the one finding above, not eleven, and the fix is the same.
    if is_package && !project.compat.is_empty() {
        let stdlib = StdlibOracle::of(env);
        for name in project.deps.keys().filter(|name| {
            let name = name.as_ref().as_str();
            !project.compat.contains_key(name)
                // A URL- or path-pinned dependency is not registry-resolved,
                // so a version bound on it means little.
                && !project.sources.contains_key(name)
                // Only when the name is *known* not to be a standard library:
                // the registry does not require compat for those, and an
                // uninstantiated project cannot tell them apart on its own.
                && stdlib.is_stdlib(name) == Some(false)
        }) {
            warn(
                "missing-compat",
                span(name),
                format!("dependency `{}` has no `[compat]` bound", name.as_ref()),
            );
        }
    }

    findings.extend(manifest_findings(env, &project));
    findings.sort_by_key(|finding| finding.range.start());
    findings
}

/// Whether a dependency name is a standard library, from the two sources that
/// can answer without running Julia. Neither alone is enough: the manifest
/// classifies only what it resolved, so an uninstantiated project needs the
/// install, and a machine with no Julia install needs the manifest.
///
/// An unclassifiable name answers `None`, and a check that cannot be sound
/// without an answer must stay quiet for it.
struct StdlibOracle {
    /// Each manifest-resolved name, with whether it is a standard library.
    manifest: HashMap<String, bool>,
    /// The located install's `stdlib/vX.Y` directory entries, if any.
    install: Option<HashSet<String>>,
}

impl StdlibOracle {
    fn of(env: &Environment) -> Self {
        let manifest = env
            .packages
            .iter()
            .map(|package| (package.name.clone(), package.kind == PackageKind::Stdlib))
            .collect();
        let install = env.install.as_ref().and_then(|install| {
            let entries = std::fs::read_dir(&install.stdlib_dir).ok()?;
            Some(
                entries
                    .filter_map(|entry| {
                        let entry = entry.ok()?;
                        entry
                            .file_type()
                            .ok()?
                            .is_dir()
                            .then(|| entry.file_name().to_string_lossy().into_owned())
                    })
                    .collect(),
            )
        });
        Self { manifest, install }
    }

    fn is_stdlib(&self, name: &str) -> Option<bool> {
        if let Some(&stdlib) = self.manifest.get(name) {
            return Some(stdlib);
        }
        Some(self.install.as_ref()?.contains(name))
    }
}

/// The checks that compare `[deps]` against the manifest's resolved set.
///
/// All of them are skipped without a manifest *or* with an empty one: an
/// uninstantiated project would otherwise light up on every dependency. That
/// also, deliberately, keeps a Julia 1.11 workspace member quiet — a member
/// project has no sibling manifest, so `manifest_file` is `None`.
fn manifest_findings(env: &Environment, project: &ProjectFile) -> Vec<ProjectFinding> {
    if env.manifest_file.is_none() || env.packages.is_empty() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for (name, declared) in &project.deps {
        // Every manifest entry of this name: the array-of-tables form exists
        // precisely so one name can have several, and matching any is a match.
        let mut entries = env
            .packages
            .iter()
            .filter(|package| package.name == *name.as_ref())
            .peekable();
        let Some(first) = entries.peek().copied() else {
            findings.push(ProjectFinding {
                check: "missing-from-manifest",
                severity: Severity::Warning,
                path: env.project_file.clone(),
                range: span(name),
                message: format!(
                    "dependency `{}` is not in the manifest; run `Pkg.instantiate()`",
                    name.as_ref()
                ),
            });
            continue;
        };
        // An unparseable UUID is a different defect, and not one reported here.
        let Ok(declared_uuid) = declared.as_ref().parse::<Uuid>() else {
            continue;
        };
        if entries.any(|package| package.uuid == declared_uuid) {
            continue;
        }
        findings.push(ProjectFinding {
            check: "uuid-mismatch",
            severity: Severity::Warning,
            path: env.project_file.clone(),
            // The value, not the key: the UUID is the wrong part.
            range: span(declared),
            message: format!(
                "UUID for `{}` disagrees with the manifest: project has `{}`, manifest has `{}`",
                name.as_ref(),
                declared_uuid,
                first.uuid
            ),
        });
    }
    findings
}

/// A spanned value's byte range.
fn span<T>(spanned: &Spanned<T>) -> TextRange {
    let span = spanned.span();
    to_range(span.start, span.end)
}

/// The range a finding with no span of its own points at: the first line, or
/// the whole (short) text when it has no newline. Never a zero-width range at
/// offset 0, which clients render inconsistently.
fn first_line(text: &str) -> TextRange {
    to_range(0, text.find('\n').unwrap_or(text.len()))
}

/// Byte offsets to a [`TextRange`], clamped to `u32`. A project file large
/// enough to overflow is not a case worth a fallible signature.
fn to_range(start: usize, end: usize) -> TextRange {
    let clamp = |offset: usize| TextSize::new(u32::try_from(offset).unwrap_or(u32::MAX));
    TextRange::new(clamp(start), clamp(end.max(start)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(text: &str) -> Vec<ProjectFinding> {
        syntax_findings(Path::new("Project.toml"), text)
    }

    #[test]
    fn a_valid_project_file_has_no_findings() {
        assert!(project("name = \"Demo\"\n\n[deps]\n").is_empty());
    }

    #[test]
    fn a_syntax_error_is_reported_at_its_span() {
        let text = "name = \"Demo\"\nuuid = \n";
        let findings = project(text);

        let [finding] = &findings[..] else {
            panic!("expected exactly one finding, got {findings:?}");
        };
        assert_eq!(finding.check, "toml-syntax");
        assert_eq!(finding.severity, Severity::Error);
        assert!(
            usize::from(finding.range.start()) >= text.find("uuid").unwrap(),
            "the range points at the offending line, not the file head: {:?}",
            finding.range
        );
    }

    /// The project file is read against a typed schema, so a value of the wrong
    /// type is caught here rather than silently dropped during resolution.
    #[test]
    fn a_wrong_typed_value_is_a_finding() {
        let findings = project("name = 42\n");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].check, "toml-syntax");
    }

    /// A manifest is checked for syntax only. Its `[deps]` shape differs
    /// between format 1.0 and 2.0, and neither is the project schema.
    #[test]
    fn a_manifest_is_checked_for_syntax_only() {
        let manifest = Path::new("Manifest.toml");
        let valid = "julia_version = \"1.11.7\"\nmanifest_format = \"2.0\"\n\n\
                     [[deps.AbstractTrees]]\nuuid = \"1520ce14-60c1-5f80-bbc7-55ef81b5835c\"\n";
        assert!(syntax_findings(manifest, valid).is_empty());

        // `name` typed wrong would be a project-schema error; in a manifest it
        // is just an unremarkable key.
        assert!(syntax_findings(manifest, "name = 42\n").is_empty());

        assert_eq!(syntax_findings(manifest, "[[deps\n").len(), 1);
    }

    // --- The semantic checks ------------------------------------------------

    const DATES: &str = "ade2ca70-3891-5945-98fb-dc099432e06a";
    const TREES: &str = "1520ce14-60c1-5f80-bbc7-55ef81b5835c";

    /// Resolve an environment from an on-disk fixture and run the semantic
    /// checks over it. Files are written under a temp dir; `manifest` and the
    /// entry file are written only when given.
    ///
    /// Built through `environment::resolve` rather than an `Environment` struct
    /// literal so a future field cannot silently skew what is under test.
    fn check(project: &str, manifest: Option<&str>, entry: Option<&str>) -> Vec<ProjectFinding> {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("Project.toml"), project).unwrap();
        if let Some(manifest) = manifest {
            std::fs::write(root.join("Manifest.toml"), manifest).unwrap();
        }
        if let Some(entry) = entry {
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(root.join("src").join(entry), "module M\nend\n").unwrap();
        }

        let ctx = crate::environment::EnvContext {
            workspace_root: root.to_path_buf(),
            julia_project: None,
            julia_depot_path: Some(root.join("depot").to_string_lossy().into_owned()),
            home: None,
            julia_bindir: None,
            path: None,
        };
        let env = crate::environment::resolve(&ctx)
            .expect("resolves")
            .expect("an environment");
        semantic_findings(&env, project)
    }

    /// The check IDs a fixture produces, in report order.
    fn checks(findings: &[ProjectFinding]) -> Vec<&'static str> {
        findings.iter().map(|finding| finding.check).collect()
    }

    fn only<'a>(findings: &'a [ProjectFinding], check: &str) -> &'a ProjectFinding {
        let matching: Vec<_> = findings.iter().filter(|f| f.check == check).collect();
        assert_eq!(matching.len(), 1, "expected one {check}, got {findings:?}");
        matching[0]
    }

    /// A package whose entry file is missing stops being a package silently:
    /// nothing harvests and its exports all go unresolved.
    #[test]
    fn a_package_without_its_entry_file_is_reported() {
        let project =
            format!("name = \"Demo\"\nuuid = \"{DATES}\"\n\n[compat]\njulia = \"1.10\"\n");
        let findings = check(&project, None, None);

        let finding = only(&findings, "missing-entry-file");
        assert_eq!(finding.severity, Severity::Warning);
        assert!(finding.message.contains("src/Demo.jl"), "{finding:?}");
        assert_eq!(
            &project[finding.range], "\"Demo\"",
            "anchored on the name value"
        );
    }

    #[test]
    fn a_package_with_its_entry_file_is_not_reported() {
        let project =
            format!("name = \"Demo\"\nuuid = \"{DATES}\"\n\n[compat]\njulia = \"1.10\"\n");
        assert!(!checks(&check(&project, None, Some("Demo.jl"))).contains(&"missing-entry-file"));
    }

    /// A plain environment is not a package: it legitimately has no entry file
    /// and no julia compat. This repo's own `Project.toml` is exactly that
    /// shape, and without the gate it would light up on every dependency.
    #[test]
    fn a_nameless_environment_is_not_held_to_package_rules() {
        let project = format!("[deps]\nDates = \"{DATES}\"\n");
        assert!(check(&project, None, None).is_empty());
    }

    /// A `name` with no `uuid` is a named environment, not a package: Julia
    /// requires both. `dev_package` is deliberately looser and stays so.
    #[test]
    fn a_uuidless_project_is_not_held_to_package_rules() {
        let project = format!("name = \"Demo\"\n\n[deps]\nDates = \"{DATES}\"\n");
        assert!(check(&project, None, None).is_empty());
    }

    #[test]
    fn a_package_without_julia_compat_is_reported() {
        let project = format!("name = \"Demo\"\nuuid = \"{DATES}\"\n\n[compat]\nFoo = \"1\"\n");
        let findings = check(&project, None, Some("Demo.jl"));

        let finding = only(&findings, "missing-julia-compat");
        assert_eq!(
            &project[finding.range], "Foo",
            "anchored on the first compat key, where the missing entry goes"
        );
    }

    /// With no `[compat]` table at all there is nothing to anchor on, so the
    /// finding falls back to the name.
    #[test]
    fn a_missing_compat_table_anchors_on_the_name() {
        let project = format!("name = \"Demo\"\nuuid = \"{DATES}\"\n");
        let findings = check(&project, None, Some("Demo.jl"));
        let finding = only(&findings, "missing-julia-compat");
        assert_eq!(&project[finding.range], "\"Demo\"");
    }

    /// One missing `[compat]` table is one finding, not one per dependency:
    /// the fix is the same for all of them.
    #[test]
    fn an_absent_compat_table_suppresses_the_per_dependency_findings() {
        let project =
            format!("name = \"Demo\"\nuuid = \"{DATES}\"\n\n[deps]\nAbstractTrees = \"{TREES}\"\n");
        assert_eq!(
            checks(&check(&project, None, Some("Demo.jl"))),
            vec!["missing-julia-compat"]
        );
    }

    #[test]
    fn a_compat_entry_naming_nothing_is_reported() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n[compat]\njulia = \"1.10\"\nGhost = \"1\"\n"
        );
        let findings = check(&project, None, Some("Demo.jl"));
        let finding = only(&findings, "unknown-compat");
        assert_eq!(&project[finding.range], "Ghost");
    }

    /// A compat entry may bound a test-only or extension-triggering dependency,
    /// which lives in `[extras]`/`[weakdeps]` rather than `[deps]`.
    #[test]
    fn compat_may_name_an_extra_or_a_weakdep() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n\
             [extras]\nAbstractTrees = \"{TREES}\"\n\n\
             [weakdeps]\nGhost = \"{DATES}\"\n\n\
             [compat]\njulia = \"1.10\"\nAbstractTrees = \"0.4\"\nGhost = \"1\"\n"
        );
        assert!(!checks(&check(&project, None, Some("Demo.jl"))).contains(&"unknown-compat"));
    }

    /// Every dependency of an uninstantiated project would otherwise be
    /// reported missing, which is noise rather than a finding.
    #[test]
    fn manifest_checks_stay_quiet_without_a_manifest() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n\
             [deps]\nAbstractTrees = \"{TREES}\"\n\n[compat]\njulia = \"1.10\"\n"
        );
        let reported = checks(&check(&project, None, Some("Demo.jl")));
        assert!(!reported.contains(&"missing-from-manifest"), "{reported:?}");
        assert!(!reported.contains(&"uuid-mismatch"), "{reported:?}");
    }

    #[test]
    fn a_dependency_absent_from_the_manifest_is_reported() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n\
             [deps]\nGhost = \"{DATES}\"\n\n[compat]\njulia = \"1.10\"\nGhost = \"1\"\n"
        );
        let manifest = format!(
            "manifest_format = \"2.0\"\n\n[[deps.AbstractTrees]]\nuuid = \"{TREES}\"\nversion = \"0.4.5\"\ngit-tree-sha1 = \"abc\"\n"
        );
        let findings = check(&project, Some(&manifest), Some("Demo.jl"));
        let finding = only(&findings, "missing-from-manifest");
        assert_eq!(&project[finding.range], "Ghost");
        assert!(finding.message.contains("Pkg.instantiate"), "{finding:?}");
    }

    #[test]
    fn a_uuid_disagreeing_with_the_manifest_is_reported() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n\
             [deps]\nAbstractTrees = \"{DATES}\"\n\n[compat]\njulia = \"1.10\"\nAbstractTrees = \"0.4\"\n"
        );
        let manifest = format!(
            "manifest_format = \"2.0\"\n\n[[deps.AbstractTrees]]\nuuid = \"{TREES}\"\nversion = \"0.4.5\"\ngit-tree-sha1 = \"abc\"\n"
        );
        let findings = check(&project, Some(&manifest), Some("Demo.jl"));

        let finding = only(&findings, "uuid-mismatch");
        assert_eq!(
            &project[finding.range],
            &format!("\"{DATES}\""),
            "anchored on the value, which is the wrong part"
        );
        assert!(finding.message.contains(TREES), "names both: {finding:?}");
        assert!(
            !checks(&findings).contains(&"missing-from-manifest"),
            "a mismatch is not also a disappearance"
        );
    }

    /// The manifest's array-of-tables form exists so one name can have several
    /// entries; matching any of them is a match.
    #[test]
    fn a_uuid_matching_any_manifest_entry_is_accepted() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n\
             [deps]\nAbstractTrees = \"{TREES}\"\n\n[compat]\njulia = \"1.10\"\nAbstractTrees = \"0.4\"\n"
        );
        let manifest = format!(
            "manifest_format = \"2.0\"\n\n\
             [[deps.AbstractTrees]]\nuuid = \"{DATES}\"\nversion = \"0.1.0\"\ngit-tree-sha1 = \"abc\"\n\n\
             [[deps.AbstractTrees]]\nuuid = \"{TREES}\"\nversion = \"0.4.5\"\ngit-tree-sha1 = \"def\"\n"
        );
        assert!(
            !checks(&check(&project, Some(&manifest), Some("Demo.jl"))).contains(&"uuid-mismatch")
        );
    }

    #[test]
    fn a_dependency_without_a_compat_bound_is_reported() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n\
             [deps]\nAbstractTrees = \"{TREES}\"\n\n[compat]\njulia = \"1.10\"\n"
        );
        let manifest = format!(
            "manifest_format = \"2.0\"\n\n[[deps.AbstractTrees]]\nuuid = \"{TREES}\"\nversion = \"0.4.5\"\ngit-tree-sha1 = \"abc\"\n"
        );
        let findings = check(&project, Some(&manifest), Some("Demo.jl"));
        let finding = only(&findings, "missing-compat");
        assert_eq!(&project[finding.range], "AbstractTrees");
    }

    /// The registry does not require compat for a standard library, and the
    /// manifest is what says a dependency is one.
    #[test]
    fn a_stdlib_dependency_needs_no_compat_bound() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{TREES}\"\n\n\
             [deps]\nDates = \"{DATES}\"\n\n[compat]\njulia = \"1.10\"\n"
        );
        // No `git-tree-sha1` and no `path`: a stdlib entry.
        let manifest = format!("manifest_format = \"2.0\"\n\n[[deps.Dates]]\nuuid = \"{DATES}\"\n");
        assert!(
            !checks(&check(&project, Some(&manifest), Some("Demo.jl"))).contains(&"missing-compat")
        );
    }

    /// With no manifest and no Julia install, nothing can say whether a
    /// dependency is a standard library — so the check that depends on knowing
    /// stays quiet rather than guessing.
    #[test]
    fn missing_compat_stays_quiet_without_a_stdlib_oracle() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{TREES}\"\n\n\
             [deps]\nDates = \"{DATES}\"\n\n[compat]\njulia = \"1.10\"\n"
        );
        assert!(!checks(&check(&project, None, Some("Demo.jl"))).contains(&"missing-compat"));
    }

    /// A URL- or path-pinned dependency is not registry-resolved, so a version
    /// bound on it means little.
    #[test]
    fn a_sourced_dependency_needs_no_compat_bound() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n\
             [deps]\nAbstractTrees = \"{TREES}\"\n\n\
             [sources]\nAbstractTrees = {{ path = \"vendor/AbstractTrees\" }}\n\n\
             [compat]\njulia = \"1.10\"\n"
        );
        let manifest = format!(
            "manifest_format = \"2.0\"\n\n[[deps.AbstractTrees]]\nuuid = \"{TREES}\"\nversion = \"0.4.5\"\ngit-tree-sha1 = \"abc\"\n"
        );
        assert!(
            !checks(&check(&project, Some(&manifest), Some("Demo.jl"))).contains(&"missing-compat")
        );
    }

    /// Findings are reported in file order, so a client shows them the way the
    /// file reads.
    #[test]
    fn findings_are_ordered_by_position() {
        let project = format!(
            "name = \"Demo\"\nuuid = \"{DATES}\"\n\n\
             [deps]\nGhost = \"{DATES}\"\n\n[compat]\nGhost = \"1\"\nStray = \"2\"\n"
        );
        let manifest = format!(
            "manifest_format = \"2.0\"\n\n[[deps.AbstractTrees]]\nuuid = \"{TREES}\"\nversion = \"0.4.5\"\ngit-tree-sha1 = \"abc\"\n"
        );
        let findings = check(&project, Some(&manifest), None);
        let starts: Vec<_> = findings.iter().map(|f| f.range.start()).collect();
        let mut sorted = starts.clone();
        sorted.sort();
        assert_eq!(starts, sorted, "{findings:?}");
    }

    /// The sharpest false-positive test available: this repo's own pinned Julia
    /// environment, a real committed `Project.toml`/`Manifest.toml` pair. It is
    /// a bare environment (deps and compat, no `name`/`uuid`), which is exactly
    /// the shape the package gates exist to spare.
    #[test]
    fn this_repos_own_environment_is_clean() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let ctx = crate::environment::EnvContext {
            workspace_root: root.to_path_buf(),
            julia_project: None,
            julia_depot_path: Some(root.join("target/nonexistent-depot").display().to_string()),
            home: None,
            julia_bindir: None,
            path: None,
        };
        let env = crate::environment::resolve(&ctx)
            .expect("the repo's own project resolves")
            .expect("an environment");
        let text = std::fs::read_to_string(&env.project_file).unwrap();

        // Guard against passing vacuously: every check must have had something
        // to look at.
        assert_eq!(env.project_file, root.join("Project.toml"));
        assert!(env.name.is_none(), "a bare environment, not a package");
        assert!(!env.direct_deps.is_empty(), "it declares dependencies");
        assert!(!env.packages.is_empty(), "its manifest resolved");

        let findings = semantic_findings(&env, &text);
        assert!(findings.is_empty(), "{findings:?}");
    }

    /// A failure to parse is `syntax_findings`' business; the semantic checks
    /// must not also report on a file they cannot read.
    #[test]
    fn semantic_checks_are_silent_on_an_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Project.toml"), "name = \"Demo\"\n").unwrap();
        let ctx = crate::environment::EnvContext {
            workspace_root: dir.path().to_path_buf(),
            julia_project: None,
            julia_depot_path: Some(dir.path().join("depot").to_string_lossy().into_owned()),
            home: None,
            julia_bindir: None,
            path: None,
        };
        let env = crate::environment::resolve(&ctx).unwrap().unwrap();

        assert!(semantic_findings(&env, "uuid = \n").is_empty());
    }

    /// A failure with no span still produces a usable range rather than a
    /// zero-width one at the file head.
    #[test]
    fn a_spanless_failure_falls_back_to_the_first_line() {
        let error = EnvironmentError::Read {
            path: PathBuf::from("Project.toml"),
            message: "permission denied".to_string(),
        };
        let finding = syntax_finding(
            Path::new("Project.toml"),
            "name = \"Demo\"\nx = 1\n",
            &error,
        );

        assert_eq!(finding.range, TextRange::new(0.into(), 13.into()));
        assert_eq!(finding.message, "permission denied");
    }
}
