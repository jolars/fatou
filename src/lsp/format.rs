//! Formatting and diagnostic conversion for the language server.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range, TextEdit};

use crate::formatter::{FormatStyle, format_node, format_with_style};
use crate::incremental::Analysis;
use crate::parser::ParseDiagnostic;
use crate::text::LineIndex;

/// Format `text` off the snapshot's cached parse when the db's tracked buffer
/// for `path` still matches it; otherwise re-parse. A write racing the read
/// trips `salsa::Cancelled`, which also falls back to a fresh parse.
///
/// No parse-error refusal: the formatter lowers ERROR nodes transparently
/// (byte-identical), matching the CLI's behavior on broken input.
pub(crate) fn format_edits_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    style: FormatStyle,
) -> Option<Vec<TextEdit>> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        let formatted = format_node(&root, style).ok();
        Some(formatted.map(|formatted| edits_for_formatted(text, formatted)))
    }));
    match cached {
        Ok(Some(edits)) => edits,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_format_edits(text, style),
    }
}

/// Compute the LSP `TextEdit`s to format `text` with `style`, re-parsing it.
///
/// Returns `None` when the formatter rejects the input. An empty `Vec` means
/// the document is already formatted.
pub fn compute_format_edits(text: &str, style: FormatStyle) -> Option<Vec<TextEdit>> {
    let formatted = format_with_style(text, style).ok()?;
    Some(edits_for_formatted(text, formatted))
}

/// The whole-document edit replacing `text` with its formatted form (empty when
/// already formatted). The single source of the edit geometry shared by the
/// re-parse path ([`compute_format_edits`]) and the cached-tree path.
pub(crate) fn edits_for_formatted(text: &str, formatted: String) -> Vec<TextEdit> {
    if formatted == text {
        return Vec::new();
    }
    let line_index = LineIndex::new(text);
    let end = line_index.byte_to_position(text.len());
    vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end,
        },
        new_text: formatted,
    }]
}

/// Convert parse diagnostics into LSP diagnostics against `text` (the source
/// the diagnostics' byte offsets index).
pub(crate) fn parse_diagnostics_to_lsp(
    diagnostics: &[ParseDiagnostic],
    text: &str,
) -> Vec<Diagnostic> {
    let line_index = LineIndex::new(text);
    diagnostics
        .iter()
        .map(|diag| Diagnostic {
            range: Range::new(
                line_index.byte_to_position(diag.start),
                line_index.byte_to_position(diag.end),
            ),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("fatou".to_string()),
            message: diag.message.clone(),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    use std::path::Path;

    /// The cached-tree format path matches the re-parse path when the db's
    /// tracked buffer is the live text, and falls back (still correctly) when
    /// the db lags the buffer or has never seen the path.
    #[test]
    fn format_via_db_matches_compute_and_falls_back() {
        let style = FormatStyle::default();
        let path = Path::new("/work/a.jl");
        let buffer = "x=f( 1 )\n";
        let expected = compute_format_edits(buffer, style);
        assert!(
            matches!(&expected, Some(edits) if !edits.is_empty()),
            "fixture must require reformatting"
        );

        // Cache hit: tracked text == buffer → format off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, buffer.to_string());
        let snapshot = db.snapshot();
        assert_eq!(
            format_edits_via_db(&snapshot, path, buffer, style),
            expected,
            "cached-tree format must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            format_edits_via_db(&stale.snapshot(), path, buffer, style),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            format_edits_via_db(&empty.snapshot(), path, buffer, style),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
