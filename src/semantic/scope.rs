//! Scopes: the tree of binding regions built from one CST walk.

use rowan::TextRange;

use super::binding::BindingId;

/// Index of a [`Scope`] in the model's scope arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub u32);

/// What construct introduced a scope. Julia distinguishes *global* scopes
/// (the file top level and each `module` body), *hard* local scopes (an
/// assignment always creates a local there unless the name is already local
/// in an enclosing local scope or declared `global`), and *soft* local
/// scopes (`for`/`while`/`try` — same rule in this model; see the module
/// docs of [`crate::semantic`] for the top-level ambiguity we resolve to
/// non-interactive semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// The file's top level: the global scope of the implicit module.
    File,
    /// A `module`/`baremodule` body: a fresh global scope. Name resolution
    /// stops here — enclosing globals are *not* visible inside.
    Module,
    /// A function-like body: `function`, `macro`, short-form `f(x) = ...`,
    /// anonymous forms, `->`, and `do` blocks.
    Function,
    /// One `let` binding's extent (bindings chain: each sees the previous).
    Let,
    /// A comprehension or generator clause's extent.
    Comprehension,
    /// A `struct`/`abstract`/`primitive` body: type parameters and fields.
    Struct,
    /// A `for` loop: iteration variables plus body (soft).
    For,
    /// A `while` loop's condition and body (soft).
    While,
    /// A `try` block (soft).
    Try,
    /// A `catch` block, with its optional catch variable (soft).
    Catch,
    /// A `finally` block (soft).
    Finally,
}

impl ScopeKind {
    /// Global scopes terminate name resolution and receive `global` bindings.
    pub fn is_global(self) -> bool {
        matches!(self, ScopeKind::File | ScopeKind::Module)
    }

    /// Hard local scopes; the distinction only matters for diagnostics on
    /// the top-level soft-scope ambiguity, not for resolution in this model.
    pub fn is_hard(self) -> bool {
        !matches!(
            self,
            ScopeKind::For
                | ScopeKind::While
                | ScopeKind::Try
                | ScopeKind::Catch
                | ScopeKind::Finally
        )
    }
}

/// A node in the scope tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub kind: ScopeKind,
    /// The enclosing scope; `None` only for the file scope.
    pub parent: Option<ScopeId>,
    /// The text extent used by position→scope lookups.
    pub range: TextRange,
    /// Bindings introduced directly in this scope, in introduction order.
    pub bindings: Vec<BindingId>,
}
