//! Project-level diagnostics derived from the include graph: unresolved static
//! includes and include cycles. Unlike per-file parse diagnostics these attach
//! to a *member* file (the one holding the offending `include(...)` call), which
//! need not be the buffer currently being analyzed, so they publish through a
//! separate, version-free path (see [`Outbound::ProjectDiagnostics`]).
//!
//! The include graph ([`ProjectGraph`]) is range-free; this module recovers each
//! call's span from a fresh parse of the offending file via
//! [`include_call_sites`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lsp_types::{Diagnostic, DiagnosticSeverity, Range};

use crate::incremental::ProjectGraph;
use crate::project::include_call_sites;
use crate::syntax::SyntaxNode;
use crate::text::{LineIndex, PositionEncoding};

/// One include-graph problem attached to a `from` file: the `raw` literal whose
/// `include(...)` call it marks, plus the rendered message and severity.
struct Problem {
    raw: String,
    message: String,
    severity: DiagnosticSeverity,
}

/// Build the include-graph diagnostics, grouped by the member file they attach
/// to. `source(path)` yields the file's `(text, parse tree)` so each `include`
/// call's span is recovered; a file that cannot be sourced is skipped. Every
/// static `include("raw")` site matching a problem's literal is marked (there is
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
                raw: unresolved.raw.clone(),
                message: format!(
                    "cannot resolve include: \"{}\" was not found",
                    unresolved.raw
                ),
                severity: DiagnosticSeverity::ERROR,
            });
    }
    for cycle in &graph.cycles {
        by_file
            .entry(cycle.from.clone())
            .or_default()
            .push(Problem {
                raw: cycle.raw.clone(),
                message: format!(
                    "include cycle: \"{}\" transitively includes this file",
                    cycle.raw
                ),
                severity: DiagnosticSeverity::WARNING,
            });
    }

    let mut out: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    for (from, problems) in by_file {
        let Some((text, tree)) = source(&from) else {
            continue;
        };
        let line_index = LineIndex::new(&text);
        let sites = include_call_sites(&tree);
        let mut diagnostics = Vec::new();
        for problem in &problems {
            for (raw, range) in &sites {
                if *raw == problem.raw {
                    diagnostics.push(Diagnostic {
                        range: Range::new(
                            line_index.byte_to_position(range.start().into(), encoding),
                            line_index.byte_to_position(range.end().into(), encoding),
                        ),
                        severity: Some(problem.severity),
                        source: Some("fatou".to_string()),
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
                raw: "missing.jl".to_string(),
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
                raw: "a.jl".to_string(),
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
                raw: "x.jl".to_string(),
            }],
            ..Default::default()
        };
        let out = graph_diagnostics(&graph, PositionEncoding::Utf16, |_| None);
        assert!(out.is_empty());
    }
}
