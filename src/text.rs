//! Text utilities shared across the CLI, diagnostics, and the language server.
//!
//! This module owns the LSP-facing side: the line index and the `didChange`
//! conversion. The pure edit machinery ([`crate::parser::Edit`] and friends)
//! lives in `fatou-parser` and is *not* mirrored here — `crate::parser` is the
//! one path to it, so a reader never has to ask which of two spellings of
//! `Edit` a given import means.

pub mod buffer;
pub mod edit;
pub(crate) mod line_diff;
pub mod line_index;

pub use buffer::TextBuffer;
pub use edit::apply_content_changes;
pub use line_index::{LineCol, LineIndex, LineStarts, PositionEncoding};
