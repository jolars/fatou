//! A lossless parser for Julia's Markdown dialect and Documenter syntax.
//!
//! Documentation strings reach this module after Julia string decoding. All
//! ranges are therefore relative to the decoded text; callers working from a
//! Julia docstring can compose them with [`crate::ast::DocSourceMap`]. Parsing
//! is entirely local and never evaluates Julia or invokes a Julia process.
//!
//! Inline constructs span paragraph continuations. The CST retains original
//! newlines and container prefixes, while typed payload accessors join ordinary
//! continuations with spaces. Wrapped payloads can have multiple content tokens;
//! each token retains its exact decoded-text range.
//!
//! Fenced code in lists and admonitions retains container prefixes as trivia.
//! Code payloads remove only those prefixes, preserving body indentation, and
//! each content token maps directly to the original decoded bytes.
//!
//! Inline `--` and `---` follow Julia 1.13's en-dash and em-dash semantics.
//! Their CST tokens retain the original spelling; typed navigation exposes
//! [`ast::Inline::EnDash`] and [`ast::Inline::EmDash`], and heading content
//! uses the corresponding Unicode characters.

pub mod ast;
mod parser;
pub mod syntax;

pub use parser::{DiagnosticKind, ParseDiagnostic, ParseOutput, parse, reconstruct};
