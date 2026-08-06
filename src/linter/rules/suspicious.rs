//! Suspicious rules: code that is legal Julia but very likely not intended.

mod assignment_in_condition;
mod constant_condition;
mod discouraged_function;
mod index_from_length;
mod missing_comparison;
mod module_shadows_parent;
mod nothing_comparison;

pub use assignment_in_condition::AssignmentInCondition;
pub use constant_condition::ConstantCondition;
pub use discouraged_function::DiscouragedFunction;
pub use index_from_length::IndexFromLength;
pub use missing_comparison::MissingComparison;
pub use module_shadows_parent::ModuleShadowsParent;
pub use nothing_comparison::NothingComparison;
