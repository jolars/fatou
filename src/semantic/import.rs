//! The import model: `using`/`import` statements as a per-file loaded-modules
//! list, `export`/`public` name lists, and qualified reads (`Foo.bar`) kept
//! separate from bare free reads. These feed the range-free firewall queries
//! (`file_exports`, `file_free_reads`, `file_qualified_reads`) and the
//! package-index resolution order of TODO.md Phase 3.

use rowan::TextRange;
use smol_str::SmolStr;

use super::binding::BindingId;
use super::scope::ScopeId;

/// Whether a loaded-modules entry came from `using` or `import`. The
/// difference matters to resolution: `using X` additionally attaches `X`'s
/// exports (resolved later against the package index), `import X` binds only
/// the module name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadKind {
    Using,
    Import,
}

/// A dotted module path in a `using`/`import` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePath {
    /// Leading relative dots: `.A` is 1, `..A` is 2, `...A` is 3.
    pub leading_dots: u32,
    /// The dotted components in order. Macro components keep the `@` sigil
    /// (`Base.Threads.@spawn` → `["Base", "Threads", "@spawn"]`), operator
    /// components are their text (`==`, `+`), and interpolated components
    /// (`import $A`) are omitted.
    pub components: Vec<SmolStr>,
}

/// One name after the colon in `using X: a, b as c`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportItem {
    /// The imported name; `@` retained for macro items.
    pub name: SmolStr,
    /// The `as` rename, when present.
    pub alias: Option<SmolStr>,
    /// The item clause's range.
    pub range: TextRange,
}

/// One `using`/`import` path clause, in source order — `import A, B` yields
/// two entries, `using X: a, b` one entry with [`items`](Self::items).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleLoad {
    pub kind: LoadKind,
    pub path: ModulePath,
    /// A whole-path rename (`import X as Y`); items carry their own aliases.
    pub alias: Option<SmolStr>,
    /// `None` for the whole-module form (`using X`); `Some` for an explicit
    /// item list (`using X: a, b`), which the colon scopes to the statement's
    /// single base path.
    pub items: Option<Vec<ImportItem>>,
    /// The clause's range (the whole statement for the item-list form).
    pub range: TextRange,
    /// The scope the statement appears in (each `module` body is its own).
    pub scope: ScopeId,
}

/// Whether a name list came from `export` or `public` (1.11+). Both mark the
/// name public API; only exported names are attached by `using`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Exported,
    Public,
}

/// One name in an `export`/`public` list, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry {
    /// The exported name; `@` retained for macro names.
    pub name: SmolStr,
    pub visibility: Visibility,
    /// The name token's range.
    pub range: TextRange,
    /// The scope the statement appears in.
    pub scope: ScopeId,
    /// The binding this entry resolves to in its global scope; `None` for
    /// names defined elsewhere (e.g. in an `include`d file) — deliberately
    /// not a free read.
    pub binding: Option<BindingId>,
}

/// A dotted access chain of plain names (`Foo.bar.baz`, `Base.@time`). The
/// root is additionally an ordinary [`IdentRef`](super::IdentRef) read; the
/// chain is recorded whole so consumers can tell a module-qualified name from
/// a bare free read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedRead {
    /// Every component including the root; a final macro component keeps `@`.
    pub path: Vec<SmolStr>,
    /// The whole chain's range.
    pub range: TextRange,
    /// The innermost scope the chain occurs in.
    pub scope: ScopeId,
    /// Whether the final component is a macro (`Base.@time`).
    pub is_macro: bool,
}
