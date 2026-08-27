//! A lossless parser for Julia's Markdown dialect and Documenter syntax.
//!
//! Documentation strings reach this module after Julia string decoding. All
//! ranges are therefore relative to the decoded text; callers working from a
//! Julia docstring can compose them with [`crate::ast::DocSourceMap`]. Parsing
//! is entirely local and never evaluates Julia or invokes a Julia process.

pub mod ast;
mod parser;
pub mod syntax;

pub use parser::{DiagnosticKind, ParseDiagnostic, ParseOutput, parse, reconstruct};
