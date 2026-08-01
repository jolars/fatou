//! Correctness rules: findings that point at a probable bug or dead code.

mod break_outside_loop;
mod call_arity;
mod duplicate_argument;
mod include_cycle;
mod julia_version_compat;
mod missing_include_file;
mod noteq_definition;
mod redefined_constant;
mod undefined_name;
mod unused_argument;
mod unused_binding;
mod unused_import;
mod unused_type_parameter;

pub use break_outside_loop::BreakOutsideLoop;
pub use call_arity::CallArity;
pub use duplicate_argument::DuplicateArgument;
pub use include_cycle::IncludeCycle;
pub use julia_version_compat::JuliaVersionCompat;
pub use missing_include_file::MissingIncludeFile;
pub use noteq_definition::NotEqDefinition;
pub use redefined_constant::RedefinedConstant;
pub use undefined_name::UndefinedName;
pub use unused_argument::UnusedArgument;
pub use unused_binding::UnusedBinding;
pub use unused_import::UnusedImport;
pub use unused_type_parameter::UnusedTypeParameter;
