//! The parse pipeline: `lex → parse_expr (Pratt) + structural (recursive
//! descent) → events → build_tree → rowan CST`.
//!
//! Losslessness is the core invariant: `reconstruct(text) == text` for all
//! inputs. The grammar is a walking skeleton over a Julia subset and grows
//! incrementally (see `TODO.md`). Incremental reparse splicing lives in
//! [`reparse`] (a single edit) and [`reparse_edits`] (a chain of them); both
//! return `None` when no strategy applies, and the host's salsa layer
//! (`fatou::incremental`) then does a full parse.

mod context;
mod core;
mod cursor;
mod diagnostics;
mod edit;
mod events;
mod expr;
mod lexer;
mod recovery;
mod reparse;
mod sexpr;
mod structural;
mod tree_builder;
mod unescape;
mod unicode_ident;
mod unicode_ops;

pub use core::{ParseDiagnostic, ParseOutput, parse, reconstruct};
// Semver-loose: exposed for the fatou CLI's completion and semantic layers,
// not a stable part of this crate's API.
pub use lexer::{KEYWORDS, is_ident_continue, is_ident_start};
pub use reparse::{
    Edit, ReparseTier, Reparsed, apply_edits, diff_edit, reparse, reparse_edits, try_apply_edits,
};
// Test support, not API: the in-crate Tenet-4 assert and the
// `tests/incremental_reparse.rs` oracle harness share this one definition so
// they can never diverge, which is the only reason it is `pub` at all.
#[doc(hidden)]
pub use reparse::fingerprint;
pub use sexpr::{normalize_sexpr, to_juliasyntax_sexpr};
// A lossless CST hands out a literal's *source*; a consumer reading it as data
// (the `include` path resolver) needs the value it denotes.
pub use unescape::{StringDecodeError, string_value};
