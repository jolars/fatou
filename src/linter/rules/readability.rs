//! Readability rules: an idiom rewrite that preserves behavior.
//!
//! The distinction from [`suspicious`](super::suspicious) is what the original
//! spelling *means*, not how it reads. A `suspicious` finding says the code
//! very likely does not do what its author intended; a `readability` finding
//! says it does exactly what was intended, in a longer way than Julia spells
//! it. That is why the rewrites here can be safe fixes: the two spellings agree
//! on every input. Where one variant of a rule's idiom does not — PCRE's `$`
//! is not quite `endswith`, as `string-boundary` records — the finding is
//! still a readability one and only that variant's fix waits for
//! `--unsafe-fixes`.

mod comparison_negation;
mod length_zero;
mod redundant_boolean;
mod string_boundary;
mod test_isa_call;
mod unnecessary_nesting;

pub use comparison_negation::ComparisonNegation;
pub use length_zero::LengthZero;
pub use redundant_boolean::RedundantBoolean;
pub use string_boundary::StringBoundary;
pub use test_isa_call::TestIsaCall;
pub use unnecessary_nesting::UnnecessaryNesting;
