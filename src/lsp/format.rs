//! Formatting and diagnostic conversion for the language server.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range, TextEdit};
use rowan::TextRange;
use similar::{DiffTag, TextDiff};

use crate::formatter::{FormatStyle, RangeFormatted, format_node, format_range, format_with_style};
use crate::incremental::Analysis;
use crate::parser::{ParseDiagnostic, parse};
use crate::text::{LineIndex, PositionEncoding, TextBuffer};

/// Format `text` off the snapshot's cached parse when the db's tracked buffer
/// for `path` still matches it; otherwise re-parse. A write racing the read
/// trips `salsa::Cancelled`, which also falls back to a fresh parse.
///
/// No parse-error refusal: the formatter lowers ERROR nodes transparently
/// (byte-identical), matching the CLI's behavior on broken input.
pub(crate) fn format_edits_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
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

/// The edits turning `text` into its formatted form (empty when already
/// formatted). The single source of the edit geometry shared by the re-parse
/// path ([`compute_format_edits`]) and the cached-tree path.
///
/// Line-scoped where the diff is small ([`line_diff_edits`]), one
/// whole-document replacement otherwise.
pub(crate) fn edits_for_formatted(
    text: &str,
    formatted: String,
    encoding: PositionEncoding,
) -> Vec<TextEdit> {
    if formatted == text {
        return Vec::new();
    }
    let line_index = LineIndex::new(text);
    if let Some(edits) = line_diff_edits(&line_index, 0, text, &formatted, encoding) {
        return edits;
    }
    let end = line_index.byte_to_position(text.len(), encoding);
    vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end,
        },
        new_text: formatted,
    }]
}

/// A diff touching more than this fraction of the span it replaces is not worth
/// expressing as hunks: past it the client's cursor, folds, and markers are
/// disturbed either way, so one replacement is the cheaper equivalent.
const MAX_DIFF_COVERAGE: f64 = 0.5;

/// The smallest set of line-granular edits turning `old` into `new`, where
/// `old` is the slice of the document at byte offset `base` — the whole
/// document when `base` is 0, a widened range-format span otherwise.
///
/// Returns `None` when the diff degenerates, covering more than
/// [`MAX_DIFF_COVERAGE`] of `old`, leaving the caller to emit its single
/// replacement instead. That is what keeps a `line_width` change or a
/// line-ending normalization, which rewrite every line, from becoming a hunk
/// per line. `None` rather than the edit itself, so neither caller has to clone
/// the formatted string it already owns.
///
/// Why lines and not something finer: a hunk boundary has to be a position the
/// client can reason about, the formatter's unit of change *is* the line, and
/// character-level diffing would cost more than the format it follows to spare
/// edits nobody can see. The edits come back ascending and non-overlapping, as
/// `textDocument/formatting` requires, and every range indexes the *original*
/// document.
fn line_diff_edits(
    line_index: &LineIndex,
    base: usize,
    old: &str,
    new: &str,
    encoding: PositionEncoding,
) -> Option<Vec<TextEdit>> {
    let diff = TextDiff::from_lines(old, new);
    // Byte offset of each line start on either side, so a line-index range from
    // the diff becomes a byte range. Built from the diff's own slices, which
    // cannot disagree with the indices its ops carry.
    let old_offsets = line_offsets(diff.iter_old_slices());
    let new_offsets = line_offsets(diff.iter_new_slices());

    let mut hunks: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    for op in diff.ops() {
        let (tag, old_lines, new_lines) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            continue;
        }
        match hunks.last_mut() {
            // Consecutive non-equal ops (a delete abutting an insert) are one
            // hunk: the client gets a replacement rather than a pair of edits
            // meeting at a point.
            Some((old_prev, new_prev))
                if old_prev.end == old_lines.start && new_prev.end == new_lines.start =>
            {
                old_prev.end = old_lines.end;
                new_prev.end = new_lines.end;
            }
            _ => hunks.push((old_lines, new_lines)),
        }
    }

    let covered: usize = hunks
        .iter()
        .map(|(old_lines, _)| old_offsets[old_lines.end] - old_offsets[old_lines.start])
        .sum();
    if hunks.len() > 1 && covered as f64 > old.len() as f64 * MAX_DIFF_COVERAGE {
        return None;
    }

    Some(
        hunks
            .into_iter()
            .map(|(old_lines, new_lines)| TextEdit {
                range: Range {
                    start: line_index
                        .byte_to_position(base + old_offsets[old_lines.start], encoding),
                    end: line_index.byte_to_position(base + old_offsets[old_lines.end], encoding),
                },
                new_text: new[new_offsets[new_lines.start]..new_offsets[new_lines.end]].to_string(),
            })
            .collect(),
    )
}

/// Byte offset of the start of each line, indexed the way the diff's line
/// indices are, with the total length appended so `offsets[line_count]` closes
/// the last line.
fn line_offsets<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<usize> {
    let mut offsets = vec![0];
    let mut at = 0;
    for line in lines {
        at += line.len();
        offsets.push(at);
    }
    offsets
}

/// Range-format `text` off the snapshot's cached parse when the db's tracked
/// buffer for `path` still matches it; otherwise re-parse. A write racing the
/// read trips `salsa::Cancelled`, which also falls back to a fresh parse.
/// The full-document path's twin ([`format_edits_via_db`]) for
/// `textDocument/rangeFormatting`.
pub(crate) fn format_range_edits_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
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

/// The edits replacing a [`format_range`] result's widened span (empty when
/// that span is already formatted). The edit-geometry twin of
/// [`edits_for_formatted`], shared by the re-parse and cached-tree paths, and
/// line-scoped within the widened span the same way.
pub(crate) fn edits_for_range_formatted(
    text: &str,
    formatted: RangeFormatted,
    encoding: PositionEncoding,
) -> Vec<TextEdit> {
    let start = usize::from(formatted.range.start());
    let end = usize::from(formatted.range.end());
    let old = text.get(start..end);
    if old == Some(formatted.text.as_str()) {
        return Vec::new();
    }
    let line_index = LineIndex::new(text);
    if let Some(edits) =
        old.and_then(|old| line_diff_edits(&line_index, start, old, &formatted.text, encoding))
    {
        return edits;
    }
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
pub(crate) fn lsp_range_to_text_range(
    text: &str,
    range: Range,
    encoding: PositionEncoding,
) -> TextRange {
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
            format_edits_via_db(
                &snapshot,
                path,
                &TextBuffer::new(buffer.to_string()),
                style,
                encoding
            ),
            expected,
            "cached-tree format must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            format_edits_via_db(
                &stale.snapshot(),
                path,
                &TextBuffer::new(buffer.to_string()),
                style,
                encoding
            ),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            format_edits_via_db(
                &empty.snapshot(),
                path,
                &TextBuffer::new(buffer.to_string()),
                style,
                encoding
            ),
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
            format_range_edits_via_db(
                &snapshot,
                path,
                &TextBuffer::new(buffer.to_string()),
                range,
                style,
                encoding
            ),
            expected,
            "cached-tree range format must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            format_range_edits_via_db(
                &stale.snapshot(),
                path,
                &TextBuffer::new(buffer.to_string()),
                range,
                style,
                encoding
            ),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            format_range_edits_via_db(
                &empty.snapshot(),
                path,
                &TextBuffer::new(buffer.to_string()),
                range,
                style,
                encoding
            ),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }

    /// Apply `edits` the way a client does: every range indexes the *original*
    /// document, so splicing from the end keeps the earlier offsets valid.
    /// Asserts the LSP requirement that the edits do not overlap along the way.
    fn apply(text: &str, edits: &[TextEdit], encoding: PositionEncoding) -> String {
        let line_index = LineIndex::new(text);
        let mut spans: Vec<(usize, usize, &str)> = edits
            .iter()
            .map(|edit| {
                (
                    line_index.position_to_byte(edit.range.start, encoding),
                    line_index.position_to_byte(edit.range.end, encoding),
                    edit.new_text.as_str(),
                )
            })
            .collect();
        spans.sort_by_key(|&(start, ..)| start);
        for pair in spans.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "edits must not overlap: {spans:?}");
        }
        let mut out = text.to_string();
        for &(start, end, new_text) in spans.iter().rev() {
            out.replace_range(start..end, new_text);
        }
        out
    }

    /// A one-line change in a longer document is one edit covering that line,
    /// not a whole-document replacement.
    #[test]
    fn format_edits_are_scoped_to_the_lines_that_change() {
        let text = "a = 1\nb = 2\nc=3\nd = 4\ne = 5\n";
        let edits =
            compute_format_edits(text, FormatStyle::default(), PositionEncoding::Utf16).unwrap();
        assert_eq!(edits.len(), 1, "one changed line is one edit: {edits:?}");
        assert_eq!(
            edits[0].range,
            Range::new(Position::new(2, 0), Position::new(3, 0)),
            "the edit must cover exactly the third line"
        );
        assert_eq!(edits[0].new_text, "c = 3\n");
    }

    /// Changes separated by untouched lines come back as separate edits.
    #[test]
    fn separated_changes_are_separate_edits() {
        let text = "a=1\nb = 2\nc = 3\nd = 4\ne=5\n";
        let edits =
            compute_format_edits(text, FormatStyle::default(), PositionEncoding::Utf16).unwrap();
        assert_eq!(edits.len(), 2, "two changed lines are two edits: {edits:?}");
        assert_eq!(
            edits[0].range,
            Range::new(Position::new(0, 0), Position::new(1, 0))
        );
        assert_eq!(edits[0].new_text, "a = 1\n");
        assert_eq!(
            edits[1].range,
            Range::new(Position::new(4, 0), Position::new(5, 0))
        );
        assert_eq!(edits[1].new_text, "e = 5\n");
    }

    /// The property the whole scheme rests on: whatever set of edits comes
    /// back, applying it to the original must reproduce `format` byte for byte.
    #[test]
    fn format_edits_reproduce_the_formatted_document() {
        let style = FormatStyle::default();
        let cases = [
            "",
            "x = 1\n",
            "x=1\n",
            "x = 1",
            "x=1",
            "a = 1\nb = 2\nc=3\nd = 4\ne = 5\n",
            "a=1\nb=2\nc=3\nd=4\n",
            "a=1\nb = 2\nc = 3\nd = 4\ne=5\n",
            "function f(x)\n  y = 1\n    z=2\n  return y+z\nend\n",
            "s = \"\u{1F600}\"\nt=\"\u{00E9}\"\nu = 3\n",
            "a = 1\r\nb=2\r\nc = 3\r\n",
            "# comment\n\n\n\nx=1\n",
            "f(a,b,c)\ng( 1 )\n",
        ];
        for text in cases {
            let formatted = format_with_style(text, style).expect("fixture must format");
            for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
                let edits = compute_format_edits(text, style, encoding).unwrap();
                assert_eq!(
                    apply(text, &edits, encoding),
                    formatted,
                    "edits must reproduce the formatted text for {text:?} ({encoding:?})"
                );
            }
        }
    }

    /// A change touching most of the document (here every line) degenerates
    /// into the single whole-document replacement rather than a hunk per line.
    #[test]
    fn wholesale_change_falls_back_to_one_edit() {
        let text = "a=1\nb=2\nc=3\nd=4\ne=5\n";
        let edits =
            compute_format_edits(text, FormatStyle::default(), PositionEncoding::Utf16).unwrap();
        assert_eq!(
            edits.len(),
            1,
            "an all-lines change must collapse to one edit: {edits:?}"
        );
        assert_eq!(
            edits[0].range,
            Range::new(Position::new(0, 0), Position::new(5, 0)),
            "the fallback edit must span the whole document"
        );
    }

    /// Range formatting narrows the same way: the widened span is the unit the
    /// formatter works on, but the edits cover only the lines that changed.
    #[test]
    fn range_format_edits_are_scoped_to_the_lines_that_change() {
        let text = "a = 1\nb = 2\nc=3\nd = 4\n";
        // A selection spanning all four statements widens to all four.
        let range = Range::new(Position::new(0, 0), Position::new(3, 5));
        let edits = compute_format_range_edits(
            text,
            range,
            FormatStyle::default(),
            PositionEncoding::Utf16,
        )
        .unwrap();
        assert_eq!(edits.len(), 1, "one changed line is one edit: {edits:?}");
        assert_eq!(
            edits[0].range,
            Range::new(Position::new(2, 0), Position::new(3, 0)),
            "the edit must cover only the changed line, not the widened span"
        );
        assert_eq!(edits[0].new_text, "c = 3\n");
    }

    /// The property [`format_edits_reproduce_the_formatted_document`] asserts,
    /// for the range path: the edits must reproduce what the formatter produced
    /// for the widened span, and leave the rest of the document alone.
    #[test]
    fn range_format_edits_reproduce_the_formatted_span() {
        let style = FormatStyle::default();
        let text = "a=1\nb = 2\nc=3\nd = 4\ne=5\n";
        for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
            // Widens to the middle three statements, leaving the outer two.
            let range = Range::new(Position::new(1, 0), Position::new(3, 5));
            let edits = compute_format_range_edits(text, range, style, encoding).unwrap();
            assert_eq!(
                apply(text, &edits, encoding),
                "a=1\nb = 2\nc = 3\nd = 4\ne=5\n",
                "only the widened span may change ({encoding:?})"
            );
        }
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
