//! Suspicious rules: code that is legal Julia but very likely not intended.

mod assignment_in_condition;
mod constant_condition;
mod discouraged_function;
mod index_from_length;
mod loop_variable_shadow;
mod missing_comparison;
mod module_shadows_parent;
mod non_public_access;
mod nothing_comparison;
mod shadowed_base_name;
mod test_bare_expression;
mod typeof_comparison;

pub use assignment_in_condition::AssignmentInCondition;
pub use constant_condition::ConstantCondition;
pub use discouraged_function::DiscouragedFunction;
pub use index_from_length::IndexFromLength;
pub use loop_variable_shadow::LoopVariableShadow;
pub use missing_comparison::MissingComparison;
pub use module_shadows_parent::ModuleShadowsParent;
pub use non_public_access::NonPublicAccess;
pub use nothing_comparison::NothingComparison;
pub use shadowed_base_name::ShadowedBaseName;
pub use test_bare_expression::TestBareExpression;
pub use typeof_comparison::TypeofComparison;
