//! `redefined-constant`: writing over a name Julia treats as constant, or
//! defining over a name that already holds a value.
//!
//! Three shapes, all runtime errors (or, for `const` over `const`, a runtime
//! warning) when both sites execute:
//! - reassigning a `const` binding (`const x = 1; x = 2`), including a second
//!   `const` declaration and augmented assignments;
//! - assigning to a global `function`/`struct`/`module` name (`f() = 1;
//!   f = 2`) — those definitions bind implicit constants;
//! - defining a function, or declaring a `const`, over a name that already
//!   holds a plain value (`x = 1; x() = 2`, `x = 1; const x = 2`).
//!
//! Driven by the semantic model: a redefinition is a non-definition `Write`
//! (or `ReadWrite`) occurrence on the binding, and the binding's kind says
//! what the first introduction was. The CST at the write site classifies the
//! second introduction (function definition name, `const` declaration target,
//! or plain assignment).
//!
//! False-positive guards:
//! - A def and a write in *disjoint* branches of the same `if` never execute
//!   together (`@static if`-style platform selection), so they are exempt —
//!   StaticLint's `in_same_if_branch`. A write merely *conditional* on the
//!   def (or vice versa) still flags.
//! - A function-definition write on a `Function` binding adds a method; on a
//!   `Type` binding it defines an outer constructor. Both legal, both skipped.
//! - Type-definition names (a second `struct S`) are skipped: an identical
//!   redefinition is legal and field comparison is out of scope.
//! - `macro` definition names are skipped entirely — `@x` and the value `x`
//!   are different namespaces even though the model resolves the definition
//!   sites to one binding.
//! - Local scopes only flag the value-then-function shape: local function
//!   names are ordinary rebindable locals, not constants.
//!
//! No fix: resolving a redefinition means renaming or deleting one of the two
//! sites, and the rule cannot know which one the author meant to keep.

use rowan::TextRange;

use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::{Access, BindingKind};
use crate::syntax::{SyntaxKind, SyntaxNode};

pub struct RedefinedConstant;

/// What construct a write occurrence's identifier sits in.
#[derive(PartialEq, Eq)]
enum WriteSite {
    /// The defined name of a `function` definition (long, bare, or short
    /// form).
    FunctionName,
    /// The defined name of a `macro` definition.
    MacroName,
    /// The defined name of a `struct`/`abstract`/`primitive`/`module`.
    TypeName,
    /// The target of a `const` declaration's assignment.
    ConstDecl,
    /// Any other assignment target.
    Plain,
}

/// Classify the write at `range` by walking from its `NAME` node up through
/// head-position wrappers to the defining construct.
fn classify_write(root: &SyntaxNode, range: TextRange) -> WriteSite {
    let token = match root.covering_element(range) {
        rowan::NodeOrToken::Token(t) => t,
        rowan::NodeOrToken::Node(_) => return WriteSite::Plain,
    };
    let Some(name) = token.parent().filter(|n| n.kind() == SyntaxKind::NAME) else {
        return WriteSite::Plain;
    };
    let mut child = name;
    let mut passed_call = false;
    while let Some(parent) = child.parent() {
        match parent.kind() {
            // Head-position wrappers between the name and its construct:
            // `f(x)`, `Foo{T}`, `f(x) where T`, `f(x)::Int`, destructuring
            // and grouping around a declaration target, `S <: Super`.
            SyntaxKind::CALL_EXPR
            | SyntaxKind::CURLY_EXPR
            | SyntaxKind::BINARY_EXPR
            | SyntaxKind::COMPARISON_EXPR => {
                if parent.children().next().as_ref() != Some(&child) {
                    return WriteSite::Plain;
                }
                passed_call |= parent.kind() == SyntaxKind::CALL_EXPR;
            }
            SyntaxKind::WHERE_EXPR
            | SyntaxKind::TYPE_ANNOTATION
            | SyntaxKind::PAREN_EXPR
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::BARE_TUPLE_EXPR
            | SyntaxKind::ARG
            | SyntaxKind::SPLAT_EXPR => {}
            SyntaxKind::SIGNATURE => {
                return match parent.parent().map(|p| p.kind()) {
                    Some(SyntaxKind::FUNCTION_DEF) => WriteSite::FunctionName,
                    Some(SyntaxKind::MACRO_DEF) => WriteSite::MacroName,
                    _ => WriteSite::TypeName,
                };
            }
            SyntaxKind::ASSIGNMENT_EXPR => {
                if parent.children().next().as_ref() != Some(&child) {
                    return WriteSite::Plain;
                }
                if passed_call {
                    // Short form `x() = ...`.
                    return WriteSite::FunctionName;
                }
                return match parent.parent().map(|p| p.kind()) {
                    Some(SyntaxKind::CONST_STMT) => WriteSite::ConstDecl,
                    _ => WriteSite::Plain,
                };
            }
            SyntaxKind::CONST_STMT => return WriteSite::ConstDecl,
            _ => return WriteSite::Plain,
        }
        child = parent;
    }
    WriteSite::Plain
}

/// The `(if-expression, branch)` pairs enclosing `range`, innermost first.
/// A branch is a direct `IF_EXPR` child holding code: the then-`BLOCK`, an
/// `ELSEIF_CLAUSE`, or the `ELSE_CLAUSE`.
fn branch_chain(root: &SyntaxNode, range: TextRange) -> Vec<(SyntaxNode, SyntaxNode)> {
    let mut chain = Vec::new();
    let mut node = match root.covering_element(range) {
        rowan::NodeOrToken::Token(t) => t.parent(),
        rowan::NodeOrToken::Node(n) => Some(n),
    };
    while let Some(current) = node {
        let parent = current.parent();
        if let Some(parent) = &parent
            && parent.kind() == SyntaxKind::IF_EXPR
            && matches!(
                current.kind(),
                SyntaxKind::BLOCK | SyntaxKind::ELSEIF_CLAUSE | SyntaxKind::ELSE_CLAUSE
            )
        {
            chain.push((parent.clone(), current));
        }
        node = parent;
    }
    chain
}

/// Whether `a` and `b` sit in different branches of a common `if`: at most
/// one of them can execute, so a redefinition between them is legal.
fn in_disjoint_branches(root: &SyntaxNode, a: TextRange, b: TextRange) -> bool {
    let chain_a = branch_chain(root, a);
    if chain_a.is_empty() {
        return false;
    }
    let chain_b = branch_chain(root, b);
    chain_a.iter().any(|(if_a, branch_a)| {
        chain_b
            .iter()
            .any(|(if_b, branch_b)| if_a == if_b && branch_a != branch_b)
    })
}

/// Kinds whose first introduction is a plain value a later `function` or
/// `const` definition cannot overwrite.
fn holds_value(kind: BindingKind) -> bool {
    matches!(
        kind,
        BindingKind::Global
            | BindingKind::Local
            | BindingKind::LetVar
            | BindingKind::Param
            | BindingKind::KeywordParam
            | BindingKind::ForVar
            | BindingKind::CatchParam
    )
}

impl Rule for RedefinedConstant {
    fn id(&self) -> &'static str {
        "redefined-constant"
    }

    fn description(&self) -> &'static str {
        "Flag a write that redefines a constant name, or defines over a name \
         that already holds a value: reassigning a `const` binding, assigning \
         to a global function, type, or module name (those bind implicit \
         constants), defining a function over a plain value, or declaring a \
         value `const` after the fact. All of these error at runtime when both \
         sites execute. A definition and a write in disjoint branches of the \
         same `if` are exempt — only one branch runs. Adding a method to a \
         function and defining an outer constructor on a type are legal and \
         stay silent. No fix: the rule cannot know which of the two \
         definitions the author meant to keep."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "Reassigning a `const` binding errors at runtime:",
                source: "const threshold = 1.0\nthreshold = 2.0\n",
            },
            Example {
                caption: "`count` already holds a value, so the method definition fails:",
                source: "count = 0\ncount(xs) = length(xs)\n",
            },
        ]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        for ident in ctx.model.idents() {
            if ident.access == Access::Read {
                continue;
            }
            let Some(id) = ident.binding else { continue };
            let binding = ctx.model.binding(id);
            if ident.range == binding.def_range {
                continue;
            }
            let global = ctx.model.scope(binding.scope).kind.is_global();
            let site = classify_write(ctx.root, ident.range);
            let message = match binding.kind {
                BindingKind::Const => match site {
                    WriteSite::MacroName | WriteSite::TypeName => continue,
                    WriteSite::FunctionName => {
                        format!(
                            "cannot define function `{}`: it already has a value",
                            binding.name
                        )
                    }
                    _ => format!("reassignment of constant `{}`", binding.name),
                },
                BindingKind::Function | BindingKind::Type | BindingKind::Module
                    if global && matches!(site, WriteSite::Plain | WriteSite::ConstDecl) =>
                {
                    format!("reassignment of constant `{}`", binding.name)
                }
                kind if holds_value(kind) && site == WriteSite::FunctionName => {
                    format!(
                        "cannot define function `{}`: it already has a value",
                        binding.name
                    )
                }
                BindingKind::Global if site == WriteSite::ConstDecl => {
                    format!(
                        "cannot declare `{}` constant: it already has a value",
                        binding.name
                    )
                }
                _ => continue,
            };
            if in_disjoint_branches(ctx.root, binding.def_range, ident.range) {
                continue;
            }
            sink.push(Diagnostic::new(self.id(), ident.range, message));
        }
    }
}
