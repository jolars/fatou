//! Range-free per-file projections of the [`SemanticModel`] — the *firewall*
//! between per-file analysis and cross-file resolution.
//!
//! Each projection strips text ranges, returning only names (or resolved
//! include targets). Editing a function body, or any edit that merely shifts
//! positions, changes the range-carrying [`SemanticModel`] but leaves these
//! projections *equal*, so the salsa queries that wrap them (see
//! [`crate::incremental`]) backdate and the project-level memos built on top
//! are not rebuilt on every keystroke. This mirrors arity's
//! `src/project/exports.rs`.
//!
//! The three name-set projections read the [`SemanticModel`]; [`include_edges`]
//! reads the parse tree directly (an `include` is an ordinary call, not a
//! binding), exactly as arity's `source_edges` reads the tree rather than the
//! model.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rowan::ast::AstNode;

use crate::ast::{AstToken, CallExpr, Expr, HasArgList};
use crate::semantic::{ScopeKind, SemanticModel};
use crate::syntax::SyntaxNode;

/// The names bound at file (top) level — what another file that `include`s this
/// one sees. Every binding whose scope is the file top level, `import`/`using`
/// names included.
///
/// A `BTreeSet` so equality is order-independent: editing a function body
/// changes the [`SemanticModel`] but leaves this set equal, so downstream
/// cross-file queries short-circuit.
pub fn file_exports(model: &SemanticModel) -> BTreeSet<String> {
    model
        .bindings()
        .iter()
        .filter(|binding| model.scope(binding.scope).kind == ScopeKind::File)
        .map(|binding| binding.name.to_string())
        .collect()
}

/// The names this file reads but binds nowhere in it — candidates for
/// resolution against another file, `Base`, or a package. The mirror of
/// [`file_exports`] (drives cross-file *use*, so a binding read only in a
/// sibling file isn't flagged unused).
pub fn file_free_reads(model: &SemanticModel) -> BTreeSet<String> {
    model
        .free_reads()
        .map(|ident| ident.name.to_string())
        .collect()
}

/// The module-qualified names this file references (`Foo.bar`, `Base.@time`),
/// each as its full dotted path. Kept separate from [`file_free_reads`]: a
/// qualified name names a member of another module, not a bare free read.
pub fn file_qualified_reads(model: &SemanticModel) -> BTreeSet<String> {
    model
        .qualified_reads()
        .iter()
        .map(|read| {
            read.path
                .iter()
                .map(|component| component.as_str())
                .collect::<Vec<_>>()
                .join(".")
        })
        .collect()
}

/// One static `include("path")` edge from this file to another source file.
/// Range-free (carries no `TextRange`) so it survives position-shifting edits;
/// a consumer recovers the call's span from the fresh parse tree per request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, salsa::Update)]
pub struct IncludeEdge {
    /// The literal string passed to `include`, exactly as written
    /// (`include("sub/a.jl")` → `"sub/a.jl"`).
    pub raw: String,
    /// `raw` resolved against the including file's directory, when that
    /// directory is known. An absolute `raw` is taken as-is.
    pub target: Option<PathBuf>,
}

/// The file's static `include("literal")` edges, in source order.
///
/// Only *statically resolvable* includes count: the callee must be the bare
/// name `include` (not `M.include`) and its sole argument a plain string
/// literal. Dynamic (`include(f)`), interpolated (`include("$d/a.jl")`),
/// prefixed (`include(raw"a.jl")`), and two-argument (`include(mapexpr, path)`)
/// forms are skipped — they cannot be resolved without evaluation.
///
/// `base_dir` is the including file's directory (`path.parent()`); a relative
/// `raw` is joined onto it to produce [`IncludeEdge::target`].
pub fn include_edges(root: &SyntaxNode, base_dir: Option<&Path>) -> Vec<IncludeEdge> {
    root.descendants()
        .filter_map(CallExpr::cast)
        .filter_map(|call| include_target(&call))
        .map(|raw| {
            let target = resolve_target(&raw, base_dir);
            IncludeEdge { raw, target }
        })
        .collect()
}

/// The literal path of `call` if it is a static `include("literal")`, else
/// `None`.
pub(crate) fn include_target(call: &CallExpr) -> Option<String> {
    // The callee must be the bare name `include` (a qualified `M.include` is a
    // `BinaryExpr`, an operator call a token — neither is an `Expr::Name`).
    let Expr::Name(callee) = call.callee()? else {
        return None;
    };
    if callee.ident()?.text() != "include" {
        return None;
    }

    // Exactly one argument, or it is `include(mapexpr, path)` — not static.
    let mut args = call.arg_list()?.args();
    let arg = args.next()?;
    if args.next().is_some() {
        return None;
    }

    // A plain string literal: no prefix (`raw"…"`) and no interpolation.
    let Expr::StringLiteral(string) = arg.expr()? else {
        return None;
    };
    if string.prefix().is_some() || string.interpolations().next().is_some() {
        return None;
    }
    Some(
        string
            .content_tokens()
            .map(|token| token.text().to_string())
            .collect(),
    )
}

/// Resolve an include's literal path against the including file's directory.
/// Absolute paths are taken as-is; a relative path needs a known `base_dir`.
pub(crate) fn resolve_target(raw: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        base_dir.map(|dir| dir.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn model_of(src: &str) -> SemanticModel {
        SemanticModel::build(&parse(src).cst)
    }

    fn names(set: &BTreeSet<String>) -> Vec<&str> {
        set.iter().map(String::as_str).collect()
    }

    fn edges_of(src: &str, base_dir: Option<&Path>) -> Vec<IncludeEdge> {
        include_edges(&parse(src).cst, base_dir)
    }

    #[test]
    fn exports_are_top_level_bindings_including_imports() {
        let m = model_of("f() = 1\nx = 2\nimport A\n");
        assert_eq!(names(&file_exports(&m)), ["A", "f", "x"]);
    }

    #[test]
    fn exports_exclude_params_and_function_locals() {
        let m = model_of("function g(a)\n    t = a\n    t\nend\n");
        assert_eq!(names(&file_exports(&m)), ["g"]);
    }

    #[test]
    fn exports_exclude_module_interior() {
        // `M` is a top-level binding; `y` lives in the module scope.
        let m = model_of("module M\ny = 1\nend\n");
        assert_eq!(names(&file_exports(&m)), ["M"]);
    }

    #[test]
    fn free_reads_are_the_unbound_names() {
        let m = model_of("f() = 1\ny = sin(x)\n");
        assert_eq!(names(&file_free_reads(&m)), ["sin", "x"]);
    }

    #[test]
    fn qualified_reads_join_the_full_dotted_path() {
        let m = model_of("a.b.c\nBase.@time f()\n");
        assert_eq!(names(&file_qualified_reads(&m)), ["Base.@time", "a.b.c"]);
    }

    #[test]
    fn collects_static_includes_in_source_order() {
        let edges = edges_of("include(\"a.jl\")\ninclude(\"sub/b.jl\")\n", None);
        let raws: Vec<_> = edges.iter().map(|edge| edge.raw.as_str()).collect();
        assert_eq!(raws, ["a.jl", "sub/b.jl"]);
        assert!(edges.iter().all(|edge| edge.target.is_none()));
    }

    #[test]
    fn resolves_relative_include_against_base_dir() {
        let edges = edges_of("include(\"sub/b.jl\")\n", Some(Path::new("/proj/src")));
        assert_eq!(edges[0].target, Some(PathBuf::from("/proj/src/sub/b.jl")));
    }

    #[test]
    fn absolute_include_ignores_base_dir() {
        let edges = edges_of("include(\"/etc/a.jl\")\n", Some(Path::new("/proj")));
        assert_eq!(edges[0].target, Some(PathBuf::from("/etc/a.jl")));
    }

    #[test]
    fn skips_dynamic_interpolated_qualified_and_two_arg_includes() {
        let edges = edges_of(
            "include(x)\ninclude(\"$d/a.jl\")\nM.include(\"a.jl\")\ninclude(f, \"a.jl\")\n",
            None,
        );
        assert!(edges.is_empty(), "only static bare `include(\"…\")` counts");
    }
}
