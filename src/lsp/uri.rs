//! `file:` URI ↔ filesystem path conversion.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use lsp_types::Uri;

/// Convert a `file:` URI to a filesystem path, or `None` if it isn't a file
/// URI or has no scheme (e.g. an editor's `untitled:` buffer).
pub(crate) fn to_path(uri: &Uri) -> Option<PathBuf> {
    let scheme = uri.scheme()?;
    if !scheme.as_str().eq_ignore_ascii_case("file") {
        return None;
    }
    let decoded = uri
        .path()
        .as_estr()
        .decode()
        .into_string_lossy()
        .into_owned();
    Some(from_uri_path(&decoded))
}

#[cfg(windows)]
fn from_uri_path(p: &str) -> PathBuf {
    // "/C:/Users/x" → "C:\Users\x"; without a drive letter the leading slash
    // stays, so "/work/x" maps to the rooted "\work\x" rather than a relative
    // path.
    let bytes = p.as_bytes();
    let has_drive =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    let trimmed = if has_drive { &p[1..] } else { p };
    PathBuf::from(trimmed.replace('/', "\\"))
}

#[cfg(not(windows))]
fn from_uri_path(p: &str) -> PathBuf {
    PathBuf::from(p)
}

/// The directory the synthetic paths of non-`file:` URIs live under. Rooted, so
/// `incremental::normalize_path` resolves it against the filesystem root rather
/// than the server's working directory, where it could alias a real file the
/// server would then read from disk.
const NON_FILE_ROOT: &str = "fatou-non-file-uri";

/// The filesystem path the db tracks `uri` under. A non-`file:` URI (an
/// editor's `untitled:` buffer, a notebook cell) has no path of its own, so it
/// gets a synthetic one derived from the whole URI: injective, so two untitled
/// buffers no longer share a single input — and with it a single reparse base
/// each one's edits would knock the other off.
pub(crate) fn to_path_or_synthetic(uri: &Uri) -> PathBuf {
    to_path(uri).unwrap_or_else(|| {
        use std::fmt::Write;

        // Percent-escape everything outside the unreserved set, so distinct URIs
        // stay distinct and no separator (or `%`) survives to split the name
        // into components of its own.
        let mut name = String::new();
        for &byte in uri.as_str().as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => name.push(byte as char),
                _ => {
                    let _ = write!(name, "%{byte:02X}");
                }
            }
        }
        // Keeps extension-based handling working, and (with `.` escaped above)
        // leaves the name unable to spell `.` or `..`.
        name.push_str(".jl");
        synthetic_dir().join(name)
    })
}

/// The one directory [`to_path_or_synthetic`] mints into: [`NON_FILE_ROOT`]
/// directly under the filesystem root.
fn synthetic_dir() -> PathBuf {
    PathBuf::from(std::path::MAIN_SEPARATOR_STR).join(NON_FILE_ROOT)
}

/// Whether `path` is one of the synthetic stand-ins [`to_path_or_synthetic`]
/// mints, rather than a real filesystem path. Such a path only identifies a
/// buffer: nothing exists there to read, and no directory of the workspace is
/// implied, so relative paths must not be resolved against it.
///
/// The whole shape is checked, not just the directory name: a real
/// `/work/fatou-non-file-uri/x.jl` under someone's workspace is a file like any
/// other, and only the rooted single-component form is ever minted here.
pub(crate) fn is_synthetic(path: &Path) -> bool {
    path.parent() == Some(synthetic_dir().as_path())
}

/// The directory a relative path *inside* the document at `path` resolves
/// against: its parent. `None` when there is no such directory — a
/// [synthetic](is_synthetic) stand-in for a non-`file` URI names no real one,
/// and a parentless path names none either.
///
/// One definition, because two features ask it: a Julia document's `include`
/// paths and a manifest's `path` entries.
pub(crate) fn anchor_dir(path: &Path) -> Option<&Path> {
    if is_synthetic(path) {
        return None;
    }
    path.parent().filter(|dir| !dir.as_os_str().is_empty())
}

/// Build a `file:` URI for the absolute filesystem `path`, percent-encoding
/// characters outside the unreserved set. The inverse of [`to_path`]; used to
/// point a go-to-definition [`Location`](lsp_types::Location) at a depot source
/// file. `None` if the path is not valid UTF-8.
pub(crate) fn from_path(path: &Path) -> Option<Uri> {
    let text = path.to_str()?;
    let mut encoded = String::from("file://");
    // On Windows the path is drive-rooted (`C:\...`); a `file:` URI needs a
    // leading slash and forward slashes.
    #[cfg(windows)]
    let text = {
        encoded.push('/');
        text.replace('\\', "/")
    };
    #[cfg(windows)]
    let text = text.as_str();
    for &byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    Uri::from_str(&encoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    #[cfg(not(windows))]
    fn file_uri_decodes_to_path() {
        let uri = Uri::from_str("file:///work/some%20dir/a.jl").unwrap();
        assert_eq!(to_path(&uri), Some(PathBuf::from("/work/some dir/a.jl")));
    }

    #[test]
    #[cfg(windows)]
    fn drive_letter_uri_decodes_to_path() {
        let uri = Uri::from_str("file:///C:/work/a%20b.jl").unwrap();
        assert_eq!(to_path(&uri), Some(PathBuf::from("C:\\work\\a b.jl")));
        // Percent-encoded drive colons (as sent by VS Code) decode the same.
        let uri = Uri::from_str("file:///c%3A/work/a.jl").unwrap();
        assert_eq!(to_path(&uri), Some(PathBuf::from("c:\\work\\a.jl")));
    }

    #[test]
    #[cfg(windows)]
    fn driveless_uri_stays_rooted() {
        let uri = Uri::from_str("file:///work/a.jl").unwrap();
        assert_eq!(to_path(&uri), Some(PathBuf::from("\\work\\a.jl")));
    }

    #[test]
    fn non_file_uri_has_no_path() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        assert_eq!(to_path(&uri), None);
    }

    #[test]
    fn non_file_uris_map_to_distinct_rooted_paths() {
        use crate::incremental::normalize_path;

        let path = |text: &str| to_path_or_synthetic(&Uri::from_str(text).unwrap());
        let first = path("untitled:Untitled-1");

        assert_ne!(first, path("untitled:Untitled-2"));
        assert_ne!(first, path("vscode-notebook-cell:Untitled-1"));
        assert_eq!(first, path("untitled:Untitled-1"), "stable per URI");

        // One component under the reserved root: the URI's `:` and `/` are
        // escaped rather than carved into directories, and normalization can
        // neither climb out of the root nor fold the path into the working
        // directory.
        assert!(first.has_root(), "{first:?} should be rooted");
        assert_eq!(first.components().count(), 3, "root, dir, file: {first:?}");
        assert_eq!(first.extension().and_then(|e| e.to_str()), Some("jl"));
        let normalized = normalize_path(&first);
        assert!(
            normalized.ends_with(
                first
                    .strip_prefix(first.components().next().unwrap())
                    .unwrap()
            ),
            "normalization should keep the root's contents: {normalized:?}"
        );
        assert!(
            !normalized.starts_with(std::env::current_dir().expect("a working directory")),
            "a synthetic path must not land in the workspace: {normalized:?}"
        );

        // A `file:` URI keeps its real path.
        #[cfg(not(windows))]
        assert_eq!(path("file:///work/a.jl"), PathBuf::from("/work/a.jl"));
    }

    /// `is_synthetic` recognizes what `to_path_or_synthetic` mints and nothing
    /// else — a real workspace file that merely happens to sit in a directory
    /// of that name is a file like any other, and must keep anchoring its
    /// relative includes.
    #[test]
    fn only_the_minted_shape_counts_as_synthetic() {
        assert!(is_synthetic(&to_path_or_synthetic(
            &Uri::from_str("untitled:Untitled-1").unwrap()
        )));
        assert!(!is_synthetic(Path::new("relative")));
        assert!(!is_synthetic(
            &PathBuf::from(NON_FILE_ROOT).join("notes.jl")
        ));
        #[cfg(not(windows))]
        {
            assert!(!is_synthetic(Path::new("/work/a.jl")));
            assert!(!is_synthetic(Path::new(
                "/work/fatou-non-file-uri-notes/a.jl"
            )));
            // A real workspace directory that merely *ends* in the reserved
            // name is not the rooted single-component form, at any depth.
            assert!(!is_synthetic(Path::new("/work/fatou-non-file-uri/a.jl")));
            assert!(!is_synthetic(Path::new(
                "/work/proj/fatou-non-file-uri/a.jl"
            )));
            // Nor is a nested file under the reserved root itself.
            assert!(!is_synthetic(Path::new("/fatou-non-file-uri/sub/a.jl")));
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn path_round_trips_through_uri() {
        let path = PathBuf::from("/home/x/.julia/packages/A b/src/A b.jl");
        let uri = from_path(&path).expect("file uri");
        // A space encodes to %20, and the URI decodes back to the exact path.
        assert!(uri.as_str().contains("%20"), "space should be encoded");
        assert_eq!(to_path(&uri), Some(path));
    }
}
