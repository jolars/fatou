//! Structural tests for the per-region control-flow graph
//! ([`fatou::semantic::FileControlFlow`]). Each case parses a small Julia
//! program, builds every region's CFG, and snapshots a textual dump of blocks +
//! statements + terminators. The reachability predicate the linter will consume
//! ([`FileControlFlow::is_unreachable`]) is asserted directly.

use fatou::parser::parse;
use fatou::semantic::FileControlFlow;
use insta::assert_snapshot;

fn build(src: &str) -> FileControlFlow {
    let parsed = parse(src);
    assert!(
        parsed.diagnostics.is_empty(),
        "parse: {:?}",
        parsed.diagnostics
    );
    FileControlFlow::build(&parsed.cst)
}

fn cfg_dump(src: &str) -> String {
    build(src).render(src)
}

/// The number of regions: the file top level plus one per function-like body
/// and `module` body.
fn region_count(src: &str) -> usize {
    1 + build(src).regions().len()
}

/// Whether the (first) statement whose source text is exactly `needle` sits in
/// an unreachable block. `needle` must occur exactly once in `src`.
fn unreachable(src: &str, needle: &str) -> bool {
    let start = src.find(needle).expect("needle occurs in source");
    assert_eq!(src.rfind(needle), Some(start), "needle must be unique");
    let range = rowan::TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(start + needle.len()).unwrap().into(),
    );
    build(src).is_unreachable(range)
}

// --- straight-line and branching -------------------------------------------

#[test]
fn sequential_statements() {
    assert_snapshot!(cfg_dump("a = 1\nb = 2\nf(a, b)\n"));
}

#[test]
fn if_without_else() {
    assert_snapshot!(cfg_dump("if c\n    g()\nend\nh()\n"));
}

#[test]
fn if_with_else() {
    assert_snapshot!(cfg_dump("if c\n    a()\nelse\n    b()\nend\nafter()\n"));
}

#[test]
fn elseif_chain() {
    assert_snapshot!(cfg_dump(
        "if a\n    x()\nelseif b\n    y()\nelse\n    z()\nend\nafter()\n"
    ));
}

#[test]
fn begin_and_let_inline_into_the_enclosing_flow() {
    assert_snapshot!(cfg_dump(
        "begin\n    a()\nend\nlet x = 1\n    b(x)\nend\nc()\n"
    ));
}

// --- regions ---------------------------------------------------------------

#[test]
fn function_body_is_its_own_region() {
    assert_snapshot!(cfg_dump("function f(x)\n    return x\nend\nf(1)\n"));
}

#[test]
fn module_and_nested_definitions_are_regions() {
    let src = "module M\nfunction f()\n    1\nend\ng() do x\n    return x\nend\nend\n";
    assert_eq!(region_count(src), 4); // top level + module + f + do block
    assert_snapshot!(cfg_dump(src));
}

#[test]
fn an_inner_function_does_not_join_the_outer_flow() {
    // The inner `return` leaves the inner region only, so `outer_tail()` stays
    // reachable.
    let src = "function outer()\n    inner = function ()\n        return 1\n    end\n    outer_tail()\nend\n";
    assert!(!unreachable(src, "outer_tail()"));
}

// --- divergence and unreachability -----------------------------------------

#[test]
fn statements_after_return_are_unreachable() {
    let src = "function f()\n    return 1\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
    assert_snapshot!(cfg_dump(src));
}

#[test]
fn both_if_arms_returning_makes_the_tail_unreachable() {
    // The case the CFG buys over the shallow "terminator is a direct statement
    // of the block" shape.
    let src = "function f(a)\n    if a\n        return 1\n    else\n        return 2\n    end\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
    assert_snapshot!(cfg_dump(src));
}

#[test]
fn an_elseif_chain_diverging_in_every_arm_makes_the_tail_unreachable() {
    let src = "function f(a, b)\n    if a\n        return 1\n    elseif b\n        return 2\n    else\n        return 3\n    end\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
}

#[test]
fn one_if_arm_returning_leaves_the_tail_reachable() {
    let src = "function f(a)\n    if a\n        return 1\n    end\n    alive()\nend\n";
    assert!(!unreachable(src, "alive()"));
}

#[test]
fn an_elseif_chain_without_else_leaves_the_tail_reachable() {
    let src = "function f(a)\n    if a\n        return 1\n    elseif b\n        return 2\n    end\n    alive()\nend\n";
    assert!(!unreachable(src, "alive()"));
}

#[test]
fn throw_and_error_diverge() {
    let src = "function f()\n    throw(ArgumentError(\"x\"))\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
    assert_snapshot!(cfg_dump(src));
}

#[test]
fn error_and_rethrow_diverge() {
    assert!(unreachable(
        "function f()\n    error(\"boom\")\n    dead()\nend\n",
        "dead()"
    ));
    assert!(unreachable(
        "function f()\n    rethrow()\n    dead()\nend\n",
        "dead()"
    ));
}

#[test]
fn a_shadowed_terminator_name_still_diverges_syntactically() {
    // The CFG is purely syntactic: a caller that must exclude a local `throw`
    // confirms the name itself (`RuleContext::resolves_to_base`).
    let src = "function f(throw)\n    throw(1)\n    tail()\nend\n";
    assert!(unreachable(src, "tail()"));
}

// --- loops -----------------------------------------------------------------

#[test]
fn for_loop_with_break_and_continue() {
    assert_snapshot!(cfg_dump(
        "for i in xs\n    if i > 1\n        break\n    end\n    if i < 0\n        continue\n    end\n    use(i)\nend\ndone()\n"
    ));
}

#[test]
fn a_for_loop_may_run_zero_times_so_the_tail_is_reachable() {
    let src = "function f(xs)\n    for x in xs\n        return x\n    end\n    alive()\nend\n";
    assert!(!unreachable(src, "alive()"));
}

#[test]
fn while_true_without_break_makes_the_tail_unreachable() {
    let src = "function f()\n    while true\n        step()\n    end\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
    assert_snapshot!(cfg_dump(src));
}

#[test]
fn while_true_with_a_break_leaves_the_tail_reachable() {
    let src = "function f()\n    while true\n        if done()\n            break\n        end\n    end\n    alive()\nend\n";
    assert!(!unreachable(src, "alive()"));
}

#[test]
fn a_break_targets_the_innermost_loop() {
    assert_snapshot!(cfg_dump(
        "for i in xs\n    for j in ys\n        break\n    end\n    outer()\nend\n"
    ));
}

#[test]
fn statements_after_break_in_the_same_block_are_unreachable() {
    let src = "for i in xs\n    break\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
}

// --- short-circuit divergence ----------------------------------------------

#[test]
fn short_circuit_return_is_a_conditional_divergence() {
    let src = "function f(x)\n    x < 0 && return 0\n    tail(x)\nend\n";
    assert!(!unreachable(src, "tail(x)"));
    assert_snapshot!(cfg_dump(src));
}

#[test]
fn an_unresolvable_short_circuit_jump_stays_a_plain_statement() {
    // `break` outside any loop is invalid Julia (`break-outside-loop` reports
    // it); the CFG must not invent a divergence and call the tail dead.
    let src = "function f(c)\n    c && break\n    tail()\nend\n";
    assert!(!unreachable(src, "tail()"));
}

#[test]
fn short_circuit_continue_branches_inside_a_loop() {
    assert_snapshot!(cfg_dump(
        "for i in xs\n    i == 2 && continue\n    use(i)\nend\n"
    ));
}

// --- goto/label ------------------------------------------------------------

#[test]
fn goto_jumps_backward_to_its_label() {
    assert_snapshot!(cfg_dump(
        "function f()\n    @label top\n    step()\n    @goto top\nend\n"
    ));
}

#[test]
fn a_forward_goto_leaves_the_label_target_reachable() {
    // The statement skipped over is dead; the labeled tail is not.
    let src =
        "function f(c)\n    c && @goto done\n    middle()\n    @label done\n    tail()\nend\n";
    assert!(!unreachable(src, "tail()"));
    assert!(!unreachable(src, "middle()"));
    assert_snapshot!(cfg_dump(src));
}

#[test]
fn code_between_an_unconditional_goto_and_its_label_is_unreachable() {
    let src = "function f()\n    @goto done\n    dead()\n    @label done\n    tail()\nend\n";
    assert!(unreachable(src, "dead()"));
    assert!(!unreachable(src, "tail()"));
    assert_snapshot!(cfg_dump(src));
}

#[test]
fn a_label_nobody_jumps_to_after_a_return_stays_unreachable() {
    let src = "function f()\n    return 1\n    @label orphan\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
}

#[test]
fn a_macro_call_in_the_region_may_hide_the_goto_that_reaches_a_label() {
    // A macro expands to code the CFG never sees, and Julia code really does
    // hide `@goto` in one (JSON3's `@eof` is exactly this shape), so a label
    // with no visible `@goto` cannot be *proven* dead once any macro is in
    // play.
    let src = "function f(pos)\n    @eof\n    return 1\n    @label invalid\n    handle()\nend\n";
    assert!(!unreachable(src, "handle()"));
    // The tail after the `return` is still dead: no expansion can rescue a
    // statement that follows an unconditional divergence in the same block.
    let plain = "function f(pos)\n    @eof\n    return 1\n    dead()\nend\n";
    assert!(unreachable(plain, "dead()"));
}

#[test]
fn a_macro_call_in_an_infinite_loop_body_may_hide_a_break() {
    let src = "function f()\n    while true\n        @maybe_stop()\n    end\n    tail()\nend\n";
    assert!(!unreachable(src, "tail()"));
}

#[test]
fn a_goto_to_an_undefined_label_is_a_plain_statement() {
    // Invalid Julia; the CFG must not invent an edge, and the tail stays
    // reachable rather than being reported as dead.
    let src = "function f()\n    @goto nowhere\n    tail()\nend\n";
    assert!(!unreachable(src, "tail()"));
}

#[test]
fn a_label_in_a_nested_function_is_not_visible_to_the_outer_region() {
    let src = "function outer()\n    inner() do\n        @label here\n        1\n    end\n    @goto here\n    tail()\nend\n";
    assert!(!unreachable(src, "tail()"));
}

// --- try/catch/finally -----------------------------------------------------

#[test]
fn try_catch_finally() {
    assert_snapshot!(cfg_dump(
        "try\n    risky()\ncatch e\n    handle(e)\nfinally\n    cleanup()\nend\nafter()\n"
    ));
}

#[test]
fn try_and_catch_both_returning_makes_the_tail_unreachable() {
    let src = "function f()\n    try\n        return 1\n    catch\n        return 2\n    end\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
}

#[test]
fn a_catch_body_is_reachable_even_when_the_try_body_diverges() {
    let src =
        "function f()\n    try\n        return 1\n    catch\n        handle()\n    end\nend\n";
    assert!(!unreachable(src, "handle()"));
}

#[test]
fn a_finally_body_is_reachable_when_both_arms_diverge() {
    let src = "function f()\n    try\n        return 1\n    catch\n        return 2\n    finally\n        cleanup()\n    end\nend\n";
    assert!(!unreachable(src, "cleanup()"));
}

// --- the unreachable index -------------------------------------------------

/// The reference implementation of [`FileControlFlow::is_unreachable`]: the
/// linear scan over every block of every region that the index replaces.
fn scan_unreachable(cfg: &FileControlFlow, range: rowan::TextRange) -> bool {
    std::iter::once(cfg.toplevel())
        .chain(cfg.regions().iter().map(|(_, graph)| graph))
        .any(|graph| {
            graph
                .iter()
                .any(|(id, block)| !graph.is_reachable(id) && block.stmts.contains(&range))
        })
}

#[test]
fn a_range_that_is_not_a_statement_is_never_unreachable() {
    // `dead` alone is a token inside the dead statement, not a statement of any
    // block, so the answer is the conservative one.
    let src = "function f()\n    return 1\n    dead()\nend\n";
    assert!(unreachable(src, "dead()"));
    assert!(!unreachable(src, "dead"));
    assert!(!unreachable(src, "function f()"));
}

// --- corpus ----------------------------------------------------------------

/// Every parser fixture as `(case name, source)`, in name order.
fn parser_fixtures() -> Vec<(String, String)> {
    // The corpus lives with the parser crate; this suite reuses it as a body
    // of real-world shapes for the control-flow graph.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/fatou-parser/tests/fixtures/parser");
    let mut cases: Vec<_> = std::fs::read_dir(&dir)
        .expect("read parser fixtures dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "fixture corpus must not be empty");
    cases
        .into_iter()
        .map(|case| {
            let name = case.file_name().unwrap().to_string_lossy().to_string();
            let src = std::fs::read_to_string(case.join("input.jl")).expect("read input.jl");
            (name, src)
        })
        .collect()
}

/// The index must answer exactly what the linear scan would, statement by
/// statement, over every parser fixture.
#[test]
fn the_index_agrees_with_a_scan_over_every_fixture() {
    for (name, src) in parser_fixtures() {
        let cfg = FileControlFlow::build(&parse(&src).cst);
        let graphs =
            std::iter::once(cfg.toplevel()).chain(cfg.regions().iter().map(|(_, graph)| graph));
        for graph in graphs {
            for block in graph.blocks() {
                for range in &block.stmts {
                    assert_eq!(
                        cfg.is_unreachable(*range),
                        scan_unreachable(&cfg, *range),
                        "`{name}`: index disagrees with the scan at {range:?}"
                    );
                }
            }
        }
    }
}

/// Every parser fixture — error-recovery trees included — must build a
/// well-formed graph: no panic, every edge in range, and the entry reachable.
/// The CFG runs on whatever the parser produced, so a malformed tree is a shape
/// it has to survive, not one it may assume away.
#[test]
fn every_parser_fixture_builds_a_well_formed_graph() {
    for (name, src) in parser_fixtures() {
        let cfg = FileControlFlow::build(&parse(&src).cst);
        let graphs =
            std::iter::once(cfg.toplevel()).chain(cfg.regions().iter().map(|(_, graph)| graph));
        for graph in graphs {
            let blocks = graph.blocks();
            assert!(!blocks.is_empty(), "`{name}`: a region has no entry block");
            assert!(
                graph.is_reachable(graph.entry()),
                "`{name}`: the entry block must be reachable"
            );
            for block in blocks {
                if let fatou::semantic::Terminator::Goto(target) = block.terminator {
                    assert!(target.index() < blocks.len(), "`{name}`: edge out of range");
                }
                if let fatou::semantic::Terminator::Branch { then_blk, else_blk } = block.terminator
                {
                    assert!(
                        then_blk.index() < blocks.len() && else_blk.index() < blocks.len(),
                        "`{name}`: edge out of range"
                    );
                }
            }
        }
    }
}
