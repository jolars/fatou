//! Correctness rules: findings that point at a probable bug or dead code.

mod break_outside_loop;
mod duplicate_argument;
mod noteq_definition;
mod undefined_name;
mod unused_argument;
mod unused_binding;
mod unused_import;
mod unused_type_parameter;

pub use break_outside_loop::BreakOutsideLoop;
pub use duplicate_argument::DuplicateArgument;
pub use noteq_definition::NotEqDefinition;
pub use undefined_name::UndefinedName;
pub use unused_argument::UnusedArgument;
pub use unused_binding::UnusedBinding;
pub use unused_import::UnusedImport;
pub use unused_type_parameter::UnusedTypeParameter;
