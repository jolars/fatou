//! Shared node-shape traits, the rust-analyzer `ast::Has*` pattern. A node that
//! implements one gets a uniform accessor, and generic code can be written
//! against the trait instead of each concrete wrapper (e.g. "the argument list
//! of any callable shape").
//!
//! The traits require a concrete wrapper as the receiver. Consumers that walk
//! the tree polymorphically hold a raw [`SyntaxNode`] instead — the
//! kind-dispatching linter rules, the semantic builder, the CFG — so the same
//! two shapes are also exposed as the free functions [`body_of`] and
//! [`condition_of`], which take the node directly.

use rowan::ast::{AstNode, support};

use super::nodes::{
    ArgList, Block, CallExpr, CatchClause, Condition, CurlyExpr, DotCallExpr, ElseClause,
    ElseifClause, FinallyClause, ForExpr, FunctionDef, IfExpr, IndexExpr, LetExpr, MacroCall,
    MacroDef, ModuleDef, StructDef, WhileExpr,
};
use crate::syntax::{JuliaLanguage, SyntaxNode};

/// A node that carries an argument list (a call, an index, a type application,
/// a broadcast call, a macro call).
pub trait HasArgList: AstNode<Language = JuliaLanguage> {
    /// The node's argument list, if present.
    fn arg_list(&self) -> Option<ArgList> {
        support::child(self.syntax())
    }
}

/// A node whose contents are a block body closed by `end` (definitions, loops,
/// and block clauses).
pub trait HasBody: AstNode<Language = JuliaLanguage> {
    /// The body block, if present.
    fn body(&self) -> Option<Block> {
        support::child(self.syntax())
    }
}

/// A node guarded by a `CONDITION` test (`if`, `elseif`, `while`).
pub trait HasCondition: AstNode<Language = JuliaLanguage> {
    /// The guarding condition, if present.
    fn condition(&self) -> Option<Condition> {
        support::child(self.syntax())
    }
}

impl HasArgList for CallExpr {}
impl HasArgList for IndexExpr {}
impl HasArgList for DotCallExpr {}
impl HasArgList for CurlyExpr {}
impl HasArgList for MacroCall {}

impl HasBody for FunctionDef {}
impl HasBody for MacroDef {}
impl HasBody for StructDef {}
impl HasBody for ModuleDef {}
impl HasBody for WhileExpr {}
impl HasBody for ForExpr {}
impl HasBody for LetExpr {}
impl HasBody for ElseifClause {}
impl HasBody for ElseClause {}
impl HasBody for CatchClause {}
impl HasBody for FinallyClause {}

impl HasCondition for IfExpr {}
impl HasCondition for ElseifClause {}
impl HasCondition for WhileExpr {}

/// The `BLOCK` child that is a node's body, as a raw node.
///
/// The [`HasBody`] counterpart for the polymorphic walkers — they dispatch on
/// [`SyntaxKind`](crate::syntax::SyntaxKind), hold a [`SyntaxNode`], and pass
/// one on, so a typed [`Block`] here would only be unwrapped again at every
/// call site. Code that already holds a wrapper should use [`HasBody::body`],
/// which is this lookup with the type kept.
///
/// `None` for a node with no block child, including a bare `function f end`.
pub fn body_of(node: &SyntaxNode) -> Option<SyntaxNode> {
    support::child::<Block>(node).map(|block| block.syntax().clone())
}

/// The `CONDITION` child guarding a node, as a raw node — the
/// [`HasCondition`] counterpart to [`body_of`], with the same rationale.
pub fn condition_of(node: &SyntaxNode) -> Option<SyntaxNode> {
    support::child::<Condition>(node).map(|cond| cond.syntax().clone())
}
