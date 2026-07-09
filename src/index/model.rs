//! The serializable data model of a harvested package: a module tree carrying
//! function groups (one per name, many methods for multiple dispatch), types,
//! consts, macros, and exported/`public` names, each stamped with a source
//! [`DefLocation`]. Produced by the [`harvest`](super::harvest) walk and stored
//! in the [`LibraryIndex`](crate::incremental::LibraryIndex) salsa input.
//!
//! Everything is position-relative and depot-independent: [`DefLocation::file`]
//! is relative to the package root, so a cached index stays valid if the depot
//! moves. Type positions are structured [`TypeExpr`](super::TypeExpr)s; value
//! positions (parameter defaults, const right-hand sides) are normalized source
//! strings, since they are arbitrary expressions rather than types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::TypeExpr;

/// A byte range in a source file, lowered from a [`rowan::TextRange`] so the
/// model does not depend on rowan's serde support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl From<rowan::TextRange> for Span {
    fn from(range: rowan::TextRange) -> Self {
        Span {
            start: range.start().into(),
            end: range.end().into(),
        }
    }
}

/// Where a definition's name is written: the file (relative to the package
/// root) and the name token's byte range. Enough for go-to-definition to jump
/// into the depot source and highlight the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefLocation {
    pub file: PathBuf,
    pub range: Span,
}

/// A harvested package: its name, the root module tree, and any non-fatal
/// diagnostics gathered along the way (unreadable files, unresolved includes,
/// parse errors, include cycles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageIndex {
    pub name: String,
    pub root: ModuleIndex,
    pub diagnostics: Vec<HarvestDiagnostic>,
}

/// One module: the top-level `module <Name>` of the package, or a nested
/// `module`/`baremodule`. `include`d files splice their top-level items into
/// the module that lexically contains the `include`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleIndex {
    pub name: String,
    /// `true` for `baremodule` (no implicit `Base`/`Core` import).
    pub bare: bool,
    pub loc: DefLocation,
    /// `export`/`public` names, in source order.
    pub exports: Vec<ExportedName>,
    /// Functions grouped by `(owner, name)`; each group holds every method.
    pub functions: Vec<FunctionGroup>,
    pub types: Vec<TypeDef>,
    pub consts: Vec<ConstDef>,
    pub macros: Vec<MacroDef>,
    pub submodules: Vec<ModuleIndex>,
}

/// An `export`ed or `public` name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedName {
    pub name: String,
    pub visibility: Visibility,
    pub loc: DefLocation,
}

/// Whether a name was made visible by `export` or `public` (1.11+).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Exported,
    Public,
}

/// Every method sharing one function name in a module — the multiple-dispatch
/// group. `owner` distinguishes a qualified extension (`Base.show`) from a bare
/// definition (`show`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionGroup {
    pub name: String,
    /// The module path a qualified extension targets (`Some(["Base"])` for
    /// `Base.show`); `None` for a bare `f`.
    pub owner: Option<Vec<String>>,
    pub methods: Vec<Method>,
    /// The docstring of the first documented method, promoted to the group.
    pub doc: Option<Docstring>,
}

/// One method: the signature of a single `function`/short-form definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Method {
    pub params: Vec<Param>,
    pub keyword_params: Vec<Param>,
    /// The `where` specs, each a [`TypeExpr::TypeVar`] (or [`TypeExpr::Raw`]).
    pub where_clauses: Vec<TypeExpr>,
    /// The declared return type of `f()::T`, if any.
    pub return_type: Option<TypeExpr>,
    /// `false` for a bodyless `function f end` method placeholder.
    pub has_body: bool,
    pub doc: Option<Docstring>,
    pub loc: DefLocation,
}

/// One parameter of a method: positional or keyword.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    /// `None` for an unnamed argument (`::Int`).
    pub name: Option<String>,
    pub type_annotation: Option<TypeExpr>,
    /// The default value as a normalized source string (`x = zeros(3)`).
    pub default: Option<String>,
    /// `true` for a slurping `x...`/`args::Int...` parameter.
    pub is_vararg: bool,
}

/// A `struct`, `abstract type`, or `primitive type` definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDef {
    pub name: String,
    pub kind: TypeKind,
    /// The `{T, S<:Real}` parameters, each a [`TypeExpr::TypeVar`].
    pub type_params: Vec<TypeExpr>,
    /// The declared supertype (right of `<:`), if any.
    pub supertype: Option<TypeExpr>,
    pub fields: Vec<Field>,
    pub doc: Option<Docstring>,
    pub loc: DefLocation,
}

/// The flavor of a [`TypeDef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeKind {
    Struct { mutable: bool },
    Abstract,
    Primitive { bits: Option<String> },
}

/// One struct field: a name, an optional `::T` annotation, and an optional
/// `@kwdef`-style default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub type_annotation: Option<TypeExpr>,
    pub default: Option<String>,
}

/// A `const` binding. `const a, b = 1, 2` yields one [`ConstDef`] per name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstDef {
    pub name: String,
    /// The right-hand side as a truncated normalized source string, if present.
    pub value_repr: Option<String>,
    pub doc: Option<Docstring>,
    pub loc: DefLocation,
}

/// A `macro` definition. `name` keeps the `@` sigil.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroDef {
    pub name: String,
    pub params: Vec<Param>,
    pub doc: Option<Docstring>,
    pub loc: DefLocation,
}

/// A docstring attached to a definition: the string literal (or `@doc` string)
/// that immediately precedes it, joined raw (no dedent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Docstring {
    pub text: String,
    pub loc: DefLocation,
}

/// A non-fatal problem encountered while harvesting. Harvesting is best-effort:
/// these are recorded and the walk continues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarvestDiagnostic {
    /// The package's `src/<Name>.jl` entry file was missing.
    EntryFileMissing { path: PathBuf },
    /// A source file could not be read.
    ReadError { path: PathBuf, message: String },
    /// An `include` chain reached a file already being walked, or a file
    /// already walked (duplicate include); it is walked only once.
    IncludeCycle { path: PathBuf },
    /// A static `include("path")` pointed at a file that could not be read, or
    /// a dynamic/interpolated/qualified `include` that cannot be resolved.
    UnresolvedInclude { raw: String, from: PathBuf },
    /// A file parsed with `count` errors; its recoverable tree was still walked.
    ParseError { path: PathBuf, count: usize },
}
