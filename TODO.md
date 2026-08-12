# TODOs

## Parser

- [ ] Lex the *broadcast* wrapping arithmetic operators `.+% .-% .*%` (and their
  augmented forms `.+%= .-%= .*%=`) in `crates/fatou-parser/src/parser/lexer.rs`. The undotted
  `+% -% *%` are supported; the dotted forms still split into `.+` + `%`, which
  mis-parses rather than erroring. No code in the smoke-test corpus uses them
  (only JuliaSyntax's own tests do). Deferred: the whole wrapping-operator family
  is unreleased in JuliaSyntax — the latest release (1.0.2) still rejects every
  form (`lex_plus`/`lex_minus`/`lex_star` have no `%`-suffix handling, verified
  2026-08-03), so no oracle bump can pin these until JuliaSyntax ships them.
  Implementing the lexer change now would be validatable only against a
  hand-authored parser fixture, not the differential oracle.

- [ ] Two error-recovery gaps left over from labeled `break`/`continue`
  (`crates/fatou-parser/src/parser/structural.rs`). Junk after a complete labeled keyword drops
  JuliaSyntax's trailing zero-width marker (`break l x y` ⇒ `(break l x)
  (error-t y)`, not `(error-t y (error-t))`), and a bare comma after one does not
  fold into a tuple (`break l, y` ⇒ `(break l) (error-t ✘ y)`, not
  `(tuple (break l) y)`) — the latter predates labels, since `break, y` behaved
  the same way. Both are error/edge shapes no corpus code hits, and neither is
  pinnable: labeled `break`/`continue` is unreleased in JuliaSyntax (the latest
  release, 1.0.2, still rejects `break lbl`, verified 2026-08-03), so no oracle
  bump can pin these until JuliaSyntax ships the feature.

### Incremental

- [ ] Maybe (deferred): a nested-block tier needs a context-parameterized
  fragment entry point (`public_context`, bracket `end` markers) — a bare
  fragment `parse()` misparses those today. A pure optimization on top of a
  sound stage 2–4.

## Formatter

## Linter

### Rules

- [x] `index-from-length` (suspicious, syn, opinionated, warning, unsafe fix):
  flags `for i in 1:length(x)`/`1:size(x, d)` when `i` indexes `x` (suggest
  `eachindex`/`axes`) and iterating a bare numeric literal (`for i in 3.5`).
  Name-based match on `length`/`size`; no type info to exempt `Vector`/`Array`,
  so gated on the loop var actually indexing the collection. The first shape
  ships an unsafe fix rewriting the `1:length`/`1:size` prefix (value-equivalent
  only for one-based dense indices, unprovable without types), withheld off
  Base's plain arity or over a comment. On by default.
  (IncorrectIterSpec, IndexFromLength)
- [x] `discouraged-function` (suspicious, sem, warning, no fix): flags a call to
  a function on the `[lint.rules.discouraged-function]` deny-list. `functions`
  replaces the built-in set, `extend-functions` adds to it, `functions = {}`
  silences the rule. Built-ins are Base functions with process-wide or
  memory-unsafe effects (`exit`, `cd`, `redirect_std*`, the `unsafe_*`/pointer
  conversions). A `do`-block call is skipped, since for `cd`/`redirect_*` that
  form is the suggested alternative. Two-tier namespace gate: a built-in name
  needs `resolves_to_base`, a project-configured one only needs to survive
  `read_is_shadowed_locally` — demanding Base confirmation for a name the
  project added would make its config inert. On by default.

### Rule infrastructure

Shared machinery the roadmap below leans on, mostly cribbed from arity's own
`TODO.md`. Note one thing we do **not** need: arity's "Phase A def-use reverse
index" (binding -> read sites, `IdentRef` -> `BindingId`) is machinery we
already have — `IdentRef::binding` and `SemanticModel::occurrences` are exactly
it. We are behind arity only on the CFG and on rule ergonomics.

- [x] `RuleContext::resolves_to_base(&CallExpr) -> bool` plus a token-level
  `read_resolves_to_base`, after arity's `src/linter/rules.rs:177,215`: one call
  that confirms a callee really is the Base/Core function and not a local
  shadow, a namespace-qualified name, or a `using`-masked import. Landed with
  the rest of the per-file machinery the three resolution-dependent rules were
  hand-rolling, all memoized on `RuleContext`: `resolver()`,
  `has_unresolvable_using()`, `file_scan()` (one shared `FileScan`, previously
  copy-pasted between `undefined-name` and `call-arity`), and
  `trusts_resolution()` for the shared soundness floor. `RuleContext` is now
  built with `new(..).with_resolution(..)` and friends rather than a struct
  literal, so the cache stays private.
- [x] A shared call-shape matcher module (arity's
  `src/linter/rules/matchers.rs`): "call to *name* with exactly *n* positional
  arguments and no named ones" is the opening line of most idiom rules. Landed
  as `src/linter/rules/matchers.rs`: `plain_call(node, name, arity)` is that
  whole opening, over `CallShape`, which splits a call site into positional
  arguments, keyword arguments (both sides of the `;`, including the
  `f(; verbose)` shorthand), and the two *open* flags a splat sets — the
  distinction a rule reasoning about arity has to respect. `call_expr` /
  `call_named` exclude a definition's signature, which is a `CALL_EXPR` too.
  `call-arity` now runs on `CallShape` instead of its private copy, which is
  what proved the API. Two pure-navigation accessors moved down to the AST
  layer where they belong (`CallExpr::callee_ident`, `Expr::name_ident`), and
  `index-from-length`, `nothing-comparison`, `missing-comparison`, and
  `file_scan` were rewritten onto them.
- [x] A per-region control-flow graph, after arity's `src/semantic/cfg.rs`.
  Landed as `src/semantic/cfg.rs`: basic blocks, the
  `Goto`/`Branch`/`Return`/`Diverge`/`Unreachable` terminator enum, structured
  recursive descent with no fixpoint, memoized per file by the
  `incremental::control_flow` salsa query (`Eq`, so it backdates) and exposed to
  rules as the lazily-built `RuleContext::control_flow()`. Regions are the file
  top level, each `module` body, and each function-like body with a `BLOCK`
  (`function`, `macro`, `do`), keyed by the new `syntax::NodePtr`. Julia's
  richer terminator set is all in: `return`, `throw`/`error`/`rethrow` (by
  name, as arity does — the consumer confirms with `resolves_to_base`),
  `break`/`continue`, and `@goto`/`@label`, which is handled by allocating a
  block per label so a forward `@goto` and a backward one agree and lowering
  *resumes* at a label after a divergence. Two Julia-specific additions beyond
  arity's shape: the idiomatic `cond && return x` short-circuit is a real
  conditional divergence, and `try`/`catch`/`finally` gets an exceptional edge
  out of the *header* (not the try body's exit), which is what keeps a `catch`
  and a `finally` reachable when the `try` body always diverges.
  Reachability is a BFS from the entry once the graph is built rather than
  arity's construction-time marking — `@goto` needs it, and it subsumes the
  `while true`-with-no-`break` case for free.
  - The one concession to Julia's macros: an expansion is code the graph never
    sees, and real code hides jumps in one (JSON3's `@eof` expands to `@goto
    invalid`), so a region containing any macro call keeps its `@label` blocks
    reachable and a `while true` whose body has one is not claimed to never
    exit. The dead tail after an unconditional `return` is unaffected.
    Validated by sweeping 6000 files of `~/.julia/packages` (22280 regions,
    67740 blocks): 7 unreachable blocks, every one hand-checked as real dead
    code (`return ex; return` in StructUtils, a `while true` with no `break`
    in an HTTP example), no false positives.
- [x] `is_unreachable` was a linear scan over every block of every region, so a
  rule asking per statement would have been quadratic. `FileControlFlow::build`
  now collects the statement ranges of the unreachable blocks into a
  `HashSet<TextRange>` as the graphs are built, and the predicate is a hash
  lookup into it. The index costs nothing in practice: unreachable code is rare,
  so the set is empty for nearly every file. `tests/cfg.rs` pins it against the
  old scan, statement by statement, over the whole parser fixture corpus.
- [x] Per-rule config: a `[lint.rules.<id>]` TOML table plus a typed per-rule
  struct in `src/config.rs` (`RulesConfig`, one field per *configurable* rule;
  `rename_all = "kebab-case"` is what makes the field name the table name).
  Threaded to rules as `ctx.config: &RulesConfig` — the whole table, not one
  rule's slice, so `Rule` stays a plain trait object and `all_rules()` keeps its
  empty signature. Carried on `ResolvedRules` and borrowed per file by the
  driver, exactly as `julia_target` already was, so no per-file entry point
  widened. Landed with `discouraged-function` as its first consumer.
  - Strictness is deliberately asymmetric: a typo in `select`/`ignore`/
    `severity` stays a warning (those are *data*), while an unknown rule ID
    under `[lint.rules]` — or an unknown key inside a rule's table — is a config
    parse error via `deny_unknown_fields` (that is *schema*).
  - Per-rule *severity* stays at `[lint.severity]` rather than moving into the
    rule table; the stamping loop in `ResolvedRules::run` is the seam if that
    ever changes.
  - Still open, now unblocked: `unused-binding`'s `ATTRIBUTE_DSL_MACROS` wants
    an `extend-attribute-dsl-macros` knob, and the throwaway-name convention
    (`_`-prefixed) wants one too.
- [x] Suppression-map refactor, prerequisite for the meta-rules below (arity's
  "§I6"): `suppression.rs` is a full port of arity's CST-driven design —
  directives (rule ID with exact range, optional `: <reason>`, raw text) are
  recorded on `RuleContext::suppressions`, node directives attach to the next
  non-trivia sibling, filtering happens inside `ResolvedRules::run` (which
  records a `DirectiveUsage` and feeds the new `Rule::check_suppressions`
  post-pass), and `ResolvedRules::enabled()` tells stale from dormant.
  Deviation from arity: a bare `# fatou-ignore-file` stays suppress-all.
- [x] Settled, not a task: the rule categories are `correctness`, `suspicious`,
  `performance`, and `readability`. Several candidates below are idiom rewrites
  rather than likely-bug reports, and `suspicious` means "legal Julia but very
  likely not intended" — which `length-zero` and `comparison-negation` plainly
  are not — so the two extra categories (both of which arity ships) earn their
  keep. Each directory is created when its *first* rule lands; no empty modules
  ahead of time. Assignments are recorded on the entries below.
  - The original entry claimed this blocked the first idiom rewrite because
    "the rule ID's docs path bakes it in". It does not: a category appears in
    no public surface. The ID is bare kebab-case with no prefix, the reference
    is one generated page (`docs/src/reference/rules.md`, one `##` section per
    rule ID in `all_rules()` order) rather than a page per category,
    `SUMMARY.md` has no per-rule entries, and `select`/`ignore`/
    `[lint.severity]`/`# fatou-ignore` all key on the bare ID. A category is
    the directory under `src/linter/rules/` and the re-export module, nothing
    more, so moving a rule between categories is a free internal refactor —
    pick the fitting one and move on.
  - No landed rule moves. `index-from-length` and `discouraged-function` are
    the two that sit closest to the line, and both stay `suspicious`: the
    `1:length(x)` shape is a real hazard for offset arrays, and a deny-listed
    call is flagged for its effects, not its style.

### Rule roadmap

- [x] `invalid-type-declaration` (correctness, sem + res, warning, no fix,
  default-off): any `TYPE_ANNOTATION` whose type is a bare name resolving to a
  function — a signature parameter, a struct field, a value-position
  typeassert alike. Partial exactly as predicted: it fires for a
  `BindingKind::Function` in this file and for a workspace package's function
  group, and stays silent for Base/Core (the export snapshot carries no kinds)
  and for imports. Beyond the shared `trusts_resolution` bail-outs, one guard
  earns its keep — an outer constructor binds `Foo` as a function too, so a
  same-named type, `const`, or module anywhere in the file or the package
  withholds the finding. In `RESOLUTION_RULES`.

- [x] The eager-broadcast family (performance, syn, warning, unsafe fixes), the
  three rules that created `src/linter/rules/performance.rs` and its directory:
  `eager-broadcast` (`all`/`any`/`count`/`maximum`/`minimum`/`prod`/`sum` over
  `f.(x)`, rewriting the argument alone to `f, x`), `sorted-extremum`
  (`sort(x)[1]`/`[begin]`/`[end]` -> `minimum`/`maximum`), and `length-findall`
  (`length(findall(p, x))` -> `count(p, x)`, both `findall` arities). Every fix
  is unsafe on one shared ground, which is what the category's module doc
  records: each rewrite is equivalent for the collection the author plainly had
  in mind and not for every operand — a broadcast scalar, a `NaN`, a `Dict` —
  and telling those apart needs types we do not have. Each rule still opens with
  `resolves_to_base`, and each name a fix splices in (`minimum`, `count`) is
  gated on `name_resolves_to_base` separately.
  - Two pieces of shared machinery came with them: `matchers::plain_broadcast` /
    `CallShape::of_broadcast` (the same argument-list policy, asked of a
    `DOT_CALL_EXPR`), and `rules/rewrite.rs` for the two questions every
    splice-the-sub-texts fix asks — would the rewrite drop a comment, and does a
    piece fit in a one-line message. The three hand-rolled comment guards in
    `length-zero`, `typeof-comparison`, and `index-from-length` predate it and
    could move over.

Deferred, and why:

- [ ] `occursin`-with-a-regex-literal rules: `occursin(r"abc", s)` ->
  `occursin("abc", s)` when the pattern has no metacharacter, and
  `occursin(r"^abc", s)` -> `startswith(s, "abc")`. Blocked on a Julia
  regex-literal analyzer (arity has `src/linter/regex.rs` for exactly this).
  (arity `fixed-regex`, `string-boundary`)
- [ ] `unnecessary-nesting` (readability): `if a; if b; body; end; end` -> `if a
  && b; body; end`, when neither `if` has an `else`. Low risk, low urgency.
  (arity `unnecessary-nesting`)
- [ ] A `Test`-stdlib rule bundle, as one cohesive change with a shared `@test`
  matcher — the Julia counterpart of arity's planned `testthat` bundle, which it
  rates "high value for test-heavy repos" (equally true here): `@test x == true`
  -> `@test x`, `@test length(x) == 0` -> `@test isempty(x)`, `@test isa(x, T)`
  -> `@test x isa T`, `@test x == nothing` -> `@test isnothing(x)`, and a
  `@test` whose argument is not a comparison or predicate at all. Gate on
  `Test` actually being loaded, as arity gates on the package being attached.
- [ ] A `documentation` category over docstrings, structurally mirroring
  arity's five roxygen rules: undocumented exported names, `@doc` argument
  lists that disagree with the signature. Larger design question than a single
  rule. Would be a fifth category beyond the settled four, which is cheap on
  its own — the taxonomy note above applies — but the bundle is what needs
  designing, not the directory.
- [ ] Not a lint rule, but noted while reading JuliaWorkspaces: its
  `layer_diagnostics.jl` publishes **TOML syntax diagnostics** for
  `Project.toml`/`Manifest.toml`. Cheap LSP diagnostic source; we already parse
  both.

Rejected after probing (do not revisit without new evidence):

- `RelativeImportTooManyDots` — could not reproduce a dedicated Julia error for
  `using ....Foo`; it degrades to a plain `UndefVarError`. Also FP-prone, since
  a file's module nesting depends on how it is `include`d.
- `UnassignedKeywordArgument` — obsolete; `f(; x)` shorthand is legal since 1.5.
- `InappropriateUseOfLiteral` — most of the shapes (`1 = 2`, `module 1`) are
  parse errors in real Julia. Check what our parser already rejects before
  spending a rule on the remainder.
- The `Number`-typed half of `IncorrectIterSpec` (`for i in n` where `n` is a
  scalar variable) — needs types we do not have. The literal half already ships
  in `index-from-length`.
- `TypeDeclOnGlobalVariable` — only applies below 1.8; fold into
  `julia-version-compat` rather than giving it a rule.
- arity's `outer-negation`, `implicit-assignment` (Julia's `f(x = 1)` is a
  keyword argument, not an assignment), `repeat` (`while true` is idiomatic in
  Julia and already exempt in `constant-condition`), `true-false-symbol`
  (`true`/`false` are keywords, not rebindable bindings), `crossprod`,
  `lengths`, `empty-assignment`, `is-numeric` — no meaningful Julia analogue.

## Language server

- [ ] Maybe: a `fatou index` CLI subcommand to warm and inspect the cache.
- [ ] Code actions beyond quick fixes: organize/sort `using` statements,
  qualify a bare name.
- [ ] `workspace/willCreateFiles` and `workspace/willDeleteFiles`, the siblings
  of the rename handlers. The `RenameMap` machinery in `src/lsp/rename_files.rs`
  is most of what a delete needs; the open question is what a delete *should*
  edit, since dropping the `include` call that names a deleted file is a
  destructive default and leaving it dangling is what the include-graph
  diagnostics already report. Create is close to a no-op. Design first.
- [ ] Renaming a package's entry file (`src/MyPkg.jl`) rebases its own includes
  but leaves `Project.toml`'s `name` alone, so the package silently stops
  matching its entry. `willRenameFiles` deliberately does not touch
  `Project.toml` — a `WorkspaceEdit` into a manifest is a bigger promise than
  the include rewrite — but the pair could at least be diagnosed.
- [ ] Maybe (deferred): a rope (`ropey`) for the live buffer, raised in #76.
  It would make locating a line O(log n) and retire `LineStarts` outright. What
  blocks it is narrower than #76's first reading, and it is *not* the lexer:
  `Token` owns its `text: String` (`parser/lexer.rs`), so nothing borrows the
  input and a chunk-based lexer is a local refactor. Nor the tree, formatter, or
  linter — rowan's green tokens own their text and `SyntaxText` is already
  chunked, and the warm LSP paths go through the CST (`format_node`,
  `check_parsed`), never the source. The three real `&str` demands are
  `parse(text: &str)`, `SourceFile.text: String` (`src/incremental.rs`), and
  above all `reparse`/`reparse_edits`, whose token- and toplevel-tier guards
  prove a splice sound by slicing and relexing regions of `prev_text`/
  `new_text`. That last one is the wall: a rewrite of the most delicate code in
  the parser crate.
  Note also that the "a rope pays a `Rope::to_string` per keystroke" objection
  is weaker than it looks — a keystroke already pays two full `String` copies
  (`analysis_thread`'s `upsert_file`, and `PrevParse { text: text.clone() }`),
  which a rope in salsa would replace with an O(1) CoW clone. The case for
  deferring rests on the size of the prize instead: with the table patched per
  edit (`src/text/buffer.rs`) a keystroke costs ~2% of the reparse it triggers,
  so what is left to win is a memmove plus one add per line after the edit site,
  against point queries the bench measured ~7x slower on a rope
  (`benches/line_index.rs`). rust-analyzer keeps documents as a plain `String`
  and applies edits with `replace_range` for the same reasons. Revisit only if
  the reparse guards go chunk-based.

## Tooling
