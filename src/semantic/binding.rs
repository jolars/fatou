//! Bindings: one entry per introduced variable, with its definition site.

use rowan::TextRange;
use smol_str::SmolStr;

use super::scope::ScopeId;

/// Index of a [`Binding`] in the model's binding arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingId(pub u32);

/// How a binding was introduced. Feeds later phases: semantic-token
/// classification (function vs type vs module), completion item kinds, and
/// the unused-binding lint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// Assignment in a global scope, or a `global` declaration.
    Global,
    /// Assignment in a local scope, or a `local` declaration.
    Local,
    /// `const x = ...`.
    Const,
    /// A positional parameter (before `;` in a signature).
    Param,
    /// A keyword parameter (after `;` in a signature).
    KeywordParam,
    /// A `for`-loop or comprehension iteration variable.
    ForVar,
    /// A `let` binding.
    LetVar,
    /// A `catch` variable.
    CatchParam,
    /// A type parameter, from `{T}` on a struct or a `where` clause.
    TypeParam,
    /// A struct field.
    Field,
    /// A `function` definition's name (long or short form).
    Function,
    /// A `macro` definition's name.
    Macro,
    /// A `struct`/`abstract type`/`primitive type` definition's name.
    Type,
    /// A `module` definition's name.
    Module,
    /// A name introduced by `using`/`import`: the last path component, its
    /// `as` alias, or an explicit item (`using X: a`). Imported macros keep
    /// the `@` sigil in the name, which keeps them invisible to value
    /// lookups (resolution matches by name).
    Import,
}

/// One variable: a single binding covers every assignment to the same
/// resolved name (Julia locals span their whole enclosing block, so a
/// reassignment targets the same variable; those show up as `Write`
/// [`IdentRef`](super::IdentRef)s, not new bindings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: SmolStr,
    pub kind: BindingKind,
    /// The scope the binding lives in.
    pub scope: ScopeId,
    /// The defining identifier token's range (the first introduction site).
    pub def_range: TextRange,
    /// Whether any resolved identifier reads this binding.
    pub read: bool,
}
