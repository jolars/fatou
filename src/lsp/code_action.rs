//! Lint quick fixes and semantic name qualification.
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
use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Range, TextEdit, Uri, WorkspaceEdit,
};

use crate::ast::{Arg, AstNode, AstToken, Interpolation, MacroName, ModuleDef, Name, Parameters};
use crate::incremental::Analysis;
use crate::linter::{self, Applicability};
use crate::parser::{is_ident_continue, is_ident_start, parse_with_options};
use crate::resolve::{Namespace, Resolver};
use crate::semantic::{Access, IdentRef, SemanticModel};
use crate::syntax::{SyntaxKind, SyntaxNode};
use crate::text::{LineIndex, PositionEncoding, TextBuffer};

use super::lint::{ServerRules, finding_to_lsp, lint_findings_via_db};

pub(crate) fn allows_kind(only: Option<&[CodeActionKind]>, kind: CodeActionKind) -> bool {
    only.is_none_or(|only| {
        only.iter().any(|requested| {
            requested.as_str().is_empty()
                || kind == *requested
                || kind
                    .as_str()
                    .strip_prefix(requested.as_str())
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
    })
}

/// Compute the quick-fix actions for `range`, linting off the snapshot's
/// cached parse when the tracked buffer for `path` still matches `text` (the
/// cache contract of [`lint_findings_via_db`]).
pub(crate) fn code_actions_via_db(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    text: &TextBuffer,
    range: Range,
    encoding: PositionEncoding,
    rules: &ServerRules,
) -> Vec<CodeActionOrCommand> {
    actions_for(
        &lint_findings_via_db(snapshot, path, text, rules),
        uri,
        text,
        range,
        encoding,
    )
}

/// Offer an explicit module access when the selected bare read has a
/// statically known origin and the qualifier is available at that use site.
pub(crate) fn qualify_name_via_db(
    snapshot: &Analysis,
    uri: &Uri,
    path: &Path,
    text: &TextBuffer,
    range: Range,
    encoding: PositionEncoding,
) -> Option<CodeActionOrCommand> {
    let line_index = text.line_index();
    let requested = selection(&line_index, range, encoding);
    let qualify = |tree: SyntaxNode, model: &SemanticModel| {
        // The semantic range of a macro name excludes its sigil.
        let start = requested.start();
        let ident = if text.as_bytes().get(usize::from(start)) == Some(&b'@') {
            model
                .ident_at(start + rowan::TextSize::from(1))
                .filter(|ident| ident.is_macro)
        } else {
            model.ident_at(start)
        }?;
        if ident.access != Access::Read
            || model.scope_at(ident.range.start()) != ident.scope
            || model
                .qualified_reads()
                .iter()
                .any(|qualified| qualified.range.contains_range(ident.range))
        {
            return None;
        }

        let token = tree.covering_element(ident.range).into_token()?;
        let parent = token.parent()?;
        let (target, namespace) = bare_name(&parent, ident)?;
        if !target.contains_range(requested) {
            return None;
        }

        let implicit_base = parent.ancestors().find_map(ModuleDef::cast).map(|module| {
            module
                .syntax()
                .first_token()
                .is_none_or(|token| token.kind() != SyntaxKind::BAREMODULE_KW)
        });
        let offset = ident.range.start();
        let mut resolver =
            Resolver::new(model, snapshot).with_workspace(snapshot.workspace_member(path));
        let scripts = snapshot
            .context()
            .map(|context| snapshot.scripts(std::slice::from_ref(&context.entry)));
        if let Some(scripts) = &scripts
            && !scripts.pending
        {
            let contexts = scripts.contexts(path);
            if !contexts.is_empty() {
                resolver = resolver.with_scripts(contexts);
            }
        }
        let (module, member) =
            resolver.qualification(&ident.name, offset, namespace, implicit_base)?;
        let mut chars = member.trim_start_matches('@').chars();
        if !chars.next().is_some_and(is_ident_start) || !chars.all(is_ident_continue) {
            return None;
        }
        let qualified = format!("{module}.{member}");
        let replacement = if Interpolation::can_cast(parent.kind()) {
            format!("({qualified})")
        } else if parent
            .parent()
            .and_then(Arg::cast)
            .and_then(|arg| arg.syntax().parent())
            .is_some_and(|container| Parameters::can_cast(container.kind()))
            && member != ident.name
        {
            // The original spelling is also the implicit keyword or field name.
            format!("{} = {qualified}", token.text())
        } else {
            qualified.clone()
        };
        let edit = TextEdit {
            range: Range::new(
                line_index.byte_to_position(target.start().into(), encoding),
                line_index.byte_to_position(target.end().into(), encoding),
            ),
            new_text: replacement,
        };
        Some(CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Qualify as `{qualified}`"),
            kind: Some(CodeActionKind::REFACTOR_REWRITE),
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
                ..Default::default()
            }),
            ..Default::default()
        }))
    };
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        (snapshot.file_text(file) == text).then(|| {
            if !snapshot.parse_diagnostics(file).is_empty() {
                return None;
            }
            qualify(snapshot.parsed_tree(file), snapshot.semantic_model(file))
        })
    }));
    match cached {
        Ok(Some(action)) => action,
        Ok(None) | Err(_) => salsa::Cancelled::catch(AssertUnwindSafe(|| {
            let parsed = parse_with_options(text, snapshot.options_for_path(path));
            if !parsed.diagnostics.is_empty() {
                return None;
            }
            let model = SemanticModel::build(&parsed.cst);
            qualify(parsed.cst, &model)
        }))
        .ok()
        .flatten(),
    }
}

fn bare_name(parent: &SyntaxNode, ident: &IdentRef) -> Option<(rowan::TextRange, Namespace)> {
    if ident.is_macro {
        let mac = MacroName::cast(parent.clone())?;
        let target = mac.syntax().text_range();
        // A bare macro consists of its sigil followed by the identifier.
        return (target.start() + rowan::TextSize::from(1) == ident.range.start()
            && target.end() == ident.range.end())
        .then_some((target, Namespace::Macro));
    }
    let name = Name::cast(parent.clone())
        .and_then(|name| name.ident())
        .or_else(|| Interpolation::cast(parent.clone())?.ident())?;
    (name.syntax().text_range() == ident.range).then_some((ident.range, Namespace::Value))
}

fn selection(line_index: &LineIndex, range: Range, encoding: PositionEncoding) -> rowan::TextRange {
    let start = line_index.position_to_byte(range.start, encoding) as u32;
    let end = line_index.position_to_byte(range.end, encoding) as u32;
    rowan::TextRange::new(start.min(end).into(), start.max(end).into())
}

/// The quick-fix actions whose findings overlap `range` (touching counts, so a
/// cursor at either edge of a finding still offers its fixes). The pure core of
/// `textDocument/codeAction`.
fn actions_for(
    findings: &[linter::Diagnostic],
    uri: &Uri,
    text: &TextBuffer,
    range: Range,
    encoding: PositionEncoding,
) -> Vec<CodeActionOrCommand> {
    let line_index = text.line_index();
    let requested = selection(&line_index, range, encoding);
    findings
        .iter()
        .filter(|finding| {
            finding.range.start() <= requested.end() && requested.start() <= finding.range.end()
        })
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
            &TextBuffer::new(text.to_string()),
            range,
            PositionEncoding::Utf16,
            &ServerRules::defaults(),
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
    fn qualification_preserves_the_selected_name_in_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jl");
        let uri = crate::lsp::uri::from_path(&path).unwrap();
        let mut db = IncrementalDatabase::default();
        db.set_library_packages(crate::index::build_system_index(None));
        for (marked, expected) in [
            ("|println(1)", Some("Base.println(1)")),
            (
                "using Base\n|println(1)",
                Some("using Base\nBase.println(1)"),
            ),
            ("f(😀, pr|intln)", Some("f(😀, Base.println)")),
            ("|@time println(1)", Some("Base.@time println(1)")),
            (
                "using Base.Threads: @spawn\n|@spawn f()",
                Some("using Base.Threads: @spawn\nBase.Threads.@spawn f()"),
            ),
            (
                "import Base as B\nusing Base: println as showit\nf(; sho|wit)",
                Some("import Base as B\nusing Base: println as showit\nf(; showit = B.println)"),
            ),
            ("\"$prin|tln\"", Some("\"$(Base.println)\"")),
            ("println(|println)", Some("println(Base.println)")),
            (
                "using Base: println\nlet Base = obj\n   |println(1)\nend",
                None,
            ),
            ("baremodule M\n|println(1)\nend", None),
            ("f(println) = |println(1)", None),
            ("f(|println) = println(1)", None),
            ("|missing_name(1)", None),
            ("|Base.println(1)", None),
            ("Base.|@time println(1)", None),
            ("|println(", None),
        ] {
            let offset = marked.find('|').unwrap();
            let source = marked.replacen('|', "", 1);
            let text = TextBuffer::new(source.clone());
            db.upsert_file(&path, text.text_arc());
            let index = text.line_index();
            let encoding = PositionEncoding::Utf16;
            let cursor = index.byte_to_position(offset, encoding);
            let action = qualify_name_via_db(
                &db.snapshot(),
                &uri,
                &path,
                &text,
                Range::new(cursor, cursor),
                encoding,
            );
            if let Some(expected) = expected {
                let Some(CodeActionOrCommand::CodeAction(action)) = action else {
                    panic!("expected a qualification for {marked:?}");
                };
                let changes = action.edit.unwrap().changes.unwrap();
                let [edit] = changes[&uri].as_slice() else {
                    panic!("expected one edit for {marked:?}");
                };
                let start = index.position_to_byte(edit.range.start, encoding);
                let end = index.position_to_byte(edit.range.end, encoding);
                let mut rewritten = source.clone();
                rewritten.replace_range(start..end, &edit.new_text);
                assert_eq!(rewritten, expected, "{marked:?}");
                assert!(crate::parser::parse(&rewritten).diagnostics.is_empty());
            } else {
                assert!(action.is_none(), "unexpected qualification for {marked:?}");
            }
        }
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
            ..linter::Diagnostic::new(
                "test-rule",
                rowan::TextRange::new(0.into(), 2.into()),
                "message",
            )
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
