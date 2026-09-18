//! File-local recognition of Test macros, shared by scope building and linting.
//!
//! Only a preceding, visible Test load supplies this static contract. Unknown
//! macros remain opaque to recognition; analysis never expands or runs them.

use crate::ast::{AstNode, AstToken, MacroCall};

use super::{BindingKind, LoadKind, ModuleLoad, ScopeId, SemanticModel};

impl SemanticModel {
    pub(crate) fn is_test_macro(&self, call: &MacroCall, scope: ScopeId, expected: &str) -> bool {
        let Some(name) = call.name() else {
            return false;
        };
        let parts: Vec<_> = name.ident_tokens().collect();
        let (called, qualified) = match parts.as_slice() {
            [called] => (called.text(), false),
            [qualifier, called] if called.text() == expected => (qualifier.text(), true),
            _ => return false,
        };
        let binding = self
            .resolve_name(called, scope, !qualified)
            .map(|id| self.binding(id));
        if binding.is_some_and(|binding| binding.kind != BindingKind::Import)
            || (qualified && binding.is_none())
        {
            return false;
        }
        self.module_loads.iter().any(|load| {
            if load.path.leading_dots != 0
                || load.range.end() > call.syntax().text_range().start()
                || !self.load_visible(load, scope)
            {
                return false;
            }
            let from_test = load.path.components.as_slice() == ["Test"];
            if let Some(binding) = binding {
                // The import must introduce this exact binding, not merely
                // another macro or module with the same spelling.
                if binding.scope != load.scope || !load.range.contains_range(binding.def_range) {
                    return false;
                }
                if qualified {
                    return from_test
                        && load.items.is_none()
                        && load.alias.as_deref().unwrap_or("Test") == called;
                }
                if let Some(items) = &load.items {
                    return from_test
                        && items.iter().any(|item| {
                            item.name.strip_prefix('@') == Some(expected)
                                && item
                                    .alias
                                    .as_deref()
                                    .unwrap_or(&item.name)
                                    .trim_start_matches('@')
                                    == called
                                && item.range.contains_range(binding.def_range)
                        });
                }
                return load.path.components.len() == 2
                    && load.path.components[0] == "Test"
                    && load.path.components[1].strip_prefix('@') == Some(expected)
                    && load
                        .alias
                        .as_deref()
                        .unwrap_or(&load.path.components[1])
                        .trim_start_matches('@')
                        == called;
            }
            from_test
                && load.items.is_none()
                && load.kind == LoadKind::Using
                && load.alias.is_none()
                && called == expected
        })
    }

    fn load_visible(&self, load: &ModuleLoad, scope: ScopeId) -> bool {
        let mut cursor = Some(scope);
        while let Some(id) = cursor {
            if id == load.scope {
                return true;
            }
            let scope = self.scope(id);
            cursor = if scope.kind.is_global() {
                None
            } else {
                scope.parent
            };
        }
        false
    }
}
