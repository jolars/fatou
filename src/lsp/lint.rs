//! Lint findings as LSP diagnostics.
//!
//! Runs the linter with the default configuration (editor-pushed settings and
//! `fatou.toml` discovery are a later roadmap item) and converts each finding
//! into an LSP diagnostic: the rule ID as `code`, the engine-stamped severity
//! mapped across, and `source: "fatou"` like the parse diagnostics it is
//! published alongside. Rules only run on a parse-clean tree (the CLI's rule),
//! so the analysis pipeline gates on empty parse diagnostics before calling in
//! here.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, NumberOrString, Range};

use crate::config::LintConfig;
use crate::incremental::Analysis;
use crate::linter::{self, ResolvedRules, Severity, lint_parsed};
use crate::parser::parse;
use crate::semantic::SemanticModel;
use crate::text::{LineIndex, PositionEncoding};

/// Lint `text` off the snapshot's cached parse and semantic model when the
/// db's tracked buffer for `path` still matches it; otherwise re-parse. A
/// write racing the read trips `salsa::Cancelled`, which also falls back to a
/// fresh parse. Returns nothing on a parse-broken buffer: rules need a clean
/// tree, and the parse errors are published by the caller already.
pub(crate) fn lint_diagnostics_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    findings_to_lsp(lint_findings_via_db(snapshot, path, text), text, encoding)
}

/// Compute the lint diagnostics for `text`, re-parsing it. The pure core of
/// the lint-diagnostic pipeline; empty on a parse-broken document.
pub fn compute_lint_diagnostics(text: &str, encoding: PositionEncoding) -> Vec<Diagnostic> {
    findings_to_lsp(lint_findings(text), text, encoding)
}

/// The raw lint findings for `text`, warm off the snapshot's cached parse and
/// semantic model (see [`lint_diagnostics_via_db`] for the cache contract).
/// Raw so code actions can reach the byte-ranged [`linter::Fix`]es a finding
/// carries, which the LSP diagnostic conversion drops.
pub(crate) fn lint_findings_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
) -> Vec<linter::Diagnostic> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        if !snapshot.parse_diagnostics(file).is_empty() {
            return Some(Vec::new());
        }
        let root = snapshot.parsed_tree(file);
        let model = snapshot.semantic_model(file);
        Some(lint_parsed(
            Some(path),
            text,
            &root,
            model,
            &default_rules(),
        ))
    }));
    match cached {
        Ok(Some(findings)) => findings,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => lint_findings(text),
    }
}

/// The raw lint findings for `text`, re-parsing it; empty on a parse-broken
/// document (rules need a clean tree).
fn lint_findings(text: &str) -> Vec<linter::Diagnostic> {
    let parsed = parse(text);
    if !parsed.diagnostics.is_empty() {
        return Vec::new();
    }
    let model = SemanticModel::build(&parsed.cst);
    lint_parsed(None, text, &parsed.cst, &model, &default_rules())
}

/// The rule set the server lints with: the defaults, until configuration
/// discovery lands (`workspace/didChangeConfiguration` + `fatou.toml`).
fn default_rules() -> ResolvedRules {
    ResolvedRules::resolve(&LintConfig::default()).0
}

fn findings_to_lsp(
    findings: Vec<linter::Diagnostic>,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let line_index = LineIndex::new(text);
    findings
        .into_iter()
        .map(|finding| finding_to_lsp(&finding, &line_index, encoding))
        .collect()
}

/// Convert one lint finding into an LSP diagnostic against `line_index`'s text
/// (the source the finding's byte offsets index).
pub(crate) fn finding_to_lsp(
    finding: &linter::Diagnostic,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> Diagnostic {
    // The `unused-` rule family flags dead code; the tag lets clients render
    // those findings faded rather than squiggled.
    let tags = finding
        .rule
        .starts_with("unused-")
        .then(|| vec![DiagnosticTag::UNNECESSARY]);
    Diagnostic {
        range: Range::new(
            line_index.byte_to_position(finding.start, encoding),
            line_index.byte_to_position(finding.end, encoding),
        ),
        severity: Some(severity_to_lsp(finding.severity)),
        code: Some(NumberOrString::String(finding.rule.clone())),
        source: Some("fatou".to_string()),
        message: finding.message.clone(),
        tags,
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
    use crate::incremental::IncrementalDatabase;
    use lsp_types::Position;

    const UNUSED_LOCAL: &str = "function f(x)\n    tmp = x + 1\n    return x\nend\n";

    #[test]
    fn unused_binding_becomes_a_tagged_warning() {
        let diags = compute_lint_diagnostics(UNUSED_LOCAL, PositionEncoding::Utf16);
        assert_eq!(diags.len(), 1);
        let diag = &diags[0];
        assert_eq!(
            diag.code,
            Some(NumberOrString::String("unused-binding".to_string()))
        );
        assert_eq!(diag.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(diag.source.as_deref(), Some("fatou"));
        assert_eq!(diag.tags, Some(vec![DiagnosticTag::UNNECESSARY]));
        assert_eq!(
            diag.range,
            Range::new(Position::new(1, 4), Position::new(1, 7)),
            "the diagnostic must cover `tmp`"
        );
        assert!(diag.message.contains("tmp"));
    }

    #[test]
    fn non_unused_rules_are_untagged() {
        let diags = compute_lint_diagnostics("if x = 1\nend\n", PositionEncoding::Utf16);
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String(
                "assignment-in-condition".to_string()
            ))
        );
        assert_eq!(diags[0].tags, None);
    }

    #[test]
    fn suppression_comments_are_honored() {
        let suppressed = "function f(x)\n    # fatou-ignore unused-binding\n    tmp = x + 1\n    return x\nend\n";
        assert_eq!(
            compute_lint_diagnostics(suppressed, PositionEncoding::Utf16),
            Vec::new()
        );
    }

    #[test]
    fn a_parse_broken_document_yields_no_lint_findings() {
        // The unterminated function also contains an unused local; rules must
        // not run on the error-recovered tree.
        let broken = "function f(x)\n    tmp = x + 1\n    return x\n";
        assert_eq!(
            compute_lint_diagnostics(broken, PositionEncoding::Utf16),
            Vec::new()
        );
    }

    /// The cached-tree lint path matches the re-parse path when the db's
    /// tracked buffer is the live text, and falls back (still correctly) when
    /// the db lags the buffer or has never seen the path.
    #[test]
    fn lint_via_db_matches_compute_and_falls_back() {
        let encoding = PositionEncoding::Utf16;
        let path = Path::new("/work/a.jl");
        let expected = compute_lint_diagnostics(UNUSED_LOCAL, encoding);
        assert_eq!(expected.len(), 1, "fixture must produce a finding");

        // Cache hit: tracked text == buffer → lint off the cached tree + model.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, UNUSED_LOCAL.to_string());
        assert_eq!(
            lint_diagnostics_via_db(&db.snapshot(), path, UNUSED_LOCAL, encoding),
            expected,
            "cached-tree lint must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            lint_diagnostics_via_db(&stale.snapshot(), path, UNUSED_LOCAL, encoding),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            lint_diagnostics_via_db(&empty.snapshot(), path, UNUSED_LOCAL, encoding),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
