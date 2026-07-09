//! The package index: a structured, serializable view of a Julia package's
//! public API, harvested from its source with fatou's own parser.
//!
//! [`harvest_package`] parses a package's `src/` entry file, follows static
//! `include()` chains to assemble the module tree, and extracts exported and
//! `public` names, function signatures grouped by name (multiple dispatch),
//! struct/abstract/primitive types with supertypes, consts, macros, and
//! docstrings — each as a [`model`] value stamped with a source location. The
//! result feeds the [`LibraryIndex`](crate::incremental::LibraryIndex) salsa
//! input, which later completion, hover, and go-to-definition read.

pub mod harvest;
pub mod model;
pub mod typeexpr;

pub use harvest::{harvest_package, harvest_package_named};
pub use model::{
    ConstDef, DefLocation, Docstring, ExportedName, Field, FunctionGroup, HarvestDiagnostic,
    MacroDef, Method, ModuleIndex, PackageIndex, Param, Span, TypeDef, TypeKind, Visibility,
};
pub use typeexpr::TypeExpr;
