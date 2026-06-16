//! The parse pipeline: `lex → parse_expr (Pratt) + structural (recursive
//! descent) → events → build_tree → rowan CST`.
//!
//! Losslessness is the core invariant: `reconstruct(text) == text` for all
//! inputs. The grammar is a walking skeleton over a Julia subset and grows
//! incrementally (see `TODO.md`); incremental reparse splicing is deferred (the
//! salsa layer in `crate::incremental` currently does a full parse per edit).

mod context;
mod core;
mod cursor;
mod diagnostics;
mod events;
mod expr;
mod lexer;
mod recovery;
mod structural;
mod tree_builder;

pub use core::{ParseDiagnostic, ParseOutput, parse, reconstruct};
