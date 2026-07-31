# TODOs

## Parser

- [ ] Lex the *broadcast* wrapping arithmetic operators `.+% .-% .*%` (and their
  augmented forms `.+%= .-%= .*%=`) in `src/parser/lexer.rs`. The undotted
  `+% -% *%` are supported; the dotted forms still split into `.+` + `%`, which
  mis-parses rather than erroring. No code in the smoke-test corpus uses them
  (only JuliaSyntax's own tests do), so this is deferred until the pinned oracle
  advances past JuliaSyntax 0.4.10, which predates the whole family.

- [ ] Two error-recovery gaps left over from labeled `break`/`continue`
  (`src/parser/structural.rs`). Junk after a complete labeled keyword drops
  JuliaSyntax's trailing zero-width marker (`break l x y` ⇒ `(break l x)
  (error-t y)`, not `(error-t y (error-t))`), and a bare comma after one does not
  fold into a tuple (`break l, y` ⇒ `(break l) (error-t ✘ y)`, not
  `(tuple (break l) y)`) — the latter predates labels, since `break, y` behaved
  the same way. Both are error/edge shapes no corpus code hits, and neither is
  pinnable until the oracle advances past JuliaSyntax 0.4.10, which predates the
  whole feature.

### Incremental

- [ ] Token/block reparse splicing beneath `parsed_document`
  (`src/incremental.rs`), à la rust-analyzer `reparsing.rs` and arity's
  `src/parser/reparse.rs`: recover the edit from old/new text, splice reused
  green subtrees, fall back to a full parse. Pin correctness with an oracle
  property test (`reparse == parse(new)` across a corpus).

## Formatter

- [ ] Canonicalize the gap before a macro's *attached* argument
  (`lower_macro_call` in `src/formatter/rules.rs`). The gap is preserved
  verbatim because it is meaning-bearing whenever a `[…]`/`(…)` suffix follows
  (`@NamedTuple{T}[x]` indexes the type; `@NamedTuple {T}[x]` hands the macro
  `{T}[x]`), so `@m{a}` and `@m {a}` both survive even where no suffix makes
  them differ. Deciding "no suffix follows" needs the parent context, which
  `lower_macro_call` does not have today.

- [x] Prefer breaking a function signature's argument list over exploding a
  short `where` clause. A single-parameter bound is now atomic: `lower_where`
  renders `where {T}` (and bare `where T`) as flat, non-breaking `{T}`, so an
  over-width `f(a, b, c) where {T}` breaks the args and keeps `) where {T}` on
  the closing line. A multi-parameter bound stays a breakable exploding group
  (`where_break` unchanged). Gated by `where_bare_signature_break`.

- [ ] Extend the where-bound break-priority to *short multi-parameter* bounds.
  A short but multi-param bound with long args (`f(longargs...) where {T, S}`)
  still explodes the bound rather than the args, because the element-count
  heuristic in `lower_where` only treats a single param as atomic. "Short"
  (fits on the closing line) is a width judgment the heuristic cannot make;
  handling it needs a conditional-layout primitive (like `HugGroup`) that
  chooses args-break-with-flat-bound over flat-args-with-broken-bound by
  measuring which first line fits.

## Linter

### Rules

- [ ] `index-from-length` (suspicious, syn, opinionated): `for i in
  1:length(x)` where `i` indexes `x` -> suggest `eachindex`/`axes`; also
  iterating a bare numeric literal (`for i in 3.5`). Name-based match on
  `length`/`size` (no resolution); StaticLint exempts known `Vector`/`Array`
  bindings, which we cannot without type info -- document as opinionated.
  (IncorrectIterSpec, IndexFromLength)
- [ ] `type-piracy` (correctness): extending an imported function with no
  owned argument type. Blocked on cross-file import and ownership
  resolution. (TypePiracy)

## Language server

- [ ] On-disk cache keyed by (name, version or `git-tree-sha1`), harvested in
  parallel (rayon) on the index pool, hot-swapped into the HIGH-durability
  `LibraryIndex` salsa input (the input itself has landed: a singleton in
  `src/incremental.rs` holding `BTreeMap<String, Arc<PackageIndex>>`, with
  `set_library_packages`/`set_package_index`/`library_package` on the db and
  `tests/library_index.rs`); re-analyze open files on swap.
- [ ] Maybe: a `fatou index` CLI subcommand to warm and inspect the cache.
- [ ] Code actions beyond quick fixes: organize/sort `using` statements,
  qualify a bare name.

### Architecture & robustness audit

Cross-applied from the arity and badness audits (both modeled on
rust-analyzer). Fatou is already ahead on several axes that are still open there
— `positionEncoding` negotiation, `didChangeWatchedFiles` watchers, salsa
durability tiers (HIGH `LibraryIndex`, LOW `SourceFile`/`WorkspaceFiles`),
firewall queries, an opaque `FileId`/`FileSourceMap`, and read-pool per-job
panic isolation — so those need no work. The items below are the remaining gaps.

- [x] Analysis-thread + main-loop panic guard. A `guard(label, f)` wraps every
  analysis-thread `select!` arm (`src/lsp/analysis_thread.rs`) and the main
  loop's request/notification/outbound dispatch (`src/lsp/server.rs`) in
  `catch_unwind`, mirroring the read pool's per-job isolation
  (`src/lsp/task_pool.rs`). A panic mid-write no longer kills the sole db writer
  and zombies the server. Covered by
  `guard_contains_a_panic_and_reports_completion`.
- [x] Mutex-poison recovery. `IncrementalDatabase::source_map` now locks through
  a helper that recovers via `PoisonError::into_inner` instead of
  `.expect(...)`, so a panic caught by the guard above leaves the path→input map
  usable rather than crashing the next access. Covered by
  `poisoned_source_map_lock_recovers`.
- [x] Parser stuck-loop guard. `ParserCtx` (`src/parser/context.rs`) carries a
  step budget ticked by every peek primitive and reset on frontier progress; a
  non-advancing loop trips `PARSER_STEP_LIMIT` and panics loudly instead of
  hanging (the LSP read pool and analysis-thread guard recover; the CLI aborts
  with a diagnosable message). Adapted from badness's `grammar.rs` to fatou's
  functional index-threaded parser rather than a stateful cursor. Covered by
  `step_guard_trips_when_wedged` and `step_budget_resets_on_progress`. Directly
  targets the `timeout` class in the smoke-test corpus.

- [x] **P1 — Request cancellation + stale-read protocol.** Read replies now
  route back through the main loop (`Outbound::ReadReply`), which tracks live
  request ids in `GlobalState.inflight_reads`: `$/cancelRequest` →
  `RequestCancelled` (-32800), and a reply against a superseded buffer →
  `ContentModified` (-32801). The main loop drains client input ahead of read
  replies so cancels are prompt and deterministic. Covered by unit tests on the
  gate/registry (`src/lsp/state.rs`) plus E2E `cancel_request_yields_request_cancelled`
  and `stale_read_yields_content_modified` (`tests/lsp.rs`). Cancellation is
  cooperative: the in-flight salsa work runs to completion and its result is
  discarded. **Follow-up:** true per-request interruption (salsa's
  `trigger_cancellation()` is storage-global, so it can't target one read
  without cancelling concurrent siblings).
- [ ] **P2 — Concurrency/scheduler test coverage.** `decide`'s idle branch is
  unit-tested and the supersede branch has pure-decision tests, but nothing
  drives the *cancellation signal flow* end to end. Add an integration test over
  `Connection::memory()` that fires rapid `didChange`s and asserts coalescing
  plus version-gated publishes (only the latest version is published, stale ones
  dropped), and one exercising `SupersedeAndStart` → `trigger_cancellation`.
- [ ] **P2 — Work-done progress for the harvester.** `spawn_workspace_harvester`
  (`src/lsp/server.rs`) walks the filesystem and parses all of Base with no
  `$/progress` reporting. Advertise the capability and emit begin/report/end
  around the harvest and any re-harvest.
- [ ] **P3 — Content-derived pull `resultId`.** Already stubbed: every pull
  returns `result_id: None` (`src/lsp/read_jobs.rs`, `semantic_tokens.rs`), so
  `Unchanged` never fires. Derive the id from a hash of the file's findings so an
  unchanged file re-pulls as `Unchanged` (bandwidth win).
- [ ] **P3 — Debug open/close balance assertion.** The event parser has
  balanced-slice helpers (`src/parser/structural.rs`) but no debug-only
  Start/Finish balance walk over the event stream (badness landed one as a
  `DropBomb` analog). Add a `debug_assert`-gated check to catch a leaked
  `open()`/`precede` splice; compiled out of release.
- [ ] **P3 — Deterministic workspace seed order.** `WorkspaceFiles` is a
  `Vec<SourceFile>` singleton input; if seed order churns between reharvests it
  bumps the input revision needlessly (the value-ordering fragility arity and
  badness both flag for their interned `Project`). Pin a stable sort at the seed
  site (`seed_workspace_members`) as an invariant.

## Tooling

- [ ] `build.rs` generating shell completions + man pages
  (clap_complete/clap_mangen), as arity does.
- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
