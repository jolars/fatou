# TODOs

## Parser

- [x] Refresh the parser oracle and generated tables for Julia 1.13 while
  retaining JuliaSyntax 1.0.2. Unicode 17 identifier starts and combining
  marks have lexer and differential-fixture coverage.

- [x] Diagnose overflowing decimal `Float64`/`Float32` literals instead of
  accepting Rust's infinite parse result. The parser now emits JuliaSyntax's
  `ErrorNumericOverflow` projection while leaving finite boundary values and
  underflow unchanged.

- [x] Fix raw triple-string quote decoding. Raw strings now apply their own
  backslash-before-quote treatment before display escaping; fixture
  `raw_triple_string_quote` locks the backslash-run siblings.

- [ ] Lex the *broadcast* wrapping arithmetic operators `.+% .-% .*%` (and their
  augmented forms `.+%= .-%= .*%=`) in
  `crates/fatou-parser/src/parser/lexer.rs`. Fatou supports undotted `+% -% *%`;
  the dotted forms split into existing operators and produce diagnostics.
  Deferred: Julia 1.13.0 and the latest JuliaSyntax release, 1.0.2, still reject
  the entire wrapping-operator family (verified 2026-09-15). The Julia upgrade
  does not unblock the pinned oracle. Implementing these extensions now would
  require a development-revision oracle or hand-authored parser fixtures.

- [ ] Two error-recovery gaps left over from labeled `break`/`continue`
  (`crates/fatou-parser/src/parser/structural.rs`). Junk after a complete labeled keyword drops
  JuliaSyntax's trailing zero-width marker (`break l x y` ⇒ `(break l x)
  (error-t y)`, not `(error-t y (error-t))`), and a bare comma after one does not
  fold into a tuple (`break l, y` ⇒ `(break l) (error-t ✘ y)`, not
  `(tuple (break l) y)`). The labeled cases remain deferred: both Julia 1.13.0
  and JuliaSyntax 1.0.2 reject `break lbl` and `continue lbl` (verified
  2026-09-15), so the current oracle cannot validate labeled recovery.

- [x] Recover bare `break, y` / `continue, y` as tuples with a diagnostic after
  the keyword, matching JuliaSyntax 1.0.2. Fixture `break_continue_tuple` covers
  assignment precedence, comments, newline continuation, and comma-separated
  containers. Tentative bracket parses discard their diagnostics before
  reparsing an argument list, so each invalid comma is reported once.

### Incremental

- [ ] Maybe (deferred): a nested-block tier needs a context-parameterized
  fragment entry point (`public_context`, bracket `end` markers) — a bare
  fragment `parse()` misparses those today. A pure optimization on top of a
  sound stage 2–4.

## Formatter

- [x] Harden `format --safe` against lossy projected leaves, non-finite float
  fingerprints, and configured line-ending conversion inside block comments.

## Documentation

Docstrings are arbitrary `@doc` metadata. Tooling interprets only statically
recoverable textual payloads; dynamic or custom forms stay opaque. Markdown and
Documenter syntax are recognized from the content itself, while documentation
style remains project policy rather than language correctness.

- [x] Match Julia 1.13's inline em dashes: `---` is one lossless `EM_DASH`
  token, exposed as `Inline::EmDash`; heading text uses the rendered em
  dash.

- [x] **Foundation:** add typed `DOC` navigation plus one attachment API for
  ordinary docstrings and the two-argument `@doc` forms. Extract ordinary and
  `raw` string payloads with Julia's escape, newline, and triple-string dedent
  semantics, retaining an exact decoded-to-source byte map. Classify
  interpolated, custom, and non-string payloads as opaque.

- [x] **Index and semantics:** replace the index's raw, undedented extraction
  with the shared model; preserve documentation per method and for types,
  fields, constants, macros, and modules; and make unsaved local docstrings
  available without waiting for a workspace re-harvest.

- [x] **Documentation parser:** add a Wasm-clean documentation module to
  `fatou-parser` for Julia's Markdown dialect, with a lossless CST and typed
  navigation. Recognize core Documenter links and fences locally; use Julia's
  Markdown stdlib only as a test-time differential oracle, never during
  analysis.

- [x] **Language features (default-on):** render decoded local and indexed
  documentation consistently; add Markdown folding and navigation; and offer
  completion and definition for explicit `@ref` plus embedded-Julia support
  where a fence declares Julia code.

- [x] **Linting:** add conservative default-on checks for malformed Julia code
  in explicit fences, argument names that disagree with an existing
  `# Arguments` section, and `@ref` targets that project resolution can prove
  missing. Landed as `invalid-docstring-code` (sem),
  `docstring-argument-mismatch` (sem), and
  `unresolved-docstring-reference` (res)—all correctness warnings without
  fixes. Documentation coverage and style remain policy rather than default
  lint.

- [x] Parse an indented code block below a list item as its own nested block.
  `emit_list_level` now removes the item's prefix before applying the four-space
  code indent, matching Julia's `Markdown.parse` without changing ordinary
  continuation lines.

- [x] Cache Markdown anchors and definitions in a demand-only salsa query over
  the file's decoded static docstring payloads. Code edits and equivalent
  literal spellings reuse the index; navigation maps decoded ranges through
  the live source maps. Embedded-Julia completion keeps its existing boundary.
  Before/after measurements and reproduction instructions live in
  `bench/documentation.json` and `bench/documentation.md`.

- [ ] **Formatting (deferred and opt-in):** only consider docstring reflow after
  corpus validation. Gate it behind `[format] docstrings = true` and
  require static content, a clean documentation parse, preserved documentation
  shape, protected code/table/math regions, idempotence, and clean Julia
  reparsing.

## Linter

- [ ] Support explicit, opt-in script entry points for `undefined-name` (#109).
  Follow static includes through the shared project model and resolver so
  standalone scripts can resolve globals from included files. Keep each entry
  point and module context separate: another caller may supply different
  globals, and merging callers' names can hide errors. Do not assume arbitrary
  open files are entry points. Retain conservative handling of dynamic
  includes, `eval`, and unresolved imports. Currently, `extend-select` enables
  the rule but does not override the guard that skips standalone files with
  includes.

### Rules

- [ ] Add an opt-in rule for functions that read known nonconstant, untyped
  globals (#109). Use the shared resolver to distinguish global bindings from
  locals and captured variables. Exempt constants, functions, types, and typed
  globals. Keep this separate from `undefined-name`: the name resolves, but
  passing its value as an argument would make the dependency explicit and
  avoid the performance cost of an untyped global.

- [x] The `Test`-stdlib bundle reuses `redundant-boolean`, `length-zero`, and
  `nothing-comparison`, and adds `test-isa-call` (readability, sem, warning,
  safe fix, default-on) plus `test-bare-expression` (suspicious, sem, warning,
  no fix, default-off). Their shared matcher requires a preceding, visible Test
  load and supports qualified, parenthesized, and aliased `@test` spellings.

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

- [x] Renaming a package's entry file within `src/` updates the tracked
  `Project.toml` or `JuliaProject.toml` name and matching top-level module
  declarations, preserving its UUID and combining the edits with include
  rewrites. Folder batches address the old URIs, and `didRenameFiles`
  re-resolves the package even without file watchers. Moves outside `src/`
  still report `missing-entry-file`.

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

- [x] Dependency code actions distinguish updates within `[compat]` bounds from
  upgrades beyond them, preserving precision, operators, and Julia unions.
  Inlay hints show the newest matching release and available upgrade.
  `src/registry.rs` reads both directory and compressed registries without Julia
  or network access; compressed results are cached until the archive changes.

- [ ] Completion of dependency names, the expensive one. On a default depot the
  registry is a `General.tar.gz`, so the full version needs gzip and tar to
  reach `Registry.toml`. Scope the first pass to packages already installed in
  the depot: no new dependency, no network. Note that *nothing* enumerates
  `<depot>/packages` today — it is only ever probed by exact slug — so even the
  cheap pass is new code.

- [x] Entry-file renames update `name` while preserving the package's `uuid`;
  see `willRenameFiles` above.

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
