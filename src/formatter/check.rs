//! `format --check`: report which files would be reformatted, with a diff.

use std::path::{Path, PathBuf};

use similar::{ChangeTag, TextDiff};

use crate::file_discovery::{FileDiscoveryError, collect_julia_files};
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
pub fn check_paths(paths: &[PathBuf], style: FormatStyle) -> Result<CheckResult, CheckError> {
    let files = collect_julia_files(paths).map_err(CheckError::Discovery)?;
    let mut changed = Vec::new();

    for path in &files {
        let original = std::fs::read_to_string(path).map_err(|err| CheckError::Io {
            path: path.clone(),
            message: err.to_string(),
        })?;
        let formatted =
            format_with_style(&original, style).map_err(|error| CheckError::Format {
                path: path.clone(),
                error,
            })?;
        if formatted != original {
            changed.push(ChangedFile {
                path: path.clone(),
                diff: line_diff(&original, &formatted),
            });
        }
    }

    Ok(CheckResult {
        checked: files.len(),
        changed,
    })
}

fn line_diff(original: &str, formatted: &str) -> String {
    let diff = TextDiff::from_lines(original, formatted);
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
    let result = check_paths(std::slice::from_ref(&path.to_path_buf()), style)?;
    Ok(result.changed.into_iter().next().map(|c| c.diff))
}
