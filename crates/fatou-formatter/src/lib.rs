//! Deterministic, rule-based formatter for the Julia language.
//!
//! This crate is the formatting engine of the [fatou](https://fatou.dev) CLI,
//! extracted so embedders (such as a dprint Wasm plugin) can use it without
//! the CLI's filesystem and process machinery. It formats a source string or
//! an already-parsed [`fatou_parser`] CST; batch file walking, caching, and
//! config discovery stay host-side.
//!
//! # Main entry points
//!
//! - [`format`] / [`format_with_style`] — format a source string.
//! - [`format_node`] / [`format_range`] — format an already-parsed
//!   [`fatou_parser::syntax::SyntaxNode`], whole or a byte range of it.
//! - [`FormatStyle`] — the layout knobs, with [`Default`] matching the fatou
//!   CLI's defaults.
//!
//! The crate is `wasm32-unknown-unknown`-compatible: no filesystem, process,
//! thread, or clock use.

pub mod formatter;

pub mod parser;
pub mod syntax;

/// The `rowan` version this crate's CST types are built on.
///
/// [`format_range`] takes a `rowan::TextRange`, so an embedder has to be able
/// to name it. Re-exporting the dependency keeps that caller version-matched
/// with us instead of making them guess a compatible `rowan` in their own
/// `Cargo.toml`.
pub use rowan;

pub use formatter::{
    FormatError, FormatStyle, LineEnding, RangeFormatted, format, format_node, format_range,
    format_with_style, print_document,
};
