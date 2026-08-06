//! CLI bridge over the [`fatou_formatter`] engine.
//!
//! The formatting engine lives in the `fatou-formatter` crate; this module
//! re-exports it and hosts the CLI-side batch check API ([`check`]), which
//! owns the file walking, parallelism, and diffing that do not belong in the
//! published engine.

pub mod check;

pub use fatou_formatter::formatter::*;

pub use check::{ChangedFile, CheckError, CheckResult, check_paths};
