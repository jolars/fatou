//! Formatter entry points.
//!
//! Walking-skeleton stage: [`format`] parses to the lossless CST, lowers it to
//! the layout IR via [`rules::lower`](crate::formatter::rules::lower), and prints
//! it. Constructs with a rule are reshaped to Fatou's deterministic style;
//! everything else is lowered transparently, so it stays byte-identical and the
//! whole pass remains idempotent while rules land incrementally.
//! [`print_document`] exercises the IR/printer foundation directly.

use rowan::TextRange;

use crate::formatter::ir::Ir;
use crate::formatter::printer::{print, print_at};
use crate::formatter::rules::{base_indent_level, lower, lower_body_range};
use crate::formatter::style::{FormatStyle, apply_line_ending};
use crate::parser::parse;
use crate::syntax::{SyntaxKind, SyntaxNode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// A construct the formatter cannot yet lay out. Unused while `format` is a
    /// lossless passthrough; reserved for when rules can reject input.
    Unsupported(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Unsupported(what) => write!(f, "unsupported construct: {what}"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Format `input` with the default style.
pub fn format(input: &str) -> Result<String, FormatError> {
    format_with_style(input, FormatStyle::default())
}

/// Format `input` with the given style: parse to the lossless CST, lower it to
/// the layout IR, and print (see the module docs).
pub fn format_with_style(input: &str, style: FormatStyle) -> Result<String, FormatError> {
    format_node(&parse(input).cst, style)
}

/// Format an already-parsed CST `root` with the given style. The language
/// server's warm path: it reuses the salsa-cached parse instead of re-parsing
/// the buffer (see `lsp::format::format_edits_via_db`).
pub fn format_node(
    root: &crate::syntax::SyntaxNode,
    style: FormatStyle,
) -> Result<String, FormatError> {
    let doc = lower(root);
    let eol = style
        .line_ending
        .resolve_detected(node_source_is_crlf(root));
    Ok(apply_line_ending(&print(&doc, style), eol))
}

/// Whether `root`'s source begins with a CRLF line ending. The CST-side twin of
/// [`source_is_crlf`](crate::formatter::style), for the callers that hold the
/// source as a tree ([`format_node`], [`format_range`]) rather than a `&str`.
/// Only [`LineEnding::Auto`](crate::formatter::LineEnding) consults it.
fn node_source_is_crlf(root: &SyntaxNode) -> bool {
    let text = root.text();
    match text.find_char('\n') {
        Some(offset) => {
            let idx = u32::from(offset);
            idx > 0 && text.char_at((idx - 1).into()) == Some('\r')
        }
        None => false,
    }
}

/// Render an arbitrary IR document. Exposed so the (forthcoming) per-construct
/// rules can be unit tested directly against the layout engine.
pub fn print_document(doc: &Ir, style: FormatStyle) -> String {
    print(doc, style)
}

/// The result of [`format_range`]: replace `range` of the source with `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeFormatted {
    /// The widened replacement span: whole statements, from the first selected
    /// line's significant start (after its leading whitespace, which is kept)
    /// to the last one's end.
    pub range: TextRange,
    /// The formatted replacement, without a trailing newline.
    pub text: String,
}

/// Format the statements of `root` that `range` touches, widened to whole
/// source lines — `textDocument/rangeFormatting`'s core. The deepest `ROOT` or
/// `BLOCK` covering `range` supplies the statement list and the structural
/// indent its wrapped lines re-indent to (via
/// [`print_at`]; the first line keeps its existing leading whitespace since
/// the replacement starts at its first significant token).
///
/// `Ok(None)` — no edits — when the selection touches no statement (only
/// whitespace) or the container has a shape the body model does not handle;
/// the formatter never mangles what it does not fully model.
pub fn format_range(
    root: &crate::syntax::SyntaxNode,
    range: TextRange,
    style: FormatStyle,
) -> Result<Option<RangeFormatted>, FormatError> {
    let container = statement_container(root, range);
    let Some((ir, span)) = lower_body_range(&container, range) else {
        return Ok(None);
    };
    let indent = base_indent_level(&container) * style.indent_width as usize;
    let mut text = print_at(&ir, style, indent);
    while text.ends_with('\n') {
        text.pop();
    }
    // The printer emits `\n`-only breaks; convert them to the source's ending so
    // a range edit never splices LF into a CRLF buffer.
    let eol = style
        .line_ending
        .resolve_detected(node_source_is_crlf(root));
    let text = apply_line_ending(&text, eol);
    Ok(Some(RangeFormatted { range: span, text }))
}

/// The deepest statement container — a `ROOT` or `BLOCK` — covering `range`,
/// falling back to `root` (which need not be a `ROOT` in tests).
fn statement_container(root: &SyntaxNode, range: TextRange) -> SyntaxNode {
    let is_container = |kind: SyntaxKind| matches!(kind, SyntaxKind::ROOT | SyntaxKind::BLOCK);
    let found = match root.covering_element(range) {
        rowan::NodeOrToken::Node(node) => node.ancestors().find(|n| is_container(n.kind())),
        rowan::NodeOrToken::Token(token) => {
            token.parent_ancestors().find(|n| is_container(n.kind()))
        }
    };
    found.unwrap_or_else(|| root.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_operator_spacing() {
        // Spaced operators get exactly one space on each side; the tight `^`
        // keeps its operands packed. The `function` body reflows to the canonical
        // body indent (4 spaces) regardless of the source's indentation.
        for (input, expected) in [
            ("x=1\n", "x = 1\n"),
            ("y= a+b\n", "y = a + b\n"),
            (
                "function g(x)\n    x ^ 2\nend\n",
                "function g(x)\n    x^2\nend\n",
            ),
            ("# comment\ny = a + b\n", "# comment\ny = a + b\n"),
        ] {
            assert_eq!(format(input).unwrap(), expected);
        }
    }

    #[test]
    fn format_is_idempotent() {
        for input in ["x=1\n", "z = a*b + c\n", "if a\n    b\nelse\n    c\nend\n"] {
            let once = format(input).unwrap();
            assert_eq!(format(&once).unwrap(), once, "not idempotent for {input:?}");
        }
    }

    #[test]
    fn line_ending_auto_mirrors_source() {
        use crate::formatter::LineEnding;
        let style = FormatStyle {
            line_ending: LineEnding::Auto,
            ..FormatStyle::default()
        };
        // A CRLF source keeps CRLF; an LF source keeps LF.
        assert_eq!(
            format_with_style("x=1\r\ny=2\r\n", style).unwrap(),
            "x = 1\r\ny = 2\r\n",
        );
        assert_eq!(
            format_with_style("x=1\ny=2\n", style).unwrap(),
            "x = 1\ny = 2\n",
        );
    }

    #[test]
    fn line_ending_explicit_overrides_source() {
        use crate::formatter::LineEnding;
        let to_crlf = FormatStyle {
            line_ending: LineEnding::Crlf,
            ..FormatStyle::default()
        };
        assert_eq!(
            format_with_style("x=1\ny=2\n", to_crlf).unwrap(),
            "x = 1\r\ny = 2\r\n",
        );
        let to_lf = FormatStyle {
            line_ending: LineEnding::Lf,
            ..FormatStyle::default()
        };
        assert_eq!(
            format_with_style("x=1\r\ny=2\r\n", to_lf).unwrap(),
            "x = 1\ny = 2\n",
        );
    }
}
