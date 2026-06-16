//! Formatter entry points.
//!
//! Walking-skeleton stage: the per-construct rules that build native IR are not
//! implemented yet (see `TODO.md`). `format` therefore reproduces its input via
//! the parser's lossless CST, which keeps it byte-identical and idempotent while
//! the IR/printer foundation is exercised by [`print_document`] and the rules
//! land incrementally. The deterministic-layout target style is Runic.jl's.

use crate::formatter::ir::Ir;
use crate::formatter::printer::print;
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

/// Format `input` with the given style. Currently a lossless passthrough routed
/// through the layout engine (see the module docs).
pub fn format_with_style(input: &str, style: FormatStyle) -> Result<String, FormatError> {
    let reconstructed: String = parse(input)
        .cst
        .descendants_with_tokens()
        .filter_map(|el| el.into_token())
        .map(|tok| tok.text().to_string())
        .collect();
    let doc = Ir::text(reconstructed);
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
    fn format_is_lossless_identity() {
        for input in [
            "x = 1\n",
            "function g(x)\n    x ^ 2\nend\n",
            "# comment\ny = a + b\n",
        ] {
            assert_eq!(format(input).unwrap(), input);
        }
    }

    #[test]
    fn format_is_idempotent() {
        let input = "if a\n    b\nelse\n    c\nend\n";
        let once = format(input).unwrap();
        assert_eq!(format(&once).unwrap(), once);
    }
}
