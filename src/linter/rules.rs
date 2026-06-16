//! The lint rule trait and registry.
//!
//! No rules ship in the groundwork phase — [`all_rules`] is empty. Rules land in
//! a later phase (see `TODO.md`); the trait and resolution machinery are in
//! place so adding one is a localized change.

use std::path::Path;

use crate::linter::diagnostic::{Diagnostic, Severity};
use crate::syntax::SyntaxNode;

/// What a rule sees when it runs against one file.
pub struct RuleContext<'a> {
    pub path: Option<&'a Path>,
    pub root: &'a SyntaxNode,
}

pub trait Rule: Send + Sync {
    /// The stable rule identifier (e.g. `unused-binding`).
    fn id(&self) -> &'static str;

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn default_enabled(&self) -> bool {
        true
    }

    /// Run the rule, emitting diagnostics. Suppression filtering happens at the
    /// check layer, not here.
    fn run(&self, ctx: &RuleContext<'_>) -> Vec<Diagnostic>;
}

/// Every rule the linter knows about. Empty for now.
pub fn all_rules() -> Vec<Box<dyn Rule>> {
    Vec::new()
}

/// The set of rules enabled for a run, after applying `select`/`ignore`.
pub struct ResolvedRules {
    rules: Vec<Box<dyn Rule>>,
}

impl ResolvedRules {
    pub fn resolve(select: Option<&[String]>, ignore: &[String]) -> Self {
        let rules = all_rules()
            .into_iter()
            .filter(|rule| {
                let enabled = match select {
                    Some(selected) => selected.iter().any(|id| id == rule.id()),
                    None => rule.default_enabled(),
                };
                enabled && !ignore.iter().any(|id| id == rule.id())
            })
            .collect();
        Self { rules }
    }

    pub fn run(&self, ctx: &RuleContext<'_>) -> Vec<Diagnostic> {
        self.rules.iter().flat_map(|rule| rule.run(ctx)).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}
