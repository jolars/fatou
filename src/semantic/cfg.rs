//! Per-region control-flow graph.
//!
//! A [`ControlFlowGraph`] is built for each **flow region** — the file top
//! level, each `module` body, and each function-like body (`function`, `macro`,
//! and a `do` block, all of which own a `BLOCK`). Regions are the boundaries
//! the flow-affecting keywords respect: `return` leaves the enclosing
//! *function*, `break`/`continue` the enclosing *loop*, and `@goto` may only
//! reach a `@label` in the same function. A [`FileControlFlow`] bundles the
//! top-level region with one CFG per region owner (keyed by [`NodePtr`]),
//! and is what [`crate::incremental::control_flow`] memoizes per file.
//!
//! The graph is built by **structured recursive descent** over the CST —
//! deterministic and local, no fixpoint. It is **purely syntactic**: a
//! divergence is recognized by node kind (`RETURN_EXPR`, `BREAK_EXPR`,
//! `CONTINUE_EXPR`) or by callee name (`throw`/`error`/`rethrow`), and no
//! resolution is consulted. A consumer that cares whether `throw` is really
//! `Base.throw` (and not a local shadow) applies that confirmation itself —
//! see [`RuleContext::resolves_to_base`](crate::linter::rules::RuleContext::resolves_to_base).
//!
//! Only *statement-level* control flow is modeled. Julia's `if`/`try` are
//! expressions, but one used as an operand (`x = if c ... end`) is left opaque,
//! as are the bodies that are single expressions rather than blocks (short-form
//! `f(x) = ...`, `x -> ...`). An opaque statement is conservative: it never
//! makes anything look unreachable.
//!
//! Reachability is computed from the entry block once the graph is built, so
//! [`FileControlFlow::is_unreachable`] answers for the tail after an
//! unconditional divergence, for an `if`/`else` that exits in *both* arms, for
//! a `while true` with no `break`, and for a `@label` nothing jumps to — the
//! signal a reachability-sensitive lint needs. The statements of the
//! unreachable blocks are indexed by range at the same time, so that predicate
//! is a hash lookup: a rule may ask it once per statement of the file without
//! the walk turning quadratic.
//!
//! The one place that syntactic reading has to give ground is the **macro**: an
//! expansion is code this graph never sees, and real Julia hides jumps in one
//! (JSON3's `@eof` macro expands to `@goto invalid`). So a region containing
//! any macro call keeps its `@label` blocks reachable, and a `while true` whose
//! body contains one is not claimed to never exit. The dead tail after an
//! unconditional `return` is unaffected: no expansion can rescue a statement
//! that follows one in the same block.

use std::collections::{HashMap, HashSet};

use rowan::TextRange;
use rowan::ast::AstNode as _;
use smol_str::SmolStr;

use crate::ast::{AstToken as _, CallExpr, body_of, condition_of};
use crate::syntax::{NodePtr, SyntaxElement, SyntaxKind, SyntaxNode};

/// Index of a [`BasicBlock`] within a [`ControlFlowGraph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(u32);

impl BlockId {
    /// The block's numeric index (for rendering and lookups).
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A maximal straight-line run of statements ending in a single [`Terminator`].
/// `stmts` are the source ranges of the statements executed, in order (a bare
/// token statement is included by its range, so this covers node *and* token
/// statements).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub stmts: Vec<TextRange>,
    pub terminator: Terminator,
}

/// How control leaves a [`BasicBlock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    /// Unconditional edge to a successor (fallthrough, loop back-edge,
    /// `break`, `continue`, `@goto`).
    Goto(BlockId),
    /// Two-way branch: an `if`, a `for`/`while` header that may skip the body,
    /// a short-circuit `&&`/`||` whose right side jumps, or the `try` header's
    /// exceptional edge to its handler.
    Branch {
        then_blk: BlockId,
        else_blk: BlockId,
    },
    /// Falls off the end of the region — a normal return to the caller.
    Return,
    /// Diverges out of the region via `return`/`throw`/`error`/`rethrow`; no
    /// in-region successor.
    Diverge,
    /// The block has no successor because it is a dead tail: the statements
    /// after an unconditional divergence.
    Unreachable,
}

impl Terminator {
    /// The blocks control can reach from here, in edge order.
    fn successors(self) -> impl Iterator<Item = BlockId> {
        let (a, b) = match self {
            Terminator::Goto(target) => (Some(target), None),
            Terminator::Branch { then_blk, else_blk } => (Some(then_blk), Some(else_blk)),
            Terminator::Return | Terminator::Diverge | Terminator::Unreachable => (None, None),
        };
        a.into_iter().chain(b)
    }
}

/// The control-flow graph of a single region. Block 0 is always the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFlowGraph {
    blocks: Vec<BasicBlock>,
    entry: BlockId,
    /// Whether each block is reachable from [`Self::entry`]; computed once at
    /// build time. A pure function of `blocks`, so it does not affect `Eq`.
    reachable: Vec<bool>,
}

impl Default for ControlFlowGraph {
    fn default() -> Self {
        Self {
            blocks: vec![BasicBlock {
                stmts: Vec::new(),
                terminator: Terminator::Return,
            }],
            entry: BlockId(0),
            reachable: vec![true],
        }
    }
}

impl ControlFlowGraph {
    /// The graph's basic blocks, block 0 first.
    pub fn blocks(&self) -> &[BasicBlock] {
        &self.blocks
    }

    /// The graph's blocks paired with their ids, block 0 first — what a walk
    /// that has to ask [`is_reachable`](Self::is_reachable) per block needs,
    /// since a [`BlockId`] is the graph's to hand out.
    pub fn iter(&self) -> impl Iterator<Item = (BlockId, &BasicBlock)> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(i, block)| (BlockId(i as u32), block))
    }

    /// The entry block.
    pub fn entry(&self) -> BlockId {
        self.entry
    }

    /// Look up a block by id.
    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.index()]
    }

    /// Whether control can reach `id` from the entry block.
    pub fn is_reachable(&self, id: BlockId) -> bool {
        self.reachable[id.index()]
    }

    /// Build the CFG for a region given its ordered statement elements.
    /// `opaque_macros` says the region contains a macro call, whose expansion
    /// may hide a `@goto` — see [`reachable_from`].
    fn build_region(
        stmts: &[SyntaxElement],
        labels: HashSet<SmolStr>,
        opaque_macros: bool,
    ) -> Self {
        let mut builder = Builder {
            blocks: Vec::new(),
            labels,
            label_blocks: HashMap::new(),
        };
        let entry = builder.new_block();
        if let Some(exit) = builder.lower_seq(stmts, entry, None) {
            builder.set_term(exit, Terminator::Return);
        }
        let mut roots = vec![entry];
        if opaque_macros {
            roots.extend(builder.label_blocks.values().copied());
        }
        let reachable = reachable_from(&builder.blocks, &roots);
        ControlFlowGraph {
            blocks: builder.blocks,
            entry,
            reachable,
        }
    }
}

/// Every region's CFG for one file: the top level plus one per region owner,
/// keyed by the owner's [`NodePtr`]. `PartialEq`/`Eq` let salsa backdate
/// the [`control_flow`](crate::incremental::control_flow) query when an edit
/// leaves the graph unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileControlFlow {
    toplevel: ControlFlowGraph,
    regions: Vec<(NodePtr, ControlFlowGraph)>,
    /// The range of every statement in an unreachable block, over all regions —
    /// the index [`Self::is_unreachable`] answers from, so a rule may ask per
    /// statement without rescanning the file's blocks each time. A pure
    /// function of the graphs, so it does not affect `Eq`. Unreachable code is
    /// rare, so this is empty for nearly every file.
    unreachable: HashSet<TextRange>,
}

impl FileControlFlow {
    /// Build every region's CFG from a parsed file root.
    pub fn build(root: &SyntaxNode) -> Self {
        let toplevel = build_region_of(root);
        let regions: Vec<_> = root
            .descendants()
            .filter(|node| is_region_owner(node.kind()))
            .filter(|node| body_of(node).is_some())
            .map(|node| (NodePtr::new(&node), build_region_of(&node)))
            .collect();
        let mut this = Self {
            toplevel,
            regions,
            unreachable: HashSet::new(),
        };
        this.unreachable = this
            .graphs()
            .flat_map(|cfg| {
                cfg.iter()
                    .filter(|(id, _)| !cfg.is_reachable(*id))
                    .flat_map(|(_, block)| block.stmts.iter().copied())
            })
            .collect();
        this
    }

    /// The file top-level region's CFG.
    pub fn toplevel(&self) -> &ControlFlowGraph {
        &self.toplevel
    }

    /// Each non-top-level region's CFG, in source order, keyed by the region
    /// owner's [`NodePtr`] (a `FUNCTION_DEF`, `MACRO_DEF`, `DO_EXPR`, or
    /// `MODULE_DEF`).
    pub fn regions(&self) -> &[(NodePtr, ControlFlowGraph)] {
        &self.regions
    }

    /// The CFG for the region owned by `ptr`, if present.
    pub fn region(&self, ptr: NodePtr) -> Option<&ControlFlowGraph> {
        self.regions
            .iter()
            .find(|(p, _)| *p == ptr)
            .map(|(_, cfg)| cfg)
    }

    /// Every region's CFG, the top level first.
    fn graphs(&self) -> impl Iterator<Item = &ControlFlowGraph> {
        std::iter::once(&self.toplevel).chain(self.regions.iter().map(|(_, cfg)| cfg))
    }

    /// Whether the statement at exactly `range` provably cannot be reached: it
    /// lands in a block no path from its region's entry reaches — the tail
    /// after an unconditional divergence, an `if`/`else` that exits in both
    /// arms, a `while true` with no `break`, or a `@label` nothing jumps to.
    ///
    /// `false` for a range that is not a statement of any region, so a caller
    /// asking about an arbitrary node gets the conservative answer.
    ///
    /// A hash lookup into the index built with the graphs, so a rule may ask
    /// once per statement of the file without the walk turning quadratic.
    pub fn is_unreachable(&self, range: TextRange) -> bool {
        self.unreachable.contains(&range)
    }

    /// Render the graph textually (region by region) for snapshot tests. `src`
    /// is the file text the ranges index into.
    pub fn render(&self, src: &str) -> String {
        let mut out = String::new();
        out.push_str("region: <toplevel>\n");
        render_region(&self.toplevel, src, &mut out);
        for (ptr, cfg) in &self.regions {
            let head = snippet(src, ptr.text_range());
            out.push_str(&format!("region: {head}\n"));
            render_region(cfg, src, &mut out);
        }
        out
    }
}

/// Build the CFG of the region owned by `owner` (the `ROOT` itself, or a node
/// whose `BLOCK` is the region body), with the labels `@goto` may target inside
/// it.
fn build_region_of(owner: &SyntaxNode) -> ControlFlowGraph {
    let body = if owner.kind() == SyntaxKind::ROOT {
        owner.clone()
    } else {
        body_of(owner).expect("region owner has a body block")
    };
    let mut labels = HashSet::new();
    collect_labels(&body, &mut labels);
    let opaque_macros = contains_opaque_macro(&body);
    ControlFlowGraph::build_region(&region_statements(&body), labels, opaque_macros)
}

/// Whether a node of this kind owns a flow region: its body is the extent a
/// `return` leaves and a `@goto` stays within. Short-form `f(x) = ...` and
/// `x -> ...` are function bodies too, but their body is a single expression
/// with no statement flow to model, so they are left opaque.
fn is_region_owner(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::FUNCTION_DEF
            | SyntaxKind::MACRO_DEF
            | SyntaxKind::DO_EXPR
            | SyntaxKind::MODULE_DEF
    )
}

/// Builder state: the block arena being filled, plus the region's labels.
struct Builder {
    blocks: Vec<BasicBlock>,
    /// The labels `@label` defines in this region; a `@goto` naming anything
    /// else (invalid Julia) is left as a plain statement.
    labels: HashSet<SmolStr>,
    /// The block each label names, allocated on first mention so a forward
    /// `@goto` and its later `@label` agree.
    label_blocks: HashMap<SmolStr, BlockId>,
}

/// The enclosing loop's targets, threaded through lowering so `break`/`continue`
/// know where to jump.
#[derive(Clone, Copy)]
struct LoopCtx {
    header: BlockId,
    after: BlockId,
}

/// A statement (or a short-circuit's right side) that leaves the straight-line
/// flow. Resolved to a [`Terminator`] against the enclosing loop and the
/// region's labels.
enum Jump {
    /// `return`, `throw`, `error`, `rethrow`: leaves the region.
    Diverge,
    Break,
    Continue,
    Goto(SmolStr),
}

impl Builder {
    fn new_block(&mut self) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).expect("block count fits in u32"));
        self.blocks.push(BasicBlock {
            stmts: Vec::new(),
            terminator: Terminator::Return,
        });
        id
    }

    fn set_term(&mut self, block: BlockId, terminator: Terminator) {
        self.blocks[block.index()].terminator = terminator;
    }

    fn push_stmt(&mut self, block: BlockId, range: TextRange) {
        self.blocks[block.index()].stmts.push(range);
    }

    fn has_predecessor(&self, target: BlockId) -> bool {
        self.blocks
            .iter()
            .any(|block| block.terminator.successors().any(|s| s == target))
    }

    /// The block a label names, allocated on first mention.
    fn label_block(&mut self, name: &SmolStr) -> BlockId {
        if let Some(id) = self.label_blocks.get(name) {
            return *id;
        }
        let id = self.new_block();
        self.label_blocks.insert(name.clone(), id);
        id
    }

    /// The terminator a [`Jump`] becomes here, or `None` when it has no target
    /// in this region (a `break`/`continue` outside any loop, a `@goto` naming
    /// an undefined label) — invalid Julia, left as a plain statement.
    fn jump_terminator(&mut self, jump: &Jump, loop_ctx: Option<LoopCtx>) -> Option<Terminator> {
        match jump {
            Jump::Diverge => Some(Terminator::Diverge),
            Jump::Break => loop_ctx.map(|lc| Terminator::Goto(lc.after)),
            Jump::Continue => loop_ctx.map(|lc| Terminator::Goto(lc.header)),
            Jump::Goto(name) => self
                .labels
                .contains(name)
                .then(|| Terminator::Goto(self.label_block(name))),
        }
    }

    /// Lower a statement sequence into `cur`, returning the block control
    /// leaves through, or `None` if control diverges before the sequence ends.
    ///
    /// After a divergence the remaining statements are dead **up to the next
    /// `@label`**: a label is an entry point, so lowering resumes there (the
    /// label's block is reachable only if some `@goto` targets it, which the
    /// reachability pass decides).
    fn lower_seq(
        &mut self,
        stmts: &[SyntaxElement],
        mut cur: BlockId,
        loop_ctx: Option<LoopCtx>,
    ) -> Option<BlockId> {
        let mut i = 0;
        while i < stmts.len() {
            match self.lower_stmt(&stmts[i], cur, loop_ctx) {
                Some(next) => {
                    cur = next;
                    i += 1;
                }
                None => {
                    let rest = &stmts[i + 1..];
                    let resume = rest
                        .iter()
                        .position(|stmt| label_definition(stmt).is_some());
                    let dead = &rest[..resume.unwrap_or(rest.len())];
                    if !dead.is_empty() {
                        let dead_blk = self.new_block();
                        self.set_term(dead_blk, Terminator::Unreachable);
                        for stmt in dead {
                            self.push_stmt(dead_blk, stmt.text_range());
                        }
                    }
                    let offset = resume?;
                    let label = &rest[offset];
                    let name = label_definition(label).expect("resume lands on a label");
                    cur = self.label_block(&name);
                    self.push_stmt(cur, label.text_range());
                    i += offset + 2;
                }
            }
        }
        Some(cur)
    }

    fn lower_stmt(
        &mut self,
        stmt: &SyntaxElement,
        cur: BlockId,
        loop_ctx: Option<LoopCtx>,
    ) -> Option<BlockId> {
        let Some(node) = stmt.as_node() else {
            self.push_stmt(cur, stmt.text_range());
            return Some(cur);
        };

        if let Some(name) = label_definition(stmt) {
            // A label starts a new block: control falls into it, and a `@goto`
            // elsewhere jumps to it.
            let target = self.label_block(&name);
            self.set_term(cur, Terminator::Goto(target));
            self.push_stmt(target, node.text_range());
            return Some(target);
        }

        if let Some(jump) = jump_of(node) {
            self.push_stmt(cur, node.text_range());
            return match self.jump_terminator(&jump, loop_ctx) {
                Some(terminator) => {
                    self.set_term(cur, terminator);
                    None
                }
                None => Some(cur),
            };
        }

        match node.kind() {
            // `begin`/`let` bodies inline into the current flow: they scope
            // names, not control. Only their statements are recorded — the
            // wrapper is not a step of its own.
            SyntaxKind::BLOCK => self.lower_seq(&region_statements(node), cur, loop_ctx),
            SyntaxKind::BEGIN_EXPR | SyntaxKind::LET_EXPR => match body_of(node) {
                Some(body) => self.lower_seq(&region_statements(&body), cur, loop_ctx),
                None => Some(cur),
            },
            SyntaxKind::IF_EXPR => {
                self.push_stmt(cur, node.text_range());
                let (guarded, unguarded) = if_arms(node);
                self.lower_if(&guarded, unguarded.as_ref(), cur, loop_ctx)
            }
            SyntaxKind::FOR_EXPR | SyntaxKind::WHILE_EXPR => {
                self.push_stmt(cur, node.text_range());
                let body = body_of(node);
                // Only a loop with no exit of its own can make its continuation
                // unreachable, so only there does a `break` hidden in a macro
                // expansion matter.
                let never_exits = node.kind() == SyntaxKind::WHILE_EXPR
                    && has_literal_true_test(node)
                    && !body.as_ref().is_some_and(contains_opaque_macro);
                let stmts = body.map(|b| region_statements(&b));
                self.lower_loop(cur, stmts.as_deref().unwrap_or_default(), never_exits)
            }
            SyntaxKind::TRY_EXPR => {
                self.push_stmt(cur, node.text_range());
                self.lower_try(node, cur, loop_ctx)
            }
            SyntaxKind::BINARY_EXPR => {
                self.push_stmt(cur, node.text_range());
                match short_circuit_jump(node) {
                    Some((jump_node, jump)) => {
                        self.lower_conditional_jump(cur, jump_node.text_range(), &jump, loop_ctx)
                    }
                    None => Some(cur),
                }
            }
            _ => {
                self.push_stmt(cur, node.text_range());
                Some(cur)
            }
        }
    }

    /// Lower `cond && return x`: the jump is taken on one edge, and control
    /// falls through on the other. An unresolvable jump leaves the statement
    /// plain.
    fn lower_conditional_jump(
        &mut self,
        cur: BlockId,
        range: TextRange,
        jump: &Jump,
        loop_ctx: Option<LoopCtx>,
    ) -> Option<BlockId> {
        // No target: not a jump after all, so control just falls through.
        let Some(terminator) = self.jump_terminator(jump, loop_ctx) else {
            return Some(cur);
        };
        let taken = self.new_block();
        let fallthrough = self.new_block();
        self.set_term(
            cur,
            Terminator::Branch {
                then_blk: taken,
                else_blk: fallthrough,
            },
        );
        self.push_stmt(taken, range);
        self.set_term(taken, terminator);
        Some(fallthrough)
    }

    /// Lower an `if`: `guarded` is the `if` arm followed by each `elseif` arm,
    /// `unguarded` the `else` arm. An `elseif` is lowered as an `else` holding
    /// the rest of the chain, so the graph nests exactly as the language does.
    fn lower_if(
        &mut self,
        guarded: &[Vec<SyntaxElement>],
        unguarded: Option<&Vec<SyntaxElement>>,
        cur: BlockId,
        loop_ctx: Option<LoopCtx>,
    ) -> Option<BlockId> {
        let Some((first, rest)) = guarded.split_first() else {
            // Malformed `if` with no arm at all.
            return Some(cur);
        };
        let then_blk = self.new_block();
        let then_exit = self.lower_seq(first, then_blk, loop_ctx);

        if rest.is_empty() && unguarded.is_none() {
            // No `else`: the false path falls straight to the join, so the `if`
            // never fully diverges.
            let join = self.new_block();
            self.set_term(
                cur,
                Terminator::Branch {
                    then_blk,
                    else_blk: join,
                },
            );
            if let Some(exit) = then_exit {
                self.set_term(exit, Terminator::Goto(join));
            }
            return Some(join);
        }

        let else_blk = self.new_block();
        let else_exit = if rest.is_empty() {
            self.lower_seq(unguarded.expect("checked above"), else_blk, loop_ctx)
        } else {
            self.lower_if(rest, unguarded, else_blk, loop_ctx)
        };
        self.set_term(cur, Terminator::Branch { then_blk, else_blk });
        self.join(&[then_exit, else_exit])
    }

    /// Lower a `for`/`while`. Both may run zero iterations, so control reaches
    /// the continuation — unless `never_exits`, which the caller sets for
    /// `while true` (Julia's infinite-loop idiom, exited only by `break`) when
    /// it can also see the whole body. The header then has no skip edge, and
    /// the continuation is unreachable if no `break` targets it.
    fn lower_loop(
        &mut self,
        cur: BlockId,
        body: &[SyntaxElement],
        never_exits: bool,
    ) -> Option<BlockId> {
        let header = self.new_block();
        self.set_term(cur, Terminator::Goto(header));
        let after = self.new_block();
        let body_blk = self.new_block();
        if never_exits {
            self.set_term(header, Terminator::Goto(body_blk));
        } else {
            self.set_term(
                header,
                Terminator::Branch {
                    then_blk: body_blk,
                    else_blk: after,
                },
            );
        }

        let loop_ctx = LoopCtx { header, after };
        if let Some(exit) = self.lower_seq(body, body_blk, Some(loop_ctx)) {
            self.set_term(exit, Terminator::Goto(header)); // back-edge
        }

        if never_exits && !self.has_predecessor(after) {
            self.set_term(after, Terminator::Unreachable);
            None
        } else {
            Some(after)
        }
    }

    /// Lower a `try`/`catch`/`finally`.
    ///
    /// An exception can be raised anywhere in the `try` body, so the handler is
    /// modeled as an edge out of the header rather than out of the body's exit
    /// — which is what keeps a `catch` (and a `finally`) reachable when the
    /// `try` body always diverges. With a `finally`, control after the whole
    /// statement is reachable whenever the `finally` falls through, even if
    /// both other arms diverge: `finally` runs on the exception path too, and
    /// this errs toward *reachable*.
    fn lower_try(
        &mut self,
        node: &SyntaxNode,
        cur: BlockId,
        loop_ctx: Option<LoopCtx>,
    ) -> Option<BlockId> {
        let catch = clause_body(node, SyntaxKind::CATCH_CLAUSE);
        let finally = clause_body(node, SyntaxKind::FINALLY_CLAUSE);

        let try_blk = self.new_block();
        let catch_blk = catch.as_ref().map(|_| self.new_block());
        let finally_blk = finally.as_ref().map(|_| self.new_block());

        match (catch_blk, finally_blk) {
            (Some(catch_blk), Some(finally_blk)) => {
                // Chain the two exceptional edges: an exception diverts to the
                // handler, and a `finally` runs whatever the handler does.
                let handler = self.new_block();
                self.set_term(
                    cur,
                    Terminator::Branch {
                        then_blk: try_blk,
                        else_blk: handler,
                    },
                );
                self.set_term(
                    handler,
                    Terminator::Branch {
                        then_blk: catch_blk,
                        else_blk: finally_blk,
                    },
                );
            }
            (Some(handler), None) | (None, Some(handler)) => self.set_term(
                cur,
                Terminator::Branch {
                    then_blk: try_blk,
                    else_blk: handler,
                },
            ),
            (None, None) => self.set_term(cur, Terminator::Goto(try_blk)),
        }

        let try_body = body_of(node)
            .map(|b| region_statements(&b))
            .unwrap_or_default();
        let try_exit = self.lower_seq(&try_body, try_blk, loop_ctx);
        let catch_exit = catch
            .zip(catch_blk)
            .and_then(|(stmts, blk)| self.lower_seq(&region_statements(&stmts), blk, loop_ctx));

        match (finally, finally_blk) {
            (Some(stmts), Some(blk)) => {
                for exit in [try_exit, catch_exit].into_iter().flatten() {
                    self.set_term(exit, Terminator::Goto(blk));
                }
                let finally_exit = self.lower_seq(&region_statements(&stmts), blk, loop_ctx);
                self.join(&[finally_exit])
            }
            _ => self.join(&[try_exit, catch_exit]),
        }
    }

    /// Merge the given exits into a fresh join block, or report divergence when
    /// none of them falls through.
    fn join(&mut self, exits: &[Option<BlockId>]) -> Option<BlockId> {
        if exits.iter().all(Option::is_none) {
            return None;
        }
        let join = self.new_block();
        for exit in exits.iter().flatten() {
            self.set_term(*exit, Terminator::Goto(join));
        }
        Some(join)
    }
}

/// Which blocks are reachable from `roots` — the entry block, plus every label
/// block when the region contains a macro call.
///
/// A macro expands to code this graph never sees, and Julia code really does
/// hide a `@goto` in one (JSON3's `@eof` macro is exactly that), so a `@label`
/// with no *visible* `@goto` cannot be proven dead in a region that uses any
/// macro. This costs nothing on the statements the dead-tail rule cares about:
/// nothing an expansion contains can rescue a statement after an unconditional
/// `return` in the same block.
fn reachable_from(blocks: &[BasicBlock], roots: &[BlockId]) -> Vec<bool> {
    let mut seen = vec![false; blocks.len()];
    let mut stack = Vec::from(roots);
    for root in roots {
        seen[root.index()] = true;
    }
    while let Some(id) = stack.pop() {
        for next in blocks[id.index()].terminator.successors() {
            if !seen[next.index()] {
                seen[next.index()] = true;
                stack.push(next);
            }
        }
    }
    seen
}

/// The statement elements directly inside `container` (a `ROOT` or a `BLOCK`):
/// its children minus trivia, comments, and `;` separators.
fn region_statements(container: &SyntaxNode) -> Vec<SyntaxElement> {
    container
        .children_with_tokens()
        .filter(|element| !is_ignorable(element.kind()))
        .collect()
}

/// Trivia and separators, which are not statements.
fn is_ignorable(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::WHITESPACE
            | SyntaxKind::NEWLINE
            | SyntaxKind::COMMENT
            | SyntaxKind::BLOCK_COMMENT
            | SyntaxKind::SEMICOLON
            | SyntaxKind::TOPLEVEL_SEMICOLON
    )
}

/// An `if`'s arms: the statements of the `if` body followed by each `elseif`
/// body (all guarded by a condition), and the `else` body if there is one.
fn if_arms(node: &SyntaxNode) -> (Vec<Vec<SyntaxElement>>, Option<Vec<SyntaxElement>>) {
    let mut guarded = Vec::new();
    let mut unguarded = None;
    if let Some(body) = body_of(node) {
        guarded.push(region_statements(&body));
    }
    for child in node.children() {
        match child.kind() {
            SyntaxKind::ELSEIF_CLAUSE => {
                if let Some(body) = body_of(&child) {
                    guarded.push(region_statements(&body));
                }
            }
            SyntaxKind::ELSE_CLAUSE => {
                unguarded = body_of(&child).map(|body| region_statements(&body));
            }
            _ => {}
        }
    }
    (guarded, unguarded)
}

/// The body of a `try` clause of the given kind.
fn clause_body(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxNode> {
    node.children()
        .find(|c| c.kind() == kind)
        .and_then(|clause| body_of(&clause))
}

/// Whether a `while`'s test is the literal `true` — Julia's infinite loop, as
/// it has no dedicated construct for one.
fn has_literal_true_test(node: &SyntaxNode) -> bool {
    condition_of(node)
        .and_then(|cond| cond.children().next())
        .is_some_and(|test| {
            test.kind() == SyntaxKind::LITERAL
                && test
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .any(|t| t.kind() == SyntaxKind::TRUE_KW)
        })
}

/// How a statement leaves the straight-line flow, if it does. Purely
/// syntactic: `throw`/`error`/`rethrow` are matched by callee name, so a
/// consumer that must exclude a local shadow confirms the name itself.
fn jump_of(node: &SyntaxNode) -> Option<Jump> {
    match node.kind() {
        SyntaxKind::RETURN_EXPR => Some(Jump::Diverge),
        SyntaxKind::BREAK_EXPR => Some(Jump::Break),
        SyntaxKind::CONTINUE_EXPR => Some(Jump::Continue),
        SyntaxKind::CALL_EXPR => {
            let callee = CallExpr::cast(node.clone())?.callee_ident()?;
            matches!(callee.text(), "throw" | "error" | "rethrow").then_some(Jump::Diverge)
        }
        SyntaxKind::MACRO_CALL => macro_with_label(node, "goto").map(Jump::Goto),
        _ => None,
    }
}

/// The label a `@label name` statement defines.
fn label_definition(stmt: &SyntaxElement) -> Option<SmolStr> {
    let node = stmt.as_node()?;
    (node.kind() == SyntaxKind::MACRO_CALL)
        .then(|| macro_with_label(node, "label"))
        .flatten()
}

/// The label named by `@goto name` / `@label name`, when `node` is a macro call
/// to `@<name>` with a bare-name argument.
fn macro_with_label(node: &SyntaxNode, macro_name: &str) -> Option<SmolStr> {
    let name = node
        .children()
        .find(|c| c.kind() == SyntaxKind::MACRO_NAME)?;
    let simple = name
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::IDENT)
        .last()?;
    if simple.text() != macro_name {
        return None;
    }
    // `@goto lbl` leaves the label a bare `NAME`; `@goto(lbl)` wraps it in an
    // argument list.
    let arg = node
        .children()
        .skip_while(|c| c.kind() != SyntaxKind::MACRO_NAME)
        .skip(1)
        .find(|c| !is_ignorable(c.kind()))?;
    let label = match arg.kind() {
        SyntaxKind::NAME => arg,
        SyntaxKind::ARG_LIST => arg
            .children()
            .find(|c| c.kind() == SyntaxKind::ARG)?
            .children()
            .find(|c| c.kind() == SyntaxKind::NAME)?,
        _ => return None,
    };
    label
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::IDENT)
        .map(|t| SmolStr::new(t.text()))
}

/// Whether `body` contains a macro call other than `@goto`/`@label`, excluding
/// nested regions. Such a call expands to statements this graph cannot see, so
/// it may hide a jump — see [`reachable_from`] and [`Builder::lower_loop`].
fn contains_opaque_macro(body: &SyntaxNode) -> bool {
    body.children().any(|child| {
        if is_region_owner(child.kind()) {
            return false;
        }
        let opaque = child.kind() == SyntaxKind::MACRO_CALL
            && macro_with_label(&child, "goto").is_none()
            && macro_with_label(&child, "label").is_none();
        opaque || contains_opaque_macro(&child)
    })
}

/// The labels `@label` defines inside `body`, excluding those in nested regions
/// — Julia requires a `@goto` and its `@label` to share a function.
fn collect_labels(body: &SyntaxNode, out: &mut HashSet<SmolStr>) {
    for child in body.children() {
        if is_region_owner(child.kind()) {
            continue;
        }
        if let Some(name) = macro_with_label(&child, "label") {
            out.insert(name);
        }
        collect_labels(&child, out);
    }
}

/// A statement-level `a && jump` / `a || jump`: the jump node and what it does.
/// Recurses through a chained `a && b && return x`, whose jump is the innermost
/// right side.
fn short_circuit_jump(node: &SyntaxNode) -> Option<(SyntaxNode, Jump)> {
    if !node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| matches!(t.kind(), SyntaxKind::AND_AND | SyntaxKind::OR_OR))
    {
        return None;
    }
    let rhs = node.children().nth(1)?;
    if let Some(jump) = jump_of(&rhs) {
        return Some((rhs, jump));
    }
    (rhs.kind() == SyntaxKind::BINARY_EXPR)
        .then(|| short_circuit_jump(&rhs))
        .flatten()
}

fn render_region(cfg: &ControlFlowGraph, src: &str, out: &mut String) {
    for (id, block) in cfg.iter() {
        let stmts = block
            .stmts
            .iter()
            .map(|range| snippet(src, *range))
            .collect::<Vec<_>>()
            .join("; ");
        let dead = if cfg.is_reachable(id) { "" } else { " (dead)" };
        let term = match block.terminator {
            Terminator::Goto(t) => format!("-> bb{}", t.index()),
            Terminator::Branch { then_blk, else_blk } => format!(
                "-> then bb{}, else bb{}",
                then_blk.index(),
                else_blk.index()
            ),
            Terminator::Return => "-> return".to_string(),
            Terminator::Diverge => "-> diverge".to_string(),
            Terminator::Unreachable => "-> unreachable".to_string(),
        };
        let i = id.index();
        out.push_str(&format!("  bb{i}{dead}: [{stmts}] {term}\n"));
    }
}

/// A one-line snippet of `src` at `range`, with interior whitespace collapsed.
fn snippet(src: &str, range: TextRange) -> String {
    let flat = src[range].split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(40) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}
