//! The formatter: consumes the CST and produces formatted text via a
//! Wadler/Prettier-style document IR ([`ir`]) printed by a single best-fit
//! layout engine ([`printer`]) that makes all line-break decisions.
//!
//! Target style is **Runic.jl**'s deterministic layout (Tenet 1: rule-based, no
//! persistent line breaks). The per-construct [`rules`] lower the CST into IR; a
//! transparent fallback keeps unhandled constructs byte-identical while coverage
//! grows (see `rules` and `core`). The Runic differential oracle
//! (`tests/runic_oracle.rs`) gates parity.

pub mod check;
pub mod core;
pub mod ir;
pub mod printer;
pub mod rules;
pub mod style;

pub use check::{ChangedFile, CheckError, CheckResult, check_paths};
pub use core::{FormatError, format, format_with_style, print_document};
pub use style::FormatStyle;
