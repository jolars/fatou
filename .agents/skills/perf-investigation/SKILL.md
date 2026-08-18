---
name: perf-investigation
description: >-
  Profile-driven performance work on Fatou's formatter, parser, or linter.
  Measure with bench/profile.sh first, read the phase split before the leaf
  list, classify the hotspot, apply the smallest matching fix, and prove median
  wall time moved before committing. Formatting output must stay byte-identical
  and the CST lossless: a perf change that alters either is a bug, not a
  trade-off.
---

Use this skill when asked to "speed up the formatter", "profile the parser",
"find the hotspots", "look into why `format --check` is slow", or anything else
where the job is *measure where the time goes on a real Julia file and recover
wall time*.

## The one rule that outranks the rest

**Output must not change.** `format(x)` is byte-identical before and after,
`format(format(x)) == format(x)`, and `reconstruct(text) == text`. There is no
speed/quality trade-off available here: the formatter is the sole authority on
layout (`AGENTS.md`, tenet 1), so a faster formatter that lays anything out
differently has broken the contract, not optimized it. The formatter fixture
gate is what proves it — see §Verify.

## Scope boundaries

- Both member crates stay `wasm32-unknown-unknown`-clean. **No threads, no
  clock, no filesystem, no process** in `fatou-parser` or `fatou-formatter` —
  which rules out the usual "just parallelize it" answer inside the engine, and
  rules out thread-local pools unless you are certain of the wasm story. Rayon
  parallelism belongs at the CLI edge (`check_paths`), where it already is.
- Incremental reparse is first-class (tenet 2). A parser change that speeds up
  the batch path but breaks or bypasses `reparse.rs` is not a win; check
  `tests/salsa_incremental.rs` and the reparse suite.
- Don't fix formatter cost by moving work into the parser or vice versa (tenets
  3 and 4). The phase split will tempt you here — resist it.
- Benchmarks and profiles are **measured, never asserted** and never a CI gate
  (`.claude/rules/docs.md`). Do not add a perf assertion to a test.

## Related rules to read first

- `.claude/rules/formatter.md` — the layout engine's contract and the fixture
  gate.
- `.claude/rules/parser.md` — losslessness and the incremental-reparse
  obligation.
- `.claude/rules/docs.md` — the benchmark artifacts (`bench/results.json`) and
  when a moved number must be re-measured and committed.

## Harness

```sh
task profile                                  # JuliaSyntax/src/parser.jl, 300 iters
task profile -- path/to/file.jl               # a file you care about
task profile -- --dir bench/corpus/DataFrames # the rayon `--check` path, 20 iters
ITERATIONS=800 task profile                   # more samples for a small delta
```

A few hundred samples is enough to rank the phase split; a fix you expect to
move something by a couple of percent needs a few thousand, so raise
`ITERATIONS` rather than trusting a thin profile.

`bench/profile.sh` builds `benches/format_compare.rs` under the `profiling`
cargo profile and samples it with perf. It prints a **phase split** and a
**self-time leaf list**, and leaves `bench/profile.svg` (flamegraph) plus
`bench/profile.data` (raw, for your own `perf report`). Both are gitignored: a
profile is a local observation, unlike `bench/results.json`.

First run needs the corpus: `task bench-corpus`.

Two things the script sets that are load-bearing, and that you will break if you
hand-roll a perf invocation:

- **`-Cforce-frame-pointers=yes` plus `--call-graph fp`.** Release codegen omits
  frame pointers, and DWARF unwinding on this toolchain drops the callchain on
  most samples — silently, leaving every inclusive view collapsed to self time
  and the flamegraph one frame deep. Frame pointers give complete stacks and 50x
  smaller recordings. Never append a bare `-g` after `--call-graph dwarf`; `-g`
  *is* `--call-graph fp` and quietly overrides it.
- **`[profile.profiling]` in the root `Cargo.toml`**, which unsets the release
  profile's `strip = "symbols"`. Profiling a plain `--release` build gives
  unresolved frames.

For wall-time verification, not profiling, use hyperfine on the real binary:

```sh
cargo build --release
hyperfine --warmup 3 -m 20 \
  'taskset -c 2 ./target/release/fatou format --check bench/corpus/JuliaSyntax'
```

`taskset` pins one core; without it, scheduler migration adds several percent of
jitter and small wins vanish into it. Discard warmups, take the median.

**Interleave the A/B when the machine is not quiet.** A quiet run has a \~4-5%
spread on this bench; a busy one drifts far more than that *between* a before
and an after measured minutes apart, which is enough to invent or erase a win.
Build both binaries first, then alternate them round by round so drift hits both
equally, and report min alongside median — min is the least contaminated
estimate of the true cost under interference:

```sh
cargo build --release --bench format_compare        # after
cp target/release/deps/format_compare-* /tmp/bench_new
git stash push <the changed file>
cargo build --release --bench format_compare        # before
cp target/release/deps/format_compare-* /tmp/bench_old
git stash pop
# then alternate old/new for N rounds and compare
```

Check `uptime` and `ps -eo pcpu,comm --sort=-pcpu | head` before believing any
number.

## Read the phase split before the leaf list

The leaf list flatters whatever is at the bottom of the stack — `malloc` and
rowan internals always look enormous and are almost never where a fix goes. The
inclusive split tells you which *phase* to open. As of 2026-08-13, after the
`fold_docstrings` fix (JuliaSyntax `parser.jl`, 137 KB):

```
format                  99%
  parse                 46%      <- still the parser, not the formatter
    parse_expr_in       16%
    parse_function_like 16%
    run_block_inner     14%
  format_node           49%
    lower               28%
      build_block_body  25%
    print_at            14%
```

Two readings that follow, and that anyone profiling "the formatter" needs to
absorb before touching anything:

1. **Over half the cost of `format()` is parsing.** `format --check` on a cold
   file cannot beat its parse. If the target is LSP latency instead, the warm
   path (`format_node` via the salsa-cached CST,
   `lsp::format::format_edits_via_db`) skips the parse entirely — so profile
   *that* phase, and don't spend effort on parse cost that the LSP never pays.
2. **Sub-percentages are shares of total, not of the phase.** `lower` at 28% is
   most of the formatter's own work, not a quarter of it.

Re-measure this table when it goes stale rather than trusting the numbers above;
they are a starting map, not a fact.

## Classify the hotspot

Fatou's profile has not yet been mined, so unlike a mature bucket list this is
mostly *open leads plus known shapes*. Add to it as things are confirmed or
ruled out — a lead that was measured and didn't pay belongs in §Don't redo.

- **Allocator traffic** — `_int_malloc`, `malloc_consolidate`, `unlink_chunk`,
  `_int_free_chunk`, `cfree`, `realloc` together run to roughly a fifth of total
  time. This is a *symptom*; the fix is always at an allocation site upstream,
  found by following callers, never at the allocator. Don't reach for a
  different global allocator as a first move — it hides the real site and the
  wasm constraint makes the choice non-obvious.
- **Per-node `Vec` in a recursive pass over the event stream** — *confirmed and
  fixed once* (`fold_docstrings`, 23% → 0.6%, -17% wall). The shape to
  recognize: a pass that rebuilds the event stream level by level, allocating
  per node and recopying every event once per level of nesting above it, when
  the transform only *inserts* events. The fix is the same each time — compute
  subtree extents once with a stack pass so a subtree can be skipped in O(1),
  mark the insertion points, splice in one final pass. `parse` still has
  post-passes; check whether any other one rebuilds rather than marks.
- **`RawVec::grow_one` / `finish_grow`** — a `Vec` growing element by element
  where the final size is known or boundable. Reserve up front. **Trap:** the
  type parameter in a `RawVec::<T>::grow_one` symbol is not reliable evidence of
  which `Vec` it is; identically-laid-out instantiations get merged. Follow the
  caller chain (`perf report -g graph,caller`), don't read the type off the
  symbol.
- **Best-fit width probing** — `printer::fits` + `fits_stack` is \~10% of total
  and is the layout engine re-walking IR to decide whether a group fits. The
  fixes that preserve output exactly are memoizing or precomputing flat widths
  on the IR node, and early-exit once the accumulated width exceeds
  `line_width`. Anything that changes *which* groups break is a tenet violation,
  not an optimization.
- **IR churn** — `Rc<[Ir]>::from_iter_exact`, `to_rc_slice`, `drop_glue::<Ir>`,
  `Rc<[Ir]>::drop_slow`. The IR is built and thrown away once per format. Look
  for `Vec<Ir>` built then converted, and for IR nodes cloned where a borrow
  would do.
- **CST cursor traversal** —
  `rowan::cursor::{Preorder::next, next_sibling,   first_child, free}` and
  `SyntaxElementChildren::next`, \~7% combined. This is the formatter walking
  the red tree; `rowan::cursor::free` means cursor nodes are being allocated and
  dropped. Proportional to how many times lowering re-walks the same children.
  Look for a rule that iterates `children()` more than once over the same node.
- **rowan green-tree construction** — `NodeCache::{token,node}`,
  `Arc::drop_slow`, `ThinArc::from_header_and_iter`. Proportional to token and
  node count. Reducing it means emitting fewer nodes, which is invasive: it
  changes the CST and therefore the parser snapshots, the oracle projection, and
  possibly formatter output. Last resort, and never by pooling the `NodeCache`
  across parses — it holds `Arc`'d green nodes, so pooling leaks in the LSP and
  produces a misleadingly warm benchmark after iteration one.

## Apply the smallest matching fix

- **Don't theorize before measuring.** Pre-sizing a `Vec`, adding a fast-path
  gate, and hoisting an allocation are all changes that *should* help and
  routinely don't. Measure each one.
- **Verify with wall time, not perf.** A change can delete a symbol from the
  top-25 without moving the median — sample relocation, not work eliminated. The
  hyperfine median is the truth; the profile only says where to look.
- **One change per commit**, so one regression can't mask another's win.
- **Revert promptly.** If 20 hyperfine runs show no median shift beyond the
  noise floor, the fix doesn't pay. Don't ship flat refactors as perf.

## Verify

Every perf commit, without exception:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

The suites that specifically catch a perf change that changed behavior:

```sh
cargo test -p fatou-formatter --test formatter        # fixtures + idempotence + clean reparse
cargo test -p fatou-parser --test parser_snapshots    # CST shape and losslessness
cargo test -p fatou-parser --test juliasyntax_oracle  # projection parity
cargo test --test salsa_incremental                   # reparse/memo firewall intact
```

For a parser change, also round-trip a real corpus:

```sh
cargo run --release -- debug format bench/corpus/JuliaSyntax
```

**Never accept an insta snapshot you have not read**, and on a perf commit a
changed snapshot is a red flag by default: the whole premise is that behavior is
identical.

If the win is large enough to move the published performance page, re-run
`task bench` and commit `bench/results.json` in the same commit — that artifact
is the sole source of the docs numbers and is never re-measured at site build.

## Commit format

Name the bucket and quote the median, including when it's in the noise — that's
the honest record and lets a reviewer decide whether to ship it at all.

```
perf(parser): hoist the per-node Vec out of `fold_docstrings`

The profile put 22% of `format()` in this pass: it allocates two Vecs per
node and rescans forward for each `Start`'s matching `Finish`. Carry the
subtree extent on the event instead.

Median `fatou format --check bench/corpus/JuliaSyntax` (20 runs, pinned):
~X ms -> ~Y ms (~Z%).
```

Keep commits atomic per area — root crate, `crates/fatou-parser`,
`crates/fatou-formatter` — because the release tooling routes versions by path
(`.claude/rules/release.md`).

## Key files

- `bench/profile.sh` — the harness. `benches/format_compare.rs` is what it
  samples; `Cargo.toml`'s `[profile.profiling]` is what makes symbols resolve.
- `crates/fatou-formatter/src/formatter/printer.rs` — `fits`, `fits_stack`,
  `print_at`. The best-fit layout engine; \~10% of total sits in width probing.
- `crates/fatou-formatter/src/formatter/rules.rs` — `lower`, `lower_node`,
  `collect_body_lines`/`collect_body_elements` (the lowering spine, so its
  inclusive share is *all* of lowering, not its own cost). 5300 lines; find the
  specific rule your hotspot names.
- `crates/fatou-formatter/src/formatter/ir.rs` — `Ir`, and the `Rc<[Ir]>`
  representation behind the churn bucket.
- `crates/fatou-parser/src/parser/core.rs` — `parse`, `emit_items`,
  `fold_docstrings`.
- `crates/fatou-parser/src/parser/{lexer,expr,structural}.rs` — the parse
  pipeline's three stages.
- `bench/corpus/JuliaSyntax/src/parser.jl` — 137 KB, the default stress file.
  `bench/corpus/DataFrames` is the docstring- and macro-heavy contrast.

## Don't redo / known traps

- **DWARF call graphs are broken on this machine.** `--call-graph dwarf` records
  fine and produces mostly *empty* callchains, so inclusive views collapse to
  self time and the flamegraph is one frame deep — with no error. Use frame
  pointers. If a profile looks implausibly flat, check stack depth first:
  `perf script -i bench/profile.data | awk 'BEGIN{RS="";FS="\n"} {print NF-1}' | sort -n | uniq -c`
- **`perf report` without `--no-inline` renames inline frames to their short
  source names**, so `formatter::core::format` appears as a bare `format` and
  greps for a phase root miss silently.
- **Don't read a generic's type parameter off a symbol name** — see the
  `RawVec::grow_one` trap above.
- **Don't pool rowan's `NodeCache` across parses.** It holds `Arc`'d green
  nodes: a leak in the language server, and a warm cache after iteration one
  makes the benchmark lie.
- **Don't profile a `--release` build.** `strip = "symbols"` is set for
  distribution; use the `profiling` profile.
- **Don't benchmark a formatter change on `--dir` alone.** That path is
  rayon-parallel across files, so a 12-thread machine hides a per-file
  regression behind spare cores. Single-file first, directory second.

## Report-back format

1. Phase and hotspot addressed (function + inclusive and self %).
2. Bucket from §Classify.
3. Median wall-time delta (hyperfine, pinned, ≥20 runs) — even if in the noise.
4. Test/clippy/fmt status, and explicitly that the formatter fixture gate and
   parser snapshots are unchanged.
5. What was tried and reverted, with the measurement that killed it.
6. Next hotspot, ranked.
