---
paths:
  - "src/index.rs"
  - "src/index/**/*"
  - "src/environment.rs"
  - "tests/base_index.rs"
  - "tests/harvest.rs"
  - "tests/library_index.rs"
  - "tests/parallel_harvest.rs"
  - "tests/environment.rs"
---

# Package index and environment rules

`src/index/` harvests a structured, serializable view of a Julia package's
public API **with fatou's own parser**; `src/environment.rs` locates the active
project and resolves each package to an on-disk source directory. Together they
feed the `LibraryIndex` salsa input that completion, hover, go-to-definition,
and the import lints read.

## Hard invariants

- **No Julia runtime, ever.** `environment.rs` mirrors Julia's loader using only
  the filesystem, and the harvester parses `.jl` sources. Shelling out to
  `julia` to answer a question here is not an option — it is the thing this
  module exists to avoid.
- **A malformed, truncated, or missing on-disk file is input, not a bug report.**
  Degrade to "no symbols for this package" rather than panicking; these files
  come from whatever the user happens to have installed.
- **The index is a cache.** A stale or missing entry must only cost precision
  (fewer known symbols), never the correctness of a lint or a format.
- `PackageIndex` is deliberately **depot-independent** and serializable: source
  roots live beside it in `HarvestedLibrary`, not inside the model.

## Environment discovery

Follows Julia's own precedence: `JULIA_PROJECT`, then a walk up from the
workspace root, then the newest default environment under
`~/.julia/environments/`. Package sources are at
`<depot>/packages/<Name>/<slug>/`, where the slug is **computed** from the UUID
and `git-tree-sha1` (`version_slug`) rather than scanned, because several
versions of a package may be installed.

## The Base/Core floor

`base.rs` harvests Base (`base/Base.jl`), Core (`base/boot.jl`), and every
stdlib (`stdlib/vX.Y/<Name>`) from a located install, exactly like a depot
package — Julia 1.12 split the Base opener into `Base_compiler.jl`, so both
layouts are harvested and merged.

With **no installation found**, minimal `Base`/`Core` indexes are synthesized
from `index/fallback/{base,core}_exports.txt` so name resolution still has a
floor instead of flagging every builtin. Those lists are **generated snapshots**
— regenerate by dumping `sort(names(Base))`/`sort(names(Core))` from a real
install (dropping gensym names containing `#`); never hand-edit.

## Declared dependencies

A workspace package's declared dependencies are its `Project.toml` `[deps]`
names, and are deliberately **not derived from the harvest**: a project that was
never instantiated, or one whose Julia install fatou could not locate, still
declares its dependencies. That is what `unresolved-import` reads.

`ProjectFile::declared_dep_names` is the **one** definition, unfiltered by
whether the paired UUID parses. Two consumers share it, and they must not
drift: the CLI through `Environment::declared_deps` →
`HarvestedLibrary::declared_deps` (no salsa db, no buffer to be stale against),
and the language server through the `project_declared_deps` salsa query, keyed
on the project file's own `SourceFile` input (`HarvestedLibrary::project_files`
supplies the paths). The server's route is what makes an **unsaved** `[deps]`
edit count — the `Project.toml` buffer is authoritative there, while everything
needing a resolved `Environment` stays on the harvester's save cadence.

The buffer is authoritative only **while a buffer exists**. `set_project_files`
seeds through `seed_disk_file`, which is create-or-return precisely so a
re-resolve cannot clobber an open, unsaved file — which means a re-resolve is
*not* what refreshes a closed one. The **watched-file sync** is:
`on_watched_files` reverts an environment file with no open document to disk,
exactly as it does a `.jl` source. Drop that and a `pkg> add` from a terminal
leaves `unresolved-import` answering to the text the file had at startup.

An editor also opens project files belonging to **no** package — a
`docs/Project.toml`, a depot one followed from a manifest link. Their text is
tracked like any other buffer, but `declared_deps_of_file` answers `None` for
them: nothing reads their `[deps]`, and a keystroke there must not re-lint the
workspace.

## Testing

Suites live at the root: `harvest.rs`, `base_index.rs`, `library_index.rs`,
`parallel_harvest.rs`, `environment.rs`. **Add a fixture for each new on-disk
shape** rather than asserting against whatever happens to be installed on the
machine — CI has no Julia depot.
