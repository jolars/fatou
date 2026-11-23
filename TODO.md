# TODOs

## Documentation

- [ ] **Formatting docstrings (opt-in):** only consider docstring reflow after
  corpus validation. Gate it behind `[format] docstrings = true` and require
  static content, a clean documentation parse, preserved documentation
  shape, protected code/table/math regions, idempotence, and clean Julia
  reparsing.

## Parser

## Formatter

## Semantic analysis

## Linter

### Rules

- [x] Detect dead assignments with control-flow-aware liveness analysis,
  separately from `unused-binding` and `loop-variable-shadow`. Report only
  when an assigned value cannot be read before it is overwritten or leaves
  scope, including when the next loop iteration rebinds it. Keep valid
  within-iteration normalization quiet, and handle closures and macros
  conservatively. Default warning with no autofix.

- [ ] Add an opt-in rule for functions that read known nonconstant, untyped
  globals (#109). Use the shared resolver to distinguish global bindings
  from locals and captured variables. Exempt constants, functions, types,
  and typed globals. Keep this separate from `undefined-name`: the name
  resolves, but passing its value as an argument would make the dependency
  explicit and avoid the performance cost of an untyped global.

## Language server

- [ ] `workspace/willCreateFiles` and `workspace/willDeleteFiles`, the siblings
  of the rename handlers. The `RenameMap` machinery in
  `src/lsp/rename_files.rs` is most of what a delete needs; the open
  question is what a delete *should* edit, since dropping the `include` call
  that names a deleted file is a destructive default and leaving it dangling
  is what the include-graph diagnostics already report. Create is close to a
  no-op. Design first.

## Project files (`Project.toml`/`Manifest.toml`)

- [ ] Completion of dependency names, the expensive one. On a default depot the
  registry is a `General.tar.gz`, so the full version needs gzip and tar to
  reach `Registry.toml`. Scope the first pass to packages already installed
  in the depot: no new dependency, no network. Note that *nothing*
  enumerates `<depot>/packages` today — it is only ever probed by exact slug
  — so even the cheap pass is new code.

- [x] Keep the workspace package and declared dependencies when the manifest
  fails to load. Report the manifest error in the editor and skip its pins.

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
