//! Zero-cost typed wrappers over the CST, using `rowan`'s `AstNode` support.
//! Each wrapper is a newtype around a [`SyntaxNode`](crate::syntax::SyntaxNode)
//! that only casts when the node's kind matches.

pub use rowan::ast::{AstChildren, AstNode, AstPtr, SyntaxNodePtr, support};

pub mod nodes;

pub use nodes::{
    ArgList, AssignmentExpr, BeginExpr, BinaryExpr, Block, CallExpr, Condition, ElseClause,
    ElseifClause, FunctionDef, IfExpr, IndexExpr, Literal, Name, ParenExpr, Root, Signature,
    UnaryExpr,
};
