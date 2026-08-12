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

- [x] The `occursin`-with-a-regex-literal pair, which came with the
  regex-literal analyzer that blocked them (`src/linter/rules/regex.rs`: reads
  the `r"..."` — `r` prefix, no flag suffix, no interpolation — and classifies
  its raw text as a fixed string or a single anchor). `fixed-regex`
  (performance, syn, warning, **safe** fix) rewrites `occursin(r"abc", s)` to
  `occursin("abc", s)` by deleting the `r` and nothing else: a fixed string
  carries no `\` and no `$`, the two characters an ordinary literal reads
  differently, and the literal keeps its own delimiters, so the string is
  unchanged. `string-boundary` (readability, syn, warning) rewrites
  `occursin(r"^abc", s)` to `startswith(s, "abc")` — **safe**, since `^` is a
  start-of-subject test — and `occursin(r"abc$", s)` to `endswith(s, "abc")`
  **unsafely**, because PCRE's `$` also matches before a final newline
  (`occursin(r"abc$", "abc\n")` is `true`, `endswith("abc\n", "abc")` is not).
  Both open with `resolves_to_base` on `occursin`; the boundary fix gates the
  name it splices in separately.
  - Which argument holds the pattern is `regex::PatternCall`'s whole job, and
    it differs per function: `occursin(r"a", s)` against `contains(s, r"a")`,
    the second argument of `startswith`/`endswith`/`split`/`eachsplit`, the
    left of each `=>` pair of a `replace`, and a curried form that fixes the
    pattern (`contains(r"a")`) against one that fixes the *haystack*
    (`occursin(s)`, which carries no pattern and is matched by nothing).
    `rsplit` is deliberately absent: it has no `Regex` method at all, so a
    regex there is an error, not an idiom. `fixed-regex` covers the whole
    table; `string-boundary` takes only the searches, since only they have a
    haystack a boundary predicate can take over, and it answers a curried
    search with a curried predicate (`startswith("a")`).
  - Two things the pair still declines, both recorded by a test:
    - A keyword-carrying call (`replace(s, p => r; count = 1)`,
      `split(s, d; limit = 2)`) is skipped, since every rule here goes through
      `plain_call`. Sound but a real miss; a `positional_call` matcher that
      ignores keywords the pattern question does not depend on would close it.
    - An anchored pattern under a predicate that anchors on its own
      (`startswith(s, r"^abc")`) is left alone by both. The anchor is redundant
      there and could be a finding of its own, but reading it as a boundary is
      wrong for the mismatched `startswith(s, r"abc$")`, which tests that `s`
      *is* `abc`.

- [x] `unnecessary-nesting` (readability, syn, warning, **safe** fix): an `if`
  whose whole body is another `if` — `if a; if b; body; end; end` -> `if a && b;
  body; end`. Both halves must be a bare `if`/`end`: an alternative on either
  breaks the equivalence (an outer `else` would newly see the case where `a`
  holds and `b` does not), which also rules out an `elseif` *clause* as the
  outer half, so only a whole `IF_EXPR` is dispatched. The fix splices the two
  tests and the inner block verbatim, parenthesizing a test looser than `&&`
  (`(a || c) && b`) off an explicit tight-shape list, since the parser's
  precedence table is not exposed over the CST; it is withheld when a comment
  sits in the discarded headers. The inner body keeps its own indentation —
  fix-then-format settles it. A deeper nest reports per adjacent pair and
  converges over the re-lint loop.

Deferred, and why:

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
- [x] Not a lint rule, and not a linter task at all: TOML syntax diagnostics for
  `Project.toml`/`Manifest.toml`, noted while reading JuliaWorkspaces'
  `layer_diagnostics.jl`. Landed as the `toml-syntax` check in *Project files*
  below, stage 1, along with the reason it cannot be a rule.

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
  `Project.toml` (a `WorkspaceEdit` into a manifest is a bigger promise than the
  include rewrite). The diagnosis half has landed as *Project files*'
  `missing-entry-file`, so the mismatch is at least reported; the edit is
  *Project files* stage 4.
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

## Project files

`Project.toml`/`Manifest.toml` steer resolution (`environment::resolve` walks
them the way Julia's loader does, `register_file_watchers` covers all five
flavors, and `is_environment_file` escalates a watched change to
`HarvestSignal::Environment`) and, since stage 1, report their own problems.
Since stage 2 a project file is also a salsa input, so its `[deps]` follow the
editor's unsaved buffer; since stage 3 an open one is a document with a route of
its own, and since stage 4a a `Project.toml` answers go-to-definition, hover,
document links, and inlay hints on a dependency name.

The target is what rust-analyzer gives `Cargo.toml`, which is narrower than it
is usually remembered as: watch-and-reload, plus a document-selector entry so
workspace-load failures land on the right file. Dependency name and version
completion is not rust-analyzer at all, it is Even Better TOML and the crates
extension. So the reload half was already finished and the language-features
half is what remains.

Settled up front: **these are not lint rules.** `Rule::interests()` is
`SyntaxKind`-keyed and dispatch is a single walk over the Julia CST
(`src/linter/rules.rs`), so a TOML check has no kind to register against, and by
tenet the linter is purely semantic over Julia.

- [x] **Stage 1: spans, then diagnostics.** `src/project_files.rs` holds the
  checks and `src/lsp/environment_diagnostics.rs` the LSP edge; the workspace
  harvester produces them, since it is the only place the resolved
  `Environment` — and the failure to resolve one, which `server.rs` used to
  swallow — exists. Seven checks: `toml-syntax` (error) plus
  `missing-entry-file`, `missing-julia-compat`, `unknown-compat`,
  `missing-from-manifest`, `uuid-mismatch`, and `missing-compat` (warnings).
  Fixed severities, no `fatou.toml` surface; the extension's existing
  `fatou.diagnostics.enable` is already the off switch.
  - `Project.toml` now parses against a typed `ProjectFile` whose fields are
    `Spanned`. Two corrections to what this entry used to claim: `toml`
    re-exports `Spanned` itself, so **no new dependency**, and `Spanned` works
    in key position, so a `[deps]` key has a span of its own. Only the project
    file is typed — no semantic finding anchors in a manifest, which dissolves
    its two incompatible layouts and dodges `serde(flatten)`/`untagged`, both
    of which buffer through serde's `Content` and silently drop the span hook.
  - Unknown keys are ignored rather than denied, unlike `fatou.toml`: this is
    Julia's schema, and `resolve`'s callers swallow `Err`, so rejecting a
    future Julia key would surface as a silently missing index.
  - Delivery took a **new** `Outbound::EnvironmentDiagnostics` variant and its
    own `env_diags` map, not the `ProjectDiagnostics` channel this entry
    originally named. Two pieces of that channel's handling — the pull
    suppression for an open document, and the `didOpen` clear — are correct
    only because `pull_diagnostics::graph_diagnostics_for` re-supplies them,
    and these can have no pull twin: a pull report is served only for an open
    document, and an open `Project.toml` has no Julia analysis to carry them.
    Sharing the map would silently drop every finding for a client that opens
    the file.
  - The gates are the substance, and each has a test for staying quiet as well
    as one for firing. The package gate (`name` *and* `uuid`) is load-bearing:
    a bare environment legitimately has deps and no compat, and this repo's own
    `Project.toml` is that shape — `this_repos_own_environment_is_clean` pins
    it against the real committed pair. `missing-compat` needs to know which
    deps are standard libraries, which neither the manifest nor a located
    install answers alone, so it stays silent for any name neither can
    classify.
  - Landed alongside: environment files are no longer fed to the Julia parse
    (`send_analysis` and `on_document_diagnostic`), which stage 1 made newly
    reachable, and `dev_package` split so `entry_file` names the entry file a
    package *would* have.

- [x] **Stage 2: buffer for `[deps]`, disk for everything else.** The project
  file was a *push* input: the harvester read disk and called
  `set_declared_deps` at HIGH durability. It is now an ordinary `SourceFile`
  keyed by path, with `project_declared_deps` deriving `[deps]` from its text —
  so only the name → input mapping (`ProjectFiles`) stays HIGH durability, and
  the file itself sits where every editable buffer sits.
  - The prize landed: edit `[deps]` with no save, and `unresolved-import`
    updates across the package (`an_unsaved_project_file_edit_clears_unresolved_import`).
  - **The line drawn.** The buffer is authoritative for `[deps]` alone.
    Everything that needs a resolved `Environment` — all six semantic project
    file checks, `toml-syntax`, the library itself — stays on the harvester's
    save/watch cadence and reads disk, because `environment::resolve` is
    disk-coupled throughout and runs on a thread with no view of the document
    map. Live *diagnostics* for the buffer belong with stage 3, where the TOML
    file becomes a real document with a route of its own; a manifest buffer is
    likewise not read, since nothing consumes one without a resolve.
  - **The other half of that line**: the buffer is authoritative only while a
    buffer exists. `set_project_files` seeds create-or-return so a re-resolve
    cannot clobber an open file, which is exactly why a re-resolve does not
    refresh a *closed* one either — the watched-file sync does, reverting an
    environment file with no open document just as it does a `.jl` source
    (`a_watched_project_file_change_clears_unresolved_import`). Without it a
    `pkg> add` from a terminal never reaches the lint.
  - The guard was written first, the mirror of the body-edit firewall: a
    `Project.toml` edit must reach the declared deps and re-parse no Julia,
    while an edit leaving `[deps]` alone backdates.
  - `ProjectFile::declared_dep_names` became the one definition of "declared",
    shared with the CLI's `HarvestedLibrary::declared_deps`, so a name whose
    UUID does not parse is declared for both. Landed alongside: a refresh nudge
    now re-analyzes a *push* client's open documents instead of being pull-only
    — the same gap meant a re-harvest never refreshed `unresolved-import` for
    such a client either.

- [x] **Stage 3: make it a real document.** `Document` now carries a
  `DocumentKind` (`Julia`/`Project`/`Manifest`), tagged once at `didOpen`, and
  `send_analysis`'s file-name fork became `route_document`: a real two-route
  dispatch on the tag, with the TOML route re-deriving the file's own findings
  from the buffer.
  - **Live project-file diagnostics landed**, the half stage 2 left on disk:
    `syntax_findings` serves the buffer at edit cadence, so a broken
    `Project.toml` reports on the keystroke, with no save and no harvester (the
    end-to-end test opens no workspace folder at all).
  - **The two cadences.** The buffer's set (`buffer_diags`) **supersedes** the
    harvester's (`env_diags`) rather than joining it: computed from different
    texts, their union is a report of neither, and a buffer that does not parse
    means the harvester's set — its own syntax verdict, or semantic findings
    whose ranges index the disk text — describes a file the user has moved past.
    A buffer that parses contributes nothing, which is what keeps the semantic
    checks visible in an open file. Bookkeeping stayed simple as a result: a
    publish goes out only when there is something to say or to take back, and
    the live entry is dropped on close. The one accepted seam is that fixing a
    syntax error without saving lets the disk verdict reappear until the save.
  - **The gate the client selector newly needed**: with environment files in the
    selector, every request can arrive for a `Project.toml`, so language
    features go through `julia_text` and answer `null` for one — a format or a
    hover would otherwise parse TOML as Julia. Pull diagnostics still report
    nothing for these documents: both producers push, so answering the pull too
    would double them up.
  - Client side, the selector gained pattern-only entries (never `language:
    "toml"`, which exists only with a TOML extension installed), including
    `**/Manifest-v*.toml` to match `is_manifest_file` and the watcher globs.

- [x] **Stage 4a: navigation.** Go-to-definition, hover, document links, and
  inlay hints on a dependency name, in `src/lsp/project_navigation.rs`.
  `project_files.rs` grew `dep_entries`/`dep_at`, the same spanned schema read
  the other way round: not what is wrong with the file but what it names, and
  where.
  - **The door.** `GlobalState::project_text` is the sibling of `julia_text`
    and the only other way into the read pool; every other handler still
    answers `null` for an environment file. A `Manifest.toml` answers nothing
    at all, so the route is per-*kind*, not per-file-is-TOML.
  - **No `compute_*`/`*_via_db` split**, unlike every Julia feature. Those
    exist to serve a cached parse tree and fall back when the tracked input
    lags the buffer; here the parse is a TOML parse of the buffer itself, so
    there is nothing to be stale against. Only the `salsa::Cancelled` guard
    remains.
  - All three tables answer (`[deps]`, `[weakdeps]`, `[extras]`) — each names a
    real package. `[compat]` does not: its keys may name `julia`.
  - Hover needed the one piece of new plumbing: `Package` lived only on the
    harvester thread, so `LibraryDeps` carries version and kind into the
    database. It is set apart from `set_library`'s three maps because it is
    keyed by what the *manifest pinned*, not by what the harvest indexed — a
    package whose source was never found has an entry in one and not the other,
    and "installed, 0.4.5, source not found" is what a reader wants told.
  - **Inlay hints** show each dependency's resolved version after its UUID.
    Not a duplicate of hover, which is the on-demand one-at-a-time answer: the
    `[deps]` table is almost entirely UUID, and the version lives in the
    `Manifest.toml` next door, which nobody opens. Version only — the kind
    belongs to "tell me about *this* dependency", and repeating "Registered
    package" down the table is noise. This is fatou's **first** inlay hint
    feature, so it took the capability, and a `provideInlayHints` entry in the
    extension's `languageFeatures` gate (which is keyed by request kind, and is
    why the other three needed no client change). A Julia document answers an
    empty list, not `null`: inlay *type* hints would need inference, which is
    out by tenet, but "this file has no hints" is the honest answer.
  - **And a refresh nudge**, which the feature does not work without: the
    versions come from the harvest, which lands seconds after `initialize`,
    while a client re-requests hints only on an edit or a scroll. A full
    harvest therefore sends `workspace/inlayHint/refresh`, capability-gated the
    way `workspace/diagnostic/refresh` is, and its own signal rather than a
    share of `DiagnosticsRefresh` — that one also fires per project-file
    keystroke, and the spec has this request force a *global* recalculation.
    Document links have the same staleness and no cure: the spec defines no
    refresh request for them.
  - Deliberately not hinted: `[compat]`. A declared-range-versus-resolved hint
    there is interesting but overlaps `missing-compat`/`unknown-compat`, and it
    is a second rendering path deserving its own decision.
  - Landed alongside: `site_locations` now normalizes, which fixes the Julia
    jump into a `dev`'d package too. Such a package's root is the manifest's
    `path` joined to the project directory, so it keeps that entry's `../`
    spelling — the filesystem resolves it and a URI does not, and a client
    comparing URIs textually opens a second tab onto a file it already has.

- [x] **Stage 4b, first half: document links on manifest `path` entries.** The
  one feature a manifest answers, and the only thing in one that anchors
  anywhere — `path` is the sole entry field naming something outside the file.
  - **The spans, which were the cost.** A manifest is parsed against a plain
    table (its 1.0 and 2.0 layouts differ) and a `toml::Value` carries none, so
    `manifest_path_entries` adds a typed schema over the one field that needs
    one. It is two parse attempts, as expected: one schema cannot cover both
    layouts, since `serde(flatten)`/`untagged` buffer through serde's `Content`
    and drop the span hook silently. 2.0 is tried first, and 1.0 only when it
    comes up empty — a 2.0 manifest's `julia_version` is a string where the 1.0
    schema wants an array of entries.
  - **The target is the root's project file**, not the root: a `dev`'d root is a
    package, what identifies a package is its `Project.toml`, and a client
    cannot open a directory URI. A root with neither spelling is linked as-is —
    that it is not a package is worth walking into, and the `path` may be a typo.
  - **No library, no database.** The path resolves through `resolve_dev_path`,
    the same join the harvest makes, so the link and the environment cannot
    disagree about where the package is; `normalize_path` collapses the `../`
    for the reason stage 4a's jump targets do. That makes it the one project-file
    feature with no snapshot in its handler, and `manifest_text` the third and
    narrowest door into the read pool.
  - Landed alongside: `uri::anchor_dir`, one definition of "the directory a
    relative path in this document resolves against", shared with the Julia
    document's `include` links.

- [ ] **Stage 4b: the rest**, deferred with stage 4a's route already in place.
  - Completion of dependency names, the expensive one. On a default depot the
    registry is a `General.tar.gz`, so the full version needs gzip and tar to
    reach `Registry.toml`. Scope the first pass to packages already installed in
    the depot: no new dependency, no network. Note that *nothing* enumerates
    `<depot>/packages` today — it is only ever probed by exact slug — so even
    the cheap pass is new code.
  - `name`/`uuid` upkeep on a rename, the edit half of the `willRenameFiles`
    entry above.

Deferred, and why:

- [ ] **`resolve` is all-or-nothing**, found while landing stage 1: a good
  `Project.toml` beside a corrupt `Manifest.toml` loses the *entire*
  environment, so there is no library and no `declared_deps`, and
  `unresolved-import` goes quiet across the whole package. Stage 1 makes that
  pairing conspicuous — it now reports the manifest's syntax error while
  completions and go-to-definition silently degrade. The fix is a partial
  resolve (keep the project half, drop `packages`), which is a real behavior
  change to `environment.rs` deserving its own commit and tests.
- [ ] An *unused dependency* check (a `[deps]` entry never `using`'d anywhere in
  the package), the inverse of `unresolved-import`. A different cost class from
  everything above: it needs a whole-package union of free reads, which is a new
  cross-file query and the likeliest thing to punch through the range-free
  projection firewall in `src/project.rs`.
- [ ] Only `Project.toml` carries semantic findings. A manifest is checked for
  syntax alone, which is deliberate (nothing anchors inside one), but a
  `[[deps.X]]` entry naming a package absent from every dependency's `deps`
  list would be a real finding if the shape ever earns one.
- A code action that *adds* a missing dependency can only ever be a plain TOML
  text edit. Resolving a name to its UUID means reading the registry, and
  shelling out to `Pkg` is off the table: no Julia runtime, at any point in the
  pipeline.

## Tooling
