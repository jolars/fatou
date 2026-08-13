//! `format --check`: report which files would be reformatted, with a diff.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use similar::{Algorithm, ChangeTag, TextDiff};

use crate::file_discovery::{ExcludeFilter, FileDiscoveryError, collect_julia_files};
use crate::formatter::core::{FormatError, format_with_style};
use crate::formatter::style::FormatStyle;

pub struct ChangedFile {
    pub path: PathBuf,
    pub diff: String,
}

pub struct CheckResult {
    pub checked: usize,
    pub changed: Vec<ChangedFile>,
}

#[derive(Debug)]
pub enum CheckError {
    Discovery(FileDiscoveryError),
    Io { path: PathBuf, message: String },
    Format { path: PathBuf, error: FormatError },
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Discovery(err) => write!(f, "{err}"),
            CheckError::Io { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
            CheckError::Format { path, error } => {
                write!(f, "failed to format {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for CheckError {}

/// Check every `.jl` file under `paths`. Files whose formatted output differs
/// from disk are collected with a unified-style diff.
pub fn check_paths(
    paths: &[PathBuf],
    style: FormatStyle,
    exclude: &ExcludeFilter,
) -> Result<CheckResult, CheckError> {
    let files = collect_julia_files(paths, exclude).map_err(CheckError::Discovery)?;

    // Each file is independent, so check them in parallel; `collect` preserves
    // the sorted discovery order for deterministic output.
    let changed = files
        .par_iter()
        .map(|path| {
            let original = std::fs::read_to_string(path).map_err(|err| CheckError::Io {
                path: path.clone(),
                message: err.to_string(),
            })?;
            let formatted =
                format_with_style(&original, style).map_err(|error| CheckError::Format {
                    path: path.clone(),
                    error,
                })?;
            Ok(if formatted != original {
                Some(ChangedFile {
                    path: path.clone(),
                    diff: line_diff(&original, &formatted),
                })
            } else {
                None
            })
        })
        .collect::<Result<Vec<_>, CheckError>>()?
        .into_iter()
        .flatten()
        .collect();

    Ok(CheckResult {
        checked: files.len(),
        changed,
    })
}

/// **Patience, not the default Myers.** Myers is `O((N+M)*D)`, and `D` is the
/// whole file whenever formatting relays a document rather than touching a few
/// lines — which is what a first `fatou format` over an unformatted project
/// does. That made the diff **93% of a `--check` run** (33 / 128 / 471 ms of a
/// 42 / 145 / 504 ms run at 4000 / 8000 / 16 000 changed lines). `similar`'s
/// disjoint fast path cannot rescue it: it runs before prefix trimming and
/// bails as soon as the two texts share a first line. Patience anchors on lines
/// unique to both sides instead, and is deterministic (no wall-clock deadline).
///
/// **This is a constant factor, not a complexity fix.** On Julia's line
/// population every algorithm measured is still quadratic on that change —
/// Myers 3.81x, Patience 3.71x, Histogram 4.08x per doubling. Patience is
/// simply ~3.2x cheaper throughout (117.9 vs 37.1 ms at 500 functions). The
/// residual is recorded in TODO.md; do not read this as "the diff is linear".
///
/// **Chosen for its worst case, not its best.** `Algorithm::Histogram` is
/// faster on real Julia (136 ms against Patience's 234 ms over 16 000 lines of
/// the bench corpus) but collapses on highly self-similar input — 430.9 ms
/// against Patience's 37.1 ms at 500 near-identical small functions. `--check`
/// runs over whatever is in the tree, generated code included, so the algorithm
/// that is *never worse than Myers* wins over the one that is usually much
/// better. Kept in step with badness's `diff_lines`, where the same trade was
/// measured the other way round.
fn line_diff(original: &str, formatted: &str) -> String {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Patience)
        .diff_lines(original, formatted);
    let mut out = String::new();
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        out.push_str(sign);
        out.push_str(change.value());
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Convenience for callers that only have a path slice (used in tests).
pub fn diff_for(path: &Path, style: FormatStyle) -> Result<Option<String>, CheckError> {
    let result = check_paths(
        std::slice::from_ref(&path.to_path_buf()),
        style,
        &ExcludeFilter::none(),
    )?;
    Ok(result.changed.into_iter().next().map(|c| c.diff))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property that outlives any particular diff algorithm: the emitted
    /// change stream must replay both sides exactly. Which lines pair with
    /// which is the algorithm's business; dropping or duplicating one is a bug
    /// in any of them, and `--check` is the only account a CI log gets of what
    /// would change.
    fn assert_reconstructs(original: &str, formatted: &str) {
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_lines(original, formatted);
        let (mut old, mut new) = (String::new(), String::new());
        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Delete => old.push_str(change.value()),
                ChangeTag::Insert => new.push_str(change.value()),
                ChangeTag::Equal => {
                    old.push_str(change.value());
                    new.push_str(change.value());
                }
            }
        }
        assert_eq!(
            old, original,
            "delete+equal stream must replay the original"
        );
        assert_eq!(
            new, formatted,
            "insert+equal stream must replay the formatted text"
        );
    }

    #[test]
    fn diff_reconstructs_both_sides() {
        assert_reconstructs("a\nb\nc\n", "a\nB\nc\n");
        // No trailing newline on either side.
        assert_reconstructs("a\nb", "a\nc");
        assert_reconstructs("", "x\n");
        assert_reconstructs("x\n", "");
        // Reindent-everything: the shape that made Myers quadratic.
        assert_reconstructs(&"        x = 1\n".repeat(400), &"    x = 1\n".repeat(400));
        // Near-identical small functions: the shape Histogram collapsed on.
        let old: String = (0..200)
            .map(|i| format!("function f{i}()\n        x = {i}\nend\n"))
            .collect();
        let new: String = (0..200)
            .map(|i| format!("function f{i}()\n    x = {i}\nend\n"))
            .collect();
        assert_reconstructs(&old, &new);
    }

    // There is deliberately **no growth-rate test** for the diff here, unlike
    // badness's `diff_scales_linearly_when_every_line_changes`. On Julia's line
    // population every algorithm measured is still quadratic on a
    // reindent-everything change -- Myers 3.81x, Patience 3.71x, Histogram 4.08x
    // per doubling -- so a ratio bound would separate none of them. What the
    // switch bought is a constant factor (117.9 / 37.1 / 430.9 ms at 500
    // functions), and asserting a wall-clock constant would just be a
    // machine-dependent flake. The remaining superlinearity is recorded in
    // TODO.md instead.
}
