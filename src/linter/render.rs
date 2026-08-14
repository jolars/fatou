//! Rendering lint findings for the CLI.
//!
//! `Pretty` draws source-context snippets with `annotate-snippets` (caret
//! underline, rule title, severity coloring, and fix hints). `Concise` is the
//! stable compact one-liner `path:line:col: severity[rule] message`, and `Json`
//! serializes the diagnostics directly.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use annotate_snippets::{AnnotationKind, Level, Renderer, Snippet};

use crate::linter::diagnostic::{Applicability, Diagnostic, Severity};
use crate::linter::docs::rule_doc_url;
use crate::linter::rules::is_shipped_rule;
use crate::text::TextBuffer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Pretty,
    Concise,
    Json,
}

/// How to render a run's findings. `use_color` selects ANSI-styled versus plain
/// snippet rendering; `rule_links` appends the rule's reference URL under each
/// finding. Both only bear on `Pretty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    pub mode: OutputMode,
    pub use_color: bool,
    pub rule_links: bool,
}

impl RenderOptions {
    /// The CLI's rendering: rule links on, since a reader of a finding in a
    /// terminal has nowhere else to look the rule up.
    pub fn new(mode: OutputMode, use_color: bool) -> Self {
        Self {
            mode,
            use_color,
            rule_links: true,
        }
    }

    /// Drop the reference URLs. For the generated rule reference itself, whose
    /// every example would otherwise link to the page it is printed on.
    pub fn without_rule_links(mut self) -> Self {
        self.rule_links = false;
        self
    }
}

/// Render `diagnostics` per `options`. `source_for` returns the source text for
/// a path so byte offsets can be turned into line/column positions (`Concise`)
/// or drawn as source-context snippets (`Pretty`).
pub fn render_findings(
    diagnostics: &[Diagnostic],
    options: RenderOptions,
    source_for: &dyn Fn(Option<&Path>) -> Option<String>,
) -> String {
    match options.mode {
        OutputMode::Json => {
            serde_json::to_string_pretty(diagnostics).unwrap_or_else(|_| "[]".to_string())
        }
        OutputMode::Concise => render_concise(diagnostics, source_for),
        OutputMode::Pretty => render_pretty(diagnostics, options, source_for),
    }
}

fn render_concise(
    diagnostics: &[Diagnostic],
    source_for: &dyn Fn(Option<&Path>) -> Option<String>,
) -> String {
    let mut out = String::new();
    // One source fetch and one line-start scan per *file*, not per finding.
    // Cached rather than grouped by path, because the report's order is the
    // diagnostic order and regrouping would change it.
    let mut sources: HashMap<Option<PathBuf>, Option<TextBuffer>> = HashMap::new();
    for diag in diagnostics {
        let path = diag.path.as_deref();
        let entry = sources
            .entry(diag.path.clone())
            .or_insert_with(|| source_for(path).map(|text| TextBuffer::new(&text)));
        let (line, column) = match entry {
            Some(buffer) => {
                let lc = buffer.byte_to_lc(diag.range.start().into());
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
            diag.message.body
        );
    }
    out
}

fn render_pretty(
    diagnostics: &[Diagnostic],
    options: RenderOptions,
    source_for: &dyn Fn(Option<&Path>) -> Option<String>,
) -> String {
    let renderer = if options.use_color {
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
        diags.sort_by_key(|d| (d.range.start(), d.range.end()));
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
                    d.message.body
                );
            }
            continue;
        };
        let index = TextBuffer::new(&source);
        for d in &diags {
            // Hand `annotate-snippets` only the span's lines, plus a line of
            // padding each side that its folding drops. It builds a source map
            // of whatever it is given, so passing the whole file made rendering
            // O(file) per finding — quadratic over a file's worth of them.
            // `line_start` anchors the gutter back to absolute line numbers, so
            // the output is unchanged.
            let (start, end) = (usize::from(d.range.start()), usize::from(d.range.end()));
            let first = index.byte_to_lc(start).line.saturating_sub(2);
            let last = index.byte_to_lc(end).line;
            let (from, to) = (index.line_start(first), index.line_start(last + 1));
            let snippet = Snippet::source(&source[from..to])
                .line_start(first + 1)
                .path(&origin)
                .annotation(
                    AnnotationKind::Primary
                        .span(start.saturating_sub(from)..end.saturating_sub(from))
                        .label(&d.message.body),
                );
            let group = severity_level(d.severity)
                .primary_title(d.rule)
                .element(snippet);
            let rendered = renderer.render(&[group]);
            let _ = writeln!(out, "{rendered}");
            if let Some(suggestion) = &d.message.suggestion {
                let _ = writeln!(out, "  = help: {suggestion}");
            }
            for fix in &d.fixes {
                let _ = writeln!(
                    out,
                    "  = help: {} ({})",
                    fix.description,
                    fix_note(fix.applicability)
                );
            }
            // Skipped for `parse-error` and any other pseudo-rule: the
            // reference has a section per *registry* rule, so a link for
            // anything else would point at an anchor that does not exist.
            if options.rule_links && is_shipped_rule(d.rule) {
                let _ = writeln!(
                    out,
                    "  = help: for further information visit {}",
                    rule_doc_url(d.rule)
                );
            }
        }
    }
    out
}

/// The parenthetical appended to a fix's `help:` line. Without it a `Safe` and
/// an `Unsafe` fix read identically, hiding that the latter is skipped by
/// `--fix` and only applied with `--unsafe-fixes`.
fn fix_note(applicability: Applicability) -> &'static str {
    match applicability {
        Applicability::Safe => "safe fix",
        Applicability::Unsafe => "unsafe fix, requires `--unsafe-fixes`",
    }
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
    use crate::linter::diagnostic::Fix;
    use rowan::TextRange;

    /// Plain `Pretty` rendering, the shape most of these tests want.
    fn pretty() -> RenderOptions {
        RenderOptions::new(OutputMode::Pretty, false)
    }

    fn warning(start: u32, end: u32, rule: &'static str, message: &str) -> Diagnostic {
        Diagnostic::new(rule, TextRange::new(start.into(), end.into()), message)
    }

    #[test]
    fn pretty_draws_snippet_with_rule_and_source() {
        let src = "x = 1\ny = 2\n";
        let diag = warning(0, 1, "unused-binding", "`x` is never used");
        let out = render_findings(&[diag], pretty(), &|_| Some(src.to_string()));
        assert!(out.contains("unused-binding"), "missing rule title:\n{out}");
        assert!(out.contains("x = 1"), "missing source line:\n{out}");
        assert!(out.contains('^'), "missing caret underline:\n{out}");
        assert!(out.contains("<stdin>"), "missing origin:\n{out}");
    }

    #[test]
    fn pretty_window_keeps_absolute_line_numbers() {
        // The snippet is sliced to the span's lines, so the gutter has to be
        // anchored back to the file's own numbering — a window that forgot
        // `line_start` would report line 1 here.
        let src = "a = 1\nb = 2\nccc = 3\nd = 4\n";
        let at = src.find("ccc").unwrap() as u32;
        let diag = warning(at, at + 3, "unused-binding", "`ccc` is never used");
        let out = render_findings(&[diag], pretty(), &|_| Some(src.to_string()));
        assert!(out.contains("<stdin>:3:1"), "wrong location:\n{out}");
        assert!(out.contains("3 | ccc = 3"), "wrong gutter:\n{out}");
        // Folding drops the padding lines the window carries, so no neighbour
        // leaks into the output.
        assert!(!out.contains("b = 2"), "neighbour leaked:\n{out}");
        assert!(!out.contains("d = 4"), "neighbour leaked:\n{out}");
    }

    #[test]
    fn rendering_is_invariant_to_trailing_file_size() {
        // The property the snippet window and the concise line-table cache both
        // exist for: rendering a finding must not depend on how much source
        // sits beyond it. It used to hold by accident, at O(file) per finding.
        let short = "x = 1\n".to_string();
        let long = format!("{short}{}", "# padding\n".repeat(2000));
        let diag = warning(0, 1, "unused-binding", "`x` is never used");
        for options in [pretty(), RenderOptions::new(OutputMode::Concise, false)] {
            let a = render_findings(std::slice::from_ref(&diag), options, &|_| {
                Some(short.clone())
            });
            let b = render_findings(std::slice::from_ref(&diag), options, &|_| {
                Some(long.clone())
            });
            assert_eq!(a, b, "{options:?}");
        }
    }

    #[test]
    fn concise_reads_each_file_once_however_many_findings() {
        // `render_concise` built a fresh `LineIndex` per diagnostic, so a
        // finding-dense file was quadratic exactly like the pretty path. The
        // cache is keyed by path, so a repeated path must be fetched once.
        let src = "a = 1\nb = 2\nc = 3\n";
        let fetches = std::cell::Cell::new(0);
        let diags: Vec<Diagnostic> = (0..3)
            .map(|i| warning(i * 6, i * 6 + 1, "unused-binding", "never used"))
            .collect();
        let out = render_findings(
            &diags,
            RenderOptions::new(OutputMode::Concise, false),
            &|_| {
                fetches.set(fetches.get() + 1);
                Some(src.to_string())
            },
        );
        assert_eq!(fetches.get(), 1, "one fetch per file, not per finding");
        // The cache must not disturb the resolved positions.
        assert!(out.contains("<stdin>:1:1"), "{out}");
        assert!(out.contains("<stdin>:2:1"), "{out}");
        assert!(out.contains("<stdin>:3:1"), "{out}");
    }

    #[test]
    fn pretty_sorts_by_offset() {
        let src = "a\nbb\nccc\n";
        let later = warning(5, 8, "later", "later finding");
        let earlier = warning(0, 1, "earlier", "earlier finding");
        let out = render_findings(&[later, earlier], pretty(), &|_| Some(src.to_string()));
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
            RenderOptions::new(OutputMode::Pretty, true),
            &|_| Some(src.to_string()),
        );
        let plain = render_findings(std::slice::from_ref(&diag), pretty(), &|_| {
            Some(src.to_string())
        });
        assert!(
            styled.contains('\u{1b}'),
            "styled output lacks ANSI:\n{styled}"
        );
        assert!(!plain.contains('\u{1b}'), "plain output has ANSI:\n{plain}");
    }

    fn diag_with_fix(applicability: Applicability) -> Diagnostic {
        let mut diag = warning(2, 3, "assign-in-cond", "use `==`");
        diag.fixes.push(Fix {
            description: "Change `=` to `==`".to_string(),
            content: "==".to_string(),
            start: 2,
            end: 3,
            applicability,
        });
        diag
    }

    #[test]
    fn pretty_shows_fix_as_help_note() {
        let src = "x = 1\n";
        let out = render_findings(&[diag_with_fix(Applicability::Safe)], pretty(), &|_| {
            Some(src.to_string())
        });
        assert!(
            out.contains("= help: Change `=` to `==` (safe fix)"),
            "missing fix help note:\n{out}"
        );
    }

    #[test]
    fn pretty_marks_unsafe_fix_in_help_note() {
        let src = "x = 1\n";
        let out = render_findings(&[diag_with_fix(Applicability::Unsafe)], pretty(), &|_| {
            Some(src.to_string())
        });
        assert!(
            out.contains("= help: Change `=` to `==` (unsafe fix, requires `--unsafe-fixes`)"),
            "unsafe fix not marked in help note:\n{out}"
        );
    }

    #[test]
    fn pretty_links_a_shipped_rule_to_its_reference_section() {
        let src = "x = 1\n";
        let diag = warning(0, 1, "unused-binding", "`x` is never used");
        let out = render_findings(&[diag], pretty(), &|_| Some(src.to_string()));
        assert!(
            out.contains(
                "= help: for further information visit \
                 https://fatou.dev/reference/rules.html#unused-binding"
            ),
            "missing rule reference link:\n{out}"
        );
    }

    #[test]
    fn pretty_does_not_link_a_pseudo_rule() {
        let src = "x = 1\n";
        let diag = warning(0, 1, crate::linter::check::PARSE_ERROR_RULE, "boom");
        let out = render_findings(&[diag], pretty(), &|_| Some(src.to_string()));
        assert!(
            !out.contains("for further information"),
            "`parse-error` has no reference section to link:\n{out}"
        );
    }

    #[test]
    fn without_rule_links_drops_the_reference_link() {
        let src = "x = 1\n";
        let diag = warning(0, 1, "unused-binding", "`x` is never used");
        let out = render_findings(&[diag], pretty().without_rule_links(), &|_| {
            Some(src.to_string())
        });
        assert!(
            out.contains("unused-binding"),
            "finding not rendered:\n{out}"
        );
        assert!(
            !out.contains("for further information"),
            "rule link not suppressed:\n{out}"
        );
    }

    #[test]
    fn pretty_falls_back_when_source_missing() {
        let diag = warning(0, 1, "some-rule", "some message");
        let out = render_findings(&[diag], pretty(), &|_| None);
        assert_eq!(out, "<stdin>: warning[some-rule] some message\n");
    }

    #[test]
    fn concise_format_is_stable() {
        let src = "x = 1\n";
        let diag = warning(0, 1, "unused-binding", "`x` is never used");
        let out = render_findings(
            &[diag],
            RenderOptions::new(OutputMode::Concise, false),
            &|_| Some(src.to_string()),
        );
        assert_eq!(
            out,
            "<stdin>:1:1: warning[unused-binding] `x` is never used\n"
        );
    }
}
