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

- [x] Stop a trailing `key => [bracket]` pair from hugging in a multi-item list.
  A homogeneous mapping like `Dict("a" => [x], "b" => [y], "c" => [z])` used to
  keep every pair flat on the opening line and explode only the last, relocating
  the asymmetry. Now `suppress_bare_bracket_pair_hug` downgrades the trailing hug
  when the tail is a pair whose innermost value is a *bare* bracket literal
  (`[]`/`()`/`{}`) and the list has >1 item — for both calls and collections. A
  sole-argument pair still hugs, a call/curly-valued tail still hugs, and a direct
  bare-literal arg (`map(cb, [a, b])`) still hugs. Gated by `pair_list_no_hug`;
  updated the `mapping` case in `pair_hug`.

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

- [x] `julia-version-compat` (correctness, syn, Error): flag syntax newer than
  the project's declared Julia support range. Target resolved by precedence
  (`--julia-version` > `[julia] version` > `Project.toml` `[compat]` >
  `Manifest.toml julia_version`), carried on `ResolvedRules` into each
  `RuleContext`. Seed feature table: `public` (1.11), `import/using ... as`
  (1.6). Follow-ups: grow the feature table as the parser exposes more
  version-gated kinds; wire the target into the `--fix` path (`fix_source`) and
  the LSP (from its resolved `Environment`), both currently `None`.
- [ ] `index-from-length` (suspicious, syn, opinionated): `for i in
  1:length(x)` where `i` indexes `x` -> suggest `eachindex`/`axes`; also
  iterating a bare numeric literal (`for i in 3.5`). Name-based match on
  `length`/`size` (no resolution); StaticLint exempts known `Vector`/`Array`
  bindings, which we cannot without type info -- document as opinionated.
  (IncorrectIterSpec, IndexFromLength)
- [ ] `type-piracy` (correctness): extending an imported function with no
  owned argument type. Blocked on cross-file import and ownership
  resolution. (TypePiracy)

### False positives (from the Makie.jl linter-investigation sweep)

- [ ] `unused-binding` on attribute-DSL macro blocks. A `name = default`
  inside a consuming macro's `begin ... end` (Makie's `@gen_defaults! d begin
  color_map = nothing => Texture end`, `@DocumentedAttributes begin space =
  :data end`) is handed to the macro as an attribute, not left a dead local,
  but the walker binds it as an ordinary local and flags it (~176 findings in
  Makie: ~144 `@gen_defaults!`, ~32 `@DocumentedAttributes`). This collides
  head-on with the deliberate `unused_binding_flags_dead_local_in_macro_block_argument`
  test, whose `@testset begin t = 1 end` is a scope-*transparent* macro where
  flagging IS correct. The linter cannot tell a consuming DSL from a
  transparent wrapper without knowing the macro, so this is a policy call:
  either exempt all macro-block bindings (matching the `redefined-constant`
  guard just added, at the cost of false negatives on `@testset`/`@inbounds`),
  or keep an allowlist of known scope-transparent macros. Repro: `printf
  '@gen_defaults! d begin\n    color = nothing\nend\n' | lint`.
- [ ] `unused-binding` misses interpolation inside non-standard string
  literals: `$x` in `js"... $x ..."` (WGLMakie, ~16) or a GLSL string is not
  counted as a use, though `@js_str` does interpolate. Plain `"$x"` is handled;
  the gap is that `foo"...$x..."` lexes `$x` as raw `STRING_CONTENT`, not an
  `INTERPOLATION` node. `@u_str`/`@format_str` *application* is now counted
  (fixed); interpolation *inside* a string macro is the remaining gap. Repro:
  `printf 'using M: @js_str\nf(rect) = (x, y = rect; js"pick($x, $y)")\n' |
  lint` flags `x`, `y`.
- [ ] `unused-binding` on `@show a, b = expr` (GLMakie `GLInfo.jl`, ~4): the
  whole tuple-assignment is the macro's argument, so `@show` uses every name,
  but the walker flags the non-first names (`typ`, `uniform_size`). Same
  macro-argument-opacity root as the DSL-block class.
- [ ] `unused-import` misses three within-file use forms (spans are otherwise
  exact; ~71% of the 264 Makie findings are the known file-scoped/`include`
  limitation, not these). Verified with `julia`: (1) an imported name used only
  inside `quote ... end` / `:( ... )` (e.g. `benchmark-ttfp.jl` `median` used
  in a `quote` macro body); (2) an imported operator via infix `a == b` or a
  parenthesized method def `(==)(a::S, b::S) = ...` (`GLTypes.jl`) — regular
  `import Base: *` + `*(a,b)=...` is counted, but the `(op)` target and infix
  tokens are not resolved to the operator's import; (3) interpolation inside a
  non-standard string macro (same lex gap as the `unused-binding` item). The
  string/command-macro *application* form (`u"ns"` -> `@u_str`) is now counted
  (fixed).

## Language server

- [x] On-disk cache keyed by (name, version or `git-tree-sha1`), harvested in
  parallel (rayon) on the index pool, hot-swapped into the HIGH-durability
  `LibraryIndex` salsa input. `src/index/cache.rs` holds an `IndexCache` of
  postcard-serialized `PackageIndex` entries at `<cache>/index/v<FORMAT>-<ver>/
  <name>/<key>.postcard` (atomic temp+rename, header-validated, best-effort),
  keyed by `git-tree-sha1` for registered packages and the Julia version for
  Base/stdlib (`CacheKey`). `harvest_libraries_parallel` (`src/index.rs`) and
  `build_system_library_cached` (`src/index/base.rs`) harvest each package
  concurrently on a dedicated capped rayon pool (`index_pool_size`,
  `src/lsp/task_pool.rs`) built in `spawn_workspace_harvester`; a warm cache
  reloads instead of re-parsing. The swap and open-file re-analysis reuse the
  existing `set_library` + `refresh_graph_diagnostics`/`DiagnosticsRefresh`
  path. Covered by `src/index/cache.rs` unit tests and
  `tests/parallel_harvest.rs`.
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
- [x] **P2 — Concurrency/scheduler test coverage.** `decide`'s idle branch is
  unit-tested and the supersede branch has pure-decision tests, but nothing
  drove the *cancellation signal flow* end to end. Covered now by
  `coalesces_rapid_didchanges_and_gates_stale_versions` (`tests/lsp.rs`): a burst
  of `didChange`s over `Connection::memory()` collapses into far fewer publishes
  than edits (coalescing), versions arrive monotonically with none past the last
  edit (the main-loop version gate drops stale results), and the final
  diagnostics reflect the last buffer. The `SupersedeAndStart` →
  `trigger_cancellation` wiring is driven deterministically by the white-box
  `try_dispatch_supersedes_inflight_and_triggers_cancellation`
  (`src/lsp/analysis_thread.rs`), which dispatches v1, leaves it in flight, then
  dispatches v2 and asserts the slot advances via the cancellation branch.
- [x] **P2 — Work-done progress for the harvester.** `spawn_workspace_harvester`
  (`src/lsp/server.rs`) reports its indexing passes via `$/progress`, gated on
  the client's `window.workDoneProgress` capability: a `HarvestProgress` reporter
  (`src/lsp/progress.rs`) mints a token, then emits begin/report/end around the
  full harvest and each single-package re-harvest.
- [x] **P3 — Content-derived pull `resultId`.** Both pull responses now key off
  a content hash (`src/lsp/result_id.rs`: hex SipHash of the findings'
  serialization, seeded deterministically). Pull diagnostics thread the client's
  `previous_result_id` through the `DocumentDiagnostic` read job and collapse to
  `Unchanged` on a match, else a `Full` report carrying the new id
  (`diagnostic_report`, `src/lsp/read_jobs.rs`). Semantic tokens now advertise
  `full/delta` (`server.rs`) and derive the `resultId` from the encoded token
  stream; a matching `full/delta` re-pull answers an empty edit list
  (`semantic_tokens_delta`), else the full set is resent (we recompute rather
  than diff, so the win is purely the unchanged case). The id keys off token
  positions/lengths/kinds, not identifier text, so a rename that leaves the
  layout intact legitimately re-pulls unchanged.
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
