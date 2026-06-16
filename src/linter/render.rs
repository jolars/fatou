//! Rendering lint findings for the CLI.
//!
//! Pretty output is a compact `path:line:col: severity[rule] message` for now;
//! it will adopt `annotate-snippets` for source-context rendering when rules
//! land (see `TODO.md`). Concise and JSON are stable.

use std::path::Path;

use crate::linter::diagnostic::Diagnostic;
use crate::text::LineIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Pretty,
    Concise,
    Json,
}

/// Render `diagnostics`. `source_for` returns the source text for a path so
/// byte offsets can be turned into line/column positions.
pub fn render_findings(
    diagnostics: &[Diagnostic],
    mode: OutputMode,
    source_for: &dyn Fn(Option<&Path>) -> Option<String>,
) -> String {
    match mode {
        OutputMode::Json => {
            serde_json::to_string_pretty(diagnostics).unwrap_or_else(|_| "[]".to_string())
        }
        OutputMode::Pretty | OutputMode::Concise => {
            let mut out = String::new();
            for diag in diagnostics {
                let path = diag.path.as_deref();
                let (line, column) = match source_for(path) {
                    Some(text) => {
                        let lc = LineIndex::new(&text).byte_to_lc(diag.start);
                        (lc.line, lc.column)
                    }
                    None => (0, 0),
                };
                let location = path
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<stdin>".to_string());
                out.push_str(&format!(
                    "{location}:{line}:{column}: {}[{}] {}\n",
                    diag.severity.label(),
                    diag.rule,
                    diag.message
                ));
            }
            out
        }
    }
}
