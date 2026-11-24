# Fatou performance measurements

Use `task bench-corpus` to fetch the pinned fixtures when needed. The default
profile uses JuliaSyntax's `src/parser.jl`. DataFrames provides a contrast with
more docstrings and macros.

```sh
task profile
task profile -- path/to/file.jl
task profile -- --dir bench/corpus/DataFrames
ITERATIONS=800 task profile
```

`bench/profile.sh` records `benches/format_compare.rs` using the `profiling`
Cargo profile and frame-pointer call chains. It prints the phase split and
leaves `bench/profile.data` and `bench/profile.svg` for follow-up. The directory
mode measures parallel checking. For timing a formatter change, also measure the
release CLI on an individual file. Use a persistent server for editor requests.

Run the formatter fixtures, incremental parser tests, or JuliaSyntax oracle as
applicable. Compare formatter output byte for byte on matching inputs. Treat
changes to the CST and formatter snapshots as behavior changes and investigate
them before accepting them.

## Historical hotspot notes

The examples below come from earlier investigations. Re-measure the current
revision and workload before applying them.

### Formatter and parser examples

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
