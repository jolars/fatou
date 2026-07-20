//! Document links (`textDocument/documentLink`): every static
//! `include("path")` string becomes a clickable link to the included file.
//!
//! Only statically resolvable includes link — the same test as the include
//! graph's [`include_edges`](crate::project::include_edges): a bare `include`
//! callee with a sole plain string-literal argument. The link covers just the
//! path text (inside the quotes) and targets the literal resolved against the
//! including file's directory, lexically normalized so `../` spellings
//! collapse. Purely lexical, no I/O: a link to a missing file is still emitted
//! (the include graph already diagnoses unresolved includes on the same call).

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{DocumentLink, Range};
use rowan::ast::AstNode;

use crate::ast::CallExpr;
use crate::incremental::{Analysis, normalize_path};
use crate::parser::parse;
use crate::project::{include_literal, resolve_target};
use crate::syntax::SyntaxNode;
use crate::text::{LineIndex, PositionEncoding};

use super::uri;

/// The document links for `text`, re-parsing it. Pure and unit-testable.
/// `base_dir` is the document's directory (`path.parent()`); a relative
/// include with no known `base_dir` yields no link.
///
/// Best-effort, with no clean-parse gate: links in the intact parts of a
/// broken buffer are still useful while the user types.
pub fn compute_document_links(
    text: &str,
    base_dir: Option<&Path>,
    encoding: PositionEncoding,
) -> Vec<DocumentLink> {
    let root = parse(text).cst;
    links_for_tree(&root, text, base_dir, encoding)
}

/// Compute document links off the snapshot's cached parse when the db's
/// tracked buffer for `path` still matches `text`; otherwise re-parse. A write
/// racing the read trips `salsa::Cancelled`, which also falls back to a fresh
/// parse. Mirrors [`folding_ranges_via_db`](super::folding::folding_ranges_via_db).
pub(crate) fn document_links_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    encoding: PositionEncoding,
) -> Vec<DocumentLink> {
    // The synthetic fallback path for non-`file` URIs ("untitled.jl") has an
    // empty parent, which must not anchor relative includes.
    let base_dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        Some(links_for_tree(&root, text, base_dir, encoding))
    }));
    match cached {
        Ok(Some(links)) => links,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_document_links(text, base_dir, encoding),
    }
}

/// Shared entry point for the fresh-parse and cached-tree paths: `root` must be
/// the parse tree of exactly `text`.
fn links_for_tree(
    root: &SyntaxNode,
    text: &str,
    base_dir: Option<&Path>,
    encoding: PositionEncoding,
) -> Vec<DocumentLink> {
    let line_index = LineIndex::new(text);
    root.descendants()
        .filter_map(CallExpr::cast)
        .filter_map(|call| {
            let literal = include_literal(&call)?;
            let raw: String = literal
                .content_tokens()
                .map(|token| token.text().to_string())
                .collect();
            let target = resolve_target(&raw, base_dir)?;
            let target = uri::from_path(&normalize_path(&target))?;
            // The link covers the path text between the quotes; an empty
            // string (`include("")`) has no content tokens and no link.
            let first = literal.content_tokens().next()?;
            let last = literal.content_tokens().last()?;
            let range = Range::new(
                line_index.byte_to_position(first.text_range().start().into(), encoding),
                line_index.byte_to_position(last.text_range().end().into(), encoding),
            );
            Some(DocumentLink {
                range,
                target: Some(target),
                tooltip: None,
                data: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    use lsp_types::Position;

    fn links(text: &str, base_dir: Option<&str>) -> Vec<DocumentLink> {
        compute_document_links(text, base_dir.map(Path::new), PositionEncoding::Utf16)
    }

    fn target(link: &DocumentLink) -> &str {
        link.target.as_ref().expect("link target").as_str()
    }

    /// A platform-native absolute path. Unix-style `/work` is *not* absolute on
    /// Windows (`is_absolute()` needs a drive letter, and `std::path::absolute`
    /// grafts the CWD's drive onto driveless paths), so prefix one there.
    /// Forward slashes, so the result can be embedded in Julia source literals.
    fn abs(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_string()
        }
    }

    /// The `file:` URI [`from_path`](uri::from_path) yields for [`abs`]`(path)`.
    fn file_uri(path: &str) -> String {
        if cfg!(windows) {
            format!("file:///C:{path}")
        } else {
            format!("file://{path}")
        }
    }

    #[test]
    fn static_include_links_and_dynamic_forms_do_not() {
        let text = concat!(
            "include(\"sub/a.jl\")\n",      // static → links
            "include(path)\n",              // dynamic callee argument
            "include(\"b$(dir).jl\")\n",    // interpolated
            "include(raw\"c.jl\")\n",       // prefixed
            "include(mapexpr, \"d.jl\")\n", // two-argument
            "M.include(\"e.jl\")\n",        // qualified callee
        );
        let links = links(text, Some(&abs("/work")));
        assert_eq!(links.len(), 1, "only the static include links");
        assert_eq!(target(&links[0]), file_uri("/work/sub/a.jl"));
        // The range covers exactly `sub/a.jl`, inside the quotes.
        assert_eq!(
            links[0].range,
            Range::new(Position::new(0, 9), Position::new(0, 17)),
        );
    }

    #[test]
    fn relative_paths_normalize_and_absolute_paths_ignore_base() {
        let text = format!(
            "include(\"../lib/b.jl\")\ninclude(\"{}\")\n",
            abs("/abs/c.jl")
        );
        let links = links(&text, Some(&abs("/work/src")));
        let targets: Vec<_> = links.iter().map(target).collect();
        assert_eq!(targets, [file_uri("/work/lib/b.jl"), file_uri("/abs/c.jl")]);
    }

    #[test]
    fn relative_include_without_a_base_dir_has_no_link() {
        let text = format!("include(\"a.jl\")\ninclude(\"{}\")\n", abs("/abs/c.jl"));
        let links = links(&text, None);
        let targets: Vec<_> = links.iter().map(target).collect();
        assert_eq!(targets, [file_uri("/abs/c.jl")]);
    }

    #[test]
    fn empty_path_has_no_link() {
        assert_eq!(links("include(\"\")\n", Some("/work")), []);
    }

    #[test]
    fn positions_count_units_of_the_negotiated_encoding() {
        // `α` is two UTF-8 bytes but one UTF-16 unit, shifting the columns.
        let text = "s = \"α\"; include(\"a.jl\")\n";
        let utf16 = compute_document_links(text, Some(Path::new("/work")), PositionEncoding::Utf16);
        let utf8 = compute_document_links(text, Some(Path::new("/work")), PositionEncoding::Utf8);
        assert_eq!(
            utf16[0].range,
            Range::new(Position::new(0, 18), Position::new(0, 22)),
        );
        assert_eq!(
            utf8[0].range,
            Range::new(Position::new(0, 19), Position::new(0, 23)),
        );
    }

    /// The cached-tree path matches the re-parse path when the db's tracked
    /// buffer is the live text, and falls back (still correctly) when the db
    /// lags the buffer or has never seen the path.
    #[test]
    fn links_via_db_match_compute_and_fall_back() {
        let path = Path::new("/work/a.jl");
        let buffer = "include(\"sub/b.jl\")\n";
        let expected =
            compute_document_links(buffer, Some(Path::new("/work")), PositionEncoding::Utf16);
        assert_eq!(expected.len(), 1, "fixture must yield a link");

        // Cache hit: tracked text == buffer → links off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, buffer.to_string());
        assert_eq!(
            document_links_via_db(&db.snapshot(), path, buffer, PositionEncoding::Utf16),
            expected,
            "cached-tree links must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            document_links_via_db(&stale.snapshot(), path, buffer, PositionEncoding::Utf16),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            document_links_via_db(&empty.snapshot(), path, buffer, PositionEncoding::Utf16),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
