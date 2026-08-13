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

### Hygiene

Deferred from the parser-crate refactor that split `expr/{array,macros,
juxtapose}.rs` out of `expr.rs` and added the kind-checked `events::finish`.

- [ ] Do not split `lexer.rs` (2144 lines) ahead of the ladder rewrite above.
  Every function is a method on one `Lexer` inside a single `impl`, so a
  `strings.rs`/`operators.rs` cut needs all five struct fields plus
  `peek`/`push`/`push_op`/`char_at` to become `pub(super)` — the whole mutable
  state exposed to siblings. The file is only ~1600 lines of code (1607+ is
  tests), and the ladder is the biggest chunk *and* the part slated to change
  shape. Split it as part of that rewrite, not before.

- [ ] `expr.rs` is 4013 lines after the three splits. The remaining candidate is
  a `prec.rs` for the binding-power constants, `infix_binding_power`,
  `next_operator`, and the `is_*_op` predicates, but they are scattered across
  the file rather than contiguous, so extracting them means reordering — which
  stops it being a pure move and makes the diff unreviewable against the
  fixtures. Worth doing only alongside a change that touches those tables anyway.

- [ ] `sexpr.rs` is 3350 lines with 122 free functions and 13 existing
  `// --- Section ---` markers that are already viable module boundaries.
  Deferred: it is the test-only oracle projector, its `project_*` helpers are
  mutually recursive with one dispatch function, and `parser-parity` edits it
  constantly.

- [ ] `DiagnosticKind::InvalidAsAlias` (`parser/diagnostics.rs`) is never
  constructed — its only two occurrences are its own declaration and its
  `stream()` arm. Either wire up the `using A as B` diagnostic its doc
  describes (that path currently emits a bare `ERROR` node with no diagnostic)
  or drop the variant.

### Incremental

- [ ] Maybe (deferred): a nested-block tier needs a context-parameterized
  fragment entry point (`public_context`, bracket `end` markers) — a bare
  fragment `parse()` misparses those today. A pure optimization on top of a
  sound stage 2–4.

## Formatter

- [ ] **`--check`'s line diff is still quadratic in the number of changed
  lines**, and it dominates the run: 93% of it before the algorithm switch, and
  still most of it after. `line_diff` (`src/formatter/check.rs`) moved from
  `similar`'s default Myers to `Algorithm::Patience`, which is ~3.2x cheaper
  throughout — 42 / 145 / 504 ms becomes 19 / 54 / 171 ms at 4000 / 8000 /
  16 000 changed lines — but that is a **constant factor, not a complexity
  fix**. On Julia's line population all three candidates still grow ~4x per
  doubling on a reindent-everything change (Myers 3.81x, Patience 3.71x,
  Histogram 4.08x at 500 → 1000 near-identical small functions), which is why
  `check.rs` carries a reconstruction test but deliberately no growth-rate one:
  a ratio bound would separate none of them.

  Why Julia is the hard case here: Patience anchors on lines *unique to both
  sides*, and reindented Julia has few — `end`, `else`, bare `)`, and short
  assignments repeat heavily, so long stretches fall through to the Myers
  fallback. badness does not have this problem (its diff measures linear, 2.0x
  per doubling), so the fix is not portable from there.

  Two directions if it becomes worth it. Trim the common prefix and suffix
  *before* handing the texts to `similar` — its disjoint fast path exists but
  never fires, because it runs before trimming and bails as soon as the two
  sides share a first line, which they always do. Or bound the work
  deterministically and fall back to a whole-file replace hunk, which is what a
  16 000-line all-changed diff amounts to for a reader anyway. Do **not** reach
  for `TextDiffConfig::timeout`: a wall-clock deadline makes `--check` output
  nondeterministic between runs.

  `Algorithm::Histogram` is a trap *here* and the comment on `line_diff` says
  so. It is the fastest on real Julia (136 ms against Patience's 234 ms over
  16 000 corpus lines) but collapses on self-similar input — 430.9 ms against
  Patience's 37.1 ms at 500 near-identical functions, i.e. worse than the Myers
  it would replace. `--check` runs over whatever is in the tree, generated code
  included.

  **The algorithm is a per-language choice; do not port a sibling's.** badness
  (LaTeX) wants Histogram outright — 643 ms against Patience's 919 ms and
  Myers' 2418 ms summed over its 60 largest real files, and its Histogram cliff
  costs 76 ms rather than the seconds it costs here. arity (R) wants Patience
  (206 vs 897 vs 1947 ms); panache (Markdown) is best on plain Myers. Both
  directions of this were learned by porting a fix verbatim and measuring it
  worse.

## Linter

### Rules

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
  rule. Would be a sixth category alongside `correctness`/`suspicious`/
  `performance`/`readability`/`meta`, which is cheap on its own — a category is
  its directory and nothing else (`.claude/rules/linter.md`), so it appears in
  no public surface and recategorizing is free — but the bundle is what needs
  designing, not the directory.

- [x] Not a lint rule, and not a linter task at all: TOML syntax diagnostics for
  `Project.toml`/`Manifest.toml`, noted while reading JuliaWorkspaces'
  `layer_diagnostics.jl`. Landed as the `toml-syntax` check in *Project files*
  below, stage 1, along with the reason it cannot be a rule.

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

- [ ] Maybe (deferred): make `parse`/`reparse` chunk-based so the one remaining
  flatten disappears. Salsa's `SourceFile` now *stores* the rope (`TextBuffer`,
  `src/incremental.rs`), so the write-phase clones it O(1) instead of
  flattening; the O(N) flatten moved into `parsed_document`, paid once per
  actual parse, where it feeds the `&str` the parser still wants. What is left
  is the parser half: `parse(text: &str)` and above all `reparse`/
  `reparse_edits`, whose token- and toplevel-tier guards prove a splice sound by
  slicing and relexing regions of `prev_text`/`new_text` — the wall named in
  #76, a rewrite of the most delicate code in the parser crate.
  `PrevParse.text: String` also stays until then: it is what those guards
  slice. Revisit only if the reparse guards go chunk-based.

## Project files (`Project.toml`/`Manifest.toml`)

- [ ] Completion of dependency names, the expensive one. On a default depot the
  registry is a `General.tar.gz`, so the full version needs gzip and tar to
  reach `Registry.toml`. Scope the first pass to packages already installed in
  the depot: no new dependency, no network. Note that *nothing* enumerates
  `<depot>/packages` today — it is only ever probed by exact slug — so even the
  cheap pass is new code.

- [ ] `name`/`uuid` upkeep on a rename, the edit half of the `willRenameFiles` entry
  above.

- [ ] **`resolve` is all-or-nothing**, found while landing stage 1: a good
  `Project.toml` beside a corrupt `Manifest.toml` loses the *entire*
  environment, so there is no library and no `declared_deps`, and
  `unresolved-import` goes quiet across the whole package. Stage 1 makes
  that pairing conspicuous — it now reports the manifest's syntax error
  while completions and go-to-definition silently degrade. The fix is a
  partial resolve (keep the project half, drop `packages`), which is a real
  behavior change to `environment.rs` deserving its own commit and tests.

- [ ] An *unused dependency* check (a `[deps]` entry never `using`'d anywhere in
  the package), the inverse of `unresolved-import`. A different cost class
  from everything above: it needs a whole-package union of free reads, which
  is a new cross-file query and the likeliest thing to punch through the
  range-free projection firewall in `src/project.rs`.

- [ ] Only `Project.toml` carries semantic findings. A manifest is checked for
  syntax alone, which is deliberate (nothing anchors inside one), but a
  `[[deps.X]]` entry naming a package absent from every dependency's `deps`
  list would be a real finding if the shape ever earns one.

- [ ] A code action that *adds* a missing dependency can only ever be a plain TOML
  text edit. Resolving a name to its UUID means reading the registry, and
  shelling out to `Pkg` is off the table: no Julia runtime, at any point in the
  pipeline.

## Tooling
