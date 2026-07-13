//! Pull diagnostics (`textDocument/diagnostic`).
//!
//! The pull model inverts the push pipeline: the client asks for a document's
//! diagnostics when it wants them (on open, after an edit, on focus), so the
//! server computes on demand instead of publishing per edit. One pull report
//! carries everything the push path would have split across two publishes:
//! parse diagnostics, lint findings (only on a parse-clean tree), and the
//! file's include-graph problems. When the client supports pull, the push
//! path stays on only for files with *no open buffer* — include-graph
//! diagnostics attach to member files the client never pulls — plus a
//! `workspace/diagnostic/refresh` nudge per re-harvest so open documents get
//! re-pulled (see `GlobalState`).

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::Diagnostic;

use crate::incremental::{Analysis, normalize_path};
use crate::parser::parse;
use crate::text::PositionEncoding;

use super::format::parse_diagnostics_to_lsp;
use super::graph_diagnostics::graph_diagnostics;
use super::lint::lint_diagnostics_via_db;

/// The full diagnostic report for one document: parse diagnostics, lint
/// findings on a clean tree, and the file's include-graph problems. Reads warm
/// off the snapshot's cached parse when the tracked buffer for `path` still
/// matches `text`; a cache miss or racing write falls back to a fresh parse.
pub(crate) fn document_diagnostics_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        Some(parse_diagnostics_to_lsp(
            snapshot.parse_diagnostics(file),
            text,
            encoding,
        ))
    }));
    let mut diags = match cached {
        Ok(Some(diags)) => diags,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => parse_diagnostics_to_lsp(&parse(text).diagnostics, text, encoding),
    };
    // Lint findings join the report on a clean tree only, exactly like the
    // push path: rules would misfire on error-recovered shapes.
    if diags.is_empty() {
        diags.extend(lint_diagnostics_via_db(snapshot, path, text, encoding));
    }
    diags.extend(graph_diagnostics_for(snapshot, path, encoding));
    diags
}

/// The include-graph diagnostics attaching to `path`, re-derived from the
/// snapshot's project graph (salsa-cached; empty without a workspace). The
/// pull twin of the harvest-driven `refresh_graph_diagnostics` publish.
fn graph_diagnostics_for(
    snapshot: &Analysis,
    path: &Path,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let computed = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let graph = snapshot.project_graph();
        graph_diagnostics(graph, encoding, |member| {
            let file = snapshot.lookup_file(member)?;
            Some((
                snapshot.file_text_of(file).to_string(),
                snapshot.parsed_tree(file),
            ))
        })
        .remove(&normalize_path(path))
        .unwrap_or_default()
    }));
    // A racing write: the client will re-pull on the edit that caused it.
    computed.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    use lsp_types::{DiagnosticSeverity, NumberOrString};
    use std::path::PathBuf;

    fn report_for(text: &str) -> Vec<Diagnostic> {
        let path = PathBuf::from("/work/a.jl");
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&path, text.to_string());
        document_diagnostics_via_db(&db.snapshot(), &path, text, PositionEncoding::Utf16)
    }

    #[test]
    fn a_clean_document_reports_nothing() {
        assert_eq!(report_for("x = 1\n"), Vec::new());
    }

    #[test]
    fn lint_findings_join_the_report() {
        let diags = report_for("function f(x)\n    tmp = x + 1\n    return x\nend\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("unused-binding".to_string()))
        );
    }

    #[test]
    fn parse_errors_suppress_lint_findings() {
        let diags = report_for("function f(x)\n    tmp = x + 1\n    return x\n");
        assert!(!diags.is_empty());
        assert!(
            diags
                .iter()
                .all(|d| d.severity == Some(DiagnosticSeverity::ERROR) && d.code.is_none()),
            "a parse-broken buffer must report parse errors only, got {diags:?}"
        );
    }

    /// An untracked path (or stale buffer) falls back to a fresh parse and
    /// still reports correctly.
    #[test]
    fn falls_back_when_the_db_lags() {
        let text = "function f(x)\n    tmp = x + 1\n    return x\nend\n";
        let db = IncrementalDatabase::default();
        let diags = document_diagnostics_via_db(
            &db.snapshot(),
            Path::new("/work/never-seen.jl"),
            text,
            PositionEncoding::Utf16,
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("unused-binding".to_string()))
        );
    }
}
