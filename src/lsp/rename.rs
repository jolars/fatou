//! Rename (`textDocument/rename`) and its validation gate
//! (`textDocument/prepareRename`).
//!
//! Rename is references-with-edits: the symbol at the cursor is classified as in
//! [`references`](super::references) — an occurrence that resolves to a binding,
//! or a name sitting on its own definition site — and every
//! [`SemanticModel::occurrences`] range is replaced with the new name. Because
//! the occurrences come from the scope-resolved model, a shadowing same-name
//! variable in a nested scope is left untouched; this is the correctness win over
//! a textual find-and-replace.
//!
//! A **workspace top-level symbol** (a global of the package under development)
//! renames across every member file: `rename_via_db` gathers its occurrences
//! from the reverse-occurrence index and returns a multi-document
//! [`WorkspaceEdit`] (see [`cross_file`]), and `prepareRename` accepts it even
//! where it is a free read (defined in a sibling file). Everything else stays
//! intra-file. A library free or qualified read (a Base/Core or `using`'d
//! symbol) has no workspace binding, so `prepareRename` reports it as not
//! renameable and `rename` yields no edit. Macro definitions rename by their
//! bare name: the occurrence ranges cover the identifier after the `@`, so the
//! sigil is preserved automatically, cross-file as well as intra-file.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{Position, PrepareRenameResponse, Range, TextEdit, Uri, WorkspaceEdit};
use rowan::{TextRange, TextSize};

use crate::incremental::Analysis;
use crate::parser::parse;
use crate::semantic::{BindingId, SemanticModel};
use crate::text::{LineIndex, PositionEncoding};

use super::cross_file;

/// The prepare-rename response for the symbol at `position` in `text`,
/// re-parsing it. Pure and unit-testable. `Some(range)` marks the identifier
/// token the client should offer to rename; `None` means the cursor is not on a
/// renameable (intra-file) binding.
pub fn compute_prepare_rename(
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<PrepareRenameResponse> {
    let model = SemanticModel::build(&parse(text).cst);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    prepare_for(&model, &line_index, offset, encoding)
}

/// The workspace edit renaming the symbol at `position` in `text` to `new_name`,
/// re-parsing it. Pure and unit-testable; `uri` is the requesting document, since
/// every edit points back at it. `Err` reports an invalid new name (the client
/// surfaces the message); `Ok(None)` is a cursor on nothing renameable.
pub fn compute_rename(
    uri: &Uri,
    text: &str,
    position: Position,
    new_name: &str,
    encoding: PositionEncoding,
) -> Result<Option<WorkspaceEdit>, String> {
    validate_new_name(new_name)?;
    let model = SemanticModel::build(&parse(text).cst);
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    Ok(rename_for(
        &model,
        uri,
        &line_index,
        offset,
        new_name,
        encoding,
    ))
}

/// Compute the prepare-rename response off the snapshot's cached parse when the
/// db's tracked buffer for `path` still matches `text`; otherwise re-parse.
/// Mirrors [`references_via_db`](super::references::references_via_db).
pub(crate) fn prepare_rename_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> Option<PrepareRenameResponse> {
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            return None;
        }
        let model = snapshot.semantic_model(file);
        // A workspace top-level symbol is renameable even when it is a free read
        // here (defined in a sibling file), where the intra-file `prepare_for`
        // would decline it. Offer the identifier under the cursor.
        if cross_file::workspace_symbol_at(snapshot, path, model, offset).is_some()
            && let Some(range) = identifier_range_at(model, offset)
        {
            return Some(Some(PrepareRenameResponse::Range(to_range(
                range,
                &line_index,
                encoding,
            ))));
        }
        Some(prepare_for(model, &line_index, offset, encoding))
    }));
    match cached {
        Ok(Some(result)) => result,
        Ok(None) | Err(_) => compute_prepare_rename(text, position, encoding),
    }
}

/// Compute the rename edit off the snapshot's cached parse when the tracked
/// buffer still matches `text`; otherwise re-parse. Mirrors
/// [`prepare_rename_via_db`].
pub(crate) fn rename_via_db(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    text: &str,
    position: Position,
    new_name: &str,
    encoding: PositionEncoding,
) -> Result<Option<WorkspaceEdit>, String> {
    validate_new_name(new_name)?;
    let line_index = LineIndex::new(text);
    let offset = TextSize::new(line_index.position_to_byte(position, encoding) as u32);
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            return None;
        }
        let model = snapshot.semantic_model(file);
        // A workspace top-level symbol renames across every member file; anything
        // else stays intra-file. A cross-file edit that touches at least this
        // file wins; an empty one (member set not seeded yet) falls through.
        if let Some((ns, name)) = cross_file::workspace_symbol_at(snapshot, path, model, offset) {
            let edit = cross_file_rename(snapshot, ns, &name, new_name, encoding);
            if edit
                .changes
                .as_ref()
                .is_some_and(|changes| !changes.is_empty())
            {
                return Some(Some(edit));
            }
        }
        Some(rename_for(
            model,
            uri,
            &line_index,
            offset,
            new_name,
            encoding,
        ))
    }));
    match cached {
        Ok(Some(result)) => Ok(result),
        Ok(None) | Err(_) => compute_rename(uri, text, position, new_name, encoding),
    }
}

/// The multi-file workspace edit renaming a workspace symbol: every occurrence
/// across the package's member files (definitions and uses) rewritten to
/// `new_name`, grouped by document. Macro occurrence ranges cover the bare name,
/// so the `@` sigil is preserved automatically, as in the intra-file path.
fn cross_file_rename(
    snapshot: &Analysis,
    namespace: crate::resolve::Namespace,
    name: &smol_str::SmolStr,
    new_name: &str,
    encoding: PositionEncoding,
) -> WorkspaceEdit {
    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for site in cross_file::gather_sites(snapshot, namespace, name, encoding) {
        changes.entry(site.uri).or_default().push(TextEdit {
            range: site.range,
            new_text: new_name.to_string(),
        });
    }
    for edits in changes.values_mut() {
        edits.sort_by_key(|e| (e.range.start.line, e.range.start.character));
        edits.dedup_by_key(|e| (e.range.start.line, e.range.start.character));
    }
    WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    }
}

/// The range of the identifier token under the cursor at `offset`: an occurrence
/// or a definition site, whether or not it binds intra-file. Used by
/// `prepareRename` to offer a workspace free read (a symbol defined in a sibling
/// file) as renameable.
fn identifier_range_at(model: &SemanticModel, offset: TextSize) -> Option<TextRange> {
    if let Some(ident) = model.ident_at(offset) {
        return Some(ident.range);
    }
    let bid = model.binding_at(offset)?;
    Some(model.binding(bid).def_range)
}

/// The binding the cursor at `offset` refers to, together with the range of the
/// identifier under the cursor: an occurrence that resolves to a binding, or a
/// name on its own definition site. A free or qualified read has no intra-file
/// binding, so it yields `None`.
fn binding_and_range_at(model: &SemanticModel, offset: TextSize) -> Option<(BindingId, TextRange)> {
    if let Some(ident) = model.ident_at(offset) {
        return ident.binding.map(|b| (b, ident.range));
    }
    let bid = model.binding_at(offset)?;
    Some((bid, model.binding(bid).def_range))
}

fn prepare_for(
    model: &SemanticModel,
    line_index: &LineIndex,
    offset: TextSize,
    encoding: PositionEncoding,
) -> Option<PrepareRenameResponse> {
    let (_, range) = binding_and_range_at(model, offset)?;
    Some(PrepareRenameResponse::Range(to_range(
        range, line_index, encoding,
    )))
}

fn rename_for(
    model: &SemanticModel,
    uri: &Uri,
    line_index: &LineIndex,
    offset: TextSize,
    new_name: &str,
    encoding: PositionEncoding,
) -> Option<WorkspaceEdit> {
    let (binding, _) = binding_and_range_at(model, offset)?;
    let mut edits: Vec<TextEdit> = model
        .occurrences(binding)
        .map(|o| TextEdit {
            range: to_range(o.range, line_index, encoding),
            new_text: new_name.to_string(),
        })
        .collect();
    edits.sort_by_key(|e| (e.range.start.line, e.range.start.character));
    // Belt and suspenders: a binding never reports the same site twice (the def
    // site is distinct from the reads), but a duplicate edit would be malformed.
    edits.dedup_by_key(|e| (e.range.start.line, e.range.start.character));
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// Accept the new name only if it is a legal Julia identifier: a leading letter
/// or underscore, then letters, digits, underscores, or a trailing `!`. This is
/// conservative (Julia also admits many Unicode symbols and `var"..."` names),
/// but it rejects the mistakes a rename should not silently apply — empty names,
/// leading digits, embedded whitespace, the `@` sigil.
fn validate_new_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let ok = match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => {
            chars.all(|c| c == '_' || c == '!' || c.is_alphanumeric())
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("`{name}` is not a valid identifier"))
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

    fn prepare(marked: &str) -> Option<PrepareRenameResponse> {
        let (src, position) = cursor(marked);
        compute_prepare_rename(&src, position, PositionEncoding::Utf16)
    }

    /// The (line, start-char, new-text) of each edit, sorted, for the rename of
    /// the symbol at the cursor to `new_name`.
    fn rename_edits(marked: &str, new_name: &str) -> Vec<(u32, u32, String)> {
        let (src, position) = cursor(marked);
        let edit = compute_rename(
            &doc_uri(),
            &src,
            position,
            new_name,
            PositionEncoding::Utf16,
        )
        .expect("a valid new name")
        .expect("a renameable symbol");
        let changes = edit.changes.expect("intra-file changes");
        let edits = changes.get(&doc_uri()).expect("edits for the document");
        edits
            .iter()
            .map(|e| {
                (
                    e.range.start.line,
                    e.range.start.character,
                    e.new_text.clone(),
                )
            })
            .collect()
    }

    #[test]
    fn renames_every_occurrence_including_the_definition() {
        let edits = rename_edits("function f()\n    x = 1\n    x| + x\nend", "y");
        assert_eq!(
            edits,
            vec![
                (1, 4, "y".to_string()),
                (2, 4, "y".to_string()),
                (2, 8, "y".to_string()),
            ]
        );
    }

    #[test]
    fn renames_from_the_definition_site() {
        // The cursor sits on the defining `x` itself, not a use.
        let edits = rename_edits("function f()\n    x| = 1\n    x\nend", "y");
        assert_eq!(
            edits,
            vec![(1, 4, "y".to_string()), (2, 4, "y".to_string())]
        );
    }

    #[test]
    fn renames_a_parameter() {
        let edits = rename_edits("function f(abc)\n    abc| + abc\nend", "z");
        assert_eq!(
            edits,
            vec![
                (0, 11, "z".to_string()),
                (1, 4, "z".to_string()),
                (1, 10, "z".to_string()),
            ]
        );
    }

    #[test]
    fn a_shadowing_local_binding_is_left_untouched() {
        // The global `x` and the function-local `x` are distinct bindings: an
        // assignment in the hard function scope creates a fresh local rather than
        // capturing the global. Renaming the global must not touch the local.
        // (A nested-function `x = ...` would instead *capture* the enclosing
        // local, so it is deliberately not the shadowing case.)
        let src = "x| = 1\nfunction f()\n    x = 2\n    x\nend\nx";
        let edits = rename_edits(src, "y");
        // Only the global definition (line 0) and its trailing use (line 5).
        assert_eq!(
            edits,
            vec![(0, 0, "y".to_string()), (5, 0, "y".to_string())]
        );
    }

    #[test]
    fn renames_a_macro_by_its_bare_name_preserving_the_sigil() {
        // The occurrence ranges cover the name after `@`, so the sigil stays and
        // the bare `greet` is what gets replaced at each site.
        let src = "macro greet|(x)\n    x\nend\n@greet 1";
        let edits = rename_edits(src, "hello");
        assert_eq!(
            edits,
            vec![(0, 6, "hello".to_string()), (3, 1, "hello".to_string())]
        );
    }

    #[test]
    fn a_free_read_is_not_renameable() {
        // `println` binds nowhere in this file.
        assert!(prepare("println|(1)").is_none());
        let (src, position) = cursor("println|(1)");
        let edit = compute_rename(
            &doc_uri(),
            &src,
            position,
            "output",
            PositionEncoding::Utf16,
        )
        .unwrap();
        assert!(edit.is_none());
    }

    #[test]
    fn prepare_rename_reports_the_identifier_range() {
        let response = prepare("function f()\n    xyz = 1\n    xy|z\nend").unwrap();
        match response {
            PrepareRenameResponse::Range(range) => {
                assert_eq!(range.start, Position::new(2, 4));
                assert_eq!(range.end, Position::new(2, 7));
            }
            other => panic!("expected a plain range, got {other:?}"),
        }
    }

    #[test]
    fn an_invalid_new_name_is_rejected() {
        let (src, position) = cursor("function f()\n    x| = 1\nend");
        for bad in ["", "1x", "a b", "@m", "x-y"] {
            let err = compute_rename(&doc_uri(), &src, position, bad, PositionEncoding::Utf16)
                .expect_err(&format!("`{bad}` should be rejected"));
            assert!(err.contains(bad) || bad.is_empty());
        }
        // A valid identifier with a trailing `!` and Unicode is accepted.
        assert!(validate_new_name("mutate!").is_ok());
        assert!(validate_new_name("δx").is_ok());
    }

    /// Cross-file rename rewrites a workspace symbol everywhere: the definition
    /// and every use, across all member files, in one multi-document edit.
    #[test]
    fn cross_file_rename_rewrites_every_member_file() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};
        use crate::text::PositionEncoding::Utf16;

        let a_text = "greet(a) = a\ngreet(1)\n";
        let b_text = "callit() = greet(2)\n";
        let (db, _) = workspace_db(&["greet"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();

        let a_path = member_path("a.jl");
        let a_uri = crate::lsp::uri::from_path(&a_path).unwrap();
        let b_uri = crate::lsp::uri::from_path(&member_path("b.jl")).unwrap();

        // Rename from the definition site in a.jl.
        let edit = rename_via_db(
            &snapshot,
            &a_uri,
            &a_path,
            a_text,
            Position::new(0, 0),
            "hello",
            Utf16,
        )
        .expect("a valid new name")
        .expect("greet is a renameable workspace symbol");
        let changes = edit.changes.expect("multi-file changes");

        // a.jl: the definition and the call; b.jl: the one call.
        let a_edits = changes.get(&a_uri).expect("edits in a.jl");
        assert_eq!(a_edits.len(), 2);
        assert!(a_edits.iter().all(|e| e.new_text == "hello"));
        let b_edits = changes.get(&b_uri).expect("edits in b.jl");
        assert_eq!(b_edits.len(), 1);
        assert_eq!(b_edits[0].range.start, Position::new(0, 11));
    }

    /// `prepareRename` accepts a workspace symbol even where it is a free read
    /// (defined in a sibling file), returning the identifier's range.
    #[test]
    fn prepare_rename_accepts_a_workspace_free_read() {
        use crate::lsp::cross_file::test_support::{member_path, workspace_db};
        use crate::text::PositionEncoding::Utf16;

        let a_text = "greet(a) = a\n";
        let b_text = "callit() = greet(2)\n";
        let (db, _) = workspace_db(&["greet"], &[("a.jl", a_text), ("b.jl", b_text)]);
        let snapshot = db.snapshot();
        let b_path = member_path("b.jl");

        let response =
            prepare_rename_via_db(&snapshot, &b_path, b_text, Position::new(0, 11), Utf16)
                .expect("the workspace free read is renameable");
        match response {
            PrepareRenameResponse::Range(range) => {
                // The `greet` token spans columns 11..16 on line 0.
                assert_eq!(range.start, Position::new(0, 11));
                assert_eq!(range.end, Position::new(0, 16));
            }
            other => panic!("expected a plain range, got {other:?}"),
        }
    }
}
