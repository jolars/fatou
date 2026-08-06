# TODOs

## Parser

- [ ] Lex the *broadcast* wrapping arithmetic operators `.+% .-% .*%` (and their
  augmented forms `.+%= .-%= .*%=`) in `src/parser/lexer.rs`. The undotted
  `+% -% *%` are supported; the dotted forms still split into `.+` + `%`, which
  mis-parses rather than erroring. No code in the smoke-test corpus uses them
  (only JuliaSyntax's own tests do). Deferred: the whole wrapping-operator family
  is unreleased in JuliaSyntax — the latest release (1.0.2) still rejects every
  form (`lex_plus`/`lex_minus`/`lex_star` have no `%`-suffix handling, verified
  2026-08-03), so no oracle bump can pin these until JuliaSyntax ships them.
  Implementing the lexer change now would be validatable only against a
  hand-authored parser fixture, not the differential oracle.

- [ ] Two error-recovery gaps left over from labeled `break`/`continue`
  (`src/parser/structural.rs`). Junk after a complete labeled keyword drops
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
  - `tests/incremental_reparse.rs` is now the slowest test binary (~23 s in
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

### Rule infrastructure

Shared machinery the roadmap below leans on. arity has all three; we have none.

- [ ] `RuleContext::resolves_to_base(&CallExpr) -> bool` plus a token-level
  `read_resolves_to_base`, after arity's `src/linter/rules.rs:177,215`: one call
  that confirms a callee really is the Base/Core function and not a local
  shadow, a namespace-qualified name, or a `using`-masked import. Today
  `call-arity` and `type-piracy` each hand-roll
  `Resolver::new(...).with_workspace(...)`. Every idiom rule below opens with
  this question, so **land the helper before writing any of them** or the
  boilerplate gets copied a dozen times. Highest-leverage item here.
- [ ] A shared call-shape matcher module (arity's
  `src/linter/rules/matchers.rs`): "call to *name* with exactly *n* positional
  arguments and no named ones" is the opening line of most idiom rules.
- [ ] Maybe: a per-region control-flow graph (arity's `src/semantic/cfg.rs`,
  ~590 lines: basic blocks, a `Goto`/`Branch`/`Return`/`Diverge`/`Unreachable`
  terminator enum, memoized per file, built by structured recursive descent
  with no fixpoint). Note that `unreachable-code` does **not** need it — arity's
  own rule fires only on the shallow "terminator is a direct statement of a
  block" shape. The CFG buys the nested cases (`if a; return; else; return;
  end; dead()`) and future definite-assignment work. Julia's terminator set is
  richer than R's (`return`, `throw`/`error`/`rethrow`, `break`/`continue`, and
  `@goto`/`@label`), and `@goto` is the one shape no structured descent
  handles — that, not `unreachable-code`, is the case that justifies the cost.
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
- [ ] `missing-comparison` (suspicious, syn, warning, safe fix): `x == missing`
  / `x != missing`. `1 == missing` is `missing`, never `true`/`false`, and
  `if 1 == missing` raises `TypeError: non-boolean (Missing) used in boolean
  context` — so the comparison is either dead or an error. Suggest `ismissing`;
  safe fix `==` -> `===` and `!=` -> `!==`, mirroring `nothing-comparison`
  exactly. The best single import from arity: same shape, same fix machinery,
  and a genuine error rather than a style nit. (arity `equals-na`)
- [ ] `unreachable-code` (correctness, syn, warning, no fix): statements after
  an unconditional `return`/`throw`/`error`/`rethrow` in the same block. Fire
  only on the unambiguous shape — a terminator that is a *direct* statement of a
  block with at least one statement after it — which needs no CFG (see above).
  A terminator nested inside an `if` leaves the tail reachable. (arity
  `unreachable-code`)
- [ ] `duplicate-include` (correctness, sem, warning, no fix): the same file
  `include`d twice, which silently re-runs its definitions. A third
  `IncludeProblemKind` beside `Missing`/`Cycle` in
  `src/linter/include_graph.rs`; the graph already has the edges.
  (DuplicateInclude)

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
