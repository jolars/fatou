//! Support for `fatou debug format`, the per-file invariant checker the
//! smoke-test workflow (`.github/workflows/smoke-test.yml`) drives.
//!
//! Output strings here are contracts: the workflow greps the parenthesized
//! failure labels (`losslessness`, `idempotency`, `format-error`), parses the
//! report's `Approx. diff start line` bullet, and predicts dump-file names
//! from [`sanitize_path_for_filename`]. Change them in lockstep.

use std::path::Path;

use crate::cli::DebugChecksArg;
use crate::formatter::{FormatStyle, format_with_style};
use crate::parser::{parse, reconstruct};

/// One invariant (or the failure to even run it) checked per file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    Losslessness,
    Idempotency,
    FormatError,
}

impl CheckKind {
    pub fn label(self) -> &'static str {
        match self {
            CheckKind::Losslessness => "losslessness",
            CheckKind::Idempotency => "idempotency",
            CheckKind::FormatError => "format-error",
        }
    }
}

/// A failed check: the two texts whose divergence is the finding. For
/// `format-error`, `left` is the error message and `right` is empty (there is
/// nothing to diff).
pub struct DebugFailure {
    pub kind: CheckKind,
    pub left: String,
    pub right: String,
}

/// Everything one file's check run produced: the pass texts (for `--dump-dir`)
/// plus any failures.
#[derive(Default)]
pub struct DebugArtifacts {
    /// `(input, parsed-reconstruction)` when the losslessness check ran.
    pub losslessness: Option<(String, String)>,
    /// `(input, once, twice)` when the idempotency check ran to completion.
    pub idempotency: Option<(String, String, String)>,
    pub failures: Vec<DebugFailure>,
}

/// The `--checks` value as it appears in output (report header and the
/// all-passed line).
pub fn checks_label(checks: DebugChecksArg) -> &'static str {
    match checks {
        DebugChecksArg::Idempotency => "idempotency",
        DebugChecksArg::Losslessness => "losslessness",
        DebugChecksArg::All => "all",
    }
}

/// Map every character outside `[A-Za-z0-9._-]` to `_`, matching the smoke-test
/// workflow's `sed 's/[^[:alnum:]._-]/_/g'` so it can predict artifact names
/// from a repo-relative path.
pub fn sanitize_path_for_filename(path: &str) -> String {
    path.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 1-based line number of the first difference between two texts. When the
/// visible lines all match (the difference is only in trailing newline
/// material) this points just past the common lines; identical texts return 1,
/// which never occurs on a failure.
pub fn first_diff_line(left: &str, right: &str) -> usize {
    let mut left_lines = left.lines();
    let mut right_lines = right.lines();
    let mut line = 1;
    loop {
        match (left_lines.next(), right_lines.next()) {
            (Some(a), Some(b)) if a == b => line += 1,
            _ => return line,
        }
    }
}

/// Render a minimal diff body: a few common context lines, then the two sides'
/// remaining lines as `-`/`+` runs starting at the first difference, capped so
/// a whole-file divergence stays readable. The smoke-test workflow never
/// parses this block (only `Approx. diff start line`), so first-mismatch
/// granularity is enough — no diff dependency needed.
fn render_window_diff(left: &str, right: &str, out: &mut String) {
    const CONTEXT: usize = 3;
    const MAX_SIDE: usize = 40;
    let start = first_diff_line(left, right) - 1;
    let context_start = start.saturating_sub(CONTEXT);
    for line in left.lines().skip(context_start).take(start - context_start) {
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }
    for (side, text) in [('-', left), ('+', right)] {
        let mut lines = text.lines().skip(start);
        for line in lines.by_ref().take(MAX_SIDE) {
            out.push(side);
            out.push_str(line);
            out.push('\n');
        }
        if lines.next().is_some() {
            out.push(side);
            out.push_str(" [truncated]\n");
        }
    }
}

/// Build the `--report` Markdown. Contract with the smoke-test workflow: the
/// `### k. \`file\` (kind)` headings carry the parenthesized failure label, and
/// each diffable failure has an `Approx. diff start line: N` bullet.
pub fn build_debug_report(
    checks: DebugChecksArg,
    files_checked: usize,
    failures: &[(String, DebugFailure)],
) -> String {
    let mut out = String::new();
    out.push_str("# Debug-format regression report\n\n");
    out.push_str(&format!(
        "- Checks: `{}`\n- Files checked: {files_checked}\n- Failures: {}\n\n",
        checks_label(checks),
        failures.len()
    ));
    if failures.is_empty() {
        out.push_str("All checks passed.\n");
        return out;
    }
    out.push_str("## Failures\n\n");
    for (idx, (file, failure)) in failures.iter().enumerate() {
        out.push_str(&format!(
            "### {}. `{}` ({})\n\n",
            idx + 1,
            file,
            failure.kind.label()
        ));
        if failure.kind == CheckKind::FormatError {
            out.push_str(&format!("- Error: {}\n\n", failure.left));
            continue;
        }
        out.push_str(&format!(
            "- Approx. diff start line: {}\n\n",
            first_diff_line(&failure.left, &failure.right)
        ));
        out.push_str("```diff\n");
        render_window_diff(&failure.left, &failure.right, &mut out);
        out.push_str("```\n\n");
    }
    out
}

/// Write one file's pass texts and failure sides into `dump_dir`. Pass texts
/// are written when their check failed, or always under `--dump-passes`. The
/// `{stem}.idempotency.{input,once,twice}.txt` names are the contract the
/// smoke-test workflow's artifact lookup depends on.
pub fn write_debug_artifacts(
    dump_dir: &Path,
    stem: &str,
    artifacts: &DebugArtifacts,
    dump_passes: bool,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dump_dir)?;
    let failed = |kind: CheckKind| artifacts.failures.iter().any(|f| f.kind == kind);

    if let Some((input, parsed)) = artifacts.losslessness.as_ref()
        && (dump_passes || failed(CheckKind::Losslessness))
    {
        std::fs::write(
            dump_dir.join(format!("{stem}.losslessness.input.txt")),
            input,
        )?;
        std::fs::write(
            dump_dir.join(format!("{stem}.losslessness.parsed.txt")),
            parsed,
        )?;
    }

    if let Some((input, once, twice)) = artifacts.idempotency.as_ref()
        && (dump_passes || failed(CheckKind::Idempotency))
    {
        std::fs::write(
            dump_dir.join(format!("{stem}.idempotency.input.txt")),
            input,
        )?;
        std::fs::write(dump_dir.join(format!("{stem}.idempotency.once.txt")), once)?;
        std::fs::write(
            dump_dir.join(format!("{stem}.idempotency.twice.txt")),
            twice,
        )?;
    }

    for failure in &artifacts.failures {
        let kind = failure.kind.label();
        std::fs::write(
            dump_dir.join(format!("{stem}.{kind}.left.txt")),
            &failure.left,
        )?;
        std::fs::write(
            dump_dir.join(format!("{stem}.{kind}.right.txt")),
            &failure.right,
        )?;
    }

    Ok(())
}

/// Run the selected checks over one file's content.
///
/// Losslessness compares the CST's reconstruction to the input. Idempotency
/// formats twice through the same pipeline `fatou format` uses. Input with
/// parse diagnostics is a `format-error` finding (the invariant could not be
/// evaluated); a first-pass output that fails to reparse cleanly or reach a
/// fixed point *is* an idempotency violation.
pub fn run_debug_checks_for_file(
    content: &str,
    style: FormatStyle,
    checks: DebugChecksArg,
) -> DebugArtifacts {
    let mut artifacts = DebugArtifacts::default();

    if matches!(checks, DebugChecksArg::Losslessness | DebugChecksArg::All) {
        let reconstructed = reconstruct(content);
        artifacts.losslessness = Some((content.to_string(), reconstructed.clone()));
        if reconstructed != content {
            artifacts.failures.push(DebugFailure {
                kind: CheckKind::Losslessness,
                left: content.to_string(),
                right: reconstructed,
            });
        }
    }

    if matches!(checks, DebugChecksArg::Idempotency | DebugChecksArg::All) {
        let diagnostics = parse(content).diagnostics;
        if let Some(first) = diagnostics.first() {
            artifacts.failures.push(DebugFailure {
                kind: CheckKind::FormatError,
                left: format!(
                    "input has {} parse diagnostic(s); first at [{}..{}]: {}",
                    diagnostics.len(),
                    first.start,
                    first.end,
                    first.message
                ),
                right: String::new(),
            });
            return artifacts;
        }
        match format_with_style(content, style) {
            Err(err) => artifacts.failures.push(DebugFailure {
                kind: CheckKind::FormatError,
                left: err.to_string(),
                right: String::new(),
            }),
            Ok(once) => {
                let once_diags = parse(&once).diagnostics;
                if let Some(first) = once_diags.first() {
                    artifacts.idempotency =
                        Some((content.to_string(), once.clone(), String::new()));
                    artifacts.failures.push(DebugFailure {
                        kind: CheckKind::Idempotency,
                        left: once,
                        right: format!(
                            "formatted output does not reparse cleanly: [{}..{}]: {}",
                            first.start, first.end, first.message
                        ),
                    });
                    return artifacts;
                }
                match format_with_style(&once, style) {
                    Ok(twice) => {
                        artifacts.idempotency =
                            Some((content.to_string(), once.clone(), twice.clone()));
                        if once != twice {
                            artifacts.failures.push(DebugFailure {
                                kind: CheckKind::Idempotency,
                                left: once,
                                right: twice,
                            });
                        }
                    }
                    Err(err) => {
                        artifacts.idempotency =
                            Some((content.to_string(), once.clone(), String::new()));
                        artifacts.failures.push(DebugFailure {
                            kind: CheckKind::Idempotency,
                            left: once,
                            right: format!("second pass failed to format: {err}"),
                        });
                    }
                }
            }
        }
    }

    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_matches_the_workflow_sed_class() {
        assert_eq!(
            sanitize_path_for_filename("sub dir/a b.jl"),
            "sub_dir_a_b.jl"
        );
        assert_eq!(sanitize_path_for_filename("ok-1.2_x.jl"), "ok-1.2_x.jl");
        // Non-ASCII maps per *character* (`å`, `/`, `ü` → three `_`). GNU sed
        // in a UTF-8 locale may instead keep non-ASCII alnums; the workflow's
        // artifact lookups are existence-guarded, so that divergence only
        // drops the artifact link for such paths.
        assert_eq!(sanitize_path_for_filename("å/ü.jl"), "___.jl");
    }

    #[test]
    fn first_diff_line_finds_the_first_mismatch() {
        assert_eq!(first_diff_line("a\nb\nc\n", "a\nB\nc\n"), 2);
        assert_eq!(first_diff_line("a\n", "a\nb\n"), 2);
        assert_eq!(first_diff_line("same\n", "same\n"), 2);
    }

    #[test]
    fn parse_diagnostics_bucket_as_format_error_only() {
        let artifacts =
            run_debug_checks_for_file("f(x\n", FormatStyle::default(), DebugChecksArg::All);
        let labels: Vec<_> = artifacts.failures.iter().map(|f| f.kind.label()).collect();
        assert_eq!(labels, ["format-error"]);
    }
}
