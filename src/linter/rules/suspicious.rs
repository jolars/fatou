//! Suspicious rules: code that is legal Julia but very likely not intended.

mod assignment_in_condition;
mod constant_condition;
mod discouraged_function;
mod index_from_length;
mod loop_variable_shadow;
mod missing_comparison;
mod module_shadows_parent;
mod nothing_comparison;
mod typeof_comparison;

pub use assignment_in_condition::AssignmentInCondition;
pub use constant_condition::ConstantCondition;
pub use discouraged_function::DiscouragedFunction;
pub use index_from_length::IndexFromLength;
pub use loop_variable_shadow::LoopVariableShadow;
pub use missing_comparison::MissingComparison;
pub use module_shadows_parent::ModuleShadowsParent;
pub use nothing_comparison::NothingComparison;
pub use typeof_comparison::TypeofComparison;
