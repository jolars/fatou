//! `shadowed-base-name`: a value binding that masks a Base/Core name, called
//! as a function further down.
//!
//! Julia has one namespace and no call-position fallback, so a binding always
//! wins over the implicit Base/Core tier — in *both* positions. Once a file
//! writes `length = 3`, every later `length(x)` tries to call the `Int`:
//!
//! ```julia
//! length = 3
//! n = length(xs)   # MethodError: objects of type Int64 are not callable
//! ```
//!
//! This is what makes the Julia rule stronger than the R one it comes from
//! (arity's `shadowed-builtin`): R looks a call-position name up in the
//! function namespace, so its `length <- 3; length(x)` is benign, while Julia's
//! is a hard error. Writing the assignment the other way round does not help —
//! a module that has already *used* `length` rejects the assignment with
//! `cannot assign a value to imported variable Base.length`, so either order is
//! broken and the rule imposes no order between the two.
//!
//! Both halves are required, as in arity: the binding alone is ordinary Julia
//! (`count = 0` is fine as long as nobody calls it), and the call alone is just
//! Base's function. The rule reports at each **call**, the site that fails, and
//! ships no fix — renaming the binding means rewriting every reference to it,
//! which is a decision rather than a tight edit.
//!
//! Only bindings that plainly hold a *value* count. Everything else is a shape
//! where calling the name is what the author meant:
//!
//! - a [`BindingKind::Function`], `Macro`, `Type`, or `Module` definition
//!   declares something callable (whether it should have extended Base's
//!   function instead is `type-piracy`'s question, not this rule's), and a
//!   [`BindingKind::Import`] (`import Base: length`) *is* the Base name;
//! - a **parameter**, positional or keyword. Passing a function in under a
//!   Base name is the higher-order idiom, not an accident: across a 467-package
//!   depot every parameter this rule matched was one
//!   (`request(stack::Base.Callable, …)` calling `stack(…)`,
//!   `expand_cxxstring_abis(…; skip = Sys.isbsd)` calling `skip(platform)`),
//!   and none was a bug. A `catch` variable is *not* a parameter in this sense
//!   — it holds the thrown exception, never a function — so it stays in;
//! - a binding the source visibly assigns something callable: a lambda, a
//!   `function` expression, another builtin under a new name
//!   (`size = Base.size`), or any qualified path, since aliasing a method
//!   (`exit = t.__exit__`) reads exactly like aliasing a field;
//! - anything whose definition *or* call sits in quoted code or a macro call,
//!   which is not what runs. A macro DSL routinely assigns Base names as
//!   declarations (Makie's `@Block` blocks spell an attribute `length = 32`)
//!   while the surrounding file keeps calling Base's function. This is the
//!   shared [`FileScan`](super::super::file_scan::FileScan) exemption every
//!   resolution-dependent rule takes.

use crate::ast::{AssignmentExpr, AstNode, AstToken, Expr};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::matchers::call_expr;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::resolve::{Namespace, Resolution};
use crate::semantic::{BindingId, BindingKind};
use crate::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

pub struct ShadowedBaseName;

impl Rule for ShadowedBaseName {
    fn id(&self) -> &'static str {
        "shadowed-base-name"
    }

    fn description(&self) -> &'static str {
        "Flag a call to a name the file binds to a value while Base or Core \
         exports it as a function. Julia has one namespace and no \
         call-position fallback, so the binding masks the Base name \
         everywhere: after `length = 3`, a later `length(xs)` raises a \
         `MethodError` for calling an `Int`, and assigning the other way \
         round fails too once the module has used the Base name. Both a \
         binding and a call are required — the binding alone is ordinary \
         Julia. Only bindings that plainly hold a value count. Method, macro, \
         type, and module definitions of the name are exempt, as is `import \
         Base: length`; so is a parameter, since passing a function in under \
         a Base name (`request(stack::Base.Callable, …)`) is the \
         higher-order idiom rather than an accident, though a `catch` \
         variable — which holds the thrown exception — still counts. A \
         binding the source visibly assigns something callable (a lambda, \
         another builtin under a new name, or any qualified path such as \
         `exit = t.__exit__`) is exempt too, as is anything defined or called \
         inside quoted code or a macro call, where a DSL may spell an \
         attribute `length = 32` without touching Base. No fix: renaming the \
         binding means rewriting every reference to it."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "The assignment masks Base's `length`, so the call fails:",
                source: "length = 3\nn = length(xs)\n",
            },
            Example {
                caption: "A `catch` variable can mask a builtin just as well:",
                source: "try\n    risky()\ncatch error\n    error(\"failed\")\nend\n",
            },
        ]
    }

    fn interests(&self) -> &'static [SyntaxKind] {
        &[SyntaxKind::CALL_EXPR]
    }

    fn check(&self, el: &SyntaxElement, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        // A definition's signature is a `CALL_EXPR` too; `call_expr` drops it.
        let Some(call) = el.as_node().and_then(call_expr) else {
            return;
        };
        // Only a bare-name callee can be masked: `Base.length(x)` names the
        // module's function outright.
        let Some(callee) = call.callee_ident() else {
            return;
        };
        let range = callee.syntax().text_range();
        if ctx.file_scan().in_skipped(range) {
            return;
        }

        let Some(resolver) = ctx.resolver() else {
            return;
        };
        let Resolution::Binding(id) =
            resolver.resolve(callee.text(), range.start(), Namespace::Value)
        else {
            return;
        };
        let binding = ctx.model.binding(id);
        // A binding a macro DSL or a quote introduced is a declaration, not the
        // value this call would reach.
        if !is_value_binding(binding.kind) || ctx.file_scan().in_skipped(binding.def_range) {
            return;
        }
        let Some(module) = ctx.base_export_module(callee.text()) else {
            return;
        };
        if assigns_a_function(ctx, id) {
            return;
        }

        let name = callee.text();
        sink.push(Diagnostic::new(
            self.id(),
            range,
            format!("`{name}` here is this file's own binding, not {module}'s `{name}`"),
        ));
    }
}

/// Whether a binding of this kind plainly holds a *value* — something a call
/// would have to invoke — rather than declaring a callable, naming the Base
/// symbol itself, or receiving whatever the caller passes (see the module
/// docs).
fn is_value_binding(kind: BindingKind) -> bool {
    match kind {
        BindingKind::Global
        | BindingKind::Local
        | BindingKind::Const
        | BindingKind::ForVar
        | BindingKind::LetVar
        | BindingKind::CatchParam => true,
        BindingKind::Param
        | BindingKind::KeywordParam
        | BindingKind::Function
        | BindingKind::Macro
        | BindingKind::Type
        | BindingKind::Module
        | BindingKind::Import
        | BindingKind::Field
        | BindingKind::TypeParam => false,
    }
}

/// Whether any site that gives `id` its value visibly assigns a function: a
/// lambda, a `function` expression, or a reference to another Base/Core
/// function (`size = Base.size`). Calling such a binding is what the author
/// wrote it for.
fn assigns_a_function(ctx: &RuleContext<'_>, id: BindingId) -> bool {
    let binding = ctx.model.binding(id);
    std::iter::once(binding.def_range)
        .chain(ctx.model.occurrences(id).map(|occ| occ.range))
        .filter_map(|range| assigned_value(ctx.root, range))
        .any(|value| is_function_valued(ctx, &value))
}

/// The expression assigned to the name occupying exactly `range`. `None` for a
/// binding site that is not a plain assignment target (a destructuring tuple, a
/// `for` clause, a `catch` variable), which the caller reads as "not visibly a
/// function".
fn assigned_value(root: &SyntaxNode, range: rowan::TextRange) -> Option<Expr> {
    let name = root.covering_element(range).into_token()?.parent()?;
    let assignment = AssignmentExpr::cast(name.parent()?)?;
    // Only the target's own assignment counts: the same name on the right
    // (`n = length`) is a read, not a definition.
    if assignment.lhs()?.syntax() != &name {
        return None;
    }
    assignment.rhs()
}

/// Whether `value` is visibly something callable: written as a function, naming
/// a Base/Core function (`sort = identity`), or reached through a qualified
/// path. The last is deliberately broad — `exit = t.__exit__` aliases a method
/// and reads no differently from aliasing a field, so a `.` path is never taken
/// as proof that the binding holds data.
fn is_function_valued(ctx: &RuleContext<'_>, value: &Expr) -> bool {
    match value {
        Expr::ArrowExpr(_) | Expr::FunctionDef(_) => true,
        Expr::Name(name) => name
            .ident()
            .is_some_and(|ident| ctx.base_export_module(ident.text()).is_some()),
        // A qualified path (`Base.size`, `t.__exit__`) parses as a `.` binary
        // expression.
        Expr::BinaryExpr(path) => path
            .op()
            .is_some_and(|op| op.syntax().kind() == SyntaxKind::DOT),
        _ => false,
    }
}
