//! Discovery of `.jl` source files from CLI path arguments.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiscoveryError {
    NonJuliaFilePath { path: PathBuf },
    Walk { path: PathBuf, message: String },
}

impl std::fmt::Display for FileDiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileDiscoveryError::NonJuliaFilePath { path } => {
                write!(f, "not a Julia (.jl) file: {}", path.display())
            }
            FileDiscoveryError::Walk { path, message } => {
                write!(f, "failed to walk {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for FileDiscoveryError {}

/// Collect `.jl` files from `paths`. A file path must end in `.jl`; a directory
/// is walked with `.gitignore` semantics. Results are sorted and deduplicated.
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
            let mut builder = WalkBuilder::new(path);
            builder.standard_filters(true);
            builder.hidden(false);
            for entry in builder.build() {
                match entry {
                    Ok(entry) => {
                        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                            continue;
                        }
                        let entry_path = entry.path().to_path_buf();
                        if is_julia_file(&entry_path) {
                            files.push(entry_path);
                        }
                    }
                    Err(err) => {
                        return Err(FileDiscoveryError::Walk {
                            path: path.clone(),
                            message: err.to_string(),
                        });
                    }
                }
            }
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

fn is_julia_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jl"))
}
