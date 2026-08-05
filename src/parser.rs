//! The parse pipeline: `lex → parse_expr (Pratt) + structural (recursive
//! descent) → events → build_tree → rowan CST`.
//!
//! Losslessness is the core invariant: `reconstruct(text) == text` for all
//! inputs. The grammar is a walking skeleton over a Julia subset and grows
//! incrementally (see `TODO.md`). Incremental reparse splicing lives in
//! [`reparse`] (a single edit) and [`reparse_edits`] (a chain of them); both
//! return `None` when no strategy applies, and the salsa layer in
//! `crate::incremental` then does a full parse.

mod context;
mod core;
mod cursor;
mod diagnostics;
mod events;
mod expr;
mod lexer;
mod recovery;
mod reparse;
mod sexpr;
mod structural;
mod tree_builder;
mod unicode_ident;
mod unicode_ops;

pub use core::{ParseDiagnostic, ParseOutput, parse, reconstruct};
pub(crate) use lexer::{KEYWORDS, is_ident_continue, is_ident_start};
pub use reparse::{
    Edit, ReparseTier, Reparsed, apply_edits, diff_edit, fingerprint, reparse, reparse_edits,
    try_apply_edits,
};
pub use sexpr::{normalize_sexpr, to_juliasyntax_sexpr};
