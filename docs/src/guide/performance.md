# Performance

Fatou is a compiled Rust formatter competing with two Julia-native tools,
[Runic](https://github.com/fredrikekre/Runic.jl) and
[JuliaFormatter](https://github.com/domluna/JuliaFormatter.jl). This page
compares their formatting throughput.

## Methodology

We measure each tool in a **warm loop**: the tool is loaded once, run through a
few warmup calls, and then timed over many iterations. This deliberately
**excludes process startup and first-call JIT compilation** for the Julia tools,
which would otherwise dominate and obscure the actual formatting cost. In other
words, these numbers reflect a long-lived editor or language-server session, not
the cold `julia -e ...` command-line invocation. That cold path is measured
separately in [Cold start](#cold-start) below.

Because each tool runs in its own runtime, we report **throughput in MB/s**,
which normalizes for byte count and stays comparable even when tools cover
different files. Each tool formats with **its own default style**; we are
measuring speed, not comparing output. A file counts for a tool only if that
tool formats it without error, and any skips are reported.

### Corpora

Two real-world projects, both pinned to a tag, picked to pull in opposite
directions:

- [**JuliaSyntax.jl**](https://github.com/JuliaLang/JuliaSyntax.jl), the parser
  Fatou targets for parity: dense branching, large token tables, and the code
  Fatou is best equipped to handle. Home turf.
- [**DataFrames.jl**](https://github.com/JuliaData/DataFrames.jl), ordinary
  library code of the kind users actually format: docstring-heavy, macro-heavy,
  built around a large indexing DSL, and roughly 2.6x the size of the
  JuliaSyntax tree.

### Scenarios

- **Single file** (three of them), through each tool's pure `String -> String`
  formatter (`fatou::formatter::format`, `Runic.format_string`,
  `JuliaFormatter.format_text`). The three targets span both size and shape:
  `parse_stream.jl` (42 KB of dense parser internals), `kinds.jl` (24 KB that is
  almost entirely one flat macro/data table), and `abstractdataframe.jl` (100 KB
  of docstring- and macro-heavy application code).
- **Project** (one per corpus): the whole `src/` tree, driven through each tool's
  own **directory entry point**, so file discovery, IO, and the tool's internal
  parallel scheduling all count. This is the "format my whole project" path.
  Fatou uses `fatou::formatter::check_paths` (glob/directory discovery plus
  rayon-parallel formatting, read-only); JuliaFormatter uses
  `format(dir; overwrite = false)` (recursive, thread-parallel, read-only).
  **Runic is excluded from these scenarios by design**: it has no in-process
  directory API (its `format_file` is single-file only, and directory walking
  lives solely in its CLI), so there is nothing to measure on the same terms.

Reproduce with `task bench` (after reloading the devenv shell so `Runic` is on
the Julia path). Results are written to `bench/results.json`.

## Setup

{{ benchmark-meta }}

## Results

Single files and whole projects get a chart each: they measure different work at
different sizes, and sharing one axis buries that. In both, Fatou is the baseline
on the dashed line at 1, and every other tool's time is plotted relative to it, so
faster tools fall below the line and slower tools rise above it.

### Single files

{{ benchmark-results-single }}

### Projects

{{ benchmark-results-project }}

## Cold start

The warm loop above is the right model for an editor or language server that
stays resident, but it hides the cost a command-line user pays on the very first
run. This section measures that **cold start** directly: each tool is invoked as
a fresh process that starts up, formats the single file once, and exits. For the
Julia tools that means paying Julia's startup, package loading, and first-call
JIT compilation every time, through the same `julia -e 'using ...'` path a shell
user would take; Fatou, a compiled binary, pays only process startup through
`fatou format`. Only one file (`parse_stream.jl`) is measured, since the numbers
are dominated by fixed startup and compilation cost, not by the file's size.

{{ benchmark-cold-start }}
