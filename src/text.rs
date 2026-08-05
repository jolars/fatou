//! Text utilities shared across the CLI, diagnostics, and the language server.

pub mod edit;
pub mod line_index;

pub use edit::{Edit, apply_content_changes, apply_edits, diff_edit, try_apply_edits};
pub use line_index::{LineCol, LineIndex, PositionEncoding};
