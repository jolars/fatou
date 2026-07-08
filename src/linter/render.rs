//! Rendering lint findings for the CLI.
//!
//! `Pretty` draws source-context snippets with `annotate-snippets` (caret
//! underline, rule title, severity coloring, and fix hints). `Concise` is the
//! stable compact one-liner `path:line:col: severity[rule] message`, and `Json`
//! serializes the diagnostics directly.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use annotate_snippets::{AnnotationKind, Level, Renderer, Snippet};

use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::text::LineIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Pretty,
    Concise,
    Json,
}

/// Render `diagnostics` in the requested mode. `source_for` returns the source
/// text for a path so byte offsets can be turned into line/column positions
/// (`Concise`) or drawn as source-context snippets (`Pretty`). `use_color`
/// selects ANSI-styled versus plain snippet rendering.
pub fn render_findings(
    diagnostics: &[Diagnostic],
    mode: OutputMode,
    use_color: bool,
    source_for: &dyn Fn(Option<&Path>) -> Option<String>,
) -> String {
    match mode {
        OutputMode::Json => {
            serde_json::to_string_pretty(diagnostics).unwrap_or_else(|_| "[]".to_string())
        }
        OutputMode::Concise => render_concise(diagnostics, source_for),
        OutputMode::Pretty => render_pretty(diagnostics, use_color, source_for),
    }
}

fn render_concise(
    diagnostics: &[Diagnostic],
    source_for: &dyn Fn(Option<&Path>) -> Option<String>,
) -> String {
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
        let _ = writeln!(
            out,
            "{location}:{line}:{column}: {}[{}] {}",
            diag.severity.label(),
            diag.rule,
            diag.message
        );
    }
    out
}

fn render_pretty(
    diagnostics: &[Diagnostic],
    use_color: bool,
    source_for: &dyn Fn(Option<&Path>) -> Option<String>,
) -> String {
    let renderer = if use_color {
        Renderer::styled()
    } else {
        Renderer::plain()
    };
    // Group by file so each snippet reuses one source string; `None` (stdin)
    // sorts first.
    let mut by_path: BTreeMap<Option<&PathBuf>, Vec<&Diagnostic>> = BTreeMap::new();
    for d in diagnostics {
        by_path.entry(d.path.as_ref()).or_default().push(d);
    }
    let mut out = String::new();
    for (path, mut diags) in by_path {
        diags.sort_by_key(|d| (d.start, d.end));
        let origin = path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<stdin>".to_string());
        let Some(source) = source_for(path.map(PathBuf::as_path)) else {
            // Source unavailable: fall back to a concise line per diagnostic.
            for d in &diags {
                let _ = writeln!(
                    out,
                    "{origin}: {}[{}] {}",
                    d.severity.label(),
                    d.rule,
                    d.message
                );
            }
            continue;
        };
        for d in &diags {
            let snippet = Snippet::source(&source).path(&origin).annotation(
                AnnotationKind::Primary
                    .span(d.start..d.end)
                    .label(&d.message),
            );
            let group = severity_level(d.severity)
                .primary_title(d.rule.as_str())
                .element(snippet);
            let rendered = renderer.render(&[group]);
            let _ = writeln!(out, "{rendered}");
            for fix in &d.fixes {
                let _ = writeln!(out, "  = help: {}", fix.description);
            }
        }
    }
    out
}

fn severity_level(s: Severity) -> Level<'static> {
    match s {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
        Severity::Info => Level::INFO,
        Severity::Hint => Level::HELP,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linter::diagnostic::{Applicability, Fix};

    fn warning(start: usize, end: usize, rule: &str, message: &str) -> Diagnostic {
        Diagnostic {
            path: None,
            start,
            end,
            rule: rule.to_string(),
            severity: Severity::Warning,
            message: message.to_string(),
            fixes: Vec::new(),
            suppressed: false,
        }
    }

    #[test]
    fn pretty_draws_snippet_with_rule_and_source() {
        let src = "x = 1\ny = 2\n";
        let diag = warning(0, 1, "unused-binding", "`x` is never used");
        let out = render_findings(&[diag], OutputMode::Pretty, false, &|_| {
            Some(src.to_string())
        });
        assert!(out.contains("unused-binding"), "missing rule title:\n{out}");
        assert!(out.contains("x = 1"), "missing source line:\n{out}");
        assert!(out.contains('^'), "missing caret underline:\n{out}");
        assert!(out.contains("<stdin>"), "missing origin:\n{out}");
    }

    #[test]
    fn pretty_sorts_by_offset() {
        let src = "a\nbb\nccc\n";
        let later = warning(5, 8, "later", "later finding");
        let earlier = warning(0, 1, "earlier", "earlier finding");
        let out = render_findings(&[later, earlier], OutputMode::Pretty, false, &|_| {
            Some(src.to_string())
        });
        let e = out.find("earlier").expect("earlier rendered");
        let l = out.find("later").expect("later rendered");
        assert!(e < l, "diagnostics not sorted by offset:\n{out}");
    }

    #[test]
    fn pretty_color_flag_toggles_ansi() {
        let src = "x = 1\n";
        let diag = warning(0, 1, "rule", "msg");
        let styled = render_findings(
            std::slice::from_ref(&diag),
            OutputMode::Pretty,
            true,
            &|_| Some(src.to_string()),
        );
        let plain = render_findings(
            std::slice::from_ref(&diag),
            OutputMode::Pretty,
            false,
            &|_| Some(src.to_string()),
        );
        assert!(
            styled.contains('\u{1b}'),
            "styled output lacks ANSI:\n{styled}"
        );
        assert!(!plain.contains('\u{1b}'), "plain output has ANSI:\n{plain}");
    }

    #[test]
    fn pretty_shows_fix_as_help_note() {
        let src = "x = 1\n";
        let mut diag = warning(2, 3, "assign-in-cond", "use `==`");
        diag.fixes.push(Fix {
            description: "Change `=` to `==`".to_string(),
            content: "==".to_string(),
            start: 2,
            end: 3,
            applicability: Applicability::Safe,
        });
        let out = render_findings(&[diag], OutputMode::Pretty, false, &|_| {
            Some(src.to_string())
        });
        assert!(
            out.contains("= help: Change `=` to `==`"),
            "missing fix help note:\n{out}"
        );
    }

    #[test]
    fn pretty_falls_back_when_source_missing() {
        let diag = warning(0, 1, "some-rule", "some message");
        let out = render_findings(&[diag], OutputMode::Pretty, false, &|_| None);
        assert_eq!(out, "<stdin>: warning[some-rule] some message\n");
    }

    #[test]
    fn concise_format_is_stable() {
        let src = "x = 1\n";
        let diag = warning(0, 1, "unused-binding", "`x` is never used");
        let out = render_findings(&[diag], OutputMode::Concise, false, &|_| {
            Some(src.to_string())
        });
        assert_eq!(
            out,
            "<stdin>:1:1: warning[unused-binding] `x` is never used\n"
        );
    }
}
