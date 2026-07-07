//! Text utilities shared across the CLI, diagnostics, and the language server.

pub mod edit;
pub mod line_index;

pub use edit::apply_content_changes;
pub use line_index::{LineCol, LineIndex};
