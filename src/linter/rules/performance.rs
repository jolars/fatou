//! Performance rules: a rewrite that avoids real work.
//!
//! The distinction from [`readability`](super::readability) is what the
//! rewrite buys. A `readability` finding says two spellings agree on every
//! input and one of them is how Julia says it; a finding here says the
//! shorter spelling also does *less* — it stops allocating an intermediate
//! collection, or stops ordering one it only needs an extreme of.
//!
//! That is also why the fixes here are usually unsafe where `readability`'s
//! are safe. Each collection rewrite is equivalent for the collection the
//! author plainly had in mind and is not equivalent for every possible
//! operand, and telling those apart needs types the linter does not have. The
//! finding stands on its own; the fix waits for `--unsafe-fixes`. What decides
//! the category is what the rewrite *buys*, not how its fix is gated:
//! `fixed-regex` avoids the same kind of real work and its equivalence turns
//! on the literal it reads rather than on an operand's type, so its fix is
//! safe.

mod eager_broadcast;
mod fixed_regex;
mod length_findall;
mod sorted_extremum;

pub use eager_broadcast::EagerBroadcast;
pub use fixed_regex::FixedRegex;
pub use length_findall::LengthFindall;
pub use sorted_extremum::SortedExtremum;
