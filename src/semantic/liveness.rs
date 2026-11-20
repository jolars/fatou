//! Backward may-liveness of local bindings on the shared control-flow graph.

use std::collections::{HashMap, HashSet};

use rowan::TextRange;
use rowan::ast::AstNode as _;

use crate::ast::{AssignmentExpr, AstToken as _, CallExpr, body_of};
use crate::syntax::{SyntaxKind, SyntaxNode};

use super::cfg::{FlowEffect, flow_children};
use super::{
    Access, BindingId, BindingKind, ControlFlowGraph, FileControlFlow, ScopeKind, SemanticModel,
};

#[derive(Clone, Copy)]
enum Effect {
    Read(BindingId),
    Kill(BindingId),
    Store(TextRange, BindingId),
}

fn transfer(effect: Effect, live: &mut HashSet<BindingId>) {
    match effect {
        Effect::Read(id) => {
            live.insert(id);
        }
        Effect::Kill(id) | Effect::Store(_, id) => {
            live.remove(&id);
        }
    }
}

fn eligible(model: &SemanticModel, id: BindingId) -> bool {
    let binding = model.binding(id);
    !model.local_type_dependencies.contains(&id)
        && !binding.name.starts_with('_')
        && match binding.kind {
            BindingKind::Local
            | BindingKind::LetVar
            | BindingKind::Param
            | BindingKind::KeywordParam => true,
            BindingKind::ForVar => model.scope(binding.scope).kind == ScopeKind::For,
            _ => false,
        }
}

fn opaque_macro(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::MACRO_CALL
        || matches!(
            node.kind(),
            SyntaxKind::STRING_LITERAL | SyntaxKind::CMD_LITERAL
        ) && node
            .children_with_tokens()
            .any(|el| el.kind() == SyntaxKind::STRING_PREFIX)
}

fn unsupported_flow(node: &SyntaxNode) -> bool {
    node.kind() == SyntaxKind::TRY_EXPR
        || CallExpr::cast(node.clone())
            .and_then(|call| call.callee_ident())
            .is_some_and(|name| matches!(name.text(), "throw" | "error" | "rethrow"))
        || flow_children(node).any(|child| unsupported_flow(&child))
}

pub(crate) fn dead_stores(
    root: &SyntaxNode,
    model: &SemanticModel,
    cfg: &FileControlFlow,
) -> Vec<(TextRange, BindingId)> {
    let mut writes = HashMap::new();
    let mut captured = HashSet::new();
    for (index, binding) in model.bindings().iter().enumerate() {
        let id = BindingId(index as u32);
        if eligible(model, id) && !binding.def_range.is_empty() {
            writes.insert(binding.def_range, id);
        }
    }
    for ident in model.idents() {
        let Some(id) = ident.binding.filter(|&id| eligible(model, id)) else {
            continue;
        };
        if ident.access != Access::Read {
            writes.insert(ident.range, id);
        }
        let mut scope = ident.scope;
        let owner = model.binding(id).scope;
        while scope != owner {
            let current = model.scope(scope);
            if matches!(current.kind, ScopeKind::Function | ScopeKind::Comprehension) {
                captured.insert(id);
                break;
            }
            let Some(parent) = current.parent else { break };
            scope = parent;
        }
    }

    // Include writes in opaque or unreachable expressions when deciding which
    // assignment belongs to unused-binding.
    let mut first_write = HashMap::new();
    for (&range, &id) in &writes {
        let info = model.binding(id);
        if info.read || !matches!(info.kind, BindingKind::Local | BindingKind::LetVar) {
            continue;
        }
        let node = match root.covering_element(range) {
            rowan::NodeOrToken::Node(node) => Some(node),
            rowan::NodeOrToken::Token(token) => token.parent(),
        };
        let is_assignment_target = node.is_some_and(|node| {
            node.ancestors()
                .filter_map(AssignmentExpr::cast)
                .next()
                .and_then(|assignment| assignment.lhs())
                .is_some_and(|target| target.syntax().text_range().contains_range(range))
        });
        if is_assignment_target {
            let first = first_write.entry(id).or_insert(range.start());
            *first = (*first).min(range.start());
        }
    }

    let macro_sites: Vec<_> = root
        .descendants()
        .filter(opaque_macro)
        .map(|node| node.text_range().start())
        .collect();
    let mut result = Vec::new();
    for (owner, graph) in std::iter::once((root.clone(), cfg.toplevel())).chain(
        cfg.regions()
            .iter()
            .map(|(ptr, graph)| (ptr.to_node(root), graph)),
    ) {
        let body = if owner == *root {
            Some(root.clone())
        } else {
            body_of(&owner)
        };
        let Some(body) = body else { continue };
        let range = body.text_range();
        let macro_start = macro_sites.partition_point(|&start| start < range.start());
        if owner.ancestors().skip(1).any(|node| opaque_macro(&node))
            || macro_sites
                .get(macro_start)
                .is_some_and(|&start| start < range.end())
            || unsupported_flow(&body)
            || !graph.supports_liveness()
        {
            continue;
        }

        let effects: Vec<Vec<_>> = graph
            .iter()
            .map(|(block, _)| {
                let mut effects = Vec::new();
                for effect in graph.effects(block) {
                    match *effect {
                        FlowEffect::Observe(range) => {
                            let start = model
                                .idents()
                                .partition_point(|ident| ident.range.start() < range.start());
                            effects.extend(
                                model.idents()[start..]
                                    .iter()
                                    .take_while(|ident| ident.range.start() < range.end())
                                    .filter(|ident| {
                                        ident.access != Access::Write && !ident.is_macro
                                    })
                                    .filter_map(|ident| ident.binding)
                                    .filter(|&id| eligible(model, id) && !captured.contains(&id))
                                    .map(Effect::Read),
                            );
                        }
                        FlowEffect::Store(target)
                        | FlowEffect::Rebind(target)
                        | FlowEffect::Save(target) => {
                            if let Some(&id) =
                                writes.get(&target).filter(|id| !captured.contains(id))
                            {
                                effects.push(match effect {
                                    FlowEffect::Store(_) => Effect::Store(target, id),
                                    FlowEffect::Save(_) => Effect::Read(id),
                                    _ => Effect::Kill(id),
                                });
                            }
                        }
                    }
                }
                effects
            })
            .collect();
        collect_dead(graph, &effects, &first_write, &mut result);
    }
    result.sort_unstable_by_key(|(range, _)| range.start());
    result.dedup();
    result
}

fn collect_dead(
    graph: &ControlFlowGraph,
    effects: &[Vec<Effect>],
    first_write: &HashMap<BindingId, rowan::TextSize>,
    result: &mut Vec<(TextRange, BindingId)>,
) {
    let mut live_in = vec![HashSet::new(); effects.len()];
    let mut predecessors = vec![Vec::new(); effects.len()];
    let mut pending = Vec::new();
    let mut queued = vec![false; effects.len()];
    for (id, block) in graph.iter() {
        if graph.is_reachable(id) {
            pending.push(id);
            queued[id.index()] = true;
            for successor in block.terminator.successors() {
                predecessors[successor.index()].push(id);
            }
        }
    }
    let live_out = |block: &super::BasicBlock, live_in: &[HashSet<BindingId>]| {
        block
            .terminator
            .successors()
            .flat_map(|succ| live_in[succ.index()].iter().copied())
            .collect::<HashSet<_>>()
    };
    while let Some(id) = pending.pop() {
        queued[id.index()] = false;
        let mut live = live_out(graph.block(id), &live_in);
        for &effect in effects[id.index()].iter().rev() {
            transfer(effect, &mut live);
        }
        if live != live_in[id.index()] {
            live_in[id.index()] = live;
            for &predecessor in &predecessors[id.index()] {
                if !queued[predecessor.index()] {
                    queued[predecessor.index()] = true;
                    pending.push(predecessor);
                }
            }
        }
    }

    for (id, block) in graph.iter().filter(|(id, _)| graph.is_reachable(*id)) {
        let mut live = live_out(block, &live_in);
        for &effect in effects[id.index()].iter().rev() {
            if let Effect::Store(range, binding) = effect {
                let covered_by_unused_binding = first_write.get(&binding) == Some(&range.start());
                if !live.contains(&binding) && !covered_by_unused_binding {
                    result.push((range, binding));
                }
            }
            transfer(effect, &mut live);
        }
    }
}
