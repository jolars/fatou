//! Project-level diagnostics derived from the include graph: unresolved static
//! includes and include cycles. Unlike per-file parse diagnostics these attach
//! to a *member* file (the one holding the offending `include(...)` call), which
//! need not be the buffer currently being analyzed, so they publish through a
//! separate path (see [`Outbound::ProjectDiagnostics`]). Script results carry
//! the buffer version used to compute their spans, or identify a closed file.
//!
//! The include graph ([`ProjectGraph`]) is range-free; this module recovers each
//! call's span from a fresh parse of the offending file via
//! [`include_call_sites`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};

use crate::incremental::ProjectGraph;
use crate::project::include_call_sites;
use crate::project::scripts::ScriptAnalysis;
use crate::syntax::SyntaxNode;
use crate::text::{PositionEncoding, TextBuffer};

/// Script diagnostics describe either a particular buffer version or a closed
/// file. A disk result must not replace diagnostics after that file opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphSource {
    Package,
    Script { version: Option<i32> },
}

/// One include-graph problem attached to a `from` file: the decoded `path` whose
/// `include(...)` call it marks, plus the rendered message and severity.
struct Problem {
    path: String,
    message: String,
    severity: DiagnosticSeverity,
    code: &'static str,
}

/// Build the include-graph diagnostics, grouped by the member file they attach
/// to. `source(path)` yields the file's `(text, parse tree)` so each `include`
/// call's span is recovered; a file that cannot be sourced is skipped. Every
/// static `include("path")` site matching a problem's literal is marked (there is
/// rarely more than one).
pub(crate) fn graph_diagnostics(
    graph: &ProjectGraph,
    encoding: PositionEncoding,
    mut source: impl FnMut(&Path) -> Option<(String, SyntaxNode)>,
) -> BTreeMap<PathBuf, Vec<Diagnostic>> {
    let mut by_file: BTreeMap<PathBuf, Vec<Problem>> = BTreeMap::new();
    for unresolved in &graph.unresolved {
        by_file
            .entry(unresolved.from.clone())
            .or_default()
            .push(Problem {
                path: unresolved.path.clone(),
                message: format!(
                    "cannot resolve include: \"{}\" was not found",
                    unresolved.path
                ),
                severity: DiagnosticSeverity::ERROR,
                code: "missing-include-file",
            });
    }
    for cycle in &graph.cycles {
        by_file
            .entry(cycle.from.clone())
            .or_default()
            .push(Problem {
                path: cycle.path.clone(),
                message: format!(
                    "include cycle: \"{}\" transitively includes this file",
                    cycle.path
                ),
                severity: DiagnosticSeverity::WARNING,
                code: "include-cycle",
            });
    }

    render_problems(by_file, encoding, |path| {
        source(path).map(|(text, tree)| (Arc::new(TextBuffer::new(text)), tree))
    })
}

pub(crate) fn script_graph_diagnostics(
    scripts: &ScriptAnalysis,
    encoding: PositionEncoding,
    source: impl FnMut(&Path) -> Option<(Arc<TextBuffer>, SyntaxNode)>,
) -> BTreeMap<PathBuf, Vec<Diagnostic>> {
    let problems = scripts
        .failed_includes()
        .into_iter()
        .map(|(from, paths)| {
            (
                from,
                paths
                    .into_iter()
                    .map(|path| Problem {
                        message: format!(
                            "cannot read include: \"{path}\" is missing or unreadable"
                        ),
                        path,
                        severity: DiagnosticSeverity::ERROR,
                        code: "missing-include-file",
                    })
                    .collect(),
            )
        })
        .collect();
    render_problems(problems, encoding, source)
}

/// Script buffers and their load results supersede the package's disk snapshot.
/// Keep package cycle findings, which the script diagnostics do not duplicate.
pub(crate) fn merge_script_diagnostics(
    mut package: BTreeMap<PathBuf, Vec<Diagnostic>>,
    scripts: &BTreeMap<PathBuf, Vec<Diagnostic>>,
    sources: &BTreeSet<PathBuf>,
) -> BTreeMap<PathBuf, Vec<Diagnostic>> {
    for (path, diagnostics) in &mut package {
        if sources.contains(path) {
            diagnostics.retain(|diag| {
                diag.code != Some(NumberOrString::String("missing-include-file".into()))
            });
        }
    }
    for (path, diagnostics) in scripts {
        package
            .entry(path.clone())
            .or_default()
            .extend(diagnostics.iter().cloned());
    }
    package.retain(|_, diagnostics| !diagnostics.is_empty());
    package
}

fn render_problems(
    by_file: BTreeMap<PathBuf, Vec<Problem>>,
    encoding: PositionEncoding,
    mut source: impl FnMut(&Path) -> Option<(Arc<TextBuffer>, SyntaxNode)>,
) -> BTreeMap<PathBuf, Vec<Diagnostic>> {
    let mut out = BTreeMap::new();
    for (from, problems) in by_file {
        let Some((text, tree)) = source(&from) else {
            continue;
        };
        let line_index = text.line_index();
        let sites = include_call_sites(&tree);
        let mut diagnostics = Vec::new();
        for problem in &problems {
            for (path, range) in &sites {
                if *path == problem.path {
                    diagnostics.push(Diagnostic {
                        range: Range::new(
                            line_index.byte_to_position(range.start().into(), encoding),
                            line_index.byte_to_position(range.end().into(), encoding),
                        ),
                        severity: Some(problem.severity),
                        source: Some("fatou".to_string()),
                        code: Some(NumberOrString::String(problem.code.into())),
                        message: problem.message.clone(),
                        ..Default::default()
                    });
                }
            }
        }
        if !diagnostics.is_empty() {
            out.insert(from, diagnostics);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incremental::{CycleEdge, UnresolvedInclude};
    use crate::parser::parse;

    fn tree_of(src: &str) -> SyntaxNode {
        parse(src).cst
    }

    #[test]
    fn marks_the_unresolved_include_call() {
        let graph = ProjectGraph {
            unresolved: vec![UnresolvedInclude {
                from: PathBuf::from("/pkg/src/Pkg.jl"),
                path: "missing.jl".to_string(),
            }],
            ..Default::default()
        };
        let text = "include(\"a.jl\")\ninclude(\"missing.jl\")\n";
        let out = graph_diagnostics(&graph, PositionEncoding::Utf16, |path| {
            (path == Path::new("/pkg/src/Pkg.jl")).then(|| (text.to_string(), tree_of(text)))
        });
        let diags = &out[&PathBuf::from("/pkg/src/Pkg.jl")];
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        // The second line's `include("missing.jl")` call.
        assert_eq!(diags[0].range.start.line, 1);
    }

    #[test]
    fn marks_the_cycle_back_edge() {
        let graph = ProjectGraph {
            cycles: vec![CycleEdge {
                from: PathBuf::from("/pkg/src/b.jl"),
                path: "a.jl".to_string(),
                to: PathBuf::from("/pkg/src/a.jl"),
            }],
            ..Default::default()
        };
        let text = "include(\"a.jl\")\n";
        let out = graph_diagnostics(&graph, PositionEncoding::Utf16, |_| {
            Some((text.to_string(), tree_of(text)))
        });
        let diags = &out[&PathBuf::from("/pkg/src/b.jl")];
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
    }

    #[test]
    fn unsourceable_file_is_skipped() {
        let graph = ProjectGraph {
            unresolved: vec![UnresolvedInclude {
                from: PathBuf::from("/pkg/src/gone.jl"),
                path: "x.jl".to_string(),
            }],
            ..Default::default()
        };
        let out = graph_diagnostics(&graph, PositionEncoding::Utf16, |_| None);
        assert!(out.is_empty());
    }

    #[test]
    fn script_failures_supersede_package_failures_and_deduplicate_entries() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("main.jl");
        let other = dir.path().join("other.jl");
        let helper = dir.path().join("helper.jl");
        let text = Arc::new(TextBuffer::new("include(\"\")\n"));
        let sources = BTreeMap::from([
            (entry.clone(), Ok(Arc::from("include(\"helper.jl\")\n"))),
            (other.clone(), Ok(Arc::from("include(\"helper.jl\")\n"))),
            (helper.clone(), Ok(text.text_arc())),
            (dir.path().to_path_buf(), Err("directory".into())),
        ]);
        let scripts = ScriptAnalysis::from_sources(&[entry, other], &sources);
        let script = script_graph_diagnostics(&scripts, PositionEncoding::Utf16, |_| {
            Some((Arc::clone(&text), tree_of(&text)))
        });
        assert_eq!(script[&helper].len(), 1);
        assert_eq!(
            script[&helper][0].range,
            Range::new(
                lsp_types::Position::new(0, 0),
                lsp_types::Position::new(0, 11)
            )
        );
        let graph = ProjectGraph {
            unresolved: vec![UnresolvedInclude {
                from: helper.clone(),
                path: "".into(),
            }],
            ..Default::default()
        };
        let package = graph_diagnostics(&graph, PositionEncoding::Utf16, |_| {
            Some((text.to_string(), tree_of(&text)))
        });
        let members = sources.keys().cloned().collect();
        let merged = merge_script_diagnostics(package.clone(), &script, &members);
        assert_eq!(merged[&helper].len(), 1);
        assert!(merge_script_diagnostics(package, &BTreeMap::new(), &members).is_empty());
    }
}
