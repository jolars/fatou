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
use ignore::gitignore::{Gitignore, GitignoreBuilder};

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

/// A compiled set of exclude patterns applied during file discovery.
///
/// Patterns use gitignore semantics and are resolved relative to a root (the
/// directory containing `fatou.toml`, or the working directory when there is no
/// config). The filter prunes matching directories and files from directory and
/// glob walks; by default it does **not** affect files a user names explicitly
/// on the command line (those are always processed, matching ruff's default
/// behavior). With `force` set (the `--force-exclude` flag), explicitly named
/// files that match a pattern are skipped too—for runners like pre-commit that
/// pass staged files as arguments.
#[derive(Debug, Clone, Default)]
pub struct ExcludeFilter {
    matcher: Option<Gitignore>,
    force: bool,
}

/// A malformed exclude pattern, surfaced to the CLI so it can report and exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludeError {
    pub pattern: String,
    pub message: String,
}

impl std::fmt::Display for ExcludeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid exclude pattern `{}`: {}",
            self.pattern, self.message
        )
    }
}

impl std::error::Error for ExcludeError {}

impl ExcludeFilter {
    /// A filter that excludes nothing. Used by callers that do their own
    /// scoping or have no config in hand.
    pub fn none() -> Self {
        Self::default()
    }

    /// Compile `patterns` into a matcher rooted at `root`.
    pub fn new(root: &Path, patterns: &[String]) -> Result<Self, ExcludeError> {
        if patterns.is_empty() {
            return Ok(Self::none());
        }
        let mut builder = GitignoreBuilder::new(root);
        for pattern in patterns.iter().cloned() {
            if let Err(err) = builder.add_line(None, &pattern) {
                return Err(ExcludeError {
                    pattern,
                    message: err.to_string(),
                });
            }
        }
        let matcher = builder.build().map_err(|err| ExcludeError {
            pattern: String::new(),
            message: err.to_string(),
        })?;
        Ok(Self {
            matcher: Some(matcher),
            force: false,
        })
    }

    /// Also apply the patterns to explicitly named files (`--force-exclude`).
    pub fn with_force_exclude(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// Whether explicitly named files are subject to the patterns.
    pub fn force(&self) -> bool {
        self.force
    }

    /// Whether `path`, named explicitly on the command line, should be skipped.
    /// Always `false` without force mode. Unlike the walk (where pruning an
    /// excluded directory hides everything beneath it), an explicit file must
    /// be tested against its ancestors too, so `deps/` catches
    /// `deps/build.jl`.
    pub fn force_excludes(&self, path: &Path) -> bool {
        if !self.force {
            return false;
        }
        match &self.matcher {
            Some(matcher) => {
                // `matched_path_or_any_parents` asserts that `path` lives under
                // the matcher root; an absolute path outside it cannot match
                // root-relative patterns anyway.
                if path.is_absolute() && !path.starts_with(matcher.path()) {
                    return false;
                }
                matcher.matched_path_or_any_parents(path, false).is_ignore()
            }
            None => false,
        }
    }

    fn is_excluded(&self, path: &Path, is_dir: bool) -> bool {
        match &self.matcher {
            Some(matcher) => matcher.matched(path, is_dir).is_ignore(),
            None => false,
        }
    }
}

/// Collect `.jl` files from `paths`. A file path must end in `.jl`; a directory
/// is walked with `.gitignore` semantics; a glob pattern is expanded against the
/// filesystem (also honoring `.gitignore`). Directory and glob walks are pruned
/// by `exclude`; explicitly named files are only subject to it under force mode
/// (see [`ExcludeFilter`]). Results are sorted and deduplicated.
pub fn collect_julia_files(
    paths: &[PathBuf],
    exclude: &ExcludeFilter,
) -> Result<Vec<PathBuf>, FileDiscoveryError> {
    let mut files = Vec::new();

    for path in paths {
        if path.is_file() {
            // The force check runs before the extension check so that an
            // excluded non-Julia file (as a runner like pre-commit may stage)
            // is silently skipped rather than a hard error.
            if exclude.force_excludes(path) {
                continue;
            }
            if !is_julia_file(path) {
                return Err(FileDiscoveryError::NonJuliaFilePath { path: path.clone() });
            }
            files.push(path.clone());
            continue;
        }

        if path.is_dir() {
            walk_julia_files(path, None, exclude, &mut files)?;
            continue;
        }

        if let Some(pattern) = path.to_str().filter(|p| has_glob_meta(p)) {
            collect_glob(pattern, exclude, &mut files)?;
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
fn collect_glob(
    pattern: &str,
    exclude: &ExcludeFilter,
    files: &mut Vec<PathBuf>,
) -> Result<(), FileDiscoveryError> {
    let matcher = GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|err| FileDiscoveryError::BadGlob {
            pattern: pattern.to_string(),
            message: err.to_string(),
        })?
        .compile_matcher();

    let base = glob_base(pattern);
    walk_julia_files(&base, Some(&matcher), exclude, files)
}

/// Walk `root` with `.gitignore` semantics, pushing every `.jl` file that also
/// satisfies `matcher` (when one is given) into `files`.
fn walk_julia_files(
    root: &Path,
    matcher: Option<&globset::GlobMatcher>,
    exclude: &ExcludeFilter,
    files: &mut Vec<PathBuf>,
) -> Result<(), FileDiscoveryError> {
    let mut builder = WalkBuilder::new(root);
    builder.standard_filters(true);
    builder.hidden(false);
    // Prune excluded entries during the walk so a matched directory (e.g.
    // `deps/`) is never descended into, matching gitignore semantics. The
    // filter is cloned into the `'static` closure.
    let filter = exclude.clone();
    builder.filter_entry(move |entry| {
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        !filter.is_excluded(entry.path(), is_dir)
    });
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
        let err =
            collect_julia_files(&[root.join("readme.md")], &ExcludeFilter::none()).unwrap_err();
        assert!(matches!(err, FileDiscoveryError::NonJuliaFilePath { .. }));
    }

    #[test]
    fn directory_is_walked_recursively() {
        let root = scratch_tree();
        let files =
            collect_julia_files(std::slice::from_ref(&root), &ExcludeFilter::none()).unwrap();
        assert_eq!(names(&files), vec!["a.jl", "b.jl", "c.jl"]);
    }

    #[test]
    fn recursive_glob_matches_nested_files() {
        let root = scratch_tree();
        let pattern = root.join("**/*.jl");
        let files = collect_julia_files(&[pattern], &ExcludeFilter::none()).unwrap();
        assert_eq!(names(&files), vec!["a.jl", "b.jl", "c.jl"]);
    }

    #[test]
    fn single_star_does_not_cross_directories() {
        let root = scratch_tree();
        let pattern = root.join("src/*.jl");
        let files = collect_julia_files(&[pattern], &ExcludeFilter::none()).unwrap();
        assert_eq!(names(&files), vec!["b.jl"]);
    }

    #[test]
    fn missing_non_glob_path_errors() {
        let err = collect_julia_files(
            &[PathBuf::from("does/not/exist.jl")],
            &ExcludeFilter::none(),
        )
        .unwrap_err();
        assert!(matches!(err, FileDiscoveryError::Walk { .. }));
    }

    fn filter(root: &Path, patterns: &[&str]) -> ExcludeFilter {
        let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
        ExcludeFilter::new(root, &patterns).unwrap()
    }

    #[test]
    fn exclude_prunes_directory_walk() {
        let root = scratch_tree();
        let files =
            collect_julia_files(std::slice::from_ref(&root), &filter(&root, &["src/"])).unwrap();
        assert_eq!(names(&files), vec!["a.jl"]);
    }

    #[test]
    fn exclude_prunes_glob_walk() {
        let root = scratch_tree();
        let pattern = root.join("**/*.jl");
        let files = collect_julia_files(&[pattern], &filter(&root, &["inner/"])).unwrap();
        assert_eq!(names(&files), vec!["a.jl", "b.jl"]);
    }

    #[test]
    fn explicit_file_is_not_excluded_without_force() {
        let root = scratch_tree();
        let excluded = root.join("src/b.jl");
        let files = collect_julia_files(std::slice::from_ref(&excluded), &filter(&root, &["src/"]))
            .unwrap();
        assert_eq!(files, vec![excluded]);
    }

    #[test]
    fn force_exclude_skips_explicitly_named_file() {
        let root = scratch_tree();
        let keep = root.join("a.jl");
        let excluded = root.join("src/b.jl");
        let filter = filter(&root, &["src/"]).with_force_exclude(true);
        let files = collect_julia_files(&[excluded, keep.clone()], &filter).unwrap();
        assert_eq!(files, vec![keep]);
    }

    #[test]
    fn force_exclude_may_leave_no_files() {
        let root = scratch_tree();
        let excluded = root.join("src/b.jl");
        // Every explicit input is excluded: an empty result, not an error.
        let filter = filter(&root, &["src/"]).with_force_exclude(true);
        let files = collect_julia_files(&[excluded], &filter).unwrap();
        assert_eq!(files, Vec::<PathBuf>::new());
    }

    #[test]
    fn force_exclude_matches_parent_directory_pattern() {
        let root = scratch_tree();
        let nested = root.join("src/inner/c.jl");
        // The walk prunes `src/` before its contents are seen; an explicit
        // file must be tested against its ancestors to match the same pattern.
        let filter = filter(&root, &["src/"]).with_force_exclude(true);
        let files = collect_julia_files(&[nested], &filter).unwrap();
        assert_eq!(files, Vec::<PathBuf>::new());
    }

    #[test]
    fn force_exclude_skips_excluded_non_julia_file() {
        let root = scratch_tree();
        let readme = root.join("readme.md");
        // The force check runs before the extension check, so an excluded
        // non-Julia file (as pre-commit may stage) is skipped, not a hard
        // error.
        let filter = filter(&root, &["*.md"]).with_force_exclude(true);
        let files = collect_julia_files(&[readme], &filter).unwrap();
        assert_eq!(files, Vec::<PathBuf>::new());
    }

    #[test]
    fn force_exclude_ignores_paths_outside_matcher_root() {
        let root = scratch_tree();
        let other = scratch_tree();
        let outside = other.join("a.jl");
        // An absolute path outside the matcher root cannot match root-relative
        // patterns; it must be processed, not skipped (and must not panic).
        let filter = filter(&root, &["a.jl"]).with_force_exclude(true);
        let files = collect_julia_files(std::slice::from_ref(&outside), &filter).unwrap();
        assert_eq!(files, vec![outside]);
    }

    #[test]
    fn invalid_exclude_pattern_is_reported() {
        // A trailing backslash escapes nothing, which the gitignore parser
        // rejects (unlike e.g. an unclosed `[`, which it treats literally).
        let err = ExcludeFilter::new(Path::new("."), &["\\".to_string()]).unwrap_err();
        assert_eq!(err.pattern, "\\");
    }
}
