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

- [ ] Reparse follow-ups left over from the stage 2-4 review. None is a
  soundness issue: every one of them degrades to a full parse at worst.
  - The base cache admits every file `parsed_document` touches, not just
    the buffers the editor is on, so one `project_graph` /
    `workspace_reference_index` sweep over more than `MAX_REPARSE_BASES`
    members evicts every open buffer's base at once and the next keystroke
    full-parses. Admitting only files that carry a staged chain (or an open
    buffer) would fix it, but it also stops the CLI and the disk-revert
    path from ever building a base, so it is a policy call rather than a
    cleanup.
  - `crate::parser` re-exports `Edit`, `apply_edits`, `try_apply_edits`,
    and `diff_edit`, all of which `crate::text` already exports, plus
    `fingerprint`, which exists only for the oracle. Pick one canonical
    path per item and `#[doc(hidden)]` what is left.
  - `REGION_MAX_FRACTION` is used as a divisor (`text_len / 4`), so the
    name reads backwards.
  - `crates/fatou-parser/tests/incremental_reparse.rs` is now the slowest test binary (~23 s in
    debug): every successful splice pays the in-crate Tenet-4 full parse on
    top of the harness's own comparison. Lower `EDITS_PER_SNIPPET`, or put
    the corpus sweep behind a feature, if CI time starts to matter.
  - The criterion dev-dependency adds 23 crates, `cc` and `alloca` among
    them, so `cargo test --all-targets` and `cargo clippy --all-targets`
    now want a C toolchain.

- [ ] Maybe (deferred): a nested-block tier needs a context-parameterized
  fragment entry point (`public_context`, bracket `end` markers) — a bare
  fragment `parse()` misparses those today. A pure optimization on top of a
  sound stage 2–4.

## Formatter

## Linter

### Rules

- [x] `index-from-length` (suspicious, syn, opinionated, warning, no fix):
  flags `for i in 1:length(x)`/`1:size(x, d)` when `i` indexes `x` (suggest
  `eachindex`/`axes`) and iterating a bare numeric literal (`for i in 3.5`).
  Name-based match on `length`/`size`; no type info to exempt `Vector`/`Array`,
  so gated on the loop var actually indexing the collection. On by default.
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
- [ ] Suppression-map refactor, prerequisite for the meta-rules below (arity's
  "§I6"): have `suppression.rs` expose the parsed directive list (rule ID,
  range, has-reason, raw text) on `RuleContext`, and have the driver
  (`check.rs`) record which suppressions actually matched a diagnostic.
  `outdated-suppression` needs that last part as a post-pass; it is not a
  per-rule concern.
- [ ] Decision, not a task: several candidates below are idiom rewrites rather
  than likely-bug reports, so they want a `performance` and/or `readability`
  category beside `correctness`/`suspicious` (arity ships both). Settle the
  category question before landing the first one, since the rule ID's docs path
  bakes it in.

### Rule roadmap

Candidates probed from StaticLint.jl's check catalog (the vendored copy in
JuliaWorkspaces.jl, whose only extra code is `UnresolvedImport`) and from
arity's R rule set (`../arity`, rule ID in parentheses where it is the source).
StaticLint `LintCodes` names in parentheses too. Each entry carries category and
cost tier: `syn` = CST + typed AST wrappers only, `sem` = needs the
`SemanticModel`, `res` = needs a `ResolutionContext`. To land one, use the
`add-lint-rule` skill (`.claude/skills/add-lint-rule/`).

Every behavioral claim below was checked against `julia` 1.12.6, and each
candidate was confirmed silent on today's linter — none is a mis-mapping of an
existing rule.

Ready now (no new infrastructure):

- [ ] `duplicate-keyword-argument` (correctness, syn, error, no fix): the same
  keyword supplied twice at a *call* site, `h(a = 1, a = 2)`. Julia rejects it
  at lowering (`syntax: keyword argument "a" repeated in call to "h"`), so it
  parses clean here and in JuliaSyntax and is always a bug. The exact call-side
  sibling of the existing definition-side `duplicate-argument`; positional and
  `;`-block keywords share one namespace, as they do there. Splatted keywords
  (`f(; kw...)`) are unknowable and must not fire.
- [ ] `const-local` (correctness, syn, error, no fix): `const` on a local
  binding — `function k(); const z = 1; end` is `syntax: unsupported const
  declaration on local variable`. Pure syntax plus a scope-kind test
  (`ScopeKind` already distinguishes local from module/top level). Cheap and
  unambiguous.
  (UnsupportedConstLocalVariable)
- [x] `missing-comparison` (suspicious, syn, warning, **unsafe** fix): `x ==
  missing` / `x != missing`, which is always `missing` and raises `TypeError`
  in a boolean context. Landed matching `nothing-comparison`'s shape rules
  (bare `BINARY_EXPR` only, so chains and the broadcast `.==`/`.!=` are out;
  `missing` matched by identifier text on either side; the `Missing` type left
  alone). The fix rewrites `==` -> `===` / `!=` -> `!==` but is `Unsafe`, not
  `Safe` as this entry first assumed: `x == nothing` dispatches `Base.==`,
  which agrees with `===` for the singleton, whereas `x == missing` evaluates
  to `missing` and the rewrite makes it a `Bool`. That change is the intent,
  but it is still a change. **First `Applicability::Unsafe` fix in the tree.**
  (arity `equals-na`)
- [ ] Follow-up from the above: `render.rs` renders a fix's `help:` line
  identically whether it is `Safe` or `Unsafe`, so a reader of the rule
  reference cannot tell that `missing-comparison`'s fix needs `--unsafe-fixes`
  (its `description()` says so in prose, which is the only reason the page is
  honest today). Mark applicability in the rendered help line — it touches
  every rule snapshot, so it wants its own change.
- [ ] `unreachable-code` (correctness, syn, warning, no fix): statements after
  an unconditional `return`/`throw`/`error`/`rethrow` in the same block. The CFG
  above has landed, so this can go straight to `RuleContext::control_flow()` and
  `FileControlFlow::is_unreachable` rather than the shallow "terminator is a
  direct statement of a block" shape, which also picks up the nested cases
  (`if a; return; else; return; end; dead()`). Still confirm the callee with
  `resolves_to_base` before reporting a `throw`/`error`/`rethrow` divergence —
  the graph matches those by name. (arity `unreachable-code`)
- [ ] `duplicate-include` (correctness, sem, warning, no fix): the same file
  `include`d twice, which silently re-runs its definitions. A third
  `IncludeProblemKind` beside `Missing`/`Cycle` in
  `src/linter/include_graph.rs`; the graph already has the edges.
  (DuplicateInclude)
- [ ] `duplicate-method` (correctness, sem, warning, no fix): two method
  definitions with identical signatures in one file — the second silently
  overwrites the first, so one of them is dead. Not in StaticLint's catalog;
  from arity's planned `duplicated-function-definition`. Compare lowered
  `TypeExpr` signatures, and be careful that differing `where` bounds, differing
  argument *names*, and `@static`-disjoint branches are all legitimate.
- [ ] `loop-variable-shadow` (suspicious, sem, warning, no fix): a nested `for`
  reusing the enclosing loop's index variable (`for i in a; for i in b`), and
  assigning to the loop variable inside its own body. Both are near-always bugs
  and both are pure scope questions the model already answers. From arity's
  planned `for-loop-index`/`for-loop-dup-index`.

Ready once `resolves_to_base` lands (idiom rewrites; see the category decision
above):

- [ ] `typeof-comparison` (syn + res, warning, unsafe fix): `typeof(x) == Int`
  should be `x isa Int` — the `==` form silently misses subtypes. Also
  `typeof(x) != T` and the mirrored literal-first spellings. Unsafe because a
  caller may genuinely want exact-type identity. (arity `class-equals`)
- [ ] `length-zero` (syn + res, warning, safe fix): `length(x) == 0` is
  `isempty(x)`, `length(x) > 0` is `!isempty(x)`; the `>= 1`/`!= 0`/`<= 0`/`< 1`
  spellings collapse the same way. (arity `nzchar`)
- [ ] `redundant-boolean` (syn, warning, safe fix): comparing to or branching on
  a boolean literal — `x == true` is `x`, `x == false` is `!x`, `c ? true :
  false` is `c`, `c ? false : true` is `!c`. Distinct from
  `constant-condition`, which owns the literal-*as*-test case. (arity
  `redundant-equals`, `redundant-ifelse`)
- [ ] `comparison-negation` (syn, warning, safe fix): `!(a == b)` is `a != b`,
  `!(x < y)` is `x >= y`. Safer in Julia than in R: `!=` is *defined* as
  `!(==)`, so the rewrite is exactly equivalent by construction. Watch the
  broadcast forms (`.==`, `.!`), which are values rather than a scalar test and
  must not fire. (arity `comparison-negation`)

Needs modest new infrastructure:

- [ ] `unresolved-import` (correctness, res, warning, no fix): `using Foo` /
  `import Foo` where `Foo` is neither a stdlib nor in the project's
  `Project.toml` `[deps]`. We are better placed for this than StaticLint — the
  `Project.toml` parse and `PackageIndex` already exist. Gate on having a real
  workspace, like `undefined-name`, and stay silent otherwise.
  (UnresolvedImport)
- [ ] `kwarg-default-mismatch` (correctness, syn + `TypeExpr` + res, error, no
  fix): a keyword argument whose literal default cannot match its declared type,
  `g(; y::Int = 1.0)`. Verified: this is **not** an implicit `convert` — a
  keyword's `::T` becomes a dispatch constraint on the lowered inner function,
  so `g()` raises a `MethodError` unconditionally. (I assumed the opposite at
  first; the check is sound.) Restrict to a literal RHS plus an annotation that
  resolves to a concrete `Core` type (`String`, `Symbol`, `Bool`, `Char`, the
  sized ints, `Float32`/`Float64`); `index/typeexpr.rs` already lowers the
  annotation. (KwDefaultMismatch)
- [ ] `function-has-no-methods` (correctness, res, warning, no fix): calling `f`
  where every visible definition is a bare forward declaration
  (`function f end`), so the call can only raise a `MethodError`. Natural
  extension of `call-arity`'s callee resolution; gate on workspace resolution,
  since an external package may add the method. (FunctionHasNoMethods)
- [ ] `shadowed-base-name` (suspicious, sem + res, warning, no fix): binding a
  name exported by Base and then using it in call position. Stronger in Julia
  than the R rule it comes from — Julia has one namespace and no call-position
  fallback, so `length = 3; length(x)` is a hard error, where R's equivalent is
  benign. Require both a binding and a later call, as arity does. (arity
  `shadowed-builtin`)
- [ ] `non-public-access` (suspicious, sem + res, warning, no fix): reading
  `Foo.bar` where `bar` is neither exported nor declared `public` by `Foo` — the
  Julia analogue of arity's planned `internal-function` (`pkg:::fn`), and a
  better-defined question here than in R, since 1.11's `public` keyword makes
  "intended API" an explicit declaration rather than a convention. The model
  already has `exports()` and `qualified_reads()`; the missing piece is reading
  `public` declarations out of a resolved package. Name it for the `public`
  keyword if `non-public-access` reads awkwardly.
- [ ] The suppression meta-rule family, blocked on the §I6 refactor above and
  ported wholesale from arity's Phase 4 — this is entirely language-independent
  and applies to `# fatou-ignore` exactly as it does to `# arity-ignore`:
  `misnamed-suppression` (names a rule ID not in `all_rule_ids()`; safe fix when
  there is an unambiguous near-match), `blanket-suppression` (no rule ID at
  all), `unexplained-suppression` (no reason given; default-off), and
  `outdated-suppression` (suppressed a diagnostic that no longer fires;
  safe-delete fix). Cheap, high signal, and they keep the suppression comments
  honest as the rule set moves under them.
- [ ] `invalid-type-declaration` (correctness, sem + res, warning, no fix):
  `f(x::g)` where `g` resolves to a function rather than a type. Needs
  "this binding is a function, not a type", which binding kinds answer for
  same-file and workspace names but not for arbitrary imports — so it is
  resolution-gated and partial. (InvalidTypeDeclaration)

Deferred, and why:

- [ ] The eager-broadcast performance family — `any(f.(x))` -> `any(f, x)`,
  `all(f.(x))` -> `all(f, x)`, `sum(f.(x))` -> `sum(f, x)`, `sort(x)[1]` ->
  `minimum(x)`, `length(findall(p, x))` -> `count(p, x)` — all of which avoid
  materializing an intermediate array. This has no StaticLint or arity
  counterpart (it is the Julia analogue of arity's `performance` category) and
  is arguably higher-value than anything ported directly, but it wants the
  matcher module and the category decision first.
- [ ] `occursin`-with-a-regex-literal rules: `occursin(r"abc", s)` ->
  `occursin("abc", s)` when the pattern has no metacharacter, and
  `occursin(r"^abc", s)` -> `startswith(s, "abc")`. Blocked on a Julia
  regex-literal analyzer (arity has `src/linter/regex.rs` for exactly this).
  (arity `fixed-regex`, `string-boundary`)
- [ ] `unnecessary-nesting`: `if a; if b; body; end; end` -> `if a && b; body;
  end`, when neither `if` has an `else`. Readability category; low risk, low
  urgency. (arity `unnecessary-nesting`)
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
  rule.
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

## Tooling
