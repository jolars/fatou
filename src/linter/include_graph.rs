//! CLI-side include following: the per-file include problems the
//! include-graph rules (`missing-include-file`, `include-cycle`,
//! `duplicate-include`) report.
//!
//! The language server derives the same problems incrementally from its seeded
//! workspace (see [`crate::incremental::project_graph`] and
//! `crate::lsp::graph_diagnostics`) and therefore passes the rules an empty
//! problem slice. The CLI is a batch tool with no seeded workspace, so this
//! module follows static `include("literal")` chains directly: edges of files
//! in the lint set come from their already-parsed trees, and a target outside
//! the set is read and parsed from disk on demand (memoized), so a cycle that
//! closes through an unlinted file is still attributed to the linted one.
//!
//! Problems are computed per *seed* (a file being linted), each blamed on the
//! seed's own offending `include` call:
//!
//! - **Missing**: the resolved target is not a file on disk (this also covers
//!   StaticLint's `IncludePathContainsNULL` — a path holding a NUL escape
//!   never names a file).
//! - **Unreadable**: the target exists but cannot be read as source text.
//! - **Cycle**: the target transitively includes the seed again, the
//!   self-include being the smallest case.
//! - **Duplicate**: an earlier `include` of the seed, from the same host
//!   module, already pulled the same target in. Unlike the other two this is a
//!   property of the seed's own text, so it is reported *alongside* them rather
//!   than instead of them: a repeated `include` of a missing file is both
//!   missing and repeated.
//!
//! Only statically resolvable includes participate, exactly the edge set of
//! [`crate::project::include_edges`]; dynamic, interpolated, qualified, and
//! two-argument forms are invisible here as everywhere else.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::incremental::normalize_path;
use crate::parse_context::{ContextCatalog, normalized_include_edges};
#[cfg(test)]
use crate::parser::parse;
use crate::parser::{ParseOptions, parse_with_options};
use crate::project::IncludeEdge;
use crate::syntax::SyntaxNode;

/// What is wrong with one static `include("path")` site of a linted file. The
/// problem is range-free like [`crate::project::IncludeEdge`], so the graph walk
/// never tracks spans: a rule matches it back to a call site either by the
/// decoded path (every site denoting it is equally at fault) or, when one repeat
/// has to be told from another, by [`edge`](Self::edge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeProblem {
    /// The path the `include` literal denotes, decoded
    /// ([`crate::project::IncludeEdge::path`]).
    pub path: String,
    /// Which of the seed's static includes this is: an index into
    /// [`crate::project::include_edges`]'s source-ordered list, which a rule
    /// reproduces by enumerating the file's `include("literal")` call sites in
    /// the same order.
    pub edge: usize,
    pub kind: IncludeProblemKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeProblemKind {
    /// The resolved target is not a file on disk.
    Missing,
    /// The target exists but could not be read as source text.
    Unreadable,
    /// The target transitively `include`s the seed file again.
    Cycle,
    /// An earlier `include` in the same file and the same host module already
    /// pulled this target in.
    Duplicate,
}

type LoadedEdges = Result<Arc<[IncludeEdge]>, IncludeProblemKind>;

/// Memoize normalized include edges and failures across all seeds in one edition.
struct EdgeStore<F> {
    load: F,
    cache: HashMap<PathBuf, LoadedEdges>,
}

impl<F: FnMut(&Path) -> LoadedEdges> EdgeStore<F> {
    fn edges(&mut self, path: &Path) -> LoadedEdges {
        if let Some(cached) = self.cache.get(path) {
            return cached.clone();
        }
        let loaded = (self.load)(path);
        self.cache.insert(path.to_path_buf(), loaded.clone());
        loaded
    }
}

/// Compute each seed's include problems, keyed by the seed's path as given.
/// Each problem sits on an include of a seed, even when the offending cycle
/// closes through files that are outside the lint set.
pub fn include_problems(seeds: &[(PathBuf, SyntaxNode)]) -> BTreeMap<PathBuf, Vec<IncludeProblem>> {
    include_problems_with_options(seeds, ParseOptions::default())
}

pub fn include_problems_with_options(
    seeds: &[(PathBuf, SyntaxNode)],
    options: ParseOptions,
) -> BTreeMap<PathBuf, Vec<IncludeProblem>> {
    let seeded: HashMap<_, _> = seeds
        .iter()
        .map(|(path, root)| (normalize_path(path), root))
        .collect();
    problems_for_seeds(seeds.iter().map(|(path, _)| path), |path| {
        if let Some(root) = seeded.get(path) {
            return Ok(normalized_include_edges(root, path));
        }
        edges_from_disk(path, options)
    })
}

/// Use the discovery pass's edges for files that were already parsed and its
/// saved source text for files seen in another edition. Fix passes replace
/// their current seed's edges while still sharing the unchanged descendants.
pub(crate) fn include_problems_from_catalog(
    paths: &[PathBuf],
    options: ParseOptions,
    catalog: &ContextCatalog,
    changed: Option<(&Path, &SyntaxNode)>,
) -> BTreeMap<PathBuf, Vec<IncludeProblem>> {
    let edges = catalog.edges.get(&options);
    problems_for_seeds(paths, |path| {
        if let Some((seed, root)) = changed
            && seed == path
        {
            return Ok(normalized_include_edges(root, path));
        }
        if let Some(edges) = edges.and_then(|entries| entries.get(path)) {
            return Ok(Arc::clone(edges));
        }
        match catalog.sources.get(path) {
            Some(Ok(text)) => Ok(normalized_include_edges(
                &parse_with_options(text, options).cst,
                path,
            )),
            Some(Err(_)) => Err(unreadable_kind(path)),
            None => edges_from_disk(path, options),
        }
    })
}

fn unreadable_kind(path: &Path) -> IncludeProblemKind {
    if path.is_file() {
        IncludeProblemKind::Unreadable
    } else {
        IncludeProblemKind::Missing
    }
}

fn edges_from_disk(path: &Path, options: ParseOptions) -> LoadedEdges {
    if !path.is_file() {
        return Err(IncludeProblemKind::Missing);
    }
    let text = std::fs::read_to_string(path).map_err(|_| IncludeProblemKind::Unreadable)?;
    Ok(normalized_include_edges(
        &parse_with_options(&text, options).cst,
        path,
    ))
}

fn problems_for_seeds<'a>(
    paths: impl IntoIterator<Item = &'a PathBuf>,
    load: impl FnMut(&Path) -> LoadedEdges,
) -> BTreeMap<PathBuf, Vec<IncludeProblem>> {
    let mut store = EdgeStore {
        load,
        cache: HashMap::new(),
    };
    let mut out = BTreeMap::new();
    for path in paths {
        let norm = normalize_path(path);
        let Ok(edges) = store.edges(&norm) else {
            continue;
        };
        let mut problems = Vec::new();
        // The same file included into two different modules runs into two
        // namespaces, so only repeated targets in the same host are duplicates.
        let mut included = HashSet::new();
        for (index, edge) in edges.iter().enumerate() {
            let Some(target) = &edge.target else {
                continue;
            };
            let problem = match store.edges(target) {
                Err(kind) => Some(kind),
                Ok(_) if reaches(&mut store, target, &norm) => Some(IncludeProblemKind::Cycle),
                Ok(_) => None,
            };
            let repeated = !included.insert((target, &edge.host_suffix));
            for kind in problem
                .into_iter()
                .chain(repeated.then_some(IncludeProblemKind::Duplicate))
            {
                problems.push(IncludeProblem {
                    path: edge.path.clone(),
                    edge: index,
                    kind,
                });
            }
        }
        if !problems.is_empty() {
            out.insert(path.clone(), problems);
        }
    }
    out
}

/// Whether `goal` is reachable from `from` (inclusive) along include edges.
fn reaches<F: FnMut(&Path) -> LoadedEdges>(
    store: &mut EdgeStore<F>,
    from: &Path,
    goal: &Path,
) -> bool {
    let mut stack = vec![from.to_path_buf()];
    let mut visited = HashSet::new();
    while let Some(path) = stack.pop() {
        if path == goal {
            return true;
        }
        if !visited.insert(path.clone()) {
            continue;
        }
        if let Ok(edges) = store.edges(&path) {
            stack.extend(edges.iter().filter_map(|edge| edge.target.clone()));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(path: &Path, src: &str) -> (PathBuf, SyntaxNode) {
        (path.to_path_buf(), parse(src).cst)
    }

    #[test]
    fn missing_target_is_reported_on_the_seed() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.jl");
        let problems = include_problems(&[seed(&main, "include(\"gone.jl\")\n")]);
        assert_eq!(
            problems[&main],
            [IncludeProblem {
                path: "gone.jl".to_string(),
                edge: 0,
                kind: IncludeProblemKind::Missing,
            }]
        );
    }

    #[test]
    fn existing_target_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
        let main = dir.path().join("main.jl");
        let problems = include_problems(&[seed(&main, "include(\"a.jl\")\n")]);
        assert!(problems.is_empty());
    }

    #[test]
    fn seeded_target_needs_no_disk_file() {
        // Both files are in the lint set; neither exists on disk.
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.jl");
        let sub = dir.path().join("a.jl");
        let problems =
            include_problems(&[seed(&main, "include(\"a.jl\")\n"), seed(&sub, "x = 1\n")]);
        assert!(problems.is_empty());
    }

    #[test]
    fn cycle_through_a_disk_file_is_attributed_to_the_seed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jl"), "include(\"main.jl\")\n").unwrap();
        let main = dir.path().join("main.jl");
        let problems = include_problems(&[seed(&main, "include(\"a.jl\")\n")]);
        assert_eq!(problems[&main][0].kind, IncludeProblemKind::Cycle);
    }

    #[test]
    fn batch_uses_the_catalog_edges_and_reprojects_a_changed_seed() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.jl");
        let sibling = dir.path().join("sibling.jl");
        let paths = [main.clone(), sibling.clone()];
        let options = ParseOptions::default();
        std::fs::write(&main, "include(\"sibling.jl\")\n").unwrap();
        std::fs::write(&sibling, "include(\"main.jl\")\n").unwrap();
        let catalog = ContextCatalog::discover(&paths, &[], &BTreeMap::new(), None);
        std::fs::write(&sibling, "x = 1\n").unwrap();

        let problems = include_problems_from_catalog(&paths, options, &catalog, None);
        assert_eq!(problems[&main][0].kind, IncludeProblemKind::Cycle);
        assert_eq!(problems[&sibling][0].kind, IncludeProblemKind::Cycle);

        let changed = parse("include(\"absent.jl\")\n").cst;
        let problems = include_problems_from_catalog(
            std::slice::from_ref(&sibling),
            options,
            &catalog,
            Some((&sibling, &changed)),
        );
        assert_eq!(problems[&sibling][0].kind, IncludeProblemKind::Missing);
        assert_eq!(problems[&sibling][0].path, "absent.jl");
    }

    #[test]
    fn every_seed_on_a_cycle_blames_its_own_edge() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jl");
        let b = dir.path().join("b.jl");
        let problems = include_problems(&[
            seed(&a, "include(\"b.jl\")\n"),
            seed(&b, "include(\"a.jl\")\n"),
        ]);
        assert_eq!(problems[&a][0].kind, IncludeProblemKind::Cycle);
        assert_eq!(problems[&b][0].kind, IncludeProblemKind::Cycle);
    }

    #[test]
    fn a_repeated_target_is_reported_on_the_later_edge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
        let main = dir.path().join("main.jl");
        // Spelled differently, so only path resolution can tell they match.
        let problems = include_problems(&[seed(&main, "include(\"a.jl\")\ninclude(\"./a.jl\")\n")]);
        assert_eq!(
            problems[&main],
            [IncludeProblem {
                path: "./a.jl".to_string(),
                edge: 1,
                kind: IncludeProblemKind::Duplicate,
            }]
        );
    }

    #[test]
    fn a_repeat_into_another_module_is_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jl"), "x = 1\n").unwrap();
        let main = dir.path().join("main.jl");
        let src = "module A\ninclude(\"a.jl\")\nend\nmodule B\ninclude(\"a.jl\")\nend\n";
        assert!(include_problems(&[seed(&main, src)]).is_empty());
    }

    #[test]
    fn a_repeat_of_a_missing_file_is_both_problems() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.jl");
        let problems =
            include_problems(&[seed(&main, "include(\"gone.jl\")\ninclude(\"gone.jl\")\n")]);
        let kinds: Vec<_> = problems[&main].iter().map(|p| (p.edge, p.kind)).collect();
        assert_eq!(
            kinds,
            [
                (0, IncludeProblemKind::Missing),
                (1, IncludeProblemKind::Missing),
                (1, IncludeProblemKind::Duplicate),
            ]
        );
    }

    #[test]
    fn diamond_is_not_a_cycle() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jl"), "include(\"c.jl\")\n").unwrap();
        std::fs::write(dir.path().join("b.jl"), "include(\"c.jl\")\n").unwrap();
        std::fs::write(dir.path().join("c.jl"), "x = 1\n").unwrap();
        let main = dir.path().join("main.jl");
        let problems = include_problems(&[seed(&main, "include(\"a.jl\")\ninclude(\"b.jl\")\n")]);
        assert!(problems.is_empty());
    }
}
