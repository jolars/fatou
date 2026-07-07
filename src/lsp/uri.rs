//! `file:` URI ↔ filesystem path conversion.

use std::path::PathBuf;

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
    // "/C:/Users/x" → "C:\Users\x"
    PathBuf::from(p.strip_prefix('/').unwrap_or(p).replace('/', "\\"))
}

#[cfg(not(windows))]
fn from_uri_path(p: &str) -> PathBuf {
    PathBuf::from(p)
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
    fn non_file_uri_has_no_path() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        assert_eq!(to_path(&uri), None);
    }
}
