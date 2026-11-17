# TODOs

## Documentation

- [ ] **Formatting docstrings (opt-in):** only consider docstring reflow after
  corpus validation. Gate it behind `[format] docstrings = true` and require
  static content, a clean documentation parse, preserved documentation
  shape, protected code/table/math regions, idempotence, and clean Julia
  reparsing.

## Parser

- [x] **Macro newline safety:** keep operators on following lines outside macro
  arguments. `(@m x\n - y)` parses as `(@m x) - y`, and formatting protects
  the macro call with parentheses. Regression coverage includes comments,
  CRLF, incremental edits, and released and development Julia oracles.

- [ ] Support Julia 1.14's multiline macro arguments when a macro call is the
  immediate child of parentheses, such as `(@info "message"\n x=1)`. Cover
  qualified and nested macros, comments, CRLF, blocks, tuple and generator
  boundaries, and following operator lines. See
  [JuliaLang/julia#60181](https://github.com/JuliaLang/julia/pull/60181).

- [ ] Preserve the enclosing index and ternary context when parsing `return`.
  Julia 1.14 accepts `a[return end]`, `a[return begin]`, `x ? return : y`,
  and `x ? return a : y`; Fatou currently rejects them. Thread the relevant
  parse context through the keyword operand parser, as for labeled `break`.

- [ ] Update the lexer for Unicode 18. Regenerate the identifier tables from the
  locked Julia binary, and add the ten missing operator suffixes: `ᵅ`, `ᵋ`,
  `ᶲ`, `˱`, `˲`, `ₔ`, `₝`, `₞`, `₟`, and U+1DFD0. Cover new identifiers and
  suffixed operators against the matching development oracle. See
  [JuliaLang/julia#60285](https://github.com/JuliaLang/julia/pull/60285) and
  [JuliaLang/julia#63505](https://github.com/JuliaLang/julia/pull/63505).

### Oracle tooling

- [x] Migrate the committed Julia 1.13 / JuliaSyntax 1.0.2 oracle corpora to the
  locked development Julia and matching JuliaSyntax 2.0. Keep the syntax
  version explicit, separate changed encodings (`macro_name`, module edition
  metadata, and operator representations) from grammar gaps, and preserve
  regression coverage while re-triaging. Adapt `generate-unicode-ops.jl` to
  replace the removed `Tokenize._nondot_symbolic_operator_kinds` API before
  regenerating the operator table. Never hand-edit generated expectations or
  tables.

## Semantic analysis

- [ ] Model labeled `@label` blocks and loop exits. Treat label names as
  control-flow names rather than variable reads, resolve labeled
  `break`/`continue` to their named targets, and account for break values
  and anonymous `@label` / `_` scopes. Preserve the existing `@goto` /
  `@label` behavior. The parser already supports labeled keywords, but the
  semantic model counts labels as variable uses, and the CFG ignores jump
  labels.

## Linter

### Rules

- [ ] Extend `julia-version-compat` to Julia 1.14 additions: `typegroup`,
  wrapping arithmetic and assignment operators (including broadcasts),
  labeled `break`/`continue`, and primitive types with non-byte-multiple
  widths. Add checks for the new macro and `return` forms as parser support
  lands. These constructs currently produce no compatibility finding when
  targeting Julia 1.13. Keep runtime compatibility separate from the
  selected syntax edition.

- [ ] Detect dead assignments with control-flow-aware liveness analysis,
  separately from `unused-binding` and `loop-variable-shadow`. Report only
  when an assigned value cannot be read before it is overwritten or leaves
  scope, including when the next loop iteration rebinds it. Keep valid
  within-iteration normalization quiet, and handle closures and macros
  conservatively. Any fix must preserve right-hand-side side effects.

- [ ] Add an opt-in rule for functions that read known nonconstant, untyped
  globals (#109). Use the shared resolver to distinguish global bindings
  from locals and captured variables. Exempt constants, functions, types,
  and typed globals. Keep this separate from `undefined-name`: the name
  resolves, but passing its value as an argument would make the dependency
  explicit and avoid the performance cost of an untyped global.

## Language server

- [ ] Regenerate LaTeX and emoji completions from the locked development Julia's
  REPL tables. Include the new Unicode 18 subscript sequences `\_w`, `\_y`,
  `\_z`, and `\_pgamma`, and review changed expansions as well as new
  entries. Do not hand-edit `src/lsp/latex_symbols.rs`.

- [ ] Code actions beyond quick fixes: organize/sort `using` statements, qualify
  a bare name.

- [ ] `workspace/willCreateFiles` and `workspace/willDeleteFiles`, the siblings
  of the rename handlers. The `RenameMap` machinery in
  `src/lsp/rename_files.rs` is most of what a delete needs; the open
  question is what a delete *should* edit, since dropping the `include` call
  that names a deleted file is a destructive default and leaving it dangling
  is what the include-graph diagnostics already report. Create is close to a
  no-op. Design first.

## Project files (`Project.toml`/`Manifest.toml`)

- [ ] Honor Julia syntax editions. Read `Project.toml`'s
  `[syntax] julia_version`, falling back to the minimum `[compat].julia`
  version and Julia's defaults for packages and scripts. Track dependency
  editions recorded in manifests and propagate the selected edition through
  CLI and LSP parsing, including unsaved project edits. Model the edition
  separately from the supported runtime range, and keep discovery static.

- [ ] Completion of dependency names, the expensive one. On a default depot the
  registry is a `General.tar.gz`, so the full version needs gzip and tar to
  reach `Registry.toml`. Scope the first pass to packages already installed
  in the depot: no new dependency, no network. Note that *nothing*
  enumerates `<depot>/packages` today — it is only ever probed by exact slug
  — so even the cheap pass is new code.

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

- [ ] A code action that *adds* a missing dependency can only ever be a plain
  TOML text edit. Resolving a name to its UUID means reading the registry,
  and shelling out to `Pkg` is off the table: no Julia runtime, at any point
  in the pipeline.
