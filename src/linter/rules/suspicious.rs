//! Suspicious rules: code that is legal Julia but very likely not intended.

mod assignment_in_condition;
mod nothing_comparison;

pub use assignment_in_condition::AssignmentInCondition;
pub use nothing_comparison::NothingComparison;
