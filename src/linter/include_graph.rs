//! CLI-side include following: the per-file include problems the
//! include-graph rules (`missing-include-file`, `include-cycle`) report.
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
//! - **Cycle**: the target transitively includes the seed again, the
//!   self-include being the smallest case.
//!
//! Only statically resolvable includes participate, exactly the edge set of
//! [`crate::project::include_edges`]; dynamic, interpolated, qualified, and
//! two-argument forms are invisible here as everywhere else.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::incremental::normalize_path;
use crate::parser::parse;
use crate::project::include_edges;
use crate::syntax::SyntaxNode;

/// What is wrong with one static `include("raw")` site of a linted file. The
/// rules match a problem back to its call site by the `raw` literal (range-free
/// like [`crate::project::IncludeEdge`], so the graph walk never tracks spans).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludeProblem {
    /// The literal string passed to `include`, exactly as written.
    pub raw: String,
    pub kind: IncludeProblemKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncludeProblemKind {
    /// The resolved target is not a file on disk.
    Missing,
    /// The target transitively `include`s the seed file again.
    Cycle,
}

/// The memoized edge store: normalized path -> that file's resolved include
/// targets (normalized). Seeds are answered from their parsed trees; other
/// paths are read and parsed from disk once. `None` marks an unloadable file
/// (unreadable, or a directory), a dead end for the cycle walk.
struct EdgeStore<'a> {
    seeded: HashMap<PathBuf, &'a SyntaxNode>,
    cache: HashMap<PathBuf, Option<Vec<PathBuf>>>,
}

impl<'a> EdgeStore<'a> {
    fn targets(&mut self, path: &Path) -> Option<&[PathBuf]> {
        if !self.cache.contains_key(path) {
            let loaded = match self.seeded.get(path) {
                Some(root) => Some(resolved_targets(root, path)),
                None => std::fs::read_to_string(path)
                    .ok()
                    .map(|text| resolved_targets(&parse(&text).cst, path)),
            };
            self.cache.insert(path.to_path_buf(), loaded);
        }
        self.cache.get(path).unwrap().as_deref()
    }

    /// Whether `path` exists as loadable source: seeded, or a file on disk.
    fn exists(&self, path: &Path) -> bool {
        self.seeded.contains_key(path) || path.is_file()
    }
}

/// The resolved include targets of one parsed file, normalized. Unresolvable
/// edges (none for an absolute, normalized `path`, whose parent always exists)
/// are dropped.
fn resolved_targets(root: &SyntaxNode, path: &Path) -> Vec<PathBuf> {
    include_edges(root, path.parent())
        .into_iter()
        .filter_map(|edge| edge.target.as_deref().map(normalize_path))
        .collect()
}

/// Compute each seed's include problems, keyed by the seed's path as given
/// (what the lint driver looks its report up under). A seed's problems all sit on
/// its own `include` calls; problems of files merely reached by the walk are
/// not reported (they are not being linted).
pub fn include_problems(seeds: &[(PathBuf, SyntaxNode)]) -> BTreeMap<PathBuf, Vec<IncludeProblem>> {
    let mut store = EdgeStore {
        seeded: seeds
            .iter()
            .map(|(path, root)| (normalize_path(path), root))
            .collect(),
        cache: HashMap::new(),
    };

    let mut out = BTreeMap::new();
    for (path, root) in seeds {
        let norm = normalize_path(path);
        let mut problems = Vec::new();
        for edge in include_edges(root, norm.parent()) {
            let Some(target) = edge.target.as_deref().map(normalize_path) else {
                continue;
            };
            if !store.exists(&target) {
                problems.push(IncludeProblem {
                    raw: edge.raw,
                    kind: IncludeProblemKind::Missing,
                });
            } else if reaches(&mut store, &target, &norm) {
                problems.push(IncludeProblem {
                    raw: edge.raw,
                    kind: IncludeProblemKind::Cycle,
                });
            }
        }
        if !problems.is_empty() {
            out.insert(path.clone(), problems);
        }
    }
    out
}

/// Whether `goal` is reachable from `from` (inclusive) along include edges — a
/// plain DFS over the memoized edge store.
fn reaches(store: &mut EdgeStore<'_>, from: &Path, goal: &Path) -> bool {
    let mut stack = vec![from.to_path_buf()];
    let mut visited = HashSet::new();
    while let Some(path) = stack.pop() {
        if path == goal {
            return true;
        }
        if !visited.insert(path.clone()) {
            continue;
        }
        if let Some(targets) = store.targets(&path) {
            stack.extend(targets.iter().cloned());
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
                raw: "gone.jl".to_string(),
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
