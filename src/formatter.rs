//! The formatter: consumes the CST and produces formatted text via a
//! Wadler/Prettier-style document IR ([`ir`]) printed by a single best-fit
//! layout engine ([`printer`]) that makes all line-break decisions.
//!
//! Target style is **Runic.jl**'s deterministic layout (Tenet 1: rule-based, no
//! persistent line breaks). The per-construct `rules` that build native IR are
//! deferred; today [`format`] is a lossless passthrough (see `core`).

pub mod check;
pub mod core;
pub mod ir;
pub mod printer;
pub mod style;

pub use check::{ChangedFile, CheckError, CheckResult, check_paths};
pub use core::{FormatError, format, format_with_style, print_document};
pub use style::FormatStyle;
