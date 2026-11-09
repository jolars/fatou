# TODOs

## Parser

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

## Documentation

- [ ] **Formatting docstrings (opt-in):** only consider docstring reflow after
  corpus validation. Gate it behind `[format] docstrings = true` and
  require static content, a clean documentation parse, preserved documentation
  shape, protected code/table/math regions, idempotence, and clean Julia
  reparsing.

## Linter

### Rules

- [ ] Detect dead assignments with control-flow-aware liveness analysis,
  separately from `unused-binding` and `loop-variable-shadow`. Report only
  when an assigned value cannot be read before it is overwritten or leaves
  scope, including when the next loop iteration rebinds it. Keep valid
  within-iteration normalization quiet, and handle closures and macros
  conservatively. Any fix must preserve right-hand-side side effects.

- [ ] Add an opt-in rule for functions that read known nonconstant, untyped
  globals (#109). Use the shared resolver to distinguish global bindings from
  locals and captured variables. Exempt constants, functions, types, and typed
  globals. Keep this separate from `undefined-name`: the name resolves, but
  passing its value as an argument would make the dependency explicit and
  avoid the performance cost of an untyped global.

## Language server

- [ ] Code actions beyond quick fixes: organize/sort `using` statements,
  qualify a bare name.

- [ ] `workspace/willCreateFiles` and `workspace/willDeleteFiles`, the siblings
  of the rename handlers. The `RenameMap` machinery in `src/lsp/rename_files.rs`
  is most of what a delete needs; the open question is what a delete *should*
  edit, since dropping the `include` call that names a deleted file is a
  destructive default and leaving it dangling is what the include-graph
  diagnostics already report. Create is close to a no-op. Design first.

## Project files (`Project.toml`/`Manifest.toml`)

- [ ] Completion of dependency names, the expensive one. On a default depot the
  registry is a `General.tar.gz`, so the full version needs gzip and tar to
  reach `Registry.toml`. Scope the first pass to packages already installed in
  the depot: no new dependency, no network. Note that *nothing* enumerates
  `<depot>/packages` today — it is only ever probed by exact slug — so even the
  cheap pass is new code.

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
