//! `unreachable-code`: a statement no path from its region's entry can reach.
//!
//! The question is answered by the file's control-flow graph
//! ([`RuleContext::control_flow`]) rather than by a shape match, so it covers
//! the nested shapes a "terminator is a direct statement of this block" scan
//! misses — an `if`/`else` that returns in *both* arms, a `while true` with no
//! `break`, a `@label` nothing jumps to — and it keeps the cases that only
//! *look* dead: a `for` may run zero times, an `if` with no `else` falls
//! through, a `catch` runs when its `try` body throws, and `a && return 1` is
//! conditional.
//!
//! One finding per dead basic block, anchored to the block's first statement:
//! a run of dead statements has a single cause, so reporting each of them would
//! say the same thing several times.
//!
//! **Namespace confirmation.** The graph is purely syntactic and recognizes a
//! divergence by callee *name* for `throw`/`error`/`rethrow`, so `f(throw)`
//! shadowing `Base.throw` would make live code look dead. A region containing
//! any such call that [`RuleContext::resolves_to_base`] does not confirm is
//! therefore left alone entirely. The gate is deliberately coarser than the
//! attribution would be — the graph does not say *which* divergence killed a
//! given block — and it errs toward silence, which is the right direction for
//! a `correctness` finding. A region with no throw-like call at all (the plain
//! dead tail after a `return`) is unaffected, since `return` is matched by node
//! kind and cannot be shadowed.

use rowan::TextRange;
use rowan::ast::AstNode as _;

use crate::ast::{AstToken as _, CallExpr};
use crate::linter::diagnostic::Diagnostic;
use crate::linter::rules::{Example, Rule, RuleContext};
use crate::semantic::ControlFlowGraph;
use crate::syntax::{SyntaxKind, SyntaxNode};

pub struct UnreachableCode;

/// The callee names the CFG treats as a divergence without resolving them.
const TERMINATOR_NAMES: [&str; 3] = ["throw", "error", "rethrow"];

/// The node whose subtree a region's graph was built from: the `ROOT` itself,
/// or the region owner's body `BLOCK`.
fn region_body(owner: &SyntaxNode) -> Option<SyntaxNode> {
    if owner.kind() == SyntaxKind::ROOT {
        return Some(owner.clone());
    }
    owner.children().find(|c| c.kind() == SyntaxKind::BLOCK)
}

/// Whether every `throw`/`error`/`rethrow` call in this region resolves to
/// Base — the confirmation the graph's name-based divergences need before a
/// finding derived from them can be trusted. Nested regions are skipped: a
/// divergence inside one leaves the enclosing graph alone.
fn terminators_confirmed(ctx: &RuleContext<'_>, body: &SyntaxNode) -> bool {
    let mut stack = vec![body.clone()];
    while let Some(node) = stack.pop() {
        for child in node.children() {
            if is_region_owner(child.kind()) {
                continue;
            }
            if child.kind() == SyntaxKind::CALL_EXPR
                && let Some(call) = CallExpr::cast(child.clone())
                && call
                    .callee_ident()
                    .is_some_and(|name| TERMINATOR_NAMES.contains(&name.text()))
                && !ctx.resolves_to_base(&call)
            {
                return false;
            }
            stack.push(child);
        }
    }
    true
}

/// Whether a node of this kind owns a flow region of its own (mirrors the
/// control-flow graph's own region boundaries).
fn is_region_owner(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FUNCTION_DEF
            | SyntaxKind::MACRO_DEF
            | SyntaxKind::DO_EXPR
            | SyntaxKind::MODULE_DEF
    )
}

/// The head statement of each of this graph's unreachable blocks.
fn dead_heads(graph: &ControlFlowGraph) -> impl Iterator<Item = TextRange> {
    graph
        .iter()
        .filter(|(id, _)| !graph.is_reachable(*id))
        .filter_map(|(_, block)| block.stmts.first().copied())
}

impl Rule for UnreachableCode {
    fn id(&self) -> &'static str {
        "unreachable-code"
    }

    fn description(&self) -> &'static str {
        "Flag a statement no path of execution can reach: the tail after an \
         unconditional `return`, `throw`, `error`, or `rethrow`, after an \
         `if`/`else` that diverges in every arm, or after a `while true` with \
         no `break`. The code runs, but the flagged statement never does, so \
         it is either dead weight or a sign that the divergence above it is \
         misplaced. Reachability comes from the file's control-flow graph, so \
         a `for` that may run zero times, an `if` with no `else`, a `catch` \
         clause, and a conditional `a && return` all keep their tails live. \
         No fix is offered: deleting the statement is a judgment call, and \
         keeping it may be the point when the divergence is the bug."
    }

    fn examples(&self) -> &'static [Example] {
        &[
            Example {
                caption: "Nothing after an unconditional `return` can run:",
                source: "function f(x)\n    return x + 1\n    println(\"never\")\nend\n",
            },
            Example {
                caption: "Both arms diverge, so the tail is dead too:",
                source: "function classify(x)\n    if x > 0\n        return :pos\n    else\n        throw(DomainError(x))\n    end\n    return :unknown\nend\n",
            },
        ]
    }

    fn check_file(&self, ctx: &RuleContext<'_>, sink: &mut Vec<Diagnostic>) {
        let cfg = ctx.control_flow();
        let regions = std::iter::once((ctx.root.clone(), cfg.toplevel())).chain(
            cfg.regions()
                .iter()
                .map(|(ptr, graph)| (ptr.to_node(ctx.root), graph)),
        );

        for (owner, graph) in regions {
            let Some(body) = region_body(&owner) else {
                continue;
            };
            if !terminators_confirmed(ctx, &body) {
                continue;
            }
            for range in dead_heads(graph) {
                sink.push(Diagnostic::new(
                    self.id(),
                    range,
                    "unreachable code: no path of execution reaches this statement".to_string(),
                ));
            }
        }
    }
}
