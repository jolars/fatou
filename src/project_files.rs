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

use std::path::{Path, PathBuf};

use rowan::{TextRange, TextSize};

use crate::environment::{Environment, EnvironmentError, is_project_file, parse_project_text};
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
pub fn semantic_findings(_env: &Environment, _project_text: &str) -> Vec<ProjectFinding> {
    Vec::new()
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
