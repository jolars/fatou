//! Readability rules: an idiom rewrite that preserves behavior.
//!
//! The distinction from [`suspicious`](super::suspicious) is what the original
//! spelling *means*, not how it reads. A `suspicious` finding says the code
//! very likely does not do what its author intended; a `readability` finding
//! says it does exactly what was intended, in a longer way than Julia spells
//! it. That is why the rewrites here can be safe fixes: the two spellings agree
//! on every input.

mod comparison_negation;
mod length_zero;
mod redundant_boolean;

pub use comparison_negation::ComparisonNegation;
pub use length_zero::LengthZero;
pub use redundant_boolean::RedundantBoolean;
