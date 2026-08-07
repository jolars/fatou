//! Reusable call-shape matchers for lint rules.
//!
//! "A call to *name* with exactly *n* positional arguments and nothing else" is
//! the opening line of most idiom rules, and Julia's call syntax makes that a
//! surprisingly long line to write: keyword arguments live on both sides of the
//! `;`, the `;`-block admits a bare-name shorthand (`f(x; verbose)`), splats
//! open the argument set on either side, a trailing `do` block passes a
//! function the argument list does not show, a generator argument
//! (`sum(x for x in xs)`) is one argument wearing no `ARG` wrapper — and
//! sometimes no argument list either — and a definition's signature is a
//! `CALL_EXPR` too. [`CallShape`] answers all of that once, and
//! [`plain_call`] collapses the whole opening into one call.
//!
//! These are thin adapters over the typed AST layer ([`CallExpr`], [`Arg`],
//! [`KeywordArg`], [`Expr`]) — structural navigation belongs on the wrappers in
//! [`crate::ast`], not here. What lives here is the *policy* the wrappers do not
//! encode: which shapes leave the argument set unknown, and which `CALL_EXPR`s
//! are not calls at all. The one place raw `children()` iteration is deliberate
//! is [`CallShape::of`], whose `_` arms are the point: an argument-list child
//! this module does not recognize must open the set rather than be skipped.
//!
//! Matching a *shape* is only half of an idiom rule. Confirming that the name
//! matched really is the Base/Core function, rather than a local shadow or a
//! masked import, is
//! [`RuleContext::resolves_to_base`](super::RuleContext::resolves_to_base)'s
//! job — ask it after matching here.

use crate::ast::{Arg, AstNode, AstToken, CallExpr, Expr, HasArgList, Ident, KeywordArg};
use crate::syntax::{SyntaxKind, SyntaxNode};

// --- callees ---------------------------------------------------------------

/// `node` cast to a call that really is a call: a definition's signature is a
/// `CALL_EXPR` too, but it declares parameters rather than passing arguments
/// (see [`in_signature_position`]).
pub fn call_expr(node: &SyntaxNode) -> Option<CallExpr> {
    let call = CallExpr::cast(node.clone())?;
    (!in_signature_position(node)).then_some(call)
}

/// `node` cast to a call whose callee is the bare name `name`.
///
/// Only a bare identifier callee matches: `Base.length(x)` spells a different
/// name, and confirming that a bare `length` *is* Base's is
/// [`RuleContext::resolves_to_base`](super::RuleContext::resolves_to_base)'s
/// separate question. A definition's signature never matches, per
/// [`call_expr`].
pub fn call_named(node: &SyntaxNode, name: &str) -> Option<CallExpr> {
    let call = call_expr(node)?;
    (call.callee_ident()?.text() == name).then_some(call)
}

/// A call to `name` passing exactly `arity` positional arguments and nothing
/// else, together with those arguments — no keyword arguments, no splat on
/// either side, and no trailing `do` block.
///
/// This is the whole opening line of a typical idiom rule: match
/// `plain_call(node, "length", 1)`, then confirm the name with
/// [`RuleContext::resolves_to_base`](super::RuleContext::resolves_to_base).
/// It is deliberately strict — every excluded shape is one where the call may
/// not mean what its arity suggests — so a rule that rewrites on a match stays
/// conservative.
pub fn plain_call(node: &SyntaxNode, name: &str, arity: usize) -> Option<(CallExpr, Vec<Expr>)> {
    let call = call_named(node, name)?;
    let shape = CallShape::of(&call);
    if !shape.is_plain(arity) {
        return None;
    }
    Some((call, shape.positional))
}

// --- argument shape --------------------------------------------------------

/// One call site's argument shape: what it passes, and what it leaves unknown.
///
/// Julia's two argument sets — positional, and the keywords that may appear on
/// either side of the `;` — are each either *closed* (every argument is
/// accounted for) or *open* (a splat, or a shape this module does not
/// recognize, could be passing anything). A rule that reasons about arity must
/// check the matching `*_open` flag before trusting a count.
pub struct CallShape {
    /// The positional arguments' expressions, in source order. Splats are
    /// **not** included: they set [`positional_open`](Self::positional_open)
    /// instead, since they pass an unknown number of arguments.
    pub positional: Vec<Expr>,
    /// The keyword arguments, in source order, from both sides of the `;`.
    pub keywords: Vec<KeywordMatch>,
    /// A positional splat (`f(xs...)`), an argument whose expression is
    /// unreadable, or an unrecognized argument-list entry leaves the positional
    /// count unknown.
    pub positional_open: bool,
    /// A keyword splat (`f(; kw...)`), an unreadable keyword name, or an
    /// unrecognized `;`-block entry leaves the keyword set unknown.
    pub keyword_open: bool,
    /// The call carries a trailing `do` block (`map(xs) do y ... end`), which
    /// passes a function as a leading argument the argument list does not show.
    pub do_block: bool,
}

/// One keyword argument at a call site.
pub struct KeywordMatch {
    /// The keyword's name token — the finding span for a rule that reports on
    /// the keyword itself.
    pub name: Ident,
    /// The passed value, or `None` for the `;`-block shorthand `f(; verbose)`,
    /// where the name is also the value.
    pub value: Option<Expr>,
}

impl CallShape {
    /// The shape of `call`'s argument list.
    pub fn of(call: &CallExpr) -> Self {
        let mut shape = CallShape {
            positional: Vec::new(),
            keywords: Vec::new(),
            positional_open: false,
            keyword_open: false,
            do_block: has_do_block(call),
        };
        let Some(args) = call.arg_list() else {
            // The lone-generator call `f(x for x in xs)` has no argument list
            // at all: the parser hangs the `GENERATOR` straight off the
            // `CALL_EXPR`, the call's own parentheses serving as its
            // delimiters. Julia passes it as one positional argument.
            shape.push_positional(lone_generator(call));
            return shape;
        };
        for child in args.syntax().children() {
            match child.kind() {
                SyntaxKind::ARG => {
                    shape.push_positional(Arg::cast(child.clone()).and_then(|arg| arg.expr()));
                }
                // A generator sharing the argument list with other arguments
                // (`f(a, x for x in xs)`, `sum(x for x in xs; init = 0)`) is
                // not wrapped in an `ARG`, but it is one positional argument.
                SyntaxKind::GENERATOR => shape.push_positional(Expr::cast(child.clone())),
                SyntaxKind::KEYWORD_ARG => shape.push_keyword(&child),
                // After the `;`: keyword arguments, the bare-name shorthand
                // `f(; verbose)`, and the keyword splat `f(; kw...)`.
                SyntaxKind::PARAMETERS => {
                    for param in child.children() {
                        match param.kind() {
                            SyntaxKind::KEYWORD_ARG => shape.push_keyword(&param),
                            SyntaxKind::ARG => shape.push_shorthand(&param),
                            _ => shape.keyword_open = true,
                        }
                    }
                }
                // Error recovery (an `ERROR` node for `f(x,,y)`) or a shape
                // this module does not know: either way the count is unknown.
                _ => shape.positional_open = true,
            }
        }
        shape
    }

    /// Whether the call passes exactly `arity` positional arguments and nothing
    /// else: no keyword arguments, neither set open, and no `do` block.
    pub fn is_plain(&self, arity: usize) -> bool {
        self.positional.len() == arity
            && self.keywords.is_empty()
            && !self.positional_open
            && !self.keyword_open
            && !self.do_block
    }

    /// Record one positional argument's expression. `None` — an argument the
    /// parser left without an expression, which only error recovery produces —
    /// opens the set rather than being counted or dropped.
    fn push_positional(&mut self, expr: Option<Expr>) {
        match expr {
            None | Some(Expr::SplatExpr(_)) => self.positional_open = true,
            Some(expr) => self.positional.push(expr),
        }
    }

    fn push_keyword(&mut self, node: &SyntaxNode) {
        let name = KeywordArg::cast(node.clone())
            .and_then(|kw| kw.name())
            .and_then(|name| name.ident());
        match name {
            // A name we cannot read (`var"x" = 1`) is still a keyword, just an
            // unknown one, so the set is open.
            None => self.keyword_open = true,
            Some(name) => {
                let value = KeywordArg::cast(node.clone()).and_then(|kw| kw.value());
                self.keywords.push(KeywordMatch { name, value });
            }
        }
    }

    /// A bare `ARG` after the `;`: the shorthand `f(; verbose)` passes the
    /// binding `verbose` as the keyword `verbose`. Anything else there (a
    /// splat, a field access) opens the set.
    fn push_shorthand(&mut self, node: &SyntaxNode) {
        let name = Arg::cast(node.clone())
            .and_then(|arg| arg.expr())
            .and_then(|expr| expr.name_ident());
        match name {
            None => self.keyword_open = true,
            Some(name) => self.keywords.push(KeywordMatch { name, value: None }),
        }
    }
}

/// The `GENERATOR` a lone-generator call (`f(x for x in xs)`) carries in place
/// of an argument list. The callee is the first child, so the search starts
/// past it: a generator *callee* (`(x for x in xs)(y)`) is not an argument.
fn lone_generator(call: &CallExpr) -> Option<Expr> {
    call.syntax()
        .children()
        .skip(1)
        .find(|child| child.kind() == SyntaxKind::GENERATOR)
        .and_then(Expr::cast)
}

// --- call-position policy --------------------------------------------------

/// Whether a `CALL_EXPR` is a definition's signature rather than a call:
/// directly under a `SIGNATURE` (the long-form `function`/`macro` definitions),
/// or the left-hand side of a short-form `f(x) = ...`, in both cases possibly
/// through a return-type annotation or a `where` clause.
pub fn in_signature_position(node: &SyntaxNode) -> bool {
    let mut current = node.clone();
    loop {
        let Some(parent) = current.parent() else {
            return false;
        };
        match parent.kind() {
            SyntaxKind::SIGNATURE => return true,
            SyntaxKind::TYPE_ANNOTATION | SyntaxKind::WHERE_EXPR => current = parent,
            SyntaxKind::ASSIGNMENT_EXPR => {
                return parent
                    .children()
                    .next()
                    .is_some_and(|first| first == current);
            }
            _ => return false,
        }
    }
}

/// Whether an `ASSIGNMENT_EXPR` is a short-form function definition
/// (`f(x) = ...`, `f(x)::T = ...`, `f(x) where {T} = ...`) rather than an
/// ordinary assignment — the same shape [`in_signature_position`] recognizes,
/// asked from the assignment rather than from its signature.
///
/// The distinction a rule needs this for is that a short-form definition's
/// right-hand side is a *function body*: it opens a local scope the way a
/// `function` block does, where a plain `x = ...` opens nothing. Only a plain
/// `=` defines; an augmented or broadcast assignment to a call is not a
/// definition. An infix operator definition (`a::T + b::T = ...`) has no call
/// on the left and so answers `false`.
pub fn is_short_form_def(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::ASSIGNMENT_EXPR
        && node
            .children_with_tokens()
            .any(|el| el.kind() == SyntaxKind::EQ)
        && node
            .children()
            .next()
            .is_some_and(|lhs| crate::semantic::signature::has_call_core(&lhs))
}

/// Whether `call` carries a trailing `do` block (`map(xs) do y ... end`), which
/// passes a function as a leading argument the argument list does not show.
pub fn has_do_block(call: &CallExpr) -> bool {
    call.syntax()
        .parent()
        .is_some_and(|parent| parent.kind() == SyntaxKind::DO_EXPR)
}

// --- operand classifiers ---------------------------------------------------

/// Whether an operand is exactly the bare name `name`.
///
/// The classifier for Julia's `Core` constants (`nothing`, `missing`), which
/// are ordinary bindings rather than keywords and so appear as plain `NAME`
/// operands. Matching is by identifier text: a qualified spelling
/// (`Base.missing`) and the capitalized *type* (`Missing`) are different names.
pub fn is_name(expr: &Expr, name: &str) -> bool {
    expr.name_ident().is_some_and(|ident| ident.text() == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::BinaryExpr;
    use crate::parser::parse;

    /// The first `CALL_EXPR` in the parse of `src`, as a node.
    fn call_node(src: &str) -> SyntaxNode {
        parse(src)
            .cst
            .descendants()
            .find(|n| n.kind() == SyntaxKind::CALL_EXPR)
            .expect("a call")
    }

    fn call(src: &str) -> CallExpr {
        CallExpr::cast(call_node(src)).expect("a call")
    }

    fn shape(src: &str) -> CallShape {
        CallShape::of(&call(src))
    }

    fn texts(exprs: &[Expr]) -> Vec<String> {
        exprs
            .iter()
            .map(|e| e.syntax().text().to_string())
            .collect()
    }

    #[test]
    fn call_named_matches_a_bare_callee() {
        assert!(call_named(&call_node("length(x)\n"), "length").is_some());
        assert!(call_named(&call_node("length(x)\n"), "size").is_none());
        // A qualified callee is a different name; confirming that `Base.length`
        // *is* `Base.length` is `resolves_to_base`'s job, not a shape match.
        assert!(call_named(&call_node("Base.length(x)\n"), "length").is_none());
        // Not a call at all.
        assert!(call_named(&call_node("length(x)\n").parent().unwrap(), "length").is_none());
    }

    #[test]
    fn call_named_rejects_definition_signatures() {
        assert!(call_named(&call_node("length(x) = 1\n"), "length").is_none());
        assert!(call_named(&call_node("function length(x)\n    1\nend\n"), "length").is_none());
        assert!(call_named(&call_node("length(x::T) where {T} = 1\n"), "length").is_none());
        assert!(
            call_named(
                &call_node("function length(x)::Int\n    1\nend\n"),
                "length"
            )
            .is_none()
        );
        // The body's call is a call.
        assert!(call_named(&call_node("f() = length(x)\n"), "f").is_none());
    }

    #[test]
    fn plain_call_wants_exactly_that_many_positional_arguments() {
        let (call, args) = plain_call(&call_node("length(x)\n"), "length", 1).expect("a match");
        assert_eq!(call.syntax().text().to_string(), "length(x)");
        assert_eq!(texts(&args), ["x"]);
        assert!(plain_call(&call_node("length(x)\n"), "length", 0).is_none());
        assert!(plain_call(&call_node("length(x)\n"), "length", 2).is_none());
        let (_, args) = plain_call(&call_node("occursin(p, s)\n"), "occursin", 2).expect("a match");
        assert_eq!(texts(&args), ["p", "s"]);
    }

    #[test]
    fn plain_call_rejects_anything_but_positional_arguments() {
        // A keyword, before or after the `;`.
        assert!(plain_call(&call_node("f(x, by = g)\n"), "f", 1).is_none());
        assert!(plain_call(&call_node("f(x; by = g)\n"), "f", 1).is_none());
        assert!(plain_call(&call_node("f(x; by)\n"), "f", 1).is_none());
        // Splats leave the count (or the keyword set) unknown.
        assert!(plain_call(&call_node("f(xs...)\n"), "f", 1).is_none());
        assert!(plain_call(&call_node("f(x; kw...)\n"), "f", 1).is_none());
        // A `do` block passes a function as a hidden leading argument.
        assert!(plain_call(&call_node("f(x) do y\n    y\nend\n"), "f", 1).is_none());
    }

    #[test]
    fn call_shape_splits_positional_and_keyword_arguments() {
        let shape = shape("f(a, b, c = 1; d = 2, e)\n");
        assert_eq!(shape.positional.len(), 2);
        assert_eq!(texts(&shape.positional), ["a", "b"]);
        let names: Vec<&str> = shape.keywords.iter().map(|k| k.name.text()).collect();
        // `c = 1` before the `;` is a keyword argument too, as Julia lowers it.
        assert_eq!(names, ["c", "d", "e"]);
        // The `;`-block shorthand `e` passes the binding `e` under its own name.
        assert!(shape.keywords[2].value.is_none());
        assert_eq!(
            shape.keywords[0]
                .value
                .as_ref()
                .map(|v| v.syntax().text().to_string()),
            Some("1".to_string())
        );
        assert!(!shape.positional_open);
        assert!(!shape.keyword_open);
        assert!(!shape.do_block);
    }

    #[test]
    fn call_shape_flags_splats_as_open() {
        let splat = shape("f(a, xs...)\n");
        assert!(splat.positional_open);
        assert!(!splat.keyword_open);
        // The splat is not counted: only the arguments we can name are.
        assert_eq!(splat.positional.len(), 1);

        let kw_splat = shape("f(a; kw...)\n");
        assert!(!kw_splat.positional_open);
        assert!(kw_splat.keyword_open);
        assert!(kw_splat.keywords.is_empty());
    }

    #[test]
    fn call_shape_counts_a_generator_as_one_positional_argument() {
        // A lone generator has no argument list at all: the parser hangs the
        // `GENERATOR` off the call, the call's parentheses serving as its
        // delimiters (hence the parentheses in its text).
        let lone = shape("minimum(f(x) for x in xs)\n");
        assert_eq!(texts(&lone.positional), ["(f(x) for x in xs)"]);
        assert!(!lone.positional_open);
        assert!(lone.is_plain(1));

        // Sharing the argument list, it is a bare `GENERATOR` sibling of the
        // other arguments rather than an `ARG`.
        let with_arg = shape("f(a, x for x in xs)\n");
        assert_eq!(texts(&with_arg.positional), ["a", "x for x in xs"]);
        assert!(with_arg.is_plain(2));

        // Keyword parameters after the `;` put the generator in a list too.
        let with_kw = shape("sum(x for x in xs; init = 0)\n");
        assert_eq!(texts(&with_kw.positional), ["x for x in xs"]);
        assert_eq!(with_kw.keywords.len(), 1);
        assert!(!with_kw.positional_open);
    }

    #[test]
    fn call_shape_opens_the_count_on_an_unrecognized_entry() {
        // `f(x,,y)` recovers as an `ERROR` node covering the junk: an entry
        // this module cannot read must open the count, not be skipped.
        let broken = shape("f(x,,y)\n");
        assert!(broken.positional_open);
        assert!(!broken.is_plain(1));
    }

    #[test]
    fn call_shape_handles_a_call_with_no_arguments() {
        let shape = shape("f()\n");
        assert!(shape.positional.is_empty());
        assert!(shape.keywords.is_empty());
        assert!(shape.is_plain(0));
    }

    #[test]
    fn call_shape_sees_a_trailing_do_block() {
        let with_do = shape("map(xs) do y\n    y\nend\n");
        assert!(with_do.do_block);
        assert!(!with_do.is_plain(1));
        assert!(!shape("map(f, xs)\n").do_block);
    }

    #[test]
    fn is_plain_wants_the_arity_and_nothing_else() {
        assert!(shape("f(a)\n").is_plain(1));
        assert!(!shape("f(a)\n").is_plain(2));
        assert!(!shape("f(a; b = 1)\n").is_plain(1));
        assert!(!shape("f(a, xs...)\n").is_plain(1));
    }

    #[test]
    fn in_signature_position_sees_through_annotations_and_where() {
        assert!(in_signature_position(&call_node("f(x) = 1\n")));
        assert!(in_signature_position(&call_node(
            "function f(x)\n    1\nend\n"
        )));
        assert!(in_signature_position(&call_node("f(x)::Int = 1\n")));
        assert!(in_signature_position(&call_node("f(x::T) where {T} = 1\n")));
        assert!(!in_signature_position(&call_node("y = f(x)\n")));
        assert!(!in_signature_position(&call_node("f(x)\n")));
    }

    #[test]
    fn is_short_form_def_recognizes_definitions_not_assignments() {
        /// The first `ASSIGNMENT_EXPR` in the parse of `src`.
        fn assign(src: &str) -> SyntaxNode {
            parse(src)
                .cst
                .descendants()
                .find(|n| n.kind() == SyntaxKind::ASSIGNMENT_EXPR)
                .expect("an assignment")
        }

        assert!(is_short_form_def(&assign("f(x) = 1\n")));
        assert!(is_short_form_def(&assign("f(x)::Int = 1\n")));
        assert!(is_short_form_def(&assign("f(x::T) where {T} = 1\n")));
        // A plain assignment, even to a call's result or from one.
        assert!(!is_short_form_def(&assign("y = f(x)\n")));
        assert!(!is_short_form_def(&assign("x = 1\n")));
        // Only a plain `=` defines.
        assert!(!is_short_form_def(&assign("f(x) += 1\n")));
        assert!(!is_short_form_def(&assign("f(x) .= 1\n")));
        // An infix operator definition carries no call on the left.
        assert!(!is_short_form_def(&assign("a::T + b::T = 1\n")));
    }

    #[test]
    fn is_name_matches_only_bare_identifiers() {
        let bin = parse("x == missing\n")
            .cst
            .descendants()
            .find_map(BinaryExpr::cast)
            .expect("a binary expr");
        assert!(is_name(&bin.rhs().unwrap(), "missing"));
        assert!(!is_name(&bin.rhs().unwrap(), "Missing"));
        assert!(!is_name(&bin.lhs().unwrap(), "missing"));

        let bin = parse("x == Base.missing\n")
            .cst
            .descendants()
            .find_map(BinaryExpr::cast)
            .expect("a binary expr");
        assert!(!is_name(&bin.rhs().unwrap(), "missing"));
    }
}
