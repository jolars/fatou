//! Quick-fix code actions from lint findings.
//!
//! `textDocument/codeAction` re-lints the document (warm off the salsa-cached
//! parse and semantic model) rather than round-tripping fix data through the
//! published diagnostics: the linter is cheap on a cached tree, and the byte
//! offsets a [`linter::Fix`] carries are only valid against the exact buffer
//! they were computed from, so recomputing against the live text is also the
//! correct thing to do. Each fix on a finding overlapping the requested range
//! becomes one quick-fix action carrying a single-document [`WorkspaceEdit`];
//! safe fixes are marked preferred, unsafe ones say so in the title (the
//! LSP has no applicability gate like the CLI's `--unsafe-fixes`).

use std::collections::HashMap;
use std::path::Path;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Range, TextEdit, Uri, WorkspaceEdit,
};

use crate::incremental::Analysis;
use crate::linter::{self, Applicability};
use crate::text::{LineIndex, PositionEncoding};

use super::format::lsp_range_to_text_range;
use super::lint::{finding_to_lsp, lint_findings_via_db};

/// Compute the quick-fix actions for `range`, linting off the snapshot's
/// cached parse when the tracked buffer for `path` still matches `text` (the
/// cache contract of [`lint_findings_via_db`]).
pub(crate) fn code_actions_via_db(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    text: &str,
    range: Range,
    encoding: PositionEncoding,
) -> Vec<CodeActionOrCommand> {
    actions_for(
        &lint_findings_via_db(snapshot, path, text),
        uri,
        text,
        range,
        encoding,
    )
}

/// The quick-fix actions whose findings overlap `range` (touching counts, so a
/// cursor at either edge of a finding still offers its fixes). The pure core of
/// `textDocument/codeAction`.
fn actions_for(
    findings: &[linter::Diagnostic],
    uri: &Uri,
    text: &str,
    range: Range,
    encoding: PositionEncoding,
) -> Vec<CodeActionOrCommand> {
    let requested = lsp_range_to_text_range(text, range, encoding);
    let (req_start, req_end) = (usize::from(requested.start()), usize::from(requested.end()));
    let line_index = LineIndex::new(text);
    findings
        .iter()
        .filter(|finding| finding.start <= req_end && req_start <= finding.end)
        .flat_map(|finding| {
            finding
                .fixes
                .iter()
                .map(|fix| action_for_fix(finding, fix, uri, &line_index, encoding))
        })
        .collect()
}

/// One quick-fix action applying `fix`, tied back to its finding's diagnostic
/// so clients can pair the action with the squiggle it resolves.
fn action_for_fix(
    finding: &linter::Diagnostic,
    fix: &linter::Fix,
    uri: &Uri,
    line_index: &LineIndex,
    encoding: PositionEncoding,
) -> CodeActionOrCommand {
    let edit = TextEdit {
        range: Range::new(
            line_index.byte_to_position(fix.start, encoding),
            line_index.byte_to_position(fix.end, encoding),
        ),
        new_text: fix.content.clone(),
    };
    let safe = fix.applicability == Applicability::Safe;
    let title = if safe {
        fix.description.clone()
    } else {
        format!("{} (unsafe)", fix.description)
    };
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![finding_to_lsp(finding, line_index, encoding)]),
        edit: Some(WorkspaceEdit {
            changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
            ..Default::default()
        }),
        is_preferred: safe.then_some(true),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    use lsp_types::Position;
    use std::str::FromStr;

    const NOTHING_COMPARISON: &str = "check(x) = x == nothing\n";

    fn test_uri() -> Uri {
        Uri::from_str("file:///work/a.jl").unwrap()
    }

    fn actions_at(text: &str, range: Range) -> Vec<CodeActionOrCommand> {
        let path = Path::new("/work/a.jl");
        let mut db = IncrementalDatabase::default();
        db.upsert_file(path, text.to_string());
        code_actions_via_db(
            &db.snapshot(),
            &test_uri(),
            path,
            text,
            range,
            PositionEncoding::Utf16,
        )
    }

    #[test]
    fn safe_fix_becomes_a_preferred_quickfix() {
        // Cursor inside the `==` of `x == nothing`.
        let cursor = Range::new(Position::new(0, 14), Position::new(0, 14));
        let actions = actions_at(NOTHING_COMPARISON, cursor);
        assert_eq!(actions.len(), 1);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected a code action, got {:?}", actions[0]);
        };
        assert_eq!(action.title, "Replace `==` with `===`");
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(action.is_preferred, Some(true));

        let diagnostics = action.diagnostics.as_ref().expect("attached diagnostic");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            Some(lsp_types::NumberOrString::String(
                "nothing-comparison".to_string()
            ))
        );

        let changes = action
            .edit
            .as_ref()
            .and_then(|edit| edit.changes.as_ref())
            .expect("single-document changes");
        let edits = &changes[&test_uri()];
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "===");
        assert_eq!(
            edits[0].range,
            Range::new(Position::new(0, 13), Position::new(0, 15)),
            "the edit must replace exactly the `==` operator"
        );
    }

    #[test]
    fn a_range_touching_the_finding_edge_still_offers_the_fix() {
        // The finding covers the whole comparison `x == nothing` (bytes 11..23);
        // a cursor at its very start touches it.
        let edge = Range::new(Position::new(0, 11), Position::new(0, 11));
        assert_eq!(actions_at(NOTHING_COMPARISON, edge).len(), 1);
    }

    #[test]
    fn a_range_outside_every_finding_yields_no_actions() {
        let elsewhere = Range::new(Position::new(0, 2), Position::new(0, 4));
        assert_eq!(actions_at(NOTHING_COMPARISON, elsewhere), Vec::new());
    }

    #[test]
    fn a_finding_without_fixes_yields_no_action() {
        // `unused-binding` carries no fix.
        let text = "function f(x)\n    tmp = x + 1\n    return x\nend\n";
        let on_tmp = Range::new(Position::new(1, 4), Position::new(1, 7));
        assert_eq!(actions_at(text, on_tmp), Vec::new());
    }

    #[test]
    fn an_unsafe_fix_is_labeled_and_not_preferred() {
        let finding = linter::Diagnostic {
            fixes: vec![linter::Fix {
                description: "Rewrite the comparison".to_string(),
                content: "===".to_string(),
                start: 0,
                end: 2,
                applicability: Applicability::Unsafe,
            }],
            ..linter::Diagnostic::new("test-rule", 0, 2, "message".to_string())
        };
        let text = "== x\n";
        let action = action_for_fix(
            &finding,
            &finding.fixes[0],
            &test_uri(),
            &LineIndex::new(text),
            PositionEncoding::Utf16,
        );
        let CodeActionOrCommand::CodeAction(action) = action else {
            panic!("expected a code action");
        };
        assert_eq!(action.title, "Rewrite the comparison (unsafe)");
        assert_eq!(action.is_preferred, None);
    }
}
