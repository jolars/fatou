# TODOs

The groundwork pass establishes the full architecture (parser pipeline, salsa
layer, formatter/linter/LSP skeletons, CLI, tooling, tests) over a deliberately
small Julia subset. This file tracks what comes next, roughly ordered by
leverage.

## Parser

- [ ] Parser: splat after a closing bracket is rejected. `f(g(x)...)`,
  `f(a[i]...)`, `f((a + b)...)`, `f(A{T}...)`, `f([1, 2]...)` yield a
  `LoneOperator` ERROR on the `...`; only the spaced spelling `f(g(x) ...)`
  parses, and a name/dotted/literal operand snugged (`f(x...)`) is fine.
  JuliaSyntax accepts all: `JuliaSyntax.parse(Expr, "f(g(x)...)")` ⇒ `f(g(x)...)`.
  Blocks the formatter's `lower_splat` from snugging bracket-closing operands (it
  bails to verbatim spaced); once fixed, drop the `ends_in_bracket` guard and
  widen `splat_spacing/`. (Handed off from formatter 2026-07-06c.)
- [ ] Parser: whitespace before a call/index/curly arg list is wrongly accepted.
  `f (a)`, `a [1]`, `A {T}`, `f(a) (b)` parse as `CALL_EXPR`/`INDEX_EXPR`/`CURLY_EXPR`
  with an interior `WHITESPACE`; JuliaSyntax rejects with `whitespace is not allowed
  here`. Surfaced by the formatter; see parser-parity RECAP queued target.

### Incremental

- [ ] Token/block reparse splicing beneath `parsed_document`
  (`src/incremental.rs`), à la rust-analyzer `reparsing.rs` and arity's
  `src/parser/reparse.rs`: recover the edit from old/new text, splice reused
  green subtrees, fall back to a full parse. Pin correctness with an oracle
  property test (`reparse == parse(new)` across a corpus).

## Formatter

- [ ] Range formatting (`textDocument/rangeFormatting`).

## Linter

- [ ] First rules (correctness + suspicious), each a `Rule` impl registered in
  `src/linter/rules.rs`.
- [ ] Autofix application engine (`apply_fixes`) honoring `Applicability`
  (safe/unsafe), with the `format → lint --fix → format --check` property
  test (Tenet 5).
- [ ] `annotate-snippets`-based pretty diagnostics rendering (dependency noted
  in `Cargo.toml`; `render.rs` is currently a compact one-liner renderer).

## Language server

The LSP roadmap, phased so each phase unblocks the next. The current server
(`src/lsp.rs`) is a single-threaded skeleton: full-document sync, whole-file
formatting, and parse-error diagnostics. Architecture and feature order follow
arity (which follows rust-analyzer). The package/stdlib index (Phase 3)
deliberately lands *before* completion and hover (Phase 4): in Julia most
identifiers resolve to Base or package symbols, so local-only completion would
feel broken on day one.

### Phase 0: server infrastructure

- [ ] Threading rework of `src/lsp.rs` after arity's model: the main loop owns
  no salsa database; a dedicated analysis thread is the sole salsa writer
  (write-phase upserts with `&mut db`, read-phase analysis on a pool under
  `salsa::Cancelled::catch`); a rayon read pool serves latency-sensitive
  requests (hover, completion, formatting); a separate single-threaded index
  pool isolates background package indexing so long harvests never starve
  reads. Pending-edit coalescing (latest version per URI wins), cancellation
  via `db.trigger_cancellation()`, version-gated diagnostic publish.
- [ ] Incremental (range) document sync (`TextDocumentSyncKind::INCREMENTAL`):
  apply range edits to the live buffer. Whole-file reparse stays fine until
  token/block reparse splicing lands (see Parser → Incremental).
- [ ] Position-encoding negotiation (UTF-16 default, honor `utf-8` when the
  client offers it) on top of `text/line_index.rs`.
- [ ] LSP test strategy: a pure `compute_*` function per feature that takes
  text plus position and returns the response type (arity's pattern), plus
  the existing in-memory connection test in `tests/lsp.rs`.

### Phase 1: syntax-only features

Pure CST walks with no semantic blockers; cheap wins that can ship while the
semantic model grows.

- [ ] Document symbols (modules, functions, macros, structs/abstract types,
  consts); the same walk later feeds workspace symbols (Phase 5).
- [ ] Folding ranges (block constructs, comment runs, import groups).
- [ ] Selection range (expand selection along CST ancestors).
- [ ] Range formatting (`textDocument/rangeFormatting`).
- [ ] Syntax-driven semantic tokens (keywords, macro calls, string macros,
  literals); refined with resolved names in Phase 6.

### Phase 2: per-file semantic model

The core enabler for everything semantic; the biggest single item.

- [ ] `SemanticModel` per file as a salsa query: one bottom-up CST walk builds
  the scope tree, bindings (definition site plus read sites), and free
  reads, honoring Julia's scoping rules—global scope per module; hard local
  scopes (function/macro bodies, `do`, `let`, comprehensions/generators);
  soft local scopes (`for`/`while`/`try`); `local`/`global` declarations;
  struct fields; type parameters (curly and `where`); keyword vs positional
  parameters; closure captures.
- [ ] Import model: `using X`, `using X: a, b`, `import X`, `import X: a`,
  `import X as Y`, `export`, and `public` (1.11+), recorded in source order
  into a per-file loaded-modules list; qualified reads (`Foo.bar`) tracked
  separately from bare free reads.
- [ ] Firewall queries after arity's pattern: `file_exports`,
  `file_free_reads`, `file_qualified_reads`, `include_edges`—stable `Eq`
  projections that survive body edits so project-level memos don't
  invalidate on every keystroke.
- [ ] `smol_str` interning for symbol names (the Tooling item lands here).

### Phase 3: package, stdlib, and environment index

The arity `rindex/` analog, but simpler: every Julia package, plus Base and
the stdlib, ships as plain source, so fatou's own parser does all the
harvesting—no Julia runtime needed.

- [ ] Environment resolution: locate the active `Project.toml`/`Manifest.toml`
  (walk up from the workspace root, then `JULIA_PROJECT`, then the newest
  `~/.julia/environments/v1.X`); parse the Manifest for the pinned package
  set (name, uuid, version, `git-tree-sha1`, deps); locate depots via
  `JULIA_DEPOT_PATH` falling back to `~/.julia` (sources live at
  `~/.julia/packages/<Name>/<slug>/src/`).
- [ ] Harvester: parse each package's `src/` with fatou's parser, following
  `include()` chains to build the module tree; extract exported/`public`
  names, function signatures (positional/keyword arguments, defaults, `::`
  annotations, `where` clauses; grouped by name since multiple dispatch
  means many methods per function), structs/abstract types with supertypes,
  consts, macros, and docstrings (the string literal or `@doc` preceding a
  definition).
- [ ] Base/stdlib index from the Julia installation's plain sources
  (`share/julia/base/`, `share/julia/stdlib/v1.X/`), plus a baked-in minimal
  Base/Core export list as fallback when no installation is found (arity's
  `StaticBaseR` analog).
- [ ] On-disk cache keyed by (name, version or `git-tree-sha1`), harvested in
  parallel (rayon) on the index pool, hot-swapped into a HIGH-durability
  `LibraryIndex` salsa input; re-analyze open files on swap.
- [ ] One shared name-resolution/masking order for all consumers (completion,
  hover, the future undefined-name lint): local scopes → explicit imports →
  `using`'d exports in source order → Base/Core implicit.
- [ ] Maybe: a `fatou index` CLI subcommand to warm and inspect the cache.

### Phase 4: core semantic features

The payoff phase, in roughly arity's shipping order.

- [ ] Completion: scope-aware locals/params, keywords, and symbols from loaded
  packages and Base; `Foo.` member completion (trigger character `.`);
  `completionItem/resolve` for lazy docs.
- [ ] Hover: local binding info; for library symbols, signature(s) and
  docstring rendered as markdown (multiple dispatch: show the method group).
- [ ] Signature help (triggers `(` and `,`), including keyword arguments.
- [ ] Go-to-definition: intra-file bindings; library symbols jump straight
  into depot sources (real files on disk—nicer than R's compiled lazy-load
  DBs).
- [ ] References and document highlight (read/write sites of a binding).
- [ ] Rename (intra-file first, with `prepareRename` validation).

### Phase 5: project and workspace level

- [ ] Project graph: workspace membership from `initialize`, `include()`
  edges, package-project awareness (a workspace `Project.toml` means the
  package under development; index its module tree like a depot package).
- [ ] Cross-file go-to-definition, references, and rename for top-level
  symbols.
- [ ] Workspace symbols (fuzzy subsequence match over top-level definitions).
- [ ] `workspace/didChangeWatchedFiles`: `Project.toml`/`Manifest.toml`
  changes re-resolve the environment; file create/delete refreshes
  membership.
- [ ] Diagnostics maturation: pull diagnostics (`textDocument/diagnostic`)
  with push fallback; lint findings as diagnostics with quick-fix code
  actions (needs the Linter section's first rules); first semantic
  diagnostics (undefined name—masking-aware, unused binding, unused import).

### Phase 6: later polish and Julia-specific ambitions

- [ ] Semantic tokens refined with resolved names (function vs macro vs type
  vs module).
- [ ] Call hierarchy (incoming/outgoing calls).
- [ ] Type hierarchy from the declared type tree (supertypes/subtypes of
  structs and abstract types)—a Julia-specific win that needs no inference.
- [ ] Multiple-dispatch-aware navigation: go-to-definition returning all
  methods of a function.
- [ ] Inlay hints for keyword-argument names and elided defaults.
- [ ] Document links for `include("...")` paths.
- [ ] Code actions beyond quick fixes: organize/sort `using` statements,
  qualify a bare name.
- [ ] `workspace/didChangeConfiguration` handling with `fatou.toml` discovery
  taking precedence (arity's rule).

## Tooling

- [ ] `build.rs` generating shell completions + man pages
  (clap_complete/clap_mangen), as arity does.
- [ ] Benchmarks (`criterion`) for parse + incremental reparse.
- [ ] `smol_str` interning for symbol names once the semantic model lands.
