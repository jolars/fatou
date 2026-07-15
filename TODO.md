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
- [x] Stamp severity in the engine, not the rule. `ResolvedRules::resolve` now
  takes the whole `LintConfig` and pairs each enabled rule with
  `config.severity override or rule.default_severity()`; `run_rules` stamps it
  onto whatever each rule call pushed, alongside the existing path stamping.
  Rules build findings via `Diagnostic::new` (no severity choice; the
  hard-error `duplicate-argument` overrides `default_severity`), and a
  `[lint.severity]` table in `fatou.toml` maps rule ID → severity, with unknown
  IDs surfaced through the same typo warning as `select`/`ignore`. Locked by
  units in `src/linter/rules.rs` + `src/config.rs` and the severity block in
  `tests/linter_rules.rs`. Same latent redundancy still exists in arity.
- [x] Precompute the node-dispatch table. The `Vec<Vec<usize>>` (sized
  `SyntaxKind::COUNT`) is now built from `interests()` once in
  `ResolvedRules::resolve` instead of on every file (`run_rules` folded into
  `ResolvedRules::run`), and the LSP caches its two rule sets (default, and
  +`undefined-name` for workspace members) in `LazyLock` statics, so the
  per-keystroke path no longer re-resolves rules or rebuilds the table. Still
  applies to arity.
- [x] Reconcile the `Diagnostic` shape with arity's: `rule: &'static str` (no
  per-finding `String`), `range: TextRange` instead of raw `usize` offsets
  (serialized as `{start, end}` like arity's), and a structured
  `ViolationData` message (`name`/`body`/`suggestion`; rules default the name
  to the rule ID via `Diagnostic::new`, and the pretty renderer prints a
  suggestion as a `help:` note). The write-only `suppressed` field is gone —
  suppression filters findings in `lint_parsed`. Kept deliberately un-arity:
  `path: Option<PathBuf>` (stdin) and `fixes: Vec<Fix>` (the autofix engine
  applies multiple fixes per finding).

### Rule roadmap

Candidates probed from StaticLint.jl's check catalog (its `LintCodes` names in
parentheses). Each entry carries category and cost tier: `syn` = CST + typed
AST wrappers only, `sem` = needs the `SemanticModel`. To land one, use the
`add-lint-rule` skill (`.claude/skills/add-lint-rule/`).

Ready now (no new infrastructure):

- [x] `nothing-comparison` (suspicious, syn): flags bare `x == nothing` /
  `x != nothing` (either side) with a safe fix `==` -> `===` and `!=` ->
  `!==`. Warning; name-based `nothing` match on `BINARY_EXPR` only (chains fold
  into `COMPARISON_EXPR`).
- [x] `unused-argument` (correctness, sem): function parameter never read in
  its body, across every signature form (long, short, anonymous, `do`).
  Warning severity but `default_enabled() == false` (dispatch-only params make
  it too noisy on by default). Skips all-underscore names and lone-literal stub
  bodies (`f(x) = 0`). No fix. (UnusedFunctionArgument)
- [x] `break-outside-loop` (correctness, syn, error severity): `break` or
  `continue` with no enclosing `for`/`while`. Ancestor walk that flags at
  function boundaries (closures, do-block and comprehension bodies) even
  inside a loop, walks through enclosing-scope positions (loop headers,
  do-call arguments, comprehension iterators), and stays silent in quotes and
  macro calls. No fix. (ShouldBeInALoop)
- [x] `constant-condition` (suspicious, syn): boolean literal as an `if`/
  `elseif`/`while` test or as a bare operand of the lazy `&&`/`||` (eager
  `&`/`|`, broadcast `.&&`/`.||`, and ternary tests are out of scope).
  Warning; exempts `while true` (Julia's idiomatic infinite loop) but still
  flags `while false`. Deliberate `false && expr` sites suppress with
  `# fatou-ignore`. No fix. (ConstIfCondition, PointlessOR, PointlessAND)
- [x] `module-shadows-parent` (suspicious, sem): nested `module`/`baremodule`
  named the same as its direct parent module (the last component of
  `SemanticModel::enclosing_module_path` at the definition). Warning; stays
  silent in quoted code and macro calls (`@eval module A` is the deliberate
  way to build one). No fix (renaming is a semantic rewrite). Grew
  `ModuleDef::name()` in the AST wrappers. (InvalidModuleName)
- [ ] `noteq-definition` (correctness, syn): defining `!=` (or `≠`) instead
  of `==`; `!=` is `const != = !(==)` and should not be overloaded. (NotEqDef)
- [ ] `unused-type-parameter` (correctness, sem): `where {T}` with `T` never
  used in the signature or body. Needs the model to bind where-clause params
  first. (UnusedTypeParameter)
- [ ] `index-from-length` (suspicious, syn, opinionated): `for i in
  1:length(x)` where `i` indexes `x` -> suggest `eachindex`/`axes`; also
  iterating a bare numeric literal (`for i in 3.5`). Name-based match on
  `length`/`size` (no resolution); StaticLint exempts known `Vector`/`Array`
  bindings, which we cannot without type info -- document as opinionated.
  (IncorrectIterSpec, IndexFromLength)

Blocked on future infrastructure:

- [x] `undefined-name` (correctness, sem): free read no resolution tier
  provides, via the shared `Resolver` masking order (locals/imports →
  workspace siblings → `using` exports → Base/Core) — Phase 3 unblocked it.
  `RuleContext` gained an optional `ResolutionContext` (a `dyn PackageSource`
  plus the workspace member context); the CLI resolves against the embedded
  Base/Core snapshot, the LSP against its harvested library. Soundness
  bail-outs: unresolvable whole-module `using`, `eval`/`@eval`, `include`
  without a workspace (or with a dynamic path), value reads inside macro
  calls, quoted code, and the module-implicit `eval`/`include`/`new`/`ccall`.
  Warning; `default_enabled() == false` (a bare file may be an `include`d
  fragment) — the LSP enables it per file for workspace members, where the
  include graph pins the host module. (MissingRef)
- [ ] `call-arity` (correctness): call-site positional/keyword counts vs. the
  method table. Blocked on method indexing plus an environment of Base
  signatures. StaticLint's noisiest check in practice (macros, callable
  structs, `do`-blocks); its `compare_f_call` min/max/kw model is a
  reasonable spec, but gate behind solid resolution. (IncorrectCallArgs)
- [ ] `type-piracy` (correctness): extending an imported function with no
  owned argument type. Blocked on cross-file import and ownership
  resolution. (TypePiracy)
- [ ] `missing-include-file` plus include-graph checks (correctness): flag
  `include` of a nonexistent path; detect include cycles. Blocked on
  include-following in project discovery. (MissingFile, IncludeLoop,
  IncludePathContainsNULL)
- [ ] `redefined-constant` (correctness): reassigning a `const` binding, or
  defining a function over a name that already holds a value. A single-file
  version is feasible with current bindings; needs branch-awareness
  (StaticLint's `in_same_if_branch`) to not flag legal redefinitions in
  disjoint `if` branches. (InvalidRedefofConst, CannotDeclareConst,
  CannotDefineFuncAlreadyHasValue)

Probed and deliberately skipped: TypeDeclOnGlobalVariable (pre-1.8 Julia
only), UnsupportedConstLocalVariable (low value), KwDefaultMismatch (fiddly
per-type literal matching with known FPs), InappropriateUseOfLiteral (mostly
parse/lowering errors the parser should surface), FileTooBig/FileNotAvailable
(operational limits of the server, not lints).

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
- [ ] Active-document prioritization in `decide`: when several URIs are pending
  at once, `decide` starts them in arbitrary `HashMap` order, so the focused
  document is not analyzed first. Low priority — a normal edit only dirties one
  URI; this bites only on a bulk multi-file dirty (e.g. a workspace-wide external
  change). Option: thread the active/most-recently-edited URI through and prefer
  it in `decide`. rust-analyzer prioritizes the visible file.
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
  Cross-file references and rename—previously covered only at the db/`Analysis`
  level (hand-built workspace via `cross_file::test_support`)—now also have a
  full-server end-to-end test (`serves_cross_file_references_and_rename` in
  `tests/lsp.rs`): it drives a real temp package through `initialize`-with-root,
  isolates the environment so the detached harvest is fast and hermetic (empty
  `PATH`/`JULIA_DEPOT_PATH`/`JULIA_BINDIR` make `locate_install` fall back to the
  embedded minimal Base, ~1ms instead of parsing all of Base), and polls
  `references` until the harvest seeds the reverse index—since the library swap
  fires no client-visible readiness signal. The `poll`/`EnvGuard` scaffolding is
  reusable for any future harvest-dependent feature.

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
  parse. Refined with resolved names in Phase 6 (landed).

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
- [x] Hover: local binding info; for library symbols, signature(s) and
  docstring rendered as markdown (multiple dispatch: show the method group).
- [x] Signature help (triggers `(` and `,`), including keyword arguments):
  pure `compute_signature_help` (`src/lsp/signature_help.rs`) finds the
  innermost enclosing `CALL_EXPR`, counts top-level commas for the active
  positional parameter (clamping into a trailing `x...` vararg) and matches the
  keyword under the cursor past the `;` against the method's keyword params. The
  callee resolves through the shared masking order (`Resolver`): a library
  method group renders one `SignatureInformation` per method (capped at 10,
  docstring as documentation), an intra-file function shows the single signature
  read off its definition's parameter list. Per-parameter label offsets come
  from the shared `signature_label` in `src/lsp/render.rs` (which `render_method`
  now reuses). Warm `signature_help_via_db` path off the cached parse; behavior
  locked by inline units plus the `serves_signature_help` end-to-end test.
- [x] Go-to-definition (`textDocument/definition`): pure `compute_definition`
  (`src/lsp/definition.rs`) classifies the symbol at the cursor exactly as hover
  does (qualified read, local occurrence, free read) but returns a `Location`.
  Intra-file bindings point back into the document at `def_range`; library
  symbols (Base/Core, `using`'d exports, `Foo.bar`) resolve through the shared
  masking order to a harvested `DefLocation`, whose package-relative path is
  joined with the package's source root and read off disk to convert the byte
  span. Warm `definition_via_db` path off the cached parse. This required wiring
  the environment/index into the live server for the first time: a detached
  background loader (`spawn_library_loader`, only when the client sends a
  workspace root) resolves the environment and harvests its library
  (`harvest_library`), handing it to the analysis thread, which swaps it into a
  new `LibraryIndex.roots` salsa field (`set_library`, exposed via
  `Analysis::package_root` and a defaulted `PackageSource::package_root`).
  Multiple-dispatch "go to all methods" stays deferred to Phase 6. Locked by
  `definition` units (intra-file plus an on-disk depot jump) and
  `serves_goto_definition` in `tests/lsp.rs`.
- [x] References and document highlight (read/write sites of a binding): pure
  `compute_references` and `compute_document_highlights`
  (`src/lsp/references.rs`) classify the symbol at the cursor as go-to-definition
  does (an occurrence resolving to a binding, or a name on its own definition
  site) and gather `SemanticModel::occurrences`—the definition plus every
  resolved `IdentRef` with its `Access`. References returns intra-file
  `Location`s honoring `includeDeclaration`; document highlight tags each site
  read/write from the `Access` (augmented `+=` reports as a write). Free and
  qualified reads (library symbols) have no intra-file binding, so both yield
  nothing—cross-file references stay a Phase 5 item. Warm `references_via_db`/
  `document_highlights_via_db` paths off the cached parse; behavior locked by
  inline units plus `serves_references` and `serves_document_highlight` in
  `tests/lsp.rs`.
- [x] Rename (intra-file, with `prepareRename` validation): pure
  `compute_prepare_rename` and `compute_rename` (`src/lsp/rename.rs`) classify
  the symbol at the cursor as references does (an occurrence resolving to a
  binding, or a name on its own definition site) and rewrite every
  `SemanticModel::occurrences` range to the new name in a single-document
  `WorkspaceEdit`. Because the occurrences come from the scope-resolved model,
  a shadowing same-name local is left untouched and a nested-function capture
  is correctly included (both locked by tests). Only intra-file bindings
  rename; a free or qualified read (library symbol) is reported non-renameable
  by `prepareRename` (which returns the identifier's range) and yields no edit,
  so cross-file rename stays a Phase 5 item. Macros rename by their bare name
  (occurrence ranges cover the identifier after the `@`, preserving the sigil);
  `new_name` is validated as a legal Julia identifier, an invalid one returning
  an LSP error rather than a silent no-op. Warm `prepare_rename_via_db`/
  `rename_via_db` paths off the cached parse; behavior locked by inline units
  plus `serves_rename` in `tests/lsp.rs`.

### Phase 5: project and workspace level

- [x] Project graph: workspace membership from `initialize`, `include()`
  edges, package-project awareness (a workspace `Project.toml` means the
  package under development; index its module tree like a depot package).
  *Package-project awareness has landed:* `Environment::dev_package`
  (`src/environment.rs`) detects a named workspace `Project.toml` with a
  matching `src/<Name>.jl`; `harvest_library`/`harvest_workspace`
  (`src/index.rs`) index it like a depot package, flagged as `workspace` on the
  `LibraryIndex` salsa input (`src/incremental.rs`). A file's free reads resolve
  against the enclosing package module through a new tier-2 in `Resolver`
  (`Resolution::Workspace`, `src/resolve.rs`), lighting up go-to-definition,
  hover, and completion for the package's own top-level symbols across files.
  The workspace package re-harvests on `didSave` via a long-lived harvester
  thread (`spawn_workspace_harvester`, `src/lsp/server.rs`) that swaps it with
  `set_package_index`. *The transitive include-edge graph proper has since
  landed:* a tracked `project_graph` query (`src/incremental.rs`) re-derives the
  include closure, forward/reverse edges, host modules, cycles, and unresolved
  includes purely from the seeded `WorkspaceFiles` and each member's
  `include_edges` firewall (now carrying a range-free `host_suffix`,
  `src/project.rs`), so editing one member re-runs only that file's edges before
  a cheap re-derivation. It keys on normalized `PathBuf` (a tracked query cannot
  reach the concrete db's path map) and distinguishes a true cycle from a diamond
  via a three-color DFS, unlike the harvester's over-broad `IncludeCycle`.
  Unresolved includes and include cycles now surface as LSP diagnostics on the
  offending `include(...)` call (`src/lsp/graph_diagnostics.rs`), published per
  member file on each harvest and merged with parse diagnostics in `GlobalState`
  (`src/lsp/state.rs`). *The authority flip has since landed:* the graph's
  `host_modules` are now the resolution authority — `workspace_member` reads a
  per-file `host_module_of` firewall over `project_graph`
  (`src/incremental.rs`) instead of the harvester's `member_modules`, so a
  file's host module tracks unsaved include-structure edits live, an unchanged
  host backdates across graph re-derivations, and a file the graph does not
  reach falls back to the root module. `member_modules` stays as the
  harvester's own record, held in lockstep by a parity test
  (`tests/library_index.rs`); drop it once graph authority has soaked.
  *Multi-folder workspaces have since landed:* `workspace_roots`
  (`src/lsp/server.rs`) honors every `initialize` folder (deduped, `root_uri`
  fallback) and advertises `workspaceFolders.supported`; the harvester
  resolves one environment per folder (deduped on the resolved project file)
  and merges them via `harvest_libraries`/`dev_packages` (`src/index.rs`) —
  system index once, depot packages first-env-wins, one dev package per
  package-project folder. `LibraryIndex.workspaces` is plural, a path routes
  to its package by longest-src-prefix (`workspace_package_for`), the one
  `project_graph` merges every package's include closure, saves route to the
  owning package, and `OccurrenceKey` carries the package name so
  cross-file references/rename never bleed across folders. *Still deferred:*
  `workspace/didChangeWorkspaceFolders` (folders are read once at
  `initialize`); depot-package version conflicts across folders resolve
  first-env-wins (the library map is name-keyed); a user-set `JULIA_PROJECT`
  wins over every folder's walk-up, collapsing all folders into one
  environment (pre-existing precedence). (Nested-`module` file membership has
  since landed; see below.)
- [x] Cross-file go-to-definition, references, and rename for top-level
  symbols. Within-package go-to-definition, hover, and completion landed with
  the project graph; cross-file **references and rename** now land on a
  salsa-input reverse-occurrence index. The harvester records the include
  closure (`PackageIndex::members`), the analysis thread seeds each member as a
  `SourceFile` input registered in the `WorkspaceFiles` singleton (via
  `seed_disk_file`, create-or-return so an open buffer is never clobbered), and
  two salsa queries build the index: the demand-only per-file
  `file_workspace_occurrences` projection (`Resolver::workspace_occurrences`,
  keyed by `(namespace, name)` so a defining file's occurrences and a calling
  file's free reads — and multi-file dispatch — stitch together) and the
  `workspace_reference_index` aggregate unioning it over the member set. The
  shared `src/lsp/cross_file.rs` classifies the workspace symbol at the cursor
  (`Resolver::workspace_symbol_at`, the same oracle as go-to-definition) and
  materializes per-file sites; `references_via_db`/`rename_via_db` escalate to
  it and fall back to the intra-file path for locals and library symbols.
  `didClose` reverts a member's input to disk (`revert_file_to_disk`) so a
  discarded buffer leaves the index; each re-harvest rebuilds `WorkspaceFiles`
  wholesale, dropping stale members. *Deferred:* qualified reads (`Pkg.foo`) —
  the model records only the whole chain's range, not the `foo` sub-span — and
  navigation into `using`'d workspace submodules.
- [x] Nested-`module` file membership. A member file now resolves against the
  module its top level actually belongs to, not always the package root, along
  two axes. (1) *Host module:* the harvester records each member file's host
  module path (`PackageIndex::member_modules`, the nested `module` its `include`
  lexically landed in); `workspace_member` (`src/incremental.rs`) returns it
  alongside the package, and `Resolver` (`src/resolve.rs`) resolves the file's
  globals and free reads against `module_at(&root, host)`. (2) *File-internal
  nesting:* `ScopeKind::Module` scopes carry their name
  (`SemanticModel::enclosing_module_path`, `src/semantic.rs`), so a symbol
  declared inside an inline `module Sub` is attributed to `host ++ [Sub]`. The
  reverse-occurrence index is keyed by `(module path, namespace, name)`, so
  same-named symbols in different modules never conflate across cross-file
  references and rename. Locked by `member_modules` harvest units
  (`tests/harvest.rs`), collision/attribution units through the real salsa index
  (`tests/library_index.rs`), and an end-to-end nested-module cross-file
  references test (`serves_cross_file_references_in_a_nested_module`,
  `tests/lsp.rs`). *Deferred:* a file included at two different module sites is
  attributed to its first (the harvester's `visited` guard walks it once); a
  single file that both contributes host-level globals *and* opens an inline
  `module` of the same name as a sibling is not distinguished beyond the path.
- [x] Workspace symbols (fuzzy subsequence match over top-level definitions):
  pure `compute_workspace_symbols` (`src/lsp/workspace_symbols.rs`) walks the
  harvested `PackageIndex` of the package under development (recursing
  submodules), keeps every function/type/const/macro/(sub)module whose name is a
  case-insensitive subsequence of the query (empty query returns all), and
  materializes each match's `DefLocation` into an on-disk `Location` the same way
  go-to-definition does (join the package-relative path with the source root,
  read the file once, convert the byte span). Symbols carry the enclosing
  module's `container_name`; kinds mirror document symbols (`MODULE`, `STRUCT`,
  `INTERFACE`, `CONSTANT`, `FUNCTION` for functions and macros). There is no
  live-buffer or cached-parse gate: the result is a projection of the
  `LibraryIndex` salsa input, so `workspace_symbols_via_db` reads the workspace
  name off the `Analysis` snapshot (itself the `PackageSource`) and delegates.
  Wired through a document-less `ReadJob::WorkspaceSymbols`, advertised via
  `workspace_symbol_provider`. Locked by inline units plus `serves_workspace_symbols`
  in `tests/lsp.rs`. *Deferred:* fuzzy *ranking* (results follow harvest order)
  and lazy `workspaceSymbol/resolve` (locations resolve eagerly).
- [x] `workspace/didChangeWatchedFiles`: watched events feed the same channels
  the editor does. An environment file (any project/manifest flavor, classified
  by `is_environment_file`, `src/environment.rs`) escalates to
  `HarvestSignal::Environment`: the harvester (`spawn_workspace_harvester`,
  `src/lsp/server.rs`) drains the queued burst (a `Pkg.add` rewrites project
  and manifest together) and restarts its resolve-and-harvest cycle, so a
  changed manifest or a created/deleted `Project.toml` reshapes the whole
  library (an empty re-resolve still sends, clearing a deleted environment).
  A `.jl` event sends `HarvestSignal::Source`, re-harvesting the owning
  workspace package exactly like a save, so created and deleted files refresh
  membership on the next member seeding; a file with no open buffer is first
  synced to disk over the widened close-to-sync channel
  (`revert_file_to_disk`), while an open buffer stays authoritative until it
  closes. Saves classify through the same `harvest_signal`, so an in-editor
  `Project.toml` save also re-resolves. Watchers (`**/*.jl` plus the four
  project/manifest names and `Manifest-v*.toml`) are registered via
  `client/registerCapability` when the client advertises
  `didChangeWatchedFiles.dynamicRegistration` and a folder is open — sent as
  the main loop starts, since `lsp-server` consumes `initialized` inside
  `initialize_finish`. Locked by classifier/capability units
  (`src/environment.rs`, `src/lsp/server.rs`) and two e2e tests
  (`registers_file_watchers_when_the_client_supports_it`,
  `watched_file_events_refresh_environment_and_membership`, `tests/lsp.rs`).
  *Deferred:* no debounce beyond burst-draining (each source event re-harvests,
  deduped on an unchanged index), and an open buffer ignores on-disk changes
  until it closes.
- [x] Diagnostics maturation: pull diagnostics (`textDocument/diagnostic`)
  with push fallback; lint findings as diagnostics with quick-fix code
  actions (needs the Linter section's first rules); first semantic
  diagnostics (undefined name—masking-aware, unused binding, unused import).
  *Lint findings as diagnostics has landed:* the analysis read-phase runs the
  linter (default config until configuration discovery lands, Phase 6) on a
  parse-clean tree and merges the findings into the same versioned publish as
  the parse diagnostics (`src/lsp/analysis_thread.rs`). `lint_parsed`
  (`src/linter/check.rs`) is the shared run-rules-and-filter-suppressions
  core; `lint_diagnostics_via_db` (`src/lsp/lint.rs`) lints off the
  salsa-cached tree and semantic model, falling back to a fresh parse on a
  cache miss or racing write. Findings map rule ID → `code`, engine-stamped
  severity across, `source: "fatou"`, and the `unused-` family carries the
  `UNNECESSARY` tag. This delivers the unused-binding and unused-import
  semantic diagnostics via the existing rules. Locked by units in
  `src/lsp/lint.rs` plus `publishes_lint_findings_as_diagnostics`
  (`tests/lsp.rs`). *Quick-fix code actions have landed:*
  `textDocument/codeAction` (a `ReadJob`, advertised as a `quickfix`-only
  provider) re-lints warm off the cached parse rather than round-tripping fix
  data through published diagnostics — a `linter::Fix`'s byte offsets are only
  valid against the exact buffer they were computed from, so recomputing
  against the live text is the correct thing to do. Each fix on a finding
  overlapping the requested range becomes one action
  (`src/lsp/code_action.rs`) with a single-document `WorkspaceEdit` and the
  finding's diagnostic attached; safe fixes are `isPreferred`, unsafe ones say
  so in the title (the LSP has no `--unsafe-fixes` gate). Locked by units plus
  `serves_quick_fix_code_actions` (`tests/lsp.rs`). *Pull diagnostics with
  push fallback has landed:* a client advertising `textDocument.diagnostic`
  gets a diagnostic provider (`identifier: "fatou"`, inter-file dependencies
  on for the include graph) and `textDocument/diagnostic` answered as a
  `ReadJob` with a full report — parse diagnostics, lint findings on a clean
  tree, and the file's include-graph problems re-derived from the cached
  `project_graph` (`src/lsp/pull_diagnostics.rs`); `resultId`/`unchanged`
  responses are deferred. With pull on, the per-edit push is off end to end
  (the analysis read-phase skips computing it; the write-phase still keeps
  the db warm), an opened document's previously pushed graph diagnostics are
  cleared as pull takes over, files with *no* open buffer keep the graph-diag
  push (the client never pulls them), and each re-harvest nudges
  `workspace/diagnostic/refresh` when the client supports it. A client
  without the capability keeps the push pipeline unchanged. Locked by units
  (`src/lsp/pull_diagnostics.rs`, `src/lsp/server.rs`) plus
  `serves_pull_diagnostics` (`tests/lsp.rs`). *The undefined-name diagnostic
  has landed* (see the Linter roadmap's `undefined-name` entry): the server
  enables the rule per file for workspace members, resolving through the
  shared masking order with the harvested library and the file's host module
  (`server_rules`, `src/lsp/lint.rs`; locked by
  `undefined_name_runs_only_for_workspace_members`). With unused-binding and
  unused-import already shipping as rules, all three first semantic
  diagnostics are live.

### Phase 6: later polish and Julia-specific ambitions

- [x] Semantic tokens refined with resolved names (function vs macro vs type
  vs module): identifiers paint by what they resolve to. Definition-site
  names, export-list entries, and resolved occurrences classify by their
  binding's `BindingKind`; free reads go through the shared masking order
  (`Resolver::resolve`) with the target module supplying the kind (functions,
  types, macros, and submodules as `namespace`; consts stay plain — the
  legend has no constant type yet). Qualified reads (`Base.Threads.@spawn`)
  paint each library-resolvable module component as `namespace` and the final
  member by its kind. The legend appends `function`/`type`/`namespace`, so
  the syntax indices stay stable. Locals, parameters, and operators (`x + y`
  lighting up as "function" is noise) stay plain; `import`ed names are not
  chased into the library (matching hover and go-to-definition) and
  `using`/`import` statement paths stay plain — both deferred. Resolved and
  syntax spans merge position-sorted (syntax wins overlaps), and
  `semantic_tokens_via_db` threads the cached model plus workspace membership
  through, so same-package siblings classify too. En route the semantic
  builder learned deep qualified macro reads — a multi-component qualifier
  parses as a nested field-access chain under `MACRO_NAME` and was dropped
  before (`walk_macro_name`, `src/semantic/builder.rs`). Locked by units
  (`src/lsp/semantic_tokens.rs`, `src/semantic.rs`) plus
  `serves_semantic_tokens` (`tests/lsp.rs`).
- [x] Call hierarchy (incoming/outgoing calls): prepare
  (`textDocument/prepareCallHierarchy`) classifies the cursor exactly as
  references does — a workspace top-level function through the reverse-
  occurrence index (resolved to its defining file's first definition site), or
  an intra-file `BindingKind::Function` binding — returning one item per
  function *name* (methods share a binding). Incoming calls ride the reverse
  index: each non-definition site is kept iff it is syntactically a call
  (`is_call_site`, re-derived per request from the cached CST — occurrences do
  not record call-ness) and attributed to its nearest *named* enclosing
  callable via a CST ancestor walk (anonymous fns and `do` blocks walk past;
  a top-level call synthesizes a module or file caller item so nothing drops).
  Outgoing calls walk the definition's subtree (nested named callables and
  modules own their own calls), resolving each plain-name callee through the
  shared masking order: intra-file bindings and workspace siblings off tracked
  text, Base/depot targets materialized from disk via the harvested
  `DefLocation` (def-site helpers factored out of go-to-definition; one
  read+parse per target file per request). Incoming/outgoing re-derive their
  item from `uri` + `selection_range` against *tracked* text (closed members
  are disk-seeded; the state handlers dispatch document-lessly like workspace
  symbols), answering `None` on skew or a racing write rather than wrong data.
  Broadcast `f.(x)` and `do`-block calls count; `enclosing`/`callable`
  extraction reuses the promoted document-symbols helpers
  (`callee_name`/`head_name`/`unwrap_head`, `src/lsp/symbols.rs`). All in
  `src/lsp/call_hierarchy.rs`; locked by inline units (intra-file, library,
  and workspace-db cross-file) plus `serves_call_hierarchy` in `tests/lsp.rs`.
  *Deferred:* macro calls (`@foo`), qualified-call sites (`Pkg.foo(x)`,
  matching `workspace_occurrences`), per-method items under multiple dispatch,
  incoming calls for library symbols, and a persisted call-site index (the
  per-request CST walk protects keystroke latency instead).
- [x] Type hierarchy from the declared type tree (supertypes/subtypes of
  structs and abstract types)—a Julia-specific win that needs no inference.
  Prepare (`textDocument/prepareTypeHierarchy`) classifies the cursor exactly
  as call hierarchy does — a workspace top-level type through the reverse-
  occurrence index (the definition rec re-derived through `type_decl_at`, so
  outer constructors sharing the struct's name never shadow the declaration),
  or an intra-file `BindingKind::Type` binding. Supertypes re-derives the
  declaration from the item, peels the `<:` clause off the signature head
  (parametric supers unwrap to their base: `AbstractArray{T,1}` →
  `AbstractArray`), and resolves the base name through the shared masking
  order: intra-file bindings and workspace siblings off tracked text,
  Base/depot types materialized from disk via the harvested `DefLocation`. No
  declared supertype answers empty (the implicit `Any` is a hierarchy root;
  no synthesized item). Subtypes rides the reverse index: each non-definition
  site is kept iff it is syntactically a *declared-supertype* position
  (`supertype_site_decl`, re-derived per request from the cached CST —
  occurrences do not record supertype-ness; curly type-param bounds, `where`
  bounds, annotations, and supertype *arguments* like `S <: Tree{Animal}` all
  reject), and its enclosing declaration is the subtype. STRUCT/INTERFACE
  kinds follow document symbols. The capability is injected into the
  serialized initialize JSON — lsp-types 0.97 ships the request/item types
  but no `type_hierarchy_provider` field. All in `src/lsp/type_hierarchy.rs`;
  locked by inline units (intra-file, library, and workspace-db cross-file)
  plus `serves_type_hierarchy` in `tests/lsp.rs`. *Deferred:* qualified
  supertypes (`<: Base.Number`, matching `workspace_occurrences`), imported
  supertypes (imports are not chased, matching hover and go-to-definition),
  and subtypes of library types (the reverse index only covers workspace
  members).
- [x] Multiple-dispatch-aware navigation: go-to-definition returning all
  methods of a function.
- [ ] Qualified method extensions in navigation: go-to-definition on
  `Base.show` surfacing workspace methods harvested with `owner`
  (`Base.show(io, x) = ...`), not just the target package's own definitions
  (same wrinkle as hover).
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
