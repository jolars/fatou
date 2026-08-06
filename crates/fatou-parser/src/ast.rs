//! Zero-cost typed wrappers over the CST — the typed AST interface, modeled on
//! rust-analyzer's `ast` module.
//!
//! Three layers, all thin views that only cast when a kind matches (no
//! allocation, no semantic analysis):
//!
//! - [`nodes`] — [`AstNode`] newtypes per node kind, their accessors, and the
//!   [`Expr`] expression sum.
//! - [`tokens`] — [`AstToken`] newtypes for strongly-typed tokens ([`Ident`],
//!   [`Operator`]).
//! - [`traits`] — the `Has*` shape traits ([`HasArgList`], [`HasBody`],
//!   [`HasCondition`]) shared across wrappers.
//!
//! This is the interface the linter, code actions, semantic builder, and LSP
//! handlers navigate the tree through. The formatter is deliberately exempt: it
//! lowers known kinds and recurses over everything else verbatim (transparent
//! fallback), so it works the raw CST directly. When adding a construct, add its
//! `ast_node!`/`ast_token!` entry, its accessors (via `support::`), any relevant
//! `Has*` impl, re-export it below, and add an accessor test.

pub use rowan::ast::{AstChildren, AstNode, AstPtr, SyntaxNodePtr, support};

pub mod nodes;
pub mod tokens;
pub mod traits;

pub use nodes::{
    AbstractDef, Arg, ArgList, ArrowExpr, AssignmentExpr, BeginExpr, BinaryExpr, Block, Braces,
    BreakExpr, CallExpr, CatchClause, CmdLiteral, Comprehension, ComprehensionIf, Condition,
    ConstStmt, ContinueExpr, CurlyExpr, DoExpr, DoParams, DotCallExpr, ElseClause, ElseifClause,
    EndMarker, ExportStmt, Expr, FinallyClause, ForBinding, ForExpr, FunctionDef, Generator,
    GlobalStmt, IfExpr, ImportStmt, IndexExpr, Interpolation, KeywordArg, LetBindings, LetExpr,
    Literal, LocalStmt, MacroCall, MacroDef, MacroName, MatrixExpr, MatrixRow, ModuleDef, Name,
    NonstandardIdentifier, Parameters, ParenExpr, PrimitiveDef, QuoteExpr, QuoteSym, ReturnExpr,
    Root, Signature, SplatExpr, StringLiteral, StructDef, TernaryExpr, TryExpr, TupleExpr,
    TypeAnnotation, UnaryExpr, UsingStmt, VectExpr, WhereExpr, WhileExpr, is_expr_kind,
};
pub use tokens::{AstToken, Ident, Operator, child_token};
pub use traits::{HasArgList, HasBody, HasCondition};
