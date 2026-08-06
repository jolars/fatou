# Command-Line Help for `fatou`

Fatou: a language server, formatter, and linter for Julia

**Usage:** `fatou [OPTIONS] <COMMAND>`

## Options

`--config <PATH>`
:   Path to an explicit `fatou.toml` (skips discovery)

`--no-config`
:   Ignore any `fatou.toml` (project, `FATOU_CONFIG`, or global) and use built-in defaults

`--color <COLOR>`
:   When to colorize human-readable output

    Default value: `auto`

    Possible values:

    - `auto`: Colorize when writing to a terminal and `NO_COLOR` is unset
    - `always`: Always colorize
    - `never`: Never colorize

`-q`, `--quiet`
:   Suppress non-essential output (errors are still shown). Under `format --check` this drops the per-file diff, leaving the list of files that would be reformatted and the summary; under `parse` it suppresses the CST

## `fatou parse`

Parse and display the CST for debugging

**Usage:** `fatou parse [OPTIONS] [FILE]`

### Arguments

`<FILE>`
:   Input file (stdin if not provided)

### Options

`--verify`
:   Verify parser losslessness (`reconstruct(text) == text`)

`--to <TO>`
:   Output representation: the lossless CST (default) or the JuliaSyntax s-expression projection (the parser oracle)

    Default value: `cst`

    Possible values:

    - `cst`: The lossless `rowan` concrete syntax tree
    - `sexpr`: The JuliaSyntax-native s-expression projection

## `fatou format`

Format `.jl` files

**Usage:** `fatou format [OPTIONS] [PATH]...`

### Arguments

`<PATH>...`
:   Input file(s) or path(s) (stdin if omitted)

### Options

`--check`
:   Check formatting without writing; prints a diff and exits non-zero if any file would change

`--line-width <N>`
:   Override the target line width

`--indent-width <N>`
:   Override the indent width

`--exclude <PATTERN>`
:   Additional gitignore-style exclude patterns (repeatable or comma-separated); augments the configured `exclude`/`extend-exclude`

`--force-exclude`
:   Apply exclude patterns to files named explicitly on the command line too (they are normally always processed); for runners like pre-commit that pass staged files as arguments

## `fatou lint`

Lint `.jl` files

**Usage:** `fatou lint [OPTIONS] [PATH]...`

### Arguments

`<PATH>...`
:   Input file(s) or path(s)

### Options

`--fix`
:   Apply safe fixes to the source and write the files back

`--unsafe-fixes`
:   Also apply fixes marked unsafe (implies `--fix`)

`--exclude <PATTERN>`
:   Additional gitignore-style exclude patterns (repeatable or comma-separated); augments the configured `exclude`/`extend-exclude`

`--force-exclude`
:   Apply exclude patterns to files named explicitly on the command line too (they are normally always processed); for runners like pre-commit that pass staged files as arguments

`--julia-version <VERSION>`
:   Target Julia version or range for version-compat checks (e.g. `1.10` or `1.6 - 1.11`); overrides `[julia] version` and the project's `Project.toml` `[compat]`

`--output <OUTPUT>`
:   Output format

    Default value: `pretty`

    Possible values: `pretty`, `concise`, `json`

## `fatou lsp`

Run the language server on stdio

**Usage:** `fatou lsp`
