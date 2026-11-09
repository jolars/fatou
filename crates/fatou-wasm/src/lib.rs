//! Browser bindings for Fatou's Julia formatter and parse diagnostics.

use fatou_formatter::FormatStyle;
use fatou_parser::parser::{ParseDiagnostic, parse};
use wasm_bindgen::prelude::*;

/// A parse diagnostic using JavaScript's UTF-16 string positions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[wasm_bindgen]
pub struct Diagnostic {
    #[wasm_bindgen(getter_with_clone)]
    pub message: String,
    /// Zero-based UTF-16 start offset.
    pub start: u32,
    /// Exclusive UTF-16 end offset.
    pub end: u32,
    /// One-based line number.
    pub line: u32,
    /// One-based UTF-16 column.
    pub column: u32,
}

/// Return parse errors without evaluating Julia source.
#[wasm_bindgen]
pub fn check(input: &str) -> Vec<Diagnostic> {
    to_diagnostics(input, parse(input).diagnostics)
}

/// Format valid Julia source, using Fatou's defaults for omitted options.
///
/// Widths must be whole numbers between 1 and 1000. Accept JavaScript numbers
/// without integer coercion so fractional and overflowing values are rejected.
#[wasm_bindgen]
pub fn format(
    input: &str,
    line_width: Option<f64>,
    indent_width: Option<f64>,
) -> Result<String, JsError> {
    format_impl(input, line_width, indent_width).map_err(|error| JsError::new(&error))
}

fn format_impl(
    input: &str,
    line_width: Option<f64>,
    indent_width: Option<f64>,
) -> Result<String, String> {
    let mut style = FormatStyle::default();
    for (name, value, target) in [
        ("line-width", line_width, &mut style.line_width),
        ("indent-width", indent_width, &mut style.indent_width),
    ] {
        if let Some(value) = value {
            if !(1.0..=1000.0).contains(&value) || value.fract() != 0.0 {
                return Err(format!(
                    "{name} must be a whole number between 1 and 1000, got {value}"
                ));
            }
            *target = value as u32;
        }
    }
    let parsed = parse(input);
    if !parsed.diagnostics.is_empty() {
        return Err("Fix the parse errors before formatting.".into());
    }
    fatou_formatter::format_node(&parsed.cst, style).map_err(|error| error.to_string())
}

fn to_diagnostics(input: &str, errors: Vec<ParseDiagnostic>) -> Vec<Diagnostic> {
    let mut wanted = Vec::with_capacity(errors.len() * 2);
    let mut diagnostics = Vec::with_capacity(errors.len());
    for (index, error) in errors.into_iter().enumerate() {
        wanted.push((error.start, index, false));
        wanted.push((error.end, index, true));
        diagnostics.push(Diagnostic {
            message: error.message,
            start: 0,
            end: 0,
            line: 1,
            column: 1,
        });
    }
    if wanted.is_empty() {
        return diagnostics;
    }
    // Recovery can report spans out of order. Translate all endpoints in one
    // source walk without changing the diagnostic order.
    wanted.sort_unstable_by_key(|&(byte, ..)| byte);
    let mut record = |index: usize, is_end: bool, offset: u32, line: u32, column: u32| {
        let diagnostic = &mut diagnostics[index];
        if is_end {
            diagnostic.end = offset;
        } else {
            diagnostic.start = offset;
            diagnostic.line = line;
            diagnostic.column = column;
        }
    };
    let (mut offset, mut line, mut column) = (0, 1, 1);
    let mut next = 0;
    for (byte, ch) in input.char_indices() {
        while let Some(&(target, index, is_end)) = wanted.get(next) {
            if target > byte {
                break;
            }
            record(index, is_end, offset, line, column);
            next += 1;
        }
        offset += ch.len_utf16() as u32;
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += ch.len_utf16() as u32;
        }
    }
    for &(_, index, is_end) in &wanted[next..] {
        record(index, is_end, offset, line, column);
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use fatou_formatter::{FormatStyle, format_with_style};
    use fatou_parser::parser::{ParseDiagnostic, parse, reconstruct};

    #[test]
    fn defaults_match_the_formatter_and_remain_idempotent() {
        for source in [
            "",
            "x=1\n",
            "function f(x)\nif x>0\nx+1\nelse\n0\nend\nend\n",
        ] {
            let formatted = format_impl(source, None, None).unwrap();
            assert_eq!(formatted, fatou_formatter::format(source).unwrap());
            assert_eq!(format_impl(&formatted, None, None).unwrap(), formatted);
            assert!(check(&formatted).is_empty());
            assert_eq!(reconstruct(source), source);
        }
    }

    #[test]
    fn custom_options_match_the_formatter() {
        let source = "function f(x,y)\n(first=x,second=y,third=x+y)\nend\n";
        let style = FormatStyle {
            line_width: 30,
            indent_width: 8,
            ..FormatStyle::default()
        };
        let formatted = format_impl(source, Some(30.0), Some(8.0)).unwrap();
        assert_eq!(formatted, format_with_style(source, style).unwrap());
        assert!(formatted.contains("\n        "));
    }

    #[test]
    fn invalid_widths_are_errors() {
        for width in [
            0.0,
            1001.0,
            u32::MAX as f64,
            -1.0,
            1.5,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert!(
                format_impl("x=1", Some(width), None)
                    .unwrap_err()
                    .contains("line-width")
            );
            assert!(
                format_impl("x=1", None, Some(width))
                    .unwrap_err()
                    .contains("indent-width")
            );
        }
        for width in [1.0, 1000.0] {
            assert!(format_impl("x=1", Some(width), Some(width)).is_ok());
        }
    }

    #[test]
    fn invalid_julia_has_diagnostics_and_cannot_be_formatted() {
        let source = "function f(\n";
        assert!(!check(source).is_empty());
        assert!(
            format_impl(source, None, None)
                .unwrap_err()
                .contains("parse error")
        );
        assert_eq!(reconstruct(source), source);
    }

    #[test]
    fn line_endings_follow_the_formatter_defaults() {
        let source = "function f(x)\r\nx+1\r\nend\r\n";
        let formatted = format_impl(source, None, None).unwrap();
        assert_eq!(formatted, fatou_formatter::format(source).unwrap());
        assert!(formatted.contains("\r\n"));
    }

    fn diagnostic(message: &str, start: usize, end: usize) -> ParseDiagnostic {
        ParseDiagnostic {
            message: message.into(),
            start,
            end,
            ..parse(")").diagnostics.remove(0)
        }
    }

    #[test]
    fn diagnostics_use_utf16_positions_even_when_out_of_order() {
        let input = "# 😀é\r\n)";
        assert_eq!(
            to_diagnostics(
                input,
                vec![diagnostic("closing", 10, 11), diagnostic("emoji", 2, 6)]
            ),
            vec![
                Diagnostic {
                    message: "closing".into(),
                    start: 7,
                    end: 8,
                    line: 2,
                    column: 1
                },
                Diagnostic {
                    message: "emoji".into(),
                    start: 2,
                    end: 4,
                    line: 1,
                    column: 3
                },
            ]
        );
        let errors = check(input);
        assert!(!errors.is_empty());
        assert!(
            errors
                .iter()
                .all(|d| (d.start, d.end, d.line, d.column) == (7, 8, 2, 1))
        );
        assert!(check("α=1\nβ=α+2\n").is_empty());
    }

    #[test]
    fn empty_and_eof_diagnostic_spans_are_supported() {
        for input in ["", "# 😀\n"] {
            let errors =
                to_diagnostics(input, vec![diagnostic("at end", input.len(), input.len())]);
            let offset = input.encode_utf16().count() as u32;
            assert_eq!((errors[0].start, errors[0].end), (offset, offset));
            assert_eq!(errors[0].line, if input.is_empty() { 1 } else { 2 });
            assert_eq!(errors[0].column, 1);
        }
    }
}
