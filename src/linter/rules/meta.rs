//! Meta rules: findings about fatou's own `# fatou-ignore` directives rather
//! than about the Julia code around them.
//!
//! Every other category answers a question about the program; these answer a
//! question about the *annotations on* the program — a rule ID that no longer
//! exists, a directive that silences everything, one that explains nothing, one
//! that suppresses nothing. They are what keeps the suppression comments honest
//! as the rule set moves under them, and they are entirely language-independent:
//! nothing here inspects Julia at all, only [`SuppressionMap::directives`].
//!
//! [`SuppressionMap::directives`]: crate::linter::suppression::SuppressionMap::directives
//!
//! Two shared facts about how these rules are themselves suppressed:
//!
//! - A *node* directive can never silence one, because its target is the next
//!   non-trivia sibling and a comment is trivia — there is no way to attach one
//!   to the comment below it. `# fatou-ignore-file <rule>` is the escape hatch.
//! - A directive never suppresses a finding that lies inside its own comment
//!   (see `SuppressionMap::applies`), so `# fatou-ignore-file:` stays reportable
//!   as a `blanket-suppression` instead of silencing the one rule that exists to
//!   report it.

mod blanket_suppression;
mod misnamed_suppression;
mod outdated_suppression;
mod unexplained_suppression;

pub use blanket_suppression::BlanketSuppression;
pub use misnamed_suppression::MisnamedSuppression;
pub use outdated_suppression::OutdatedSuppression;
pub use unexplained_suppression::UnexplainedSuppression;
