/// A parse-time diagnostic: a message anchored to a byte range in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub message: String,
    pub start: usize,
    pub end: usize,
}

pub(crate) fn push_diagnostic(
    diagnostics: &mut Vec<ParseDiagnostic>,
    message: &str,
    start: usize,
    end: usize,
) {
    diagnostics.push(ParseDiagnostic {
        message: message.to_string(),
        start,
        end,
    });
}
