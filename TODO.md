# TODOs

## Parser

- [x] Parse direct parenthesized anonymous-function parameters as a tuple before
  `->`, including singleton, typed, and `;` keyword-parameter forms, while
  keeping a parenthesized `where` signature transparent. This fixes
  `(a; b=1) -> c` and its siblings without changing standalone parenthesized
  blocks.

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
  its directory and nothing else (`AGENTS.md`, "Linter"), so it appears in
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

- [x] The per-keystroke text copies are gone, and the rope from #76 stays
  deferred — now on measurement rather than on argument. Two changes: `Token`
  borrows its text from the input (`Token<'src> { text: &'src str }`), which
  retired the per-token `String` and cut lexing ~65% and a full parse ~15%;
  and the document text is one shared `Arc<str>` across the buffer, salsa's
  `SourceFile`, and `PrevParse`, which turned the write-phase copy and the
  base clone into refcount bumps and the staleness compare into `Arc::ptr_eq`
  (a no-op upsert went 39 us -> 150 ns at 1 MB). An edit now rebuilds the
  string instead of splicing in place, +10-30 us at 1 MB, well under the
  reparse it precedes.

  Measured against a full ropey conversion of the same paths (PR #85): the
  rope reproduced none of the lexer win that the borrow alone gives — its
  chunk machinery costs 10-19% against `&str` tokens, and its multi-chunk LSP
  path another ~55% — and it cannot match `ptr_eq` on the unchanged-text
  check, since rope equality walks chunks (27 us at 1 MB). What a rope
  uniquely buys is the didChange splice: 0.7 us flat at 1 MB against our
  ~34 us rebuild. That is real and nothing else reproduces it, but it is
  ~30 us on a path whose reparse costs 150 us+, and the same branch lost
  ~2.5 ms per keystroke to per-byte rope iteration in `diff_edit`. Revisit
  only if fatou starts targeting documents where the splice dominates, and
  only after the `diff_edit` bypass below.

- [ ] Skip `diff_edit` when the staged chain is a single verified edit.
  `parsed_document` declines a chain below two edits, so the ordinary
  one-keystroke case always re-derives, by diffing the two whole texts, an
  edit the language server just handed it. `benches/salsa_keystroke.rs` puts
  that at ~200 us of a ~500 us keystroke at 1 MB — more than the token-tier
  reparse it feeds. The chain is already verified against the previous text
  (`reparse_edits`' fits check), so a single-edit chain can go straight to
  `reparse` with no new trust; `diff_edit` stays the fallback for a text that
  changed by a route carrying no edits.

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
