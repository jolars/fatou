//! Suspicious rules: code that is legal Julia but very likely not intended.

mod assignment_in_condition;

pub use assignment_in_condition::AssignmentInCondition;
