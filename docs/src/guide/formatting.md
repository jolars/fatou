# Formatting

Fatou formats Julia source without starting Julia. After
[installation](getting-started.md#installation), run it from your project
directory.

## Format Files

Format one file in place, or pass a directory to format its `.jl` files:

```sh
fatou format src/example.jl
fatou format .
```

You can pass several paths in one invocation. Directory walks honor `.gitignore`
and the exclusions in `fatou.toml`. Explicitly named files are still processed
unless you add `--force-exclude`; see [excluding
files](configuration.md#excluding-files).

## Check Without Writing

Use `--check` to verify formatting in CI or before committing:

```sh
fatou format --check .
```

The command prints diffs and exits nonzero if any file would change. Add
`--quiet` to keep the file list and summary without printing diffs. See
[Integrations](integrations.md) for GitHub Actions, pre-commit, and dprint.

## Verify Preservation Before Writing

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

## Format Standard Input

Pass `-` to read a buffer from standard input and write the formatted result to
standard output:

```sh
cat src/example.jl | fatou format -
```

`--check` requires file paths. For formatting inside an editor, follow [Editor
Setup](editors.md).

## Choose Formatting Settings

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

Formatting lays out code. To apply lint fixes as well, run `fatou lint --fix .`
before `fatou format .`; see [Linting](linting.md#apply-fixes).
