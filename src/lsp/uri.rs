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
    #[cfg(not(windows))]
    fn path_round_trips_through_uri() {
        let path = PathBuf::from("/home/x/.julia/packages/A b/src/A b.jl");
        let uri = from_path(&path).expect("file uri");
        // A space encodes to %20, and the URI decodes back to the exact path.
        assert!(uri.as_str().contains("%20"), "space should be encoded");
        assert_eq!(to_path(&uri), Some(path));
    }
}
