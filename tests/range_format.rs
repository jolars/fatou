//! Range formatting (`textDocument/rangeFormatting`) behavior: selections
//! widen to whole statements, the replacement preserves the first line's
//! existing indentation, wrapped lines re-indent to the enclosing block's
//! structural depth, and untouched neighbors stay byte-identical.
//!
//! Selections are marked inline with `«` and `»` (stripped before parsing).

use fatou::formatter::{FormatStyle, format_with_style};
use fatou::lsp::compute_format_range_edits;
use fatou::text::{LineIndex, PositionEncoding};
use lsp_types::{Position, Range, TextEdit};

/// Split a `«»`-marked source into the clean text and the marked selection.
fn extract(marked: &str) -> (String, Range) {
    let start_byte = marked.find('«').expect("start marker");
    let without_start = marked.replacen('«', "", 1);
    let end_byte = without_start.find('»').expect("end marker");
    let text = without_start.replacen('»', "", 1);
    let index = LineIndex::new(&text);
    let range = Range::new(
        index.byte_to_position(start_byte, PositionEncoding::Utf16),
        index.byte_to_position(end_byte, PositionEncoding::Utf16),
    );
    (text, range)
}

/// Range-format the marked selection with `style`, returning the clean text
/// and the computed edits.
fn edits_with_style(marked: &str, style: FormatStyle) -> (String, Vec<TextEdit>) {
    let (text, range) = extract(marked);
    let edits = compute_format_range_edits(&text, range, style, PositionEncoding::Utf16)
        .expect("formatter accepts the input");
    (text, edits)
}

fn edits(marked: &str) -> (String, Vec<TextEdit>) {
    edits_with_style(marked, FormatStyle::default())
}

/// Apply `edits` (non-overlapping, as the LSP requires) to `text`.
fn apply(text: &str, edits: &[TextEdit]) -> String {
    let index = LineIndex::new(text);
    let mut spliced: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            (
                index.position_to_byte(edit.range.start, PositionEncoding::Utf16),
                index.position_to_byte(edit.range.end, PositionEncoding::Utf16),
                edit.new_text.as_str(),
            )
        })
        .collect();
    spliced.sort_by_key(|&(start, _, _)| start);
    let mut out = text.to_string();
    for (start, end, new_text) in spliced.into_iter().rev() {
        out.replace_range(start..end, new_text);
    }
    out
}

/// A width that forces the three-operand chains below to break.
fn narrow() -> FormatStyle {
    FormatStyle {
        line_width: 16,
        indent_width: 4,
    }
}

#[test]
fn mid_statement_selection_widens_to_the_statement() {
    let (text, edits) = edits("x=f«»( 1 )\n");
    assert_eq!(apply(&text, &edits), "x = f(1)\n");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "x = f(1)");
}

#[test]
fn formats_only_the_selected_statement() {
    let (text, edits) = edits("a=1\n«b =2»\nc= 3\n");
    assert_eq!(apply(&text, &edits), "a=1\nb = 2\nc= 3\n");
}

#[test]
fn formats_a_run_of_statements() {
    let (text, edits) = edits("a=1\n«b =2\nc= 3»\nd =4\n");
    assert_eq!(apply(&text, &edits), "a=1\nb = 2\nc = 3\nd =4\n");
    assert_eq!(edits.len(), 1, "one edit covering the widened run");
}

#[test]
fn preserves_the_first_lines_existing_indentation() {
    // The replacement span starts at the statement's first significant token,
    // so the (non-canonical) six-space indent survives a range format.
    let (text, edits) = edits("function f(x)\n      «x=1»\nend\n");
    assert_eq!(apply(&text, &edits), "function f(x)\n      x = 1\nend\n");
    assert_eq!(edits[0].new_text, "x = 1");
}

#[test]
fn wrapped_lines_take_the_blocks_structural_indent() {
    let (_, edits) = edits_with_style("function f()\n    «y = aaa + bbb + ccc»\nend\n", narrow());
    // Base depth one block (4 columns): continuation lines land at 8.
    assert_eq!(edits[0].new_text, "y = aaa +\n        bbb +\n        ccc");
}

#[test]
fn nested_blocks_deepen_the_structural_indent() {
    let (_, edits) = edits_with_style(
        "function f()\n    let\n        «y = aaa + bbb + ccc»\n    end\nend\n",
        narrow(),
    );
    // Two blocks deep (8 columns): continuation lines land at 12.
    assert_eq!(
        edits[0].new_text,
        "y = aaa +\n            bbb +\n            ccc"
    );
}

#[test]
fn elseif_arms_sit_one_block_deep() {
    let (_, edits) = edits_with_style(
        "if a\n    y = 1\nelseif b\n    «z = aaa + bbb + ccc»\nend\n",
        narrow(),
    );
    assert_eq!(edits[0].new_text, "z = aaa +\n        bbb +\n        ccc");
}

#[test]
fn a_lone_top_level_modules_body_stays_flush() {
    // `module_should_indent` keeps a lone top-level module's body at column 0,
    // so the block contributes no structural indent.
    let (_, edits) = edits_with_style("module M\n«y = aaa + bbb + ccc»\nend\n", narrow());
    assert_eq!(edits[0].new_text, "y = aaa +\n    bbb +\n    ccc");
}

#[test]
fn semicolon_joined_statements_reflow_one_per_line() {
    let (text, edits) = edits("«a=1; b=2»\n");
    assert_eq!(apply(&text, &edits), "a = 1\nb = 2\n");
}

#[test]
fn blank_lines_between_selected_statements_are_kept_and_capped() {
    let (text, edits) = edits("«a=1\n\n\n\nb=2»\n");
    assert_eq!(apply(&text, &edits), "a = 1\n\nb = 2\n");
}

#[test]
fn a_trailing_comment_stays_attached() {
    let (text, edits) = edits("«x=1   # note»\n");
    assert_eq!(apply(&text, &edits), "x = 1 # note\n");
}

#[test]
fn a_selection_in_whitespace_is_a_noop() {
    let (_, edits) = edits("a = 1\n\n«»\n\nb = 2\n");
    assert_eq!(edits, Vec::new());
}

#[test]
fn an_already_formatted_selection_yields_no_edits() {
    let (_, edits) = edits("a=1\n«b = 2»\nc=3\n");
    assert_eq!(edits, Vec::new());
}

#[test]
fn a_whole_document_selection_matches_the_full_formatter() {
    let marked = "«x=f( 1 )\n\nfunction g(x)\n    x ^ 2\nend»\n";
    let (text, edits) = edits(marked);
    assert_eq!(
        apply(&text, &edits),
        format_with_style(&text, FormatStyle::default()).unwrap()
    );
}

#[test]
fn range_formatting_converges_with_the_full_formatter() {
    // Applying a range edit never blocks the full formatter: formatting the
    // patched text equals formatting the original.
    let (text, edits) = edits("x=1\nfunction g(x)\n    «x ^ 2»\nend\ny= 2\n");
    let patched = apply(&text, &edits);
    let style = FormatStyle::default();
    assert_eq!(
        format_with_style(&patched, style).unwrap(),
        format_with_style(&text, style).unwrap()
    );
}

#[test]
fn edit_positions_follow_the_negotiated_encoding() {
    // U+1F600 is 4 bytes in UTF-8, 2 UTF-16 units: the edit's end column
    // depends on the encoding, its line does not.
    let text = "x=\"\u{1F600}\"\n";
    let range = Range::new(Position::new(0, 0), Position::new(0, 1));
    let style = FormatStyle::default();
    let utf16 = compute_format_range_edits(text, range, style, PositionEncoding::Utf16)
        .unwrap()
        .remove(0);
    let utf8 = compute_format_range_edits(text, range, style, PositionEncoding::Utf8)
        .unwrap()
        .remove(0);
    assert_eq!(utf16.new_text, "x = \"\u{1F600}\"");
    assert_eq!(utf8.new_text, utf16.new_text);
    assert_eq!(utf16.range.end, Position::new(0, 6));
    assert_eq!(utf8.range.end, Position::new(0, 8));
}
