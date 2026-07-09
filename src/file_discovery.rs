//! Discovery of `.jl` source files from CLI path arguments.
//!
//! Each argument is one of three things: an explicit `.jl` file, a directory
//! (walked with `.gitignore` semantics), or a glob pattern such as
//! `src/**/*.jl`. Globbing is handled internally so patterns work even when the
//! shell does not expand them (quoted arguments, `fatou.toml`-driven runs, or
//! platforms without shell globbing).

use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use ignore::WalkBuilder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiscoveryError {
    NonJuliaFilePath { path: PathBuf },
    BadGlob { pattern: String, message: String },
    Walk { path: PathBuf, message: String },
}

impl std::fmt::Display for FileDiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileDiscoveryError::NonJuliaFilePath { path } => {
                write!(f, "not a Julia (.jl) file: {}", path.display())
            }
            FileDiscoveryError::BadGlob { pattern, message } => {
                write!(f, "invalid glob pattern `{pattern}`: {message}")
            }
            FileDiscoveryError::Walk { path, message } => {
                write!(f, "failed to walk {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for FileDiscoveryError {}

/// Collect `.jl` files from `paths`. A file path must end in `.jl`; a directory
/// is walked with `.gitignore` semantics; a glob pattern is expanded against the
/// filesystem (also honoring `.gitignore`). Results are sorted and deduplicated.
pub fn collect_julia_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, FileDiscoveryError> {
    let mut files = Vec::new();

    for path in paths {
        if path.is_file() {
            if !is_julia_file(path) {
                return Err(FileDiscoveryError::NonJuliaFilePath { path: path.clone() });
            }
            files.push(path.clone());
            continue;
        }

        if path.is_dir() {
            walk_julia_files(path, None, &mut files)?;
            continue;
        }

        if let Some(pattern) = path.to_str().filter(|p| has_glob_meta(p)) {
            collect_glob(pattern, &mut files)?;
            continue;
        }

        return Err(FileDiscoveryError::Walk {
            path: path.clone(),
            message: "path does not exist".to_string(),
        });
    }

    files.sort();
    files.dedup();
    Ok(files)
}

/// Expand a glob `pattern` into `.jl` files. The walk is rooted at the pattern's
/// longest literal prefix so we never scan more of the tree than necessary, then
/// every discovered file is tested against the full pattern.
fn collect_glob(pattern: &str, files: &mut Vec<PathBuf>) -> Result<(), FileDiscoveryError> {
    let matcher = GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|err| FileDiscoveryError::BadGlob {
            pattern: pattern.to_string(),
            message: err.to_string(),
        })?
        .compile_matcher();

    let base = glob_base(pattern);
    walk_julia_files(&base, Some(&matcher), files)
}

/// Walk `root` with `.gitignore` semantics, pushing every `.jl` file that also
/// satisfies `matcher` (when one is given) into `files`.
fn walk_julia_files(
    root: &Path,
    matcher: Option<&globset::GlobMatcher>,
    files: &mut Vec<PathBuf>,
) -> Result<(), FileDiscoveryError> {
    let mut builder = WalkBuilder::new(root);
    builder.standard_filters(true);
    builder.hidden(false);
    for entry in builder.build() {
        match entry {
            Ok(entry) => {
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }
                let entry_path = entry.path();
                if !is_julia_file(entry_path) {
                    continue;
                }
                if let Some(matcher) = matcher
                    && !matcher.is_match(normalize(entry_path))
                {
                    continue;
                }
                files.push(entry_path.to_path_buf());
            }
            Err(err) => {
                return Err(FileDiscoveryError::Walk {
                    path: root.to_path_buf(),
                    message: err.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// The longest leading run of components in `pattern` that contain no glob
/// metacharacters. Used as the walk root; falls back to `.` when the very first
/// component is already a pattern (e.g. `*.jl` or `**/*.jl`).
fn glob_base(pattern: &str) -> PathBuf {
    let mut base = PathBuf::new();
    for component in Path::new(pattern).components() {
        match component.as_os_str().to_str() {
            Some(s) if has_glob_meta(s) => break,
            _ => base.push(component),
        }
    }
    if base.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        base
    }
}

/// Strip a leading `./` so paths from a `.`-rooted walk line up with a pattern
/// that has no literal prefix (`*.jl` should match `./foo.jl`).
fn normalize(path: &Path) -> &Path {
    path.strip_prefix(".").unwrap_or(path)
}

fn has_glob_meta(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
}

fn is_julia_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small tree under a fresh temp dir and return its root.
    fn scratch_tree() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fatou-fd-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/inner")).unwrap();
        std::fs::write(root.join("a.jl"), "x = 1\n").unwrap();
        std::fs::write(root.join("readme.md"), "hi\n").unwrap();
        std::fs::write(root.join("src/b.jl"), "y = 2\n").unwrap();
        std::fs::write(root.join("src/inner/c.jl"), "z = 3\n").unwrap();
        root
    }

    fn names(files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn explicit_file_must_be_julia() {
        let root = scratch_tree();
        let err = collect_julia_files(&[root.join("readme.md")]).unwrap_err();
        assert!(matches!(err, FileDiscoveryError::NonJuliaFilePath { .. }));
    }

    #[test]
    fn directory_is_walked_recursively() {
        let root = scratch_tree();
        let files = collect_julia_files(std::slice::from_ref(&root)).unwrap();
        assert_eq!(names(&files), vec!["a.jl", "b.jl", "c.jl"]);
    }

    #[test]
    fn recursive_glob_matches_nested_files() {
        let root = scratch_tree();
        let pattern = root.join("**/*.jl");
        let files = collect_julia_files(&[pattern]).unwrap();
        assert_eq!(names(&files), vec!["a.jl", "b.jl", "c.jl"]);
    }

    #[test]
    fn single_star_does_not_cross_directories() {
        let root = scratch_tree();
        let pattern = root.join("src/*.jl");
        let files = collect_julia_files(&[pattern]).unwrap();
        assert_eq!(names(&files), vec!["b.jl"]);
    }

    #[test]
    fn missing_non_glob_path_errors() {
        let err = collect_julia_files(&[PathBuf::from("does/not/exist.jl")]).unwrap_err();
        assert!(matches!(err, FileDiscoveryError::Walk { .. }));
    }
}
