//! The formatter: consumes the CST and produces formatted text via a
//! Wadler/Prettier-style document IR ([`ir`]) printed by a single best-fit
//! layout engine ([`printer`]) that makes all line-break decisions.
//!
//! The style is Fatou's own deterministic layout (Tenet 1: rule-based, no
//! persistent line breaks); there is no external reference formatter. The
//! per-construct [`rules`] lower the CST into IR; a transparent fallback keeps
//! unhandled constructs byte-identical while coverage grows (see `rules` and
//! `core`). Hand-authored fixtures (`tests/formatter.rs`) gate the output.

pub mod check;
pub mod core;
pub mod ir;
pub mod printer;
pub mod rules;
pub mod style;

pub use check::{ChangedFile, CheckError, CheckResult, check_paths};
pub use core::{
    FormatError, RangeFormatted, format, format_node, format_range, format_with_style,
    print_document,
};
pub use style::FormatStyle;
