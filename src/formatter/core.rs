//! Formatter entry points.
//!
//! Walking-skeleton stage: [`format`] parses to the lossless CST, lowers it to
//! the layout IR via [`rules::lower`](crate::formatter::rules::lower), and prints
//! it. Constructs with a rule are reshaped to Fatou's deterministic style;
//! everything else is lowered transparently, so it stays byte-identical and the
//! whole pass remains idempotent while rules land incrementally.
//! [`print_document`] exercises the IR/printer foundation directly.

use crate::formatter::ir::Ir;
use crate::formatter::printer::print;
use crate::formatter::rules::lower;
use crate::formatter::style::FormatStyle;
use crate::parser::parse;

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
    let doc = lower(&parse(input).cst);
    Ok(print(&doc, style))
}

/// Render an arbitrary IR document. Exposed so the (forthcoming) per-construct
/// rules can be unit tested directly against the layout engine.
pub fn print_document(doc: &Ir, style: FormatStyle) -> String {
    print(doc, style)
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
}
