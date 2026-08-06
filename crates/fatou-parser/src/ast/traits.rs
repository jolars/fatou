//! Shared node-shape traits, the rust-analyzer `ast::Has*` pattern. A node that
//! implements one gets a uniform accessor, and generic code can be written
//! against the trait instead of each concrete wrapper (e.g. "the argument list
//! of any callable shape").

use rowan::ast::{AstNode, support};

use super::nodes::{
    ArgList, Block, CallExpr, CatchClause, Condition, CurlyExpr, DotCallExpr, ElseClause,
    ElseifClause, FinallyClause, ForExpr, FunctionDef, IfExpr, IndexExpr, LetExpr, MacroCall,
    MacroDef, ModuleDef, StructDef, WhileExpr,
};
use crate::syntax::JuliaLanguage;

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
