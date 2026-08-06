//! Text utilities shared across the CLI, diagnostics, and the language server.
//!
//! The pure edit machinery ([`Edit`] and friends) lives in `fatou-parser` and
//! is re-exported here; this module owns the LSP-facing side (line index and
//! `didChange` conversion).

pub mod edit;
pub mod line_index;

pub use edit::apply_content_changes;
pub use fatou_parser::parser::{Edit, apply_edits, diff_edit, try_apply_edits};
pub use line_index::{LineCol, LineIndex, PositionEncoding};
