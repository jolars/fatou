//! Formatting and diagnostic conversion for the language server.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range, TextEdit};
use rowan::TextRange;

use crate::formatter::{FormatStyle, RangeFormatted, format_node, format_range, format_with_style};
use crate::incremental::Analysis;
use crate::parser::{ParseDiagnostic, parse};
use crate::text::{LineIndex, PositionEncoding};

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
    encoding: PositionEncoding,
) -> Option<Vec<TextEdit>> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        let formatted = format_node(&root, style).ok();
        Some(formatted.map(|formatted| edits_for_formatted(text, formatted, encoding)))
    }));
    match cached {
        Ok(Some(edits)) => edits,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_format_edits(text, style, encoding),
    }
}

/// Compute the LSP `TextEdit`s to format `text` with `style`, re-parsing it.
///
/// Returns `None` when the formatter rejects the input. An empty `Vec` means
/// the document is already formatted.
pub fn compute_format_edits(
    text: &str,
    style: FormatStyle,
    encoding: PositionEncoding,
) -> Option<Vec<TextEdit>> {
    let formatted = format_with_style(text, style).ok()?;
    Some(edits_for_formatted(text, formatted, encoding))
}

/// The whole-document edit replacing `text` with its formatted form (empty when
/// already formatted). The single source of the edit geometry shared by the
/// re-parse path ([`compute_format_edits`]) and the cached-tree path.
pub(crate) fn edits_for_formatted(
    text: &str,
    formatted: String,
    encoding: PositionEncoding,
) -> Vec<TextEdit> {
    if formatted == text {
        return Vec::new();
    }
    let line_index = LineIndex::new(text);
    let end = line_index.byte_to_position(text.len(), encoding);
    vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end,
        },
        new_text: formatted,
    }]
}

/// Range-format `text` off the snapshot's cached parse when the db's tracked
/// buffer for `path` still matches it; otherwise re-parse. A write racing the
/// read trips `salsa::Cancelled`, which also falls back to a fresh parse.
/// The full-document path's twin ([`format_edits_via_db`]) for
/// `textDocument/rangeFormatting`.
pub(crate) fn format_range_edits_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    range: Range,
    style: FormatStyle,
    encoding: PositionEncoding,
) -> Option<Vec<TextEdit>> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        let text_range = lsp_range_to_text_range(text, range, encoding);
        Some(match format_range(&root, text_range, style) {
            Ok(Some(formatted)) => Some(edits_for_range_formatted(text, formatted, encoding)),
            // The selection touches no statement, or an unmodeled container
            // shape: nothing to do rather than an error.
            Ok(None) => Some(Vec::new()),
            Err(_) => None,
        })
    }));
    match cached {
        Ok(Some(edits)) => edits,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_format_range_edits(text, range, style, encoding),
    }
}

/// Compute the LSP `TextEdit`s to format the statements `range` touches,
/// re-parsing `text`. The pure core of `textDocument/rangeFormatting`.
///
/// Returns `None` when the formatter rejects the input. An empty `Vec` means
/// there is nothing to change: the widened selection is already formatted, or
/// it touches no statement at all.
pub fn compute_format_range_edits(
    text: &str,
    range: Range,
    style: FormatStyle,
    encoding: PositionEncoding,
) -> Option<Vec<TextEdit>> {
    let root = parse(text).cst;
    let text_range = lsp_range_to_text_range(text, range, encoding);
    match format_range(&root, text_range, style).ok()? {
        Some(formatted) => Some(edits_for_range_formatted(text, formatted, encoding)),
        None => Some(Vec::new()),
    }
}

/// The single edit replacing a [`format_range`] result's widened span (empty
/// when that span is already formatted). The edit-geometry twin of
/// [`edits_for_formatted`], shared by the re-parse and cached-tree paths.
pub(crate) fn edits_for_range_formatted(
    text: &str,
    formatted: RangeFormatted,
    encoding: PositionEncoding,
) -> Vec<TextEdit> {
    let start = usize::from(formatted.range.start());
    let end = usize::from(formatted.range.end());
    if text.get(start..end) == Some(formatted.text.as_str()) {
        return Vec::new();
    }
    let line_index = LineIndex::new(text);
    vec![TextEdit {
        range: Range {
            start: line_index.byte_to_position(start, encoding),
            end: line_index.byte_to_position(end, encoding),
        },
        new_text: formatted.text,
    }]
}

/// Convert an LSP selection to the byte range it covers, clamped to `text`
/// (via [`LineIndex::position_to_byte`]'s clamping) and normalized so an
/// inverted selection cannot panic `TextRange::new`.
fn lsp_range_to_text_range(text: &str, range: Range, encoding: PositionEncoding) -> TextRange {
    let line_index = LineIndex::new(text);
    let start = line_index.position_to_byte(range.start, encoding);
    let end = line_index.position_to_byte(range.end, encoding);
    TextRange::new(
        (start.min(end) as u32).into(),
        (start.max(end) as u32).into(),
    )
}

/// Convert parse diagnostics into LSP diagnostics against `text` (the source
/// the diagnostics' byte offsets index).
pub(crate) fn parse_diagnostics_to_lsp(
    diagnostics: &[ParseDiagnostic],
    text: &str,
    encoding: PositionEncoding,
) -> Vec<Diagnostic> {
    let line_index = LineIndex::new(text);
    diagnostics
        .iter()
        .map(|diag| Diagnostic {
            range: Range::new(
                line_index.byte_to_position(diag.start, encoding),
                line_index.byte_to_position(diag.end, encoding),
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
        let encoding = PositionEncoding::Utf16;
        let path = Path::new("/work/a.jl");
        let buffer = "x=f( 1 )\n";
        let expected = compute_format_edits(buffer, style, encoding);
        assert!(
            matches!(&expected, Some(edits) if !edits.is_empty()),
            "fixture must require reformatting"
        );

        // Cache hit: tracked text == buffer → format off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, buffer.to_string());
        let snapshot = db.snapshot();
        assert_eq!(
            format_edits_via_db(&snapshot, path, buffer, style, encoding),
            expected,
            "cached-tree format must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            format_edits_via_db(&stale.snapshot(), path, buffer, style, encoding),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            format_edits_via_db(&empty.snapshot(), path, buffer, style, encoding),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }

    /// The cached-tree range-format path matches the re-parse path when the
    /// db's tracked buffer is the live text, and falls back (still correctly)
    /// when the db lags the buffer or has never seen the path.
    #[test]
    fn format_range_via_db_matches_compute_and_falls_back() {
        let style = FormatStyle::default();
        let encoding = PositionEncoding::Utf16;
        let path = Path::new("/work/a.jl");
        let buffer = "a=1\nx=f( 1 )\nb =2\n";
        // A cursor selection inside the second statement.
        let range = Range::new(Position::new(1, 3), Position::new(1, 3));
        let expected = compute_format_range_edits(buffer, range, style, encoding);
        assert!(
            matches!(&expected, Some(edits) if edits.len() == 1),
            "fixture must require a scoped edit"
        );

        // Cache hit: tracked text == buffer → format off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, buffer.to_string());
        let snapshot = db.snapshot();
        assert_eq!(
            format_range_edits_via_db(&snapshot, path, buffer, range, style, encoding),
            expected,
            "cached-tree range format must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            format_range_edits_via_db(&stale.snapshot(), path, buffer, range, style, encoding),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            format_range_edits_via_db(&empty.snapshot(), path, buffer, range, style, encoding),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }

    /// The whole-document replacement range's end position follows the
    /// negotiated encoding when the last line contains multi-byte characters.
    #[test]
    fn edit_end_position_follows_encoding() {
        // U+1F600 is 4 bytes in UTF-8, 2 UTF-16 units.
        let text = "x = \"\u{1F600}\"";
        let formatted = "y".to_string();
        let end_utf16 = edits_for_formatted(text, formatted.clone(), PositionEncoding::Utf16)[0]
            .range
            .end;
        let end_utf8 = edits_for_formatted(text, formatted, PositionEncoding::Utf8)[0]
            .range
            .end;
        assert_eq!(end_utf16, Position::new(0, 8));
        assert_eq!(end_utf8, Position::new(0, 10));
    }
}
