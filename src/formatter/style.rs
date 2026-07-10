use crate::config::FormatConfig;

/// The knobs that govern layout: the target line width, the indent step, and the
/// newline style emitted at the end of each line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatStyle {
    pub line_width: u32,
    pub indent_width: u32,
    pub line_ending: LineEnding,
}

impl Default for FormatStyle {
    fn default() -> Self {
        FormatStyle {
            line_width: 92,
            indent_width: 4,
            line_ending: LineEnding::default(),
        }
    }
}

impl From<&FormatConfig> for FormatStyle {
    fn from(config: &FormatConfig) -> Self {
        FormatStyle {
            line_width: config.line_width,
            indent_width: config.indent_width,
            line_ending: config.line_ending,
        }
    }
}

/// The character sequence the formatter emits at the end of each line.
///
/// The layout engine always builds output with `\n` line breaks (the printer is
/// the sole authority on *where* breaks go); this only selects the byte sequence
/// those breaks render as in the final string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// Detect the newline style per file from the first line ending in the
    /// source, defaulting to `\n` when the source has none. The default.
    #[default]
    Auto,
    /// Always `\n` (Unix).
    Lf,
    /// Always `\r\n` (Windows).
    Crlf,
    /// `\n` on Unix, `\r\n` on Windows.
    Native,
}

impl LineEnding {
    /// Resolve to the concrete sequence to emit. `source` is consulted only for
    /// [`LineEnding::Auto`], which mirrors the source's first line ending.
    pub fn resolve(self, source: &str) -> &'static str {
        self.resolve_detected(source_is_crlf(source))
    }

    /// Resolve to the concrete sequence given a precomputed CRLF detection. Used
    /// by callers that hold the source as a CST rather than a `&str` (see
    /// [`node_source_is_crlf`](crate::formatter::core)).
    pub(crate) fn resolve_detected(self, source_is_crlf: bool) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
            LineEnding::Native => {
                if cfg!(windows) {
                    "\r\n"
                } else {
                    "\n"
                }
            }
            LineEnding::Auto => {
                if source_is_crlf {
                    "\r\n"
                } else {
                    "\n"
                }
            }
        }
    }
}

/// Whether the source's first line ending is CRLF. A bare `\r` (old Mac) or no
/// newline at all reads as LF.
pub(crate) fn source_is_crlf(source: &str) -> bool {
    match source.find('\n') {
        Some(idx) => idx > 0 && source.as_bytes()[idx - 1] == b'\r',
        None => false,
    }
}

/// Re-render `formatted` (built with `\n` breaks, but possibly carrying verbatim
/// `\r\n` from multi-line string tokens copied out of the source) with a uniform
/// line ending. CRLF is first canonicalized to LF so the target is applied
/// exactly once; a lone `\r` is left untouched.
pub(crate) fn apply_line_ending(formatted: &str, eol: &str) -> String {
    let lf = if formatted.contains('\r') {
        formatted.replace("\r\n", "\n")
    } else {
        formatted.to_string()
    };
    if eol == "\n" {
        lf
    } else {
        lf.replace('\n', eol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detects_crlf_from_first_line_ending() {
        assert_eq!(LineEnding::Auto.resolve("a\r\nb\n"), "\r\n");
        assert_eq!(LineEnding::Auto.resolve("a\nb\r\n"), "\n");
        assert_eq!(LineEnding::Auto.resolve("no newline"), "\n");
        assert_eq!(LineEnding::Auto.resolve(""), "\n");
    }

    #[test]
    fn explicit_endings_ignore_source() {
        assert_eq!(LineEnding::Lf.resolve("a\r\n"), "\n");
        assert_eq!(LineEnding::Crlf.resolve("a\n"), "\r\n");
    }

    #[test]
    fn apply_canonicalizes_then_expands() {
        // Verbatim CRLF (e.g. from a multi-line string) is normalized before the
        // target is applied, so CRLF target never doubles to `\r\r\n`.
        assert_eq!(apply_line_ending("a\nb\r\nc\n", "\r\n"), "a\r\nb\r\nc\r\n");
        assert_eq!(apply_line_ending("a\nb\r\nc\n", "\n"), "a\nb\nc\n");
    }
}
