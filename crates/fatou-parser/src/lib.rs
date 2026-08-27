//! Lossless CST parser, typed AST wrappers, and incremental reparser for the
//! Julia language.
//!
//! This crate is the parsing engine of the [fatou](https://fatou.dev) CLI,
//! extracted so other tools can embed it. It parses Julia source into a
//! [rowan](https://docs.rs/rowan) concrete syntax tree that preserves every
//! byte of the input (`reconstruct(text) == text` for all inputs), reports
//! recoverable diagnostics instead of failing, and supports incremental
//! reparsing of edited buffers.
//!
//! # Main entry points
//!
//! - [`parser::parse`] — parse source text into a [`parser::ParseOutput`]
//!   (green tree plus diagnostics).
//! - [`parser::reparse`] and [`parser::reparse_edits`] — incremental reparse
//!   of an edited buffer; a `None` means "do a full parse".
//! - [`syntax`] — the [`syntax::SyntaxKind`] enum and rowan node/token
//!   aliases shared by every consumer of the tree.
//! - [`ast`] — typed node wrappers over the CST, including
//!   [`ast::DocAttachment`] for documentation syntax and statically recoverable
//!   string payloads.
//!
//! The crate is `wasm32-unknown-unknown`-compatible: no filesystem, process,
//! thread, or clock use.

pub mod ast;
mod keywords;
pub mod parser;
pub mod syntax;
mod tokens;
