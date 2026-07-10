//! References (`textDocument/references`) and document highlight
//! (`textDocument/documentHighlight`).
//!
//! Both answer the same question — "where is the binding under the cursor used?"
//! — off one building block, [`SemanticModel::occurrences`], which yields the
//! definition site plus every resolved identifier with its [`Access`]. The
//! symbol at the cursor is classified as in [`definition`](super::definition):
//! an occurrence that resolves to a binding, or a name sitting on its own
//! definition site. A qualified or free read (a Base/Core or `using`'d library
//! symbol) has no intra-file binding, so it yields nothing here; cross-file
//! references are a Phase 5 item.
//!
//! References returns [`Location`]s in the current document (honoring the
//! request's `include_declaration`); document highlight returns
//! [`DocumentHighlight`]s tagged read/write from the occurrence's [`Access`].

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{DocumentHighlight, DocumentHighlightKind, Location, Position, Range, Uri};
use rowan::{TextRange, TextSize};

use crate::incremental::Analysis;
use crate::parser::parse;
use crate::semantic::{Access, BindingId, SemanticModel};
use crate::text::{LineIndex, PositionEncoding};

/// The references to the symbol at `position` in `text`, re-parsing it. Pure and
/// unit-testable; `uri` is the requesting document, since results point back at
/// it. `include_declaration` keeps the definition site in the list.
pub fn compute_references(
    uri: &Uri,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let model = SemanticModel::build(&parse(text).cst);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    references_for(
        &model,
        uri,
        &line_index,
        offset,
        encoding,
        include_declaration,
    )
}

/// The document highlights for the symbol at `position` in `text`, re-parsing
/// it. Pure and unit-testable.
pub fn compute_document_highlights(
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<Vec<DocumentHighlight>> {
    let model = SemanticModel::build(&parse(text).cst);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    highlights_for(&model, &line_index, offset, encoding)
}

/// Compute references off the snapshot's cached parse when the db's tracked
/// buffer for `path` still matches `text`; otherwise re-parse. Mirrors
/// [`definition_via_db`](super::definition::definition_via_db).
pub(crate) fn references_via_db(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            return None;
        }
        let model = snapshot.semantic_model(file);
        Some(references_for(
            model,
            uri,
            &line_index,
            offset,
            encoding,
            include_declaration,
        ))
    }));
    match cached {
        Ok(Some(result)) => result,
        Ok(None) | Err(_) => compute_references(uri, text, position, encoding, include_declaration),
    }
}

/// Compute document highlights off the snapshot's cached parse when the tracked
/// buffer still matches `text`; otherwise re-parse. Mirrors
/// [`references_via_db`].
pub(crate) fn document_highlights_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<Vec<DocumentHighlight>> {
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            return None;
        }
        let model = snapshot.semantic_model(file);
        Some(highlights_for(model, &line_index, offset, encoding))
    }));
    match cached {
        Ok(Some(result)) => result,
        Ok(None) | Err(_) => compute_document_highlights(text, position, encoding),
    }
}

/// The binding the cursor at `offset` refers to: an occurrence that resolves to
/// a binding, or a name sitting on its own definition site. A free or qualified
/// read has no intra-file binding, so it yields `None`. Shared by both requests.
fn binding_at_cursor(model: &SemanticModel, offset: TextSize) -> Option<BindingId> {
    if let Some(ident) = model.ident_at(offset) {
        return ident.binding;
    }
    model.binding_at(offset)
}

fn references_for(
    model: &SemanticModel,
    uri: &Uri,
    line_index: &LineIndex,
    offset: TextSize,
    encoding: PositionEncoding,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let binding = binding_at_cursor(model, offset)?;
    let mut locations: Vec<Location> = model
        .occurrences(binding)
        .filter(|o| include_declaration || !o.is_def)
        .map(|o| Location {
            uri: uri.clone(),
            range: to_range(o.range, line_index, encoding),
        })
        .collect();
    locations.sort_by_key(|l| (l.range.start.line, l.range.start.character));
    Some(locations)
}

fn highlights_for(
    model: &SemanticModel,
    line_index: &LineIndex,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<Vec<DocumentHighlight>> {
    let binding = binding_at_cursor(model, offset)?;
    let mut highlights: Vec<DocumentHighlight> = model
        .occurrences(binding)
        .map(|o| DocumentHighlight {
            range: to_range(o.range, line_index, encoding),
            kind: Some(highlight_kind(o.access)),
        })
        .collect();
    highlights.sort_by_key(|h| (h.range.start.line, h.range.start.character));
    Some(highlights)
}

/// Map an occurrence's [`Access`] to an LSP highlight kind. An augmented
/// assignment (`x += 1`) both reads and writes; LSP has no combined kind, so it
/// reports as a write, like rust-analyzer.
fn highlight_kind(access: Access) -> DocumentHighlightKind {
    match access {
        Access::Read => DocumentHighlightKind::READ,
        Access::Write | Access::ReadWrite => DocumentHighlightKind::WRITE,
    }
}

fn to_range(range: TextRange, line_index: &LineIndex, encoding: PositionEncoding) -> Range {
    Range {
        start: line_index.byte_to_position(range.start().into(), encoding),
        end: line_index.byte_to_position(range.end().into(), encoding),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn doc_uri() -> Uri {
        Uri::from_str("file:///work/s.jl").unwrap()
    }

    /// The position of the `|` marker in `marked` (stripped before parsing).
    fn cursor(marked: &str) -> (String, Position) {
        let offset = marked.find('|').expect("a cursor marker");
        let src = marked.replacen('|', "", 1);
        let line_index = LineIndex::new(&src);
        let position = line_index.byte_to_position(offset, PositionEncoding::Utf16);
        (src, position)
    }

    fn refs(marked: &str, include_declaration: bool) -> Option<Vec<Location>> {
        let (src, position) = cursor(marked);
        compute_references(
            &doc_uri(),
            &src,
            position,
            PositionEncoding::Utf16,
            include_declaration,
        )
    }

    fn highlights(marked: &str) -> Option<Vec<DocumentHighlight>> {
        let (src, position) = cursor(marked);
        compute_document_highlights(&src, position, PositionEncoding::Utf16)
    }

    #[test]
    fn local_references_include_the_definition() {
        // Cursor on a use of `x`; with the declaration, all three sites appear.
        let locs = refs("function f()\n    x = 1\n    x|\n    x + 1\nend", true).unwrap();
        let ranges: Vec<_> = locs
            .iter()
            .map(|l| (l.range.start.line, l.range.start.character))
            .collect();
        assert_eq!(ranges, vec![(1, 4), (2, 4), (3, 4)]);
    }

    #[test]
    fn references_can_exclude_the_declaration() {
        let locs = refs("function f()\n    x = 1\n    x|\n    x + 1\nend", false).unwrap();
        let ranges: Vec<_> = locs
            .iter()
            .map(|l| (l.range.start.line, l.range.start.character))
            .collect();
        // The `x = 1` definition on line 1 is dropped.
        assert_eq!(ranges, vec![(2, 4), (3, 4)]);
    }

    #[test]
    fn references_from_the_definition_site() {
        // Cursor on the defining `x` itself still finds every use.
        let locs = refs("function f()\n    x| = 1\n    x\nend", true).unwrap();
        assert_eq!(locs.len(), 2);
    }

    #[test]
    fn highlight_kinds_distinguish_read_from_write() {
        let hs = highlights("function f()\n    x = 1\n    x| = 2\n    x\nend").unwrap();
        let kinds: Vec<_> = hs
            .iter()
            .map(|h| (h.range.start.line, h.kind.unwrap()))
            .collect();
        // The two assignments write; the trailing use reads.
        assert_eq!(
            kinds,
            vec![
                (1, DocumentHighlightKind::WRITE),
                (2, DocumentHighlightKind::WRITE),
                (3, DocumentHighlightKind::READ),
            ]
        );
    }

    #[test]
    fn augmented_assignment_highlights_as_a_write() {
        let hs = highlights("function f()\n    x = 1\n    x| += 1\nend").unwrap();
        let write = hs
            .iter()
            .find(|h| h.range.start.line == 2)
            .expect("the `x += 1` occurrence");
        assert_eq!(write.kind, Some(DocumentHighlightKind::WRITE));
    }

    #[test]
    fn parameter_references_reach_every_use() {
        let locs = refs("function f(abc)\n    abc| + abc\nend", true).unwrap();
        // The parameter plus its two uses on line 1.
        assert_eq!(locs.len(), 3);
    }

    #[test]
    fn free_read_has_no_intra_file_references() {
        // `println` binds nowhere in this file, so there is nothing to report.
        assert!(refs("println|(1)", true).is_none());
        assert!(highlights("println|(1)").is_none());
    }
}
