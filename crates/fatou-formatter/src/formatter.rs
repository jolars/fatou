//! The formatter: consumes the CST and produces formatted text via a
//! Wadler/Prettier-style document IR ([`ir`]) printed by a single best-fit
//! layout engine ([`printer`]) that makes all line-break decisions.
//!
//! The style is Fatou's own deterministic layout (Tenet 1: rule-based, no
//! persistent line breaks); there is no external reference formatter. The
//! per-construct [`rules`] lower the CST into IR; a transparent fallback keeps
//! unhandled constructs byte-identical while coverage grows (see `rules` and
//! `core`). Hand-authored fixtures (`tests/formatter.rs`) gate the output.

pub mod core;
pub mod ir;
pub mod printer;
pub mod rules;
pub mod style;

pub use core::{
    FormatError, RangeFormatted, VerifiedFormatError, format, format_node, format_range,
    format_verified, format_verified_with_style, format_with_style, print_document,
};
pub use style::{FormatStyle, LineEnding};
