# Language server

Fatou helps you write and explore Julia code through your editor. It provides
completion, navigation, refactoring, formatting, and diagnostics without
starting Julia or running your code.

Follow [Editor setup](editors.md) to connect your editor. The features below use
your editor's usual commands. Some, such as inlay hints and semantic
highlighting, may need to be enabled in the editor.

## Write and explore Julia code

Completion suggests names visible at the cursor and members of modules such as
`Base` and `LinearAlgebra`. Hover over a name to read its documentation, or use
signature help while writing a function call to see its parameters.

Use your editor's navigation commands to follow the code:

  | Task                                   | Editor command              |
  | -------------------------------------- | --------------------------- |
  | Jump to where a name is defined        | Go to Definition            |
  | Find uses of a name                    | Find References             |
  | Browse definitions in the current file | Document symbols or outline |
  | Search definitions across the project  | Workspace symbols           |
  | Explore callers and callees            | Call hierarchy              |
  | Explore relationships between types    | Type hierarchy              |

Fatou also highlights uses of the name under the cursor, colors code through
semantic highlighting, and supports folding code sections and expanding a
selection to the surrounding expression.

## Format code and fix problems

Format a whole Julia file or a selected range with your editor's formatting
command. The result follows the same settings as `fatou format`. See
[Formatting](formatting.md) for the formatting policy and options.

Parse errors and lint findings appear as you edit. When Fatou can suggest a fix,
you can apply it through the editor's quick-fix menu. Choose lint rules and
suppressions as described in [Linting](linting.md).

Files in a workspace package also receive project-aware checks, including
`undefined-name` and `call-arity`. Your lint selection and `ignore` settings
still apply. For standalone scripts that span several files, [declare their
entry points](configuration.md#checking-standalone-scripts) to enable analysis
across their includes.

## Rename symbols, files, and packages

Use Rename on a symbol to update its references. If your editor supports LSP
file operations, renaming a file or folder also updates the `include` paths that
refer to it.

Renaming a package's entry file from `src/MyPkg.jl` to `src/NewPkg.jl` also
updates the package's `name` in `Project.toml` or `JuliaProject.toml` and
matching top-level `module MyPkg` declarations. The new file must stay in the
package's `src/` directory and have a valid Julia identifier as its name.

Fatou uses unsaved project text and preserves the UUID, comments, and other
fields. You still need to update imports and references to the package name
elsewhere.

## Work with docstrings

Hover presents static docstrings as Markdown, both from your current file and
from installed packages. While editing a docstring, you can browse its headings
in the document outline and fold sections. Links to external pages, headings,
and footnotes are navigable.

In an explicit Documenter link such as `[norm](@ref LinearAlgebra.norm)`, Fatou
completes the target and navigates to its definition. Targets can refer to Julia
symbols or documentation anchors.

Code fences labeled `julia`, `julia-repl`, or `jldoctest`, and Documenter
directives containing Julia code, support completion, hover, signature help, Go
to Definition, selection ranges, folding, and semantic highlighting.
Documentation produced by dynamic or custom `@doc` expressions is not analyzed.

## Unicode symbol input

Type a backslash to complete Julia's LaTeX and emoji input sequences. Accepting
a suggestion replaces the whole sequence with its Unicode character:

  | Sequence   | Inserts |
  | ---------- | ------- |
  | `\alpha`   | α       |
  | `\_1`      | ₁       |
  | `\:smile:` | 😄      |

A bare `\` lists LaTeX sequences, and `\:` lists emoji. Keep typing to narrow
the suggestions. The sequences come from Julia's REPL symbol tables.

Fatou avoids offering these suggestions inside string macros and command
literals, such as `r"\d"`, `raw"\n"`, and `` `ls \d` ``. In plain strings,
completion waits for a second character after the backslash, so an escape such
as `"\n` does not bring up `\nabla`. Longer sequences such as `\nu` and `\alpha`
still work in strings and docstrings.

## Manage your package environment

Fatou checks `Project.toml` and `Manifest.toml` and reports problems in the file
where they occur, even when it is closed. Attach the server to those files for
diagnostics as you type and the features below. The VS Code extension does this
automatically; see [Other LSP clients](editors.md#other-lsp-clients) for other
editors. Formatting is available for Julia files only.

### Inspect dependencies

In `Project.toml`, hover over a dependency name to see its resolved version,
package kind, and source path. Go to Definition or follow its document link to
open the package's entry file. Inlay hints show resolved versions beside the
UUIDs in `[deps]`.

In `Manifest.toml`, a local dependency's `path` entry links to that package's
`Project.toml`.

### Update compatibility bounds

Place the cursor on a dependency declaration or its existing `[compat]` entry
and open the code-action menu:

- **Update** raises the bound to the newest stable release it already allows.
- **Upgrade** allows the newest stable release outside the current bound.

For example, if releases `1.9.3` and `2.3.4` are available:

  | Current bound | Update    | Upgrade |
  | ------------- | --------- | ------- |
  | `"1.2"`       | `"1.9"`   | `"2.3"` |
  | `"1"`         | No change | `"2"`   |

Actions appear only when they change the bound. Fatou preserves version
precision, operators, quotes, comments, and whitespace. For comma-separated
ranges, Update changes the matching range, and Upgrade appends a range so
earlier supported versions remain allowed.

With inlay hints enabled, each `[compat]` value shows the newest matching
release and any available upgrade, such as `v1.9.3 → v2.3.4`. These are
available registry versions; the hints beside `[deps]` UUIDs show the versions
your environment has resolved.

Updates work in `Project.toml` and `JuliaProject.toml`, including `[weakdeps]`,
`[extras]`, and unsaved edits. Fatou skips yanked releases, prereleases, and
dependencies overridden in `[sources]`.

Suggestions use locally installed registries. Refresh your registries through
Julia's package manager when you need newer suggestions. Missing or malformed
registry data can reduce the suggestions available.

Applying an action edits the project file. Use Julia's package manager afterward
to resolve the environment and update `Manifest.toml`. Fatou does not check
whether a suggested release is compatible with your Julia version or other
dependencies.

## Understand analysis limits

Fatou analyzes source files and installed package information without evaluating
Julia. Code generated at runtime and dynamic includes can limit what it can
resolve. Missing package sources can also reduce completion and navigation
results.

Completion after a dot currently covers module members, such as names under
`LinearAlgebra.`. It does not infer an arbitrary value's type to suggest fields
or methods.

See the [project configuration reference](../reference/configuration.md#project)
for analysis limits and how Fatou reports incomplete analysis.
