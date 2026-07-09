# TODOs

The groundwork pass establishes the full architecture (parser pipeline, salsa
layer, formatter/linter/LSP skeletons, CLI, tooling, tests) over a deliberately
small Julia subset. This file tracks what comes next, roughly ordered by
leverage.

## Parser

### Incremental

- [ ] Token/block reparse splicing beneath `parsed_document`
  (`src/incremental.rs`), à la rust-analyzer `reparsing.rs` and arity's
  `src/parser/reparse.rs`: recover the edit from old/new text, splice reused
  green subtrees, fall back to a full parse. Pin correctness with an oracle
  property test (`reparse == parse(new)` across a corpus).

## Formatter

## Linter

- [x] One-pass rule engine after arity's model: each `Rule` declares its
  `interests` (subscribed `SyntaxKind`s) and `check`s per element in a single
  shared `descendants_with_tokens` traversal (dispatch table indexed by
  `SyntaxKind::COUNT`), or does a whole-file `check_file` pass off the
  `SemanticModel`. `RuleContext` carries the CST root and the model;
  `run_rules` stamps the path and stably sorts by `(start, end, rule)`.
- [x] First rules (correctness + suspicious), each a `Rule` impl registered in
  `src/linter/rules.rs`: `unused-binding` (dead `Local`/`LetVar`),
  `unused-import` (explicit imports only; whole-module `using X` exempt),
  `duplicate-argument` (per-scope duplicate params, all signature forms), and
  `assignment-in-condition` (bare `=` in an `if`/`elseif`/`while` test, with a
  safe `=`->`==` fix). Behavior locked in `tests/linter_rules.rs`.
- [x] Auto-generated rule reference from rule metadata (`description` +
  `examples`): `render_rule_doc` (`src/linter/docs.rs`) runs the real linter on
  each example, `examples/docgen.rs` writes the mdBook pages, and
  `tests/rule_docs.rs` snapshot-pins them plus guards that every rule is
  documented and every example still triggers.
- [x] Autofix application engine (`apply_fixes`, `fix_source`) honoring
  `Applicability` (safe/unsafe), wired to a Ruff-style `lint --fix` /
  `--unsafe-fixes` CLI. `apply_fixes` (`src/linter/fix.rs`) applies
  non-overlapping byte-range replacements right-to-left; `fix_source` re-lints
  to a fixpoint. `render_rule_doc` gained an "After applying the fix" block so
  fixable rules document their result. Engine tests in `src/linter/fix.rs` and
  `tests/autofix.rs`.
- [x] `annotate-snippets`-based pretty diagnostics rendering. `Pretty` draws
  source-context snippets (caret, rule title, severity color, fix hints);
  `Concise` keeps the one-liner. A global `--color auto|always|never` flag gates
  ANSI; human output goes to stderr, JSON to stdout.
- [x] Report unknown rule IDs in `select`/`ignore`. `ResolvedRules::resolve` now
  returns `(Self, Vec<String>)` with the unrecognized IDs, surfaced on
  `LintResult` so the CLI warns on a typo'd `select`/`ignore`.
- [ ] Stamp severity in the engine, not the rule. `Rule::default_severity()` is
  currently declared but never consulted — rules emit a hardcoded `Severity`, so
  overriding it is a no-op. Have `run_rules` assign
  `config_override.unwrap_or(rule.default_severity())`, which also unlocks
  per-rule severity configuration (`fatou.toml`). Same latent redundancy exists
  in arity.
- [ ] Precompute the node-dispatch table. `run_rules` rebuilds the
  `Vec<Vec<usize>>` (sized `SyntaxKind::COUNT`) from `interests()` on every file;
  the interests are fixed once `ResolvedRules` is built. Move the table into
  `ResolvedRules` so the LSP's per-keystroke path drops a per-file allocation and
  rebuild. Also applies to arity.
- [ ] Reconcile the `Diagnostic` shape with arity's when the autofix engine or
  `annotate-snippets` lands: `rule: &'static str` (no per-finding `String`),
  `TextRange` instead of raw `usize` offsets, and a structured message
  (`name`/`body`/`suggestion`) for richer LSP code actions.

## Language server

The LSP roadmap, phased so each phase unblocks the next. Architecture and
feature order follow arity (which follows rust-analyzer). The package/stdlib
index (Phase 3) deliberately lands *before* completion and hover (Phase 4): in
Julia most identifiers resolve to Base or package symbols, so local-only
completion would feel broken on day one.

### Phase 0: server infrastructure

- [x] Threading rework of `src/lsp.rs` after arity's model: the main loop owns
  no salsa database; a dedicated analysis thread
  (`src/lsp/analysis_thread.rs`) is the sole salsa writer (write-phase
  `upsert_file` with `&mut db`; read-phase parse + diagnostic conversion on
  the read pool under `salsa::Cancelled::catch`, holding a read-only
  `Analysis` snapshot — `incremental.rs` — so read jobs *can't* write). The
  read pool (`src/lsp/task_pool.rs`, rust-analyzer's `TaskPool` rather than
  rayon's global pool, which has no priority concept) is sized to the
  machine's parallelism and serves latency-sensitive requests: formatting
  runs warm off the salsa-cached parse via `format_node` when the tracked
  buffer matches the live text, falling back to a fresh parse on a cache miss
  or a racing write. Pending edits are coalesced (latest version per URI
  wins) and scheduled by the pure, unit-tested `decide` function: at most one
  analysis in flight; a strictly-newer edit of the *same* URI cancels it via
  `db.trigger_cancellation()`, a different URI waits its turn. The main
  loop's version gate drops publishes for closed or superseded documents
  (backstopping the finish-during-cancel race); diagnostics now carry the
  buffer version. End-to-end coverage in `tests/lsp.rs` (open with a parse
  error → versioned diagnostics → fixing edit → clear on close).
  **Deferred:** the single-thread index pool lands with the package index
  (Phase 3) — there is no unbounded background job to isolate yet.
- [x] Incremental (range) document sync (`TextDocumentSyncKind::INCREMENTAL`):
  range edits are spliced into the live buffer by the pure
  `apply_content_changes` (`src/text/edit.rs`) — sequential application per
  the spec, full-replacement (`range: None`) changes still honored, positions
  clamped via `LineIndex::position_to_byte`. Whole-file reparse stays fine
  until token/block reparse splicing lands (see Parser → Incremental).
- [x] Position-encoding negotiation (UTF-16 default, honor `utf-8` when the
  client offers it) on top of `text/line_index.rs`: `PositionEncoding` threads
  from the two-step initialize handshake (`negotiate_position_encoding` in
  `src/lsp/server.rs`) through document sync, diagnostics, and formatting.
- [ ] LSP test strategy: a pure `compute_*` function per feature that takes
  text plus position and returns the response type (arity's pattern), plus
  the existing in-memory connection test in `tests/lsp.rs`. Established by
  document symbols (`compute_document_symbols`); apply to each new feature.

### Phase 1: syntax-only features

Pure CST walks with no semantic blockers; cheap wins that can ship while the
semantic model grows.

- [x] Document symbols (modules, functions, macros, structs/abstract types,
  consts); the same walk later feeds workspace symbols (Phase 5). Pure
  `compute_document_symbols` walk in `src/lsp/symbols.rs` (macros as `@name`,
  signature in `detail` for multiple dispatch, struct fields as `FIELD`
  children, qualified extension names kept whole), plus a
  `document_symbols_via_db` warm path off the cached parse mirroring
  formatting's.
- [x] Folding ranges (block constructs, comment runs, import groups): pure
  `compute_folding_ranges` walk in `src/lsp/folding.rs` (definition and
  expression blocks fold through their `end`, `elseif`/`else`/`catch`/
  `finally` arms fold individually, runs of ≥2 whole-line comments and of
  consecutive `using`/`import` statements group, multi-line block comments
  and import statements fold alone), plus a `folding_ranges_via_db` warm path
  off the cached parse. Folds are line-only (no character offsets), so the
  result is independent of the negotiated position encoding.
- [x] Selection range (expand selection along CST ancestors): pure
  `compute_selection_ranges` in `src/lsp/selection.rs` (token under the cursor
  first — skipped for whitespace, kept for comments — then ancestor nodes with
  same-extent wrappers deduped; a cursor on a token boundary starts from the
  more selectable side, identifiers first), plus a `selection_ranges_via_db`
  warm path off the cached parse. Positions are character-precise, so the
  negotiated encoding threads through both directions of the conversion.
- [x] Range formatting (`textDocument/rangeFormatting`): `format_range`
  (`src/formatter/core.rs`) widens the selection to whole statements in the
  deepest enclosing `ROOT`/`BLOCK` (via source spans recorded on
  `collect_body_lines`), lowers just those lines, and prints them with
  `print_at` at the block's structural indent — the first line keeps its
  existing leading whitespace, so the single `TextEdit` replaces exactly the
  widened span. Pure `compute_format_range_edits` plus a
  `format_range_edits_via_db` warm path off the cached parse
  (`src/lsp/format.rs`); behavior locked in `tests/range_format.rs`
  (widening, structural vs preserved indent, non-indenting module bodies,
  blank-line capping, trailing comments, no-op selections, convergence with
  the full formatter, encoding-aware positions).
- [x] Syntax-driven semantic tokens (`textDocument/semanticTokens/full`):
  pure `compute_semantic_tokens` walk in `src/lsp/semantic_tokens.rs` over a
  four-type legend (keyword, macro, string, number) — keywords including
  `true`/`false`, macro names as sigil plus final component (qualifiers stay
  plain until Phase 6 resolves namespaces), string-macro prefixes and
  suffixes as macros around a string body, and number/char literals.
  Interpolations stay unpainted, byte-adjacent same-kind tokens coalesce,
  and multi-line spans split per line (most clients reject multiline
  tokens); the delta encoding counts code units of the negotiated encoding
  via `LineIndex`. Plus a `semantic_tokens_via_db` warm path off the cached
  parse. Refined with resolved names in Phase 6.

### Phase 2: per-file semantic model

The core enabler for everything semantic; the biggest single item.

- [x] `SemanticModel` per file as a salsa query (`semantic_model` in
  `src/incremental.rs`, model in `src/semantic/`): flat scope/binding arenas
  with `SmolStr` names and resolved `IdentRef`s (arity's shape). One
  declare-then-walk pass per scope mirrors Julia's hoisting rule (any
  assignment makes the name local to the whole scope), so forward closure
  captures resolve; assignment targets follow the innermost-local rule with
  `local`/`global` routing. Covers global scope per module (module bodies
  neither see nor leak enclosing names), hard scopes (function/macro/short/
  anonymous/arrow/`do` bodies, `let` with per-binding chaining,
  comprehensions/generators, struct bodies with type params and fields),
  soft scopes (`for`/`while`/`try`/`catch`/`finally`, iterables walked
  outside their variables), keyword vs positional parameters, `where`/curly
  type params, and a separate macro namespace. Documented deviations:
  top-level soft-scope assignment takes the non-interactive reading (new
  local); macros are opaque; `for outer`, `var"..."`, and property
  destructuring `(; a, b) = t` deferred. The query keeps structural `Eq`,
  so same-shape edits backdate (locked in `tests/salsa_incremental.rs`);
  position-shifting edits wait on the firewall projections below.
- [x] Import model: `using X`, `using X: a, b`, `import X`, `import X: a`,
  `import X as Y`, `export`, and `public` (1.11+), recorded in source order
  into a per-file loaded-modules list (`module_loads` on the model: kind,
  relative-dot count, path components, aliases, item lists); qualified reads
  (`Foo.bar`, `Base.@time`) tracked separately from bare free reads in
  `qualified_reads`. Imported names bind (`BindingKind::Import`: the last
  path component, the `as` alias, or the colon items — semantics verified
  against Julia 1.12), so reads resolve intra-file; imported macros keep
  their `@` sigil, invisible to value lookups. `export`/`public` names
  resolve against their global scope and mark the binding used without
  entering the free reads; `using X`'s unknowable exports stay free reads
  for Phase 3 resolution.
- [x] Firewall queries after arity's pattern: `file_exports`,
  `file_free_reads`, `file_qualified_reads`, `include_edges`—stable `Eq`
  projections that survive body edits so project-level memos don't
  invalidate on every keystroke. Pure projections in `src/project.rs`, tracked
  wrappers in `incremental.rs`; backdating across position-shifting edits is
  locked in `tests/salsa_incremental.rs`.
- [x] `smol_str` interning for symbol names (the Tooling item lands here):
  binding and identifier names are `SmolStr`.

### Phase 3: package, stdlib, and environment index

The arity `rindex/` analog, but simpler: every Julia package, plus Base and
the stdlib, ships as plain source, so fatou's own parser does all the
harvesting—no Julia runtime needed.

- [x] Environment resolution: locate the active `Project.toml`/`Manifest.toml`
  (walk up from the workspace root, then `JULIA_PROJECT`, then the newest
  `~/.julia/environments/v1.X`); parse the Manifest for the pinned package
  set (name, uuid, version, `git-tree-sha1`, deps); locate depots via
  `JULIA_DEPOT_PATH` falling back to `~/.julia` (sources live at
  `~/.julia/packages/<Name>/<slug>/src/`).
- [x] Harvester (`src/index/`): parse each package's `src/` with fatou's
  parser, following static `include("literal")` chains (reusing
  `project::include_target`/`resolve_target`) interleaved with the module walk
  so an `include` splices into the module that lexically contains it. Extracts
  exported/`public` names, function signatures grouped by `(owner, name)` for
  multiple dispatch (positional/keyword params with defaults, `where` specs,
  return types; `Base.show` records its owner), struct/abstract/primitive types
  with supertypes and fields (incl. `@kwdef` defaults and inner constructors),
  consts, macros, and docstrings (the `DOC`-folded string literal or an
  explicit `@doc`). Type positions lower to a structured `TypeExpr`
  (name/qualified, `Applied`, `Union`, `Tuple`, `TypeVar` with bounds, `Raw`
  fallback); value positions stay normalized source strings. Every symbol
  carries a `DefLocation` (package-relative path + name span) for later
  go-to-definition. `@doc`/`@kwdef` are understood; other macros are
  transparent wrappers recursed into only when their argument is itself a
  definition (`@inline f() = ...`). Best-effort: missing/unreadable/dynamic
  includes, parse errors, and include cycles are recorded as
  `HarvestDiagnostic`s and the walk continues. Locked by inline units plus
  `tests/harvest.rs`; the signature/type helpers factored into
  `src/semantic/signature.rs`. Smoke-harvested against a real depot package.
- [x] Base/stdlib index from the Julia installation's plain sources
  (`share/julia/base/`, `share/julia/stdlib/v1.X/`), plus a baked-in minimal
  Base/Core export list as fallback when no installation is found (arity's
  `StaticBaseR` analog). `environment::locate_install` finds the install without
  running Julia (`JULIA_BINDIR` override, juliaup's `juliaup.json`, then `julia`
  on `PATH`, following NixOS shell wrappers to the real binary) and fills stdlib
  `Package.source`s; `index::build_system_index` harvests Base (merging
  `Base_compiler.jl`+`Base.jl` for the 1.12 split), Core (`boot.jl`), and every
  stdlib via the generalized `harvest_entry`, falling back to the embedded
  `src/index/fallback/{base,core}_exports.txt` snapshots. Locked by
  `tests/base_index.rs` and the `locate_install` cases in `tests/environment.rs`.
- [ ] On-disk cache keyed by (name, version or `git-tree-sha1`), harvested in
  parallel (rayon) on the index pool, hot-swapped into the HIGH-durability
  `LibraryIndex` salsa input (the input itself has landed: a singleton in
  `src/incremental.rs` holding `BTreeMap<String, Arc<PackageIndex>>`, with
  `set_library_packages`/`set_package_index`/`library_package` on the db and
  `tests/library_index.rs`); re-analyze open files on swap.
- [x] One shared name-resolution/masking order for all consumers (completion,
  hover, the future undefined-name lint): local scopes → explicit imports →
  `using`'d exports in source order → Base/Core implicit. `src/resolve.rs`'s
  `Resolver` borrows one `SemanticModel` plus a `PackageSource` (the harvested
  library, implemented for the raw harvest map and for the read-only `Analysis`
  snapshot). `resolve` walks the four tiers and returns the first hit as a
  `Resolution` (tiers 1-2 collapse to `Binding`, since explicit imports are file
  bindings; the library tiers name the source module); `visible` enumerates
  every name in the same order with shadowing dropped, for completion. Macros
  resolve in a parallel `Namespace`, reconciling the model's bare macro-def vs.
  `@`-sigil imported-macro bindings. `using` visibility respects module-body
  scope boundaries; relative/interpolated `using`s and `baremodule`'s Base/Core
  suppression are deferred. Wired onto `Analysis` as `resolve_name`/`visible_names`
  and locked by inline units plus `tests/resolve.rs`.
- [ ] Maybe: a `fatou index` CLI subcommand to warm and inspect the cache.

### Phase 4: core semantic features

The payoff phase, in roughly arity's shipping order.

- [x] Completion: scope-aware locals/params, keywords, and symbols from loaded
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
- [x] `smol_str` interning for symbol names once the semantic model lands
  (landed with the semantic model, Phase 2).
