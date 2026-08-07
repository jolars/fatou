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

- [x] `duplicate-keyword-argument` (correctness, syn, error, no fix): the same
  keyword supplied twice at a *call* site, `h(a = 1, a = 2)`. Julia rejects it
  at lowering (`syntax: keyword argument "a" repeated in call to "h"`), so it
  parses clean here and in JuliaSyntax and is always a bug. The exact call-side
  sibling of the existing definition-side `duplicate-argument`; positional and
  `;`-block keywords share one namespace, as they do there. Landed as a
  `CALL_EXPR` rule over `CallShape::keywords`, spanning the repeated name token
  only. A keyword splat contributes no name and so never fires on its own, but
  it does not silence the check either: `julia` rejects
  `h(a = 1; kw..., a = 2)` all the same. Definition signatures stay
  `duplicate-argument`'s (via `matchers::call_expr`), and quoted code and macro
  calls are exempt as they are for `call-arity` — neither is lowered as
  written.
- [x] `const-local` (correctness, syn, error, no fix): `const` on a local
  binding — `function k(); const z = 1; end` is `syntax: unsupported const
  declaration on local variable`. Landed as an ancestor walk from `CONST_STMT`
  (`break-outside-loop`'s shape), *not* the `ScopeKind` test this entry
  assumed: the model's `While`/`For` scope ranges cover the condition and the
  iterator spec, which evaluate in the *enclosing* scope, so a scope-kind
  lookup would false-positive on `while (const z = 1; false)`. The walk stops
  local on function/macro bodies (long and short form, default arguments
  included), `->`, do bodies, `let` bodies, `for`/`while` bodies, every part of
  a `try`, and comprehension/generator bodies; global on `module` and the file
  root; and legal on a `struct` body, where `const` is a mutable-struct field
  attribute. Quoted code and macro calls stay silent and win even past a local
  boundary, so the walk continues rather than reporting on the spot. A `let`'s
  binding list is conservatively exempt: the first binding is enclosing but the
  later ones are local, and missing those beats guessing. `global const` and
  `local const` are exempt too — each raises a *different* Julia error (see
  below). Short-form definitions go through the new
  `matchers::is_short_form_def`.
  (UnsupportedConstLocalVariable)
- [x] Follow-up from the above: the two neighboring `const` errors `const-local`
  deliberately leaves alone. Both landed (correctness, syn, error, no fix), and
  the three rules now share `correctness::const_decl` for reading the
  `global`/`local` modifier (either order — `global const x = 1` nests the
  `const` under the modifier, `const global x = 1` the other way round) and the
  quote/macro-call exemption.
  - `global-const-in-function`: as this entry assumed, an innermost-*function*
    test rather than `const-local`'s innermost-*local* one. The function
    boundaries are `function`/`macro`, `->`, do bodies, short-form definitions
    (default arguments included), and comprehension/generator bodies, which
    lower to closures; `let`, `for`, `while`, `try`, `begin`/`if`, `module`, and
    `struct` are *not* boundaries, so a soft scope nested in a function is still
    a finding. Iterator specs and a do-call's call part stay exempt, as in
    `const-local`. Verified case by case against Julia 1.12.
    (`global const` declaration not allowed inside function)
  - `local-const`, **not** `const-without-assignment`: half of that entry's
    premise was already covered. Bare `const z`, `const z::Int`, and `const x
    += 1` are rejected by JuliaSyntax at *parse* time, so
    `parser::core::flag_invalid_const_decls` already reports them under
    `parse-error` — and a file with a parse diagnostic never reaches the rules,
    so a lint rule for them would be dead code. `local const z = 1` (and `const
    local z = 1`) parses clean and fails only at lowering, which leaves it as
    the rule's whole subject; the id names that construct instead of Julia's
    misleading message. No scope test, as the entry said.
    (expected assignment after `const`)
- [x] Parser gap found while landing the above: an unparenthesized keyword
  statement as a comprehension or generator body swallowed the `for` clause as
  raw tokens instead of closing the body and opening a `FOR_BINDING`. `[const x
  = 1 for i in 1:1]` parsed as `COMPREHENSION > CONST_STMT` with the `FOR_KW`
  *inside* the `CONST_STMT`, where JuliaSyntax builds a generator; the
  parenthesized `[(const x = 1) for i in 1:1]` was already fine. Fixed:
  `KwStmt::ExprTuple` (`const`/`global`/`local`/`return`) now carries a
  `for_ends` flag, set in every non-statement position, that ends the statement
  at a same-line `for` (and at the whitespace before it, which the generator's
  layout owns). A nested keyword statement inherits the boundary through
  `parse_kw_stmt_operand`, so `[global const x = 1 for i in 1:1]` nests too.
  The operand position itself is *not* gated: a `for` directly after the keyword
  is its operand (`[return for i in 1:1]` ⇒ `(vect (return (for …)))`).
  `const-local` and `global-const-in-function` now report the right spans.
  Toplevel `const x = 1 for i in 1:1` still keeps the loose tokens rather than
  JuliaSyntax's `(error-t for …)` recovery — error shapes are a separate phase.
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
- [x] Follow-up from the above: `render.rs` rendered a fix's `help:` line
  identically whether it is `Safe` or `Unsafe`, so a reader of the rule
  reference could not tell that `missing-comparison`'s fix needs
  `--unsafe-fixes` (its `description()` says so in prose, which was the only
  reason the page was honest). Landed as a parenthetical on the help line:
  `(safe fix)` or `(unsafe fix, requires `--unsafe-fixes`)`. Marking *both*
  rather than only the unsafe case, since "no marker" is not something a reader
  can read as "safe". Only the four fix-carrying rule snapshots moved.
- [x] Same vein, borrowed from `panache`: every finding now ends with `= help:
  for further information visit https://fatou.dev/reference/rules.html#<id>`.
  The anchor is mdBook's slug for the `## `<id>`` heading `render_rule_doc`
  writes, so `every_rule_is_documented` is what keeps the link from 404ing.
  Gated on `rules::is_shipped_rule` (a `LazyLock` set over `all_rule_ids`, since
  rebuilding the boxed registry per diagnostic is not free): the `parse-error`
  pseudo-rule rides the same channel without a reference section. Rendering
  grew a `RenderOptions` struct rather than a fourth positional argument,
  because `docs.rs` needs `without_rule_links` — the reference page would
  otherwise link every example to the page it is printed on. The LSP was
  missing the equivalent entirely: `finding_to_lsp` now sets
  `code_description`, so clients linkify the rule code.
- [x] `unreachable-code` (correctness, syn, warning, no fix): statements after
  an unconditional `return`/`throw`/`error`/`rethrow` in the same block. Landed
  as a whole-file pass over `RuleContext::control_flow()`, reporting the *head
  statement* of each unreachable block rather than every dead statement — a
  dead run has one cause. Iterating the region graphs directly (rather than
  asking `FileControlFlow::is_unreachable` per statement) is what gives that
  per-block granularity, and it picks up the nested cases for free (`if a;
  return; else; return; end; dead()`, `while true` with no `break`, a `@label`
  nothing jumps to). Namespace confirmation is *region-wide*, not per
  divergence: the graph never says which divergence killed a given block, so a
  region holding any `throw`/`error`/`rethrow` call that `resolves_to_base`
  cannot confirm is skipped entirely. A region with no throw-like call at all
  is unaffected, since `return` is matched by node kind and cannot be shadowed.
  (arity `unreachable-code`)
- [x] `duplicate-include` (correctness, sem, warning, no fix): the same file
  `include`d twice, which silently re-runs its definitions. A third
  `IncludeProblemKind` beside `Missing`/`Cycle` in
  `src/linter/include_graph.rs`; the graph already has the edges. Landed as a
  whole-file pass flagging the second and later `include`s: repetition is keyed
  on the *resolved* target (so `"a.jl"` and `"./a.jl"` match) paired with the
  call's host module (so the same file included into two `module` blocks is
  not a repeat), and a diamond stays out as it does for `include-cycle`.
  `Duplicate` is reported *beside* `Missing`/`Cycle` rather than instead of
  them — repetition is a property of the file's own text — which is also why
  `IncludeProblem` gained an `edge` index: matching problems back by the raw
  literal cannot tell one repeat of a literal from another.
  (DuplicateInclude)
- [x] `duplicate-method` (correctness, sem, warning, no fix): two method
  definitions with identical signatures in one file — the second silently
  overwrites the first, so one of them is dead. Not in StaticLint's catalog;
  from arity's planned `duplicated-function-definition`. Landed as a whole-file
  pass over `harvest_tree`, the same lowered-`TypeExpr` method table
  `call-arity` reads, so the two agree on what a file defines. The key is what
  dispatch sees: module, `(name, owner)` group, positional parameter types
  (unannotated is `Any`) with vararg flags, and the `where` specs. Argument
  names, defaults, keyword arguments, and the return type are deliberately
  *out* of the key — Julia dispatches on none of them, so `f(x::Int; a = 1)`
  really is replaced by `f(x::Int; b = 2)`. `where` bounds *are* in the key,
  and the comparison is structural rather than alpha-converting, so a renamed
  type variable reads as a difference (a miss, never a false positive).
  `@static`-disjoint branches stay out for free — the harvest walk enters
  neither a conditional's branches nor a function body — and definitions under
  a macro call are skipped outright, since a macro may rewrite the signature it
  is handed. Only the second and later definitions are reported.
- [ ] `loop-variable-shadow` (suspicious, sem, warning, no fix): a nested `for`
  reusing the enclosing loop's index variable (`for i in a; for i in b`), and
  assigning to the loop variable inside its own body. Both are near-always bugs
  and both are pure scope questions the model already answers. From arity's
  planned `for-loop-index`/`for-loop-dup-index`.

Ready once `resolves_to_base` lands (idiom rewrites; categories per the decision
above):

- [ ] `typeof-comparison` (suspicious, syn + res, warning, unsafe fix):
  `typeof(x) == Int` should be `x isa Int` — the `==` form silently misses
  subtypes, which is a likely bug rather than an idiom, so this one stays
  `suspicious`. Also `typeof(x) != T` and the mirrored literal-first spellings.
  Unsafe because a caller may genuinely want exact-type identity. (arity
  `class-equals`)
- [ ] `length-zero` (readability, syn + res, warning, safe fix): `length(x) ==
  0` is `isempty(x)`, `length(x) > 0` is `!isempty(x)`; the `>= 1`/`!= 0`/
  `<= 0`/`< 1` spellings collapse the same way. **First `readability` rule —
  creates `src/linter/rules/readability.rs` and its directory.** (arity
  `nzchar`)
- [ ] `redundant-boolean` (readability, syn, warning, safe fix): comparing to or
  branching on a boolean literal — `x == true` is `x`, `x == false` is `!x`, `c
  ? true : false` is `c`, `c ? false : true` is `!c`. Distinct from
  `constant-condition`, which owns the literal-*as*-test case. (arity
  `redundant-equals`, `redundant-ifelse`)
- [ ] `comparison-negation` (readability, syn, warning, safe fix): `!(a == b)`
  is `a != b`, `!(x < y)` is `x >= y`. Safer in Julia than in R: `!=` is
  *defined* as `!(==)`, so the rewrite is exactly equivalent by construction.
  Watch the broadcast forms (`.==`, `.!`), which are values rather than a
  scalar test and must not fire. (arity `comparison-negation`)

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

- [ ] The eager-broadcast family (performance) — `any(f.(x))` -> `any(f, x)`,
  `all(f.(x))` -> `all(f, x)`, `sum(f.(x))` -> `sum(f, x)`, `sort(x)[1]` ->
  `minimum(x)`, `length(findall(p, x))` -> `count(p, x)` — all of which avoid
  materializing an intermediate array. This has no StaticLint or arity
  counterpart (it is the Julia analogue of arity's `performance` category) and
  is arguably higher-value than anything ported directly. The category question
  is settled and `matchers` has landed, so what remains is scheduling: whichever
  of these lands first creates `src/linter/rules/performance.rs` and its
  directory.
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

## Tooling
