# Performance

These benchmarks compare Fatou's formatting speed with Runic and JuliaFormatter,
and its language-server speed and memory use with LanguageServer.jl and JETLS.
Fatou runs as a compiled Rust program; the comparison tools run in Julia.

## Formatter

We compare Fatou with [Runic](https://github.com/fredrikekre/Runic.jl) and
[JuliaFormatter](https://github.com/domluna/JuliaFormatter.jl) on source files
from [JuliaSyntax.jl](https://github.com/JuliaLang/JuliaSyntax.jl) and
[DataFrames.jl](https://github.com/JuliaData/DataFrames.jl). Each tool uses its
default formatting style.

The single-file and project benchmarks keep each tool loaded between runs, so
Julia startup and compilation do not count. The cold-start benchmark below
includes those costs.

{{ benchmark-meta }}

### Single files

The three files cover parser internals (`parse_stream.jl`), a large data table
(`kinds.jl`), and docstring- and macro-heavy library code
(`abstractdataframe.jl`).

The charts show time relative to Fatou: **1 is the Fatou baseline, and 10 means
ten times as long**. Lower is faster. Each step on the logarithmic axis is a
tenfold increase. Tooltips and expandable tables show the measured times and
throughput.

{{ benchmark-results-single }}

### Whole projects

This comparison checks formatting across each project's `src/` tree, including
file discovery, reading files, and parallel processing. Runic is omitted because
it has no in-process directory API. The tables list any files a tool skipped.

{{ benchmark-results-project }}

### Cold start

Each run starts a fresh process, formats `parse_stream.jl` once, and exits. This
is the cost of a command-line invocation, including Julia startup, package
loading, and compilation for Runic and JuliaFormatter.

{{ benchmark-cold-start }}

## Language servers

We compare Fatou with
[LanguageServer.jl](https://github.com/julia-vscode/LanguageServer.jl) and
[JETLS](https://github.com/aviatesk/JETLS.jl) in the same DataFrames.jl
workspace. The benchmark opens the same files, collects diagnostics, and sends
editor requests to each server.

The servers provide different analyses: Fatou analyzes source statically,
LanguageServer.jl analyzes the Julia workspace and its environment, and JETLS
performs type inference through JET. These results show the time and memory each
editor session uses.

{{ memory-meta }}

### Speed

The startup measurements cover three stages:

- **Initialize** measures the server's response to the editor's initialization
  request.
- **Workspace quiescence** estimates when background CPU activity settles.
- **Open files ready** measures the time from opening the files until the
  harness receives diagnostics for each file and background work settles.

Artifacts from the earlier harness label CPU-based estimates as readiness,
including the estimate after opening files.

After startup, we time requests for document symbols, hover information,
definitions, references, and renaming. The chart shows median response times.
Tooltips and expandable tables include the 95th percentile (p95) and the number
of results returned, which can differ between servers. The p95 is the time
within which 95% of requests finish. Lower is faster.

{{ lsp-speed }}

### Memory

The chart shows resident memory (**RSS**) for each server and its child
processes. **Baseline** is after initialization, **Settled** is after the open
files have been analyzed, and **Peak** is the highest sample during the session.
The current harness reports the median milestone across six fresh sessions per
server, after an untimed cache-priming session. Earlier artifacts record one
session per server.

{{ memory-servers }}

## Command-line memory use

For a CI job or pre-commit hook, Fatou runs once and exits. This table shows
peak memory use for `format`, `lint`, and `parse` on individual files and whole
source trees. Whole-tree runs process files in parallel, so several syntax trees
can be in memory at once.

{{ memory-cli }}

## Reproduce the results

Run this command from the repository root in the devenv shell:

```sh
task bench
```

Run `task bench-format` for the formatter benchmarks or `task bench-lsp` for the
language-server benchmarks. Devenv supplies stable Julia for both suites. The
tasks write `bench/results.json` and `bench/memory.json`, respectively. These
committed files supply the charts, machine details, and tool versions shown
above. Building the documentation reads them without running the benchmarks.

Set `BENCH_OUT` to write an exploratory comparison to a separate file. The
current harness also measures completion after an unsaved edit and diagnostics
after introducing and repairing a syntax error. Only validated responses
contribute to the latency distributions.

### Compare two Fatou builds

Use release binaries and an instantiated DataFrames workspace. The comparison
task above prepares the workspace and its Julia depot. If you only want to run
Fatou, prepare the workspace directly:

```sh
task bench-corpus
JULIA_DEPOT_PATH="$PWD/bench/lsenv/depot:" \
  "$BENCH_JULIA" --startup-file=no \
  --project=bench/corpus/DataFrames -e 'using Pkg; Pkg.instantiate(); Pkg.precompile()'
```

```sh
task bench-lsp-ab -- \
  --baseline /path/to/baseline/fatou --candidate /path/to/candidate/fatou \
  --out bench/lsp-ab.tmp.json
```

Point both options at the same binary first to estimate measurement noise. The
output includes raw samples, per-session results, and paired deltas (candidate
minus baseline). Run on an idle machine so unrelated work has less effect on the
timings.
