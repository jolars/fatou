# Formatting

Fatou formats Julia source without starting Julia. After
[installation](../installation.md), run it from your project directory. Try it
in your browser in the [playground](../playground.md).

Fatou removes leading and trailing blank lines inside ordinary blocks and keeps
up to one blank line between statements. `module` and `baremodule` bodies,
including nested modules, also keep up to one leading and trailing blank line.
Comments retain their attachment to statements.

## Format files

Format one file in place, or pass a directory to format its `.jl` files:

```sh
fatou format src/example.jl
fatou format .
```

You can pass several paths in one invocation. Directory walks honor `.gitignore`
and the exclusions in `fatou.toml`. Explicitly named files are still processed
unless you add `--force-exclude`; see [excluding
files](configuration.md#excluding-files).

## Check without writing

Use `--check` to verify formatting in CI or before committing:

```sh
fatou format --check .
```

The command prints diffs and exits nonzero if any file would change. Add
`--quiet` to keep the file list and summary without printing diffs. See
[Integrations](integrations.md) for GitHub Actions, pre-commit, and dprint.

## Verify preservation before writing

Use `--safe` to require verification that formatting preserves the parsed
program and comments:

```sh
fatou format --safe src/
```

Fatou refuses malformed input or a result it cannot verify. When several files
are supplied, it checks the entire batch before writing any of them. Combine the
flag with `--check` to verify preservation and check layout without writes:

```sh
fatou format --safe --check .
```

## Format standard input

Pass `-` to read a buffer from standard input and write the formatted result to
standard output:

```sh
cat src/example.jl | fatou format -
```

`--check` requires file paths. For formatting inside an editor, follow [Editor
setup](editors.md).

## Choose formatting settings

Put shared settings in `fatou.toml`:

```toml
[format]
line-width = 100
indent-width = 4
```

For a single run, use `fatou format --line-width 100 src/example.jl`. The
[configuration guide](configuration.md#formatting) explains how to choose a
style, and the [configuration reference](../reference/configuration.md) lists
all options and defaults.

To apply lint fixes before formatting, follow the [fix-and-format
workflow](linting.md#apply-fixes).
