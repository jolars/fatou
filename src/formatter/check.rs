//! `format --check`: report which files would be reformatted, with a diff.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use similar::DiffTag;

use crate::file_discovery::{ExcludeFilter, FileDiscoveryError, collect_julia_files};
use crate::formatter::core::{FormatError, format_with_style};
use crate::formatter::style::FormatStyle;
use crate::text::line_diff::bounded_line_diff;

pub struct ChangedFile {
    pub path: PathBuf,
    original: String,
    formatted: String,
}

impl ChangedFile {
    /// Render the line diff on demand. Callers that only need the changed path
    /// never pay for diff construction.
    pub fn diff(&self) -> String {
        line_diff(&self.original, &self.formatted)
    }

    /// Consume the changed file and render its diff, allowing batch callers to
    /// release the retained texts as each parallel job completes.
    pub fn into_diff(self) -> (PathBuf, String) {
        let diff = line_diff(&self.original, &self.formatted);
        (self.path, diff)
    }
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
/// from disk retain both texts so callers can render a diff only when needed.
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
                    original,
                    formatted,
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

fn line_diff(original: &str, formatted: &str) -> String {
    let diff = bounded_line_diff(original, formatted);
    let mut out = String::new();
    for op in diff.ops() {
        let (tag, old_lines, new_lines) = op.as_tag_tuple();
        if tag != DiffTag::Insert {
            for line in &diff.old_lines()[old_lines] {
                push_diff_line(
                    &mut out,
                    if tag == DiffTag::Equal { ' ' } else { '-' },
                    line,
                );
            }
        }
        if tag != DiffTag::Delete && tag != DiffTag::Equal {
            for line in &diff.new_lines()[new_lines] {
                push_diff_line(&mut out, '+', line);
            }
        }
    }
    out
}

fn push_diff_line(out: &mut String, sign: char, line: &str) {
    out.push(sign);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push('\n');
    }
}

/// Convenience for callers that only have a path slice (used in tests).
pub fn diff_for(path: &Path, style: FormatStyle) -> Result<Option<String>, CheckError> {
    let result = check_paths(
        std::slice::from_ref(&path.to_path_buf()),
        style,
        &ExcludeFilter::none(),
    )?;
    Ok(result
        .changed
        .into_iter()
        .next()
        .map(|changed| changed.into_diff().1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_renders_both_sides() {
        assert_eq!(line_diff("a\nb\nc\n", "a\nB\nc\n"), " a\n-b\n+B\n c\n");
        assert_eq!(line_diff("a\nb", "a\nc"), " a\n-b\n+c\n");
    }
}
