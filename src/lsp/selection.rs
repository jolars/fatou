//! Selection ranges (`textDocument/selectionRange`): expand the selection
//! along CST ancestors, powering editors' "expand selection". A pure CST
//! walk — the chain for a position is the token under the cursor followed by
//! its ancestor nodes, so no semantic model is needed.
//!
//! Conventions: the innermost step is the token itself (skipped for
//! whitespace, where selecting the run is not a useful step; kept for
//! comments), ancestors sharing the extent of the step below them are dropped
//! (wrapper nodes often span exactly their only child), and a cursor on the
//! boundary between two tokens starts from the more interesting side
//! (identifiers beat other tokens beat comments beat whitespace, ties go
//! right).

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Position, Range, SelectionRange};
use rowan::{TextRange, TextSize, TokenAtOffset};

use crate::incremental::Analysis;
use crate::parser::parse;
use crate::syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use crate::text::{LineIndex, PositionEncoding};

/// The selection-range chain for each of `positions` in `text`, re-parsing it.
/// Pure and unit-testable; single-file by nature.
///
/// Best-effort, with no clean-parse gate: expansion along the intact parts of
/// a broken buffer is still useful while the user types.
pub fn compute_selection_ranges(
    text: &str,
    positions: &[Position],
    encoding: PositionEncoding,
) -> Vec<SelectionRange> {
    let root = parse(text).cst;
    selections_for_tree(&root, text, positions, encoding)
}

/// Compute selection ranges off the snapshot's cached parse when the db's
/// tracked buffer for `path` still matches `text`; otherwise re-parse. A write
/// racing the read trips `salsa::Cancelled`, which also falls back to a fresh
/// parse. Mirrors [`folding_ranges_via_db`](super::folding::folding_ranges_via_db).
pub(crate) fn selection_ranges_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    positions: &[Position],
    encoding: PositionEncoding,
) -> Vec<SelectionRange> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        let root = snapshot.parsed_tree(file);
        Some(selections_for_tree(&root, text, positions, encoding))
    }));
    match cached {
        Ok(Some(ranges)) => ranges,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => compute_selection_ranges(text, positions, encoding),
    }
}

/// Shared entry point for the fresh-parse and cached-tree paths: `root` must be
/// the parse tree of exactly `text`.
fn selections_for_tree(
    root: &SyntaxNode,
    text: &str,
    positions: &[Position],
    encoding: PositionEncoding,
) -> Vec<SelectionRange> {
    let line_index = LineIndex::new(text);
    positions
        .iter()
        .map(|&position| {
            let offset = line_index.position_to_byte(position, encoding);
            link_chain(&range_chain(root, offset), &line_index, encoding)
        })
        .collect()
}

/// The widening chain of byte ranges at `offset`, innermost first: the token
/// under the cursor, then its ancestor nodes up to the root, with steps that
/// do not grow the range dropped. Never empty — an empty tree still
/// contributes the root's (empty) range.
fn range_chain(root: &SyntaxNode, offset: usize) -> Vec<TextRange> {
    let token = match root.token_at_offset(TextSize::new(offset as u32)) {
        TokenAtOffset::None => None,
        TokenAtOffset::Single(token) => Some(token),
        TokenAtOffset::Between(left, right) => Some(pick_boundary_token(left, right)),
    };
    let Some(token) = token else {
        return vec![root.text_range()];
    };
    let mut chain = Vec::new();
    if !matches!(token.kind(), SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE) {
        chain.push(token.text_range());
    }
    for node in token.parent_ancestors() {
        let range = node.text_range();
        if chain.last() != Some(&range) {
            chain.push(range);
        }
    }
    chain
}

/// The token to expand from when the cursor sits on the boundary between two:
/// prefer the more selectable kind, and the right token on a tie (rowan's own
/// right-bias convention).
fn pick_boundary_token(left: SyntaxToken, right: SyntaxToken) -> SyntaxToken {
    fn priority(kind: SyntaxKind) -> u8 {
        match kind {
            SyntaxKind::WHITESPACE | SyntaxKind::NEWLINE => 0,
            SyntaxKind::COMMENT | SyntaxKind::BLOCK_COMMENT => 1,
            SyntaxKind::IDENT => 3,
            _ => 2,
        }
    }
    if priority(right.kind()) >= priority(left.kind()) {
        right
    } else {
        left
    }
}

/// Fold a non-empty innermost-first chain of byte ranges into the LSP's linked
/// representation, converting each to positions in the negotiated encoding.
fn link_chain(
    chain: &[TextRange],
    line_index: &LineIndex<'_>,
    encoding: PositionEncoding,
) -> SelectionRange {
    let mut linked: Option<SelectionRange> = None;
    for &range in chain.iter().rev() {
        linked = Some(SelectionRange {
            range: Range::new(
                line_index.byte_to_position(range.start().into(), encoding),
                line_index.byte_to_position(range.end().into(), encoding),
            ),
            parent: linked.map(Box::new),
        });
    }
    linked.expect("range_chain is never empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;

    /// The cached-tree path matches the re-parse path when the db's tracked
    /// buffer is the live text, and falls back (still correctly) when the db
    /// lags the buffer or has never seen the path.
    #[test]
    fn selections_via_db_match_compute_and_fall_back() {
        let path = Path::new("/work/a.jl");
        let buffer = "function f(x)\n    x + 1\nend\n";
        let positions = [Position::new(1, 4)];
        let expected = compute_selection_ranges(buffer, &positions, PositionEncoding::Utf8);
        assert_eq!(expected.len(), 1, "fixture must yield a chain");

        // Cache hit: tracked text == buffer → chains off the cached tree.
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, buffer.to_string());
        assert_eq!(
            selection_ranges_via_db(
                &db.snapshot(),
                path,
                buffer,
                &positions,
                PositionEncoding::Utf8
            ),
            expected,
            "cached-tree chains must match the re-parse path"
        );

        // Stale db (tracked text lags the buffer) → fall back to a fresh parse.
        let mut stale = IncrementalDatabase::default();
        stale.upsert_file(path, "y = 1\n".to_string());
        assert_eq!(
            selection_ranges_via_db(
                &stale.snapshot(),
                path,
                buffer,
                &positions,
                PositionEncoding::Utf8
            ),
            expected,
            "version skew must fall back to the buffer text"
        );

        // Untracked path → fall back as well.
        let empty = IncrementalDatabase::default();
        assert_eq!(
            selection_ranges_via_db(
                &empty.snapshot(),
                path,
                buffer,
                &positions,
                PositionEncoding::Utf8
            ),
            expected,
            "untracked path must fall back to the buffer text"
        );
    }
}
