//! Text utilities shared across the CLI, diagnostics, and the language server.
//!
//! This module owns the LSP-facing side: the text buffer (text stored as a
//! rope, which is also its own line index) and the `didChange` conversion. The
//! pure edit machinery ([`crate::parser::Edit`] and friends) lives in
//! `fatou-parser` and is *not* mirrored here — `crate::parser` is the one path
//! to it, so a reader never has to ask which of two spellings of `Edit` a given
//! import means.

pub mod buffer;
pub mod edit;

pub use buffer::{LineCol, PositionEncoding, TextBuffer};
pub use edit::apply_content_changes;
