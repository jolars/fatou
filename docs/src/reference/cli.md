# CLI Reference

Run `fatou --help`, or `fatou <command> --help`, for the authoritative,
version-specific help. This page summarizes the commands and their options.

## `fatou`

Fatou: a language server, formatter, and linter for Julia.

**Usage**: `fatou [OPTIONS] <COMMAND>`

Global options:

- `--config <PATH>` — Path to an explicit `fatou.toml` (skips discovery).
- `--no-config` — Ignore any discovered `fatou.toml` and use built-in defaults.

## `fatou parse`

Parse and display the CST for debugging.

**Usage**: `fatou parse [OPTIONS] [FILE]`

- `<FILE>` — Input file (stdin if not provided).
- `--quiet` — Suppress CST output to stdout.
- `--verify` — Verify parser losslessness (`reconstruct(text) == text`).
- `--to <TO>` — Output representation: the lossless CST or the JuliaSyntax
  s-expression projection (the parser oracle). Default `cst`; one of `cst` or
  `sexpr`.

## `fatou format`

Format `.jl` files.

**Usage**: `fatou format [OPTIONS] [PATH]...`

- `<PATH>` — Input file(s) or path(s) (stdin if omitted).
- `--check` — Check formatting without writing; prints a diff and exits non-zero
  if any file would change.
- `--line-width <N>` — Override the target line width.
- `--indent-width <N>` — Override the indent width.

## `fatou lint`

Lint `.jl` files.

**Usage**: `fatou lint [OPTIONS] [PATH]...`

- `<PATH>` — Input file(s) or path(s).
- `--check` — Required in the groundwork phase: lint reports findings without
  writing fixes.
- `--output <OUTPUT>` — Output format. Default `pretty`; one of `pretty`,
  `concise`, or `json`.

## `fatou lsp`

Run the language server on stdio.

**Usage**: `fatou lsp`
