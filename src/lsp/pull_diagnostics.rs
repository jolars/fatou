//! Pull diagnostics (`textDocument/diagnostic`).
//!
//! The pull model inverts the push pipeline: the client asks for a document's
//! diagnostics when it wants them (on open, after an edit, on focus), so the
//! server computes on demand instead of publishing per edit. One pull report
//! carries everything the push path would have split across two publishes:
//! parse diagnostics, lint findings (only on a parse-clean tree), and the
//! file's include-graph problems. When the client supports pull, the push
//! path stays on only for files with *no open buffer* — include-graph
//! diagnostics attach to member files the client never pulls — plus a
//! `workspace/diagnostic/refresh` nudge per re-harvest so open documents get
//! re-pulled (see `GlobalState`).

use std::panic::AssertUnwindSafe;
use std::path::Path;

use lsp_types::Diagnostic;

use crate::incremental::{Analysis, normalize_path};
use crate::parse_context::{EditionSelection, SourceContext};
use crate::parser::parse_with_options;
use crate::text::{PositionEncoding, TextBuffer};

use super::format::parse_diagnostics_to_lsp;
use super::graph_diagnostics::{
    graph_diagnostics, merge_script_diagnostics, script_graph_diagnostics,
};
use super::lint::{ServerRules, lint_diagnostics_via_db};

/// The full diagnostic report for one document: parse diagnostics, lint
/// findings on a clean tree, and the file's include-graph problems. Reads warm
/// off the snapshot's cached parse when the tracked buffer for `path` still
/// matches `text`; a cache miss or racing write falls back to a fresh parse.
fn document_diagnostics_single(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
    encoding: PositionEncoding,
    rules: &ServerRules,
    include_graph: bool,
) -> Vec<Diagnostic> {
    let cached = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let file = snapshot.lookup_file(path)?;
        if snapshot.file_text(file) != text {
            // The tracked input lags the live buffer; the cached tree is stale.
            return None;
        }
        Some(parse_diagnostics_to_lsp(
            snapshot.parse_diagnostics(file),
            text,
            encoding,
        ))
    }));
    let mut diags = match cached {
        Ok(Some(diags)) => diags,
        // Cache miss (`Ok(None)`) or a racing write (`Err`): re-parse from text.
        Ok(None) | Err(_) => parse_diagnostics_to_lsp(
            &parse_with_options(text, snapshot.options_for_path(path)).diagnostics,
            text,
            encoding,
        ),
    };
    // Lint findings join the report on a clean tree only, exactly like the
    // push path: rules would misfire on error-recovered shapes.
    if diags.is_empty() {
        diags.extend(lint_diagnostics_via_db(
            snapshot, path, text, encoding, rules,
        ));
    }
    if include_graph {
        diags.extend(graph_diagnostics_for(snapshot, path, text, encoding, rules));
    }
    diags
}

/// The same source may load in several programs. Preserve every diagnostic,
/// label findings confined to some contexts, and deduplicate shared findings.
pub(crate) fn document_diagnostics_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
    encoding: PositionEncoding,
    rules: &ServerRules,
    contexts: &[(SourceContext, EditionSelection)],
) -> Vec<Diagnostic> {
    diagnostics_via_db(snapshot, path, text, encoding, rules, true, contexts)
}

pub(crate) fn source_diagnostics_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
    encoding: PositionEncoding,
    rules: &ServerRules,
) -> Vec<Diagnostic> {
    let contexts = snapshot
        .lookup_file(path)
        .map_or(&[][..], |file| snapshot.source_editions(file).as_slice());
    diagnostics_via_db(snapshot, path, text, encoding, rules, false, contexts)
}

fn diagnostics_via_db(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
    encoding: PositionEncoding,
    rules: &ServerRules,
    include_graph: bool,
    contexts: &[(SourceContext, EditionSelection)],
) -> Vec<Diagnostic> {
    if contexts.is_empty() {
        return document_diagnostics_single(snapshot, path, text, encoding, rules, include_graph);
    }
    let reports = contexts.iter().map(|(context, selected)| {
        document_diagnostics_single(
            &snapshot.with_context(context.clone(), selected.options),
            path,
            text,
            encoding,
            rules,
            include_graph,
        )
    });
    let mut merged = if contexts.len() == 1 {
        reports.into_iter().next().unwrap()
    } else {
        let mut groups = Vec::<(Diagnostic, Vec<usize>)>::new();
        let mut keys = std::collections::HashMap::new();
        for (context, report) in reports.enumerate() {
            for diagnostic in report {
                let key = serde_json::to_vec(&diagnostic).expect("diagnostic serializes");
                let index = *keys.entry(key).or_insert_with(|| {
                    let index = groups.len();
                    groups.push((diagnostic, Vec::new()));
                    index
                });
                let indices = &mut groups[index].1;
                if indices.last() != Some(&context) {
                    indices.push(context);
                }
            }
        }
        groups
            .into_iter()
            .map(|(mut diagnostic, indices)| {
                if indices.len() != contexts.len() {
                    let labels = indices
                        .into_iter()
                        .map(|index| {
                            let (context, selection) = &contexts[index];
                            format!(
                                "{} ({})",
                                context.entry.display(),
                                selection.options.syntax_edition
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    diagnostic
                        .message
                        .push_str(&format!(" [syntax contexts: {labels}]"));
                }
                diagnostic
            })
            .collect()
    };
    for (_, selected) in contexts {
        if let Some(issue) = &selected.issue {
            let diagnostic = Diagnostic {
                range: lsp_types::Range::default(),
                severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                source: Some("fatou".into()),
                message: format!(
                    "syntax edition is uncertain: {}: {}",
                    issue.path.display(),
                    issue.message
                ),
                ..Default::default()
            };
            if !merged.contains(&diagnostic) {
                merged.push(diagnostic);
            }
        }
    }
    merged
}

/// The include-graph diagnostics attaching to `path`, re-derived from the
/// snapshot's project graph (salsa-cached; empty without a workspace). The
/// pull twin of the harvest-driven `refresh_graph_diagnostics` publish.
fn graph_diagnostics_for(
    snapshot: &Analysis,
    path: &Path,
    text: &TextBuffer,
    encoding: PositionEncoding,
    rules: &ServerRules,
) -> Vec<Diagnostic> {
    let computed = salsa::Cancelled::catch(AssertUnwindSafe(|| {
        let graph = snapshot.project_graph();
        let package = graph_diagnostics(graph, encoding, |member| {
            if member != path {
                return None;
            }
            let file = snapshot.lookup_file(member)?;
            Some((
                snapshot.file_text_of(file).to_string(),
                snapshot.parsed_tree(file),
            ))
        });
        let scripts = snapshot.scripts(&rules.entry_points);
        let script = script_graph_diagnostics(&scripts, encoding, |member| {
            if member != path {
                return None;
            }
            let file = snapshot.lookup_file(member)?;
            (snapshot.file_text(file) == text.text()).then(|| {
                (
                    std::sync::Arc::new(text.clone()),
                    snapshot.parsed_tree(file),
                )
            })
        });
        let sources = scripts
            .programs
            .iter()
            .flat_map(|p| p.members.keys().cloned())
            .collect();
        merge_script_diagnostics(package, &script, &sources)
            .remove(&normalize_path(path))
            .unwrap_or_default()
    }));
    // A racing write: the client will re-pull on the edit that caused it.
    computed.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::IncrementalDatabase;
    use lsp_types::{DiagnosticSeverity, NumberOrString};
    use std::path::PathBuf;

    fn document_diagnostics(
        snapshot: &Analysis,
        path: &Path,
        text: &TextBuffer,
        encoding: PositionEncoding,
        rules: &ServerRules,
    ) -> Vec<Diagnostic> {
        let contexts = snapshot.capture_source_editions(path);
        document_diagnostics_via_db(snapshot, path, text, encoding, rules, &contexts)
    }

    #[test]
    fn loose_includes_do_not_create_a_package_graph() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.jl");
        std::fs::write(dir.path().join("helper.jl"), "f(x) = x\n").unwrap();
        let text = TextBuffer::new("include(\"helper.jl\")\n");
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&path, text.text_arc());
        let rules = ServerRules::defaults();
        assert!(
            document_diagnostics(
                &db.snapshot(),
                &path,
                &text,
                PositionEncoding::Utf16,
                &rules
            )
            .is_empty()
        );
    }

    #[test]
    fn script_include_failures_are_reported_until_repaired() {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let helper = dir.path().join("helper.jl");
        let mut db = IncrementalDatabase::default();
        let (mut rules, _) = ServerRules::from_config(&Default::default());
        rules.entry_points = vec![entry.clone()];
        for target in ["", "missing.jl", "helper.jl"] {
            let text = TextBuffer::new(format!("include(\"{target}\")\nf() = typo\n"));
            let sources = BTreeMap::from([
                (entry.clone(), Ok(text.text_arc())),
                (dir.path().join(target), Err("unreadable".into())),
            ]);
            db.set_script_sources(&rules.entry_points, &sources, &Default::default());
            let diagnostics = document_diagnostics(
                &db.snapshot(),
                &entry,
                &text,
                PositionEncoding::Utf16,
                &rules,
            );
            assert_eq!(diagnostics.len(), 1, "{target:?}: {diagnostics:?}");
            let push = source_diagnostics_via_db(
                &db.snapshot(),
                &entry,
                &text,
                PositionEncoding::Utf16,
                &rules,
            );
            assert!(
                push.is_empty(),
                "the script worker owns push graph findings: {push:?}"
            );
            assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
            assert_eq!(diagnostics[0].range.start.line, 0);
            assert!(diagnostics[0].message.contains("include"));
        }
        let text = TextBuffer::new("include(\"helper.jl\")\nf() = provided + typo\n");
        let sources = BTreeMap::from([
            (entry.clone(), Ok(text.text_arc())),
            (helper, Ok(Arc::from("provided = 1\n"))),
        ]);
        db.set_script_sources(&rules.entry_points, &sources, &Default::default());
        let diagnostics = document_diagnostics(
            &db.snapshot(),
            &entry,
            &text,
            PositionEncoding::Utf16,
            &rules,
        );
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].message.contains("`typo`"));
    }

    fn report_for(text: &str) -> Vec<Diagnostic> {
        let path = PathBuf::from("/work/a.jl");
        let mut db = IncrementalDatabase::default();
        db.upsert_file(&path, text.to_string());
        document_diagnostics(
            &db.snapshot(),
            &path,
            &TextBuffer::new(text.to_string()),
            PositionEncoding::Utf16,
            &ServerRules::defaults(),
        )
    }

    #[test]
    fn a_clean_document_reports_nothing() {
        assert_eq!(report_for("x = 1\n"), Vec::new());
    }

    #[test]
    fn lint_findings_join_the_report() {
        let diags = report_for("function f(x)\n    tmp = x + 1\n    return x\nend\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("unused-binding".to_string()))
        );
    }

    #[test]
    fn parse_errors_suppress_lint_findings() {
        let diags = report_for("function f(x)\n    tmp = x + 1\n    return x\n");
        assert!(!diags.is_empty());
        assert!(
            diags
                .iter()
                .all(|d| d.severity == Some(DiagnosticSeverity::ERROR) && d.code.is_none()),
            "a parse-broken buffer must report parse errors only, got {diags:?}"
        );
    }

    /// An untracked path (or stale buffer) falls back to a fresh parse and
    /// still reports correctly.
    #[test]
    fn falls_back_when_the_db_lags() {
        let text = "function f(x)\n    tmp = x + 1\n    return x\nend\n";
        let db = IncrementalDatabase::default();
        let diags = document_diagnostics(
            &db.snapshot(),
            Path::new("/work/never-seen.jl"),
            &TextBuffer::new(text.to_string()),
            PositionEncoding::Utf16,
            &ServerRules::defaults(),
        );
        assert_eq!(diags.len(), 1);
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("unused-binding".to_string()))
        );
    }
}
