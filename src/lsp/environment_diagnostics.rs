//! The LSP edge of the project-file checks: read each environment file, run
//! [`crate::project_files`] over it, and map the byte-ranged findings to
//! `lsp_types::Diagnostic`.
//!
//! Not to be confused with [`super::graph_diagnostics`], which reports
//! *include-graph* problems on `.jl` member files. These report on
//! `Project.toml`/`Manifest.toml` themselves, ride their own `Outbound`
//! variant, and are produced by the workspace harvester — the only place the
//! resolved `Environment`, and the failure to resolve one, exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};

use crate::environment::{Environment, EnvironmentError};
use crate::linter::Severity;
use crate::project_files::{self, ProjectFinding};
use crate::text::{PositionEncoding, TextBuffer};

/// Every finding of one resolve pass, grouped by the file it attaches to. A
/// file with no findings is absent rather than mapped to an empty vector; the
/// caller clears it against the set it published last time.
pub(crate) type EnvironmentFindings = BTreeMap<PathBuf, Vec<Diagnostic>>;

/// The findings for one resolved environment: its project file's semantic
/// checks, plus a syntax check of the manifest, which resolution accepted but
/// which may still be malformed in ways the checks care about.
///
/// `source` yields a file's text so a byte span becomes a range; a file that
/// cannot be read contributes nothing. Injected rather than read directly so
/// the mapping tests need no filesystem.
pub(crate) fn environment_diagnostics(
    env: &Environment,
    encoding: PositionEncoding,
    mut source: impl FnMut(&Path) -> Option<String>,
) -> EnvironmentFindings {
    let mut out = EnvironmentFindings::new();
    if let Some(text) = source(&env.project_file) {
        let findings = project_files::semantic_findings(env, &text);
        insert(&mut out, &env.project_file, &text, findings, encoding);
    }
    out
}

/// The diagnostics an open environment-file *buffer* carries on its own: the
/// text-only checks, which need no resolved [`Environment`] and so run at edit
/// cadence on the main loop rather than on the harvester's save/watch cadence.
///
/// That is the whole of what a buffer can answer. The semantic checks stay with
/// [`environment_diagnostics`] above, because a resolved environment exists
/// only on the harvester thread, which reads disk — the line stage 2 drew. The
/// parse is a TOML parse of a file measured in kilobytes, which is why it can
/// sit on the main loop at all.
pub(crate) fn buffer_diagnostics(
    path: &Path,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let findings = project_files::syntax_findings(path, text);
    if findings.is_empty() {
        return Vec::new();
    }
    let line_index = TextBuffer::new(text);
    findings
        .iter()
        .map(|finding| to_lsp(finding, &line_index, encoding))
        .collect()
}

/// The findings for a resolve that failed outright. Placed on the file the
/// error names, which is the only thing known about a project that could not be
/// read: no [`Environment`] exists to check.
pub(crate) fn resolve_failure_diagnostics(
    error: &EnvironmentError,
    encoding: PositionEncoding,
    source: impl FnOnce(&Path) -> Option<String>,
) -> (PathBuf, Vec<Diagnostic>) {
    let path = match error {
        EnvironmentError::Read { path, .. } | EnvironmentError::Parse { path, .. } => path.clone(),
    };
    let text = source(&path).unwrap_or_default();
    let finding = project_files::syntax_finding(&path, &text, error);
    let line_index = TextBuffer::new(&text);
    (path, vec![to_lsp(&finding, &line_index, encoding)])
}

fn insert(
    out: &mut EnvironmentFindings,
    path: &Path,
    text: &str,
    findings: Vec<ProjectFinding>,
    encoding: PositionEncoding,
) {
    if findings.is_empty() {
        return;
    }
    let line_index = TextBuffer::new(text);
    out.entry(path.to_path_buf()).or_default().extend(
        findings
            .iter()
            .map(|finding| to_lsp(finding, &line_index, encoding)),
    );
}

/// The finding's byte range resolved against `line_index`. The check ID rides
/// as the diagnostic `code`, but with no `code_description`: unlike a lint
/// rule, a project-file check has no reference section to link to.
fn to_lsp(
    finding: &ProjectFinding,
    line_index: &TextBuffer,
    encoding: PositionEncoding,
) -> Diagnostic {
    Diagnostic {
        range: Range::new(
            line_index.byte_to_position(finding.range.start().into(), encoding),
            line_index.byte_to_position(finding.range.end().into(), encoding),
        ),
        severity: Some(severity_to_lsp(finding.severity)),
        code: Some(NumberOrString::String(finding.check.to_string())),
        source: Some("fatou".to_string()),
        message: finding.message.clone(),
        ..Default::default()
    }
}

fn severity_to_lsp(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
        Severity::Hint => DiagnosticSeverity::HINT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parse_failure_maps_its_span_to_a_range() {
        let text = "name = \"Demo\"\nuuid = \n";
        let error = EnvironmentError::Parse {
            path: PathBuf::from("/work/Project.toml"),
            message: "invalid string".to_string(),
            span: Some(21..22),
        };

        let (path, diags) = resolve_failure_diagnostics(&error, PositionEncoding::Utf16, |_| {
            Some(text.to_string())
        });

        assert_eq!(path, PathBuf::from("/work/Project.toml"));
        let [diag] = &diags[..] else {
            panic!("expected one diagnostic, got {diags:?}");
        };
        assert_eq!(diag.range.start.line, 1, "the second line");
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source.as_deref(), Some("fatou"));
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("toml-syntax".to_string()))
        );
        assert!(
            diag.code_description.is_none(),
            "a check ID names no reference section"
        );
    }

    /// The buffer route reports what a text alone can decide, and stays silent
    /// on a file that parses — the semantic checks are the harvester's, since
    /// only it has a resolved environment.
    #[test]
    fn the_buffer_route_reports_syntax_alone() {
        let path = PathBuf::from("/work/Project.toml");
        let diags =
            buffer_diagnostics(&path, "name = \"Demo\"\nuuid = \n", PositionEncoding::Utf16);
        let [diag] = &diags[..] else {
            panic!("expected one diagnostic, got {diags:?}");
        };
        assert_eq!(diag.range.start.line, 1);
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("toml-syntax".to_string()))
        );

        // A package with no `[compat]` bound on `julia` is a *semantic*
        // finding: it needs a resolved environment, so the buffer says nothing.
        assert!(
            buffer_diagnostics(
                &path,
                "name = \"Demo\"\nuuid = \"00000000-0000-0000-0000-000000000001\"\n",
                PositionEncoding::Utf16,
            )
            .is_empty()
        );
    }

    /// An unreadable file still reports: the diagnostic degrades to the head of
    /// an empty text rather than being dropped, which would leave the user with
    /// no index and no explanation.
    #[test]
    fn an_unreadable_file_still_reports() {
        let error = EnvironmentError::Read {
            path: PathBuf::from("/work/Project.toml"),
            message: "permission denied".to_string(),
        };

        let (_, diags) = resolve_failure_diagnostics(&error, PositionEncoding::Utf16, |_| None);

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "permission denied");
    }
}
