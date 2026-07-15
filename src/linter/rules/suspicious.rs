//! Suspicious rules: code that is legal Julia but very likely not intended.

mod assignment_in_condition;
mod constant_condition;
mod module_shadows_parent;
mod nothing_comparison;

pub use assignment_in_condition::AssignmentInCondition;
pub use constant_condition::ConstantCondition;
pub use module_shadows_parent::ModuleShadowsParent;
pub use nothing_comparison::NothingComparison;
