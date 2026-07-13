//! Correctness rules: findings that point at a probable bug or dead code.

mod duplicate_argument;
mod unused_argument;
mod unused_binding;
mod unused_import;

pub use duplicate_argument::DuplicateArgument;
pub use unused_argument::UnusedArgument;
pub use unused_binding::UnusedBinding;
pub use unused_import::UnusedImport;
