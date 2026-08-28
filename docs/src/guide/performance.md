# Performance

Fatou is a compiled Rust tool where the alternatives are Julia programs running
in a Julia runtime. This page measures where that difference shows up: **how
fast it formats**, against [Runic](https://github.com/fredrikekre/Runic.jl) and
[JuliaFormatter](https://github.com/domluna/JuliaFormatter.jl), and **how
quickly it responds and how much memory it holds**, against the Julia language
servers [LanguageServer.jl](https://github.com/julia-vscode/LanguageServer.jl)
and [JETLS](https://github.com/aviatesk/JETLS.jl).

Every number here comes from a benchmark you can re-run: `task bench` for
formatter throughput and `task bench-lsp` for language-server speed and memory.
Both write a committed artifact that this page renders; nothing is measured at
site-build time, so a figure that moves has to be re-measured and committed
deliberately.

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
- **Project** (one per corpus): the whole `src/` tree, driven through each
  tool's own **directory entry point**, so file discovery, IO, and the tool's
  internal parallel scheduling all count. This is the "format my whole project"
  path. Fatou uses `fatou::formatter::check_paths` (glob/directory discovery
  plus rayon-parallel formatting, read-only); JuliaFormatter uses
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
different sizes, and sharing one axis buries that. In both, Fatou is the
baseline on the dashed line at 1, and every other tool's time is plotted
relative to it, so faster tools fall below the line and slower tools rise above
it.

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

## Language servers

The comparison here is not against the formatters but against
**LanguageServer.jl**, which runs a Julia runtime and indexes the whole
environment through a SymbolServer child process, and **JETLS**, which runs a
Julia runtime and performs real type inference through JET.

**This is not like-for-like work, and the numbers should not be read as if it
were.** Those two servers know things Fatou cannot: JETLS can tell you a method
call will not resolve at the types it actually gets, because it ran the
inference. Fatou's semantics are static, with no Julia runtime anywhere in the
pipeline, so it never pays for one. What follows measures **what an editor
session costs**, not the price of equivalent analysis.

### Methodology

Every server opens the same workspace and is driven through the same scripted
session over stdio, the boring one an editor produces on open:

```text
initialize -> initialized -> wait for the server to go quiet
  -> didOpen the largest files in the tree -> diagnostics
  -> documentSymbol and hover -> wait for it to go quiet again
```

What ends each phase is **quiescence, not a fixed wait**: a phase is over once
aggregate CPU across the process tree stays under 5% of one core for five
seconds. The servers here differ by two orders of magnitude in how long they
take to finish thinking, and any fixed sleep would flatter one end of that
range.

The speed results split an editor session into cold readiness and warm requests:

- **Initialize** is the `initialize` request round trip from a fresh process.
- **Workspace ready** runs from process start to the beginning of the final
  quiet window after background indexing.
- **Open files ready** runs from the burst of `didOpen` notifications through
  diagnostics and the beginning of the next quiet window.
- **Document symbols** and **hover** are warm stdio round trips after the server
  has settled. They span three open files; hover targets the first
  source-defined symbol in each file.
- **Definition**, **references**, and **rename** use `_names` at a pinned call
  site in `selection.jl`. Its definitions live elsewhere, and its references
  span the project. References include declarations; rename constructs the
  `WorkspaceEdit` but does not apply it.

Each target gets two unmeasured warmup rounds, then 20 measured rounds. The
tables report the median and p95. They also report the median serialized result
size and how many symbols, locations, or edits each server returns across how
many files. Those counts matter: two servers are not doing comparable work when
one returns fewer destinations or edits.

Sampling covers the **whole process tree** every 150 ms, so a server that fans
work out to a helper process is charged for it — which is exactly what
LanguageServer.jl's SymbolServer pass is. Three milestones come out of each run:

- **Baseline** — the handshake is done and nothing is open yet. This is the
  floor a server costs for existing.
- **Settled** — files open, diagnostics in, the tree quiet again. This is the
  figure that matters: what the session holds while you work.
- **Peak** — the maximum over every sample. For a server with a short-lived
  helper this is the only milestone that ever sees it, which is why
  LanguageServer.jl peaks well above where it settles.

Resident set size is what the tables report. The harness also records
proportional set size, which splits shared pages between the processes mapping
them; at settle the two agree within a few megabytes for all three servers, so
nothing here is an artifact of double-counted shared memory.

### Setup

{{ memory-meta }}

### Speed

{{ lsp-speed }}

Readiness is measured once per server because starting and indexing the Julia
servers dominates a run. The warm requests have enough repetitions to expose
their distribution, but they intentionally measure cached editor queries—not the
first analysis of a newly opened file.

### Memory

{{ memory-servers }}

Two caveats worth carrying away from that table. Julia's resident memory
includes garbage the collector has not returned yet, and there is no way to ask
a server to collect through the protocol — these are the numbers the operating
system sees, which is also the number your laptop feels, but a forced collection
would hand some of it back. And every server is measured once per run rather
than averaged over many, since each Julia server takes the better part of a
minute to settle; Fatou's figure moves by a few megabytes between runs, and the
Julia servers' by a few tens. The gap is two orders of magnitude wider than
either, so neither caveat threatens the conclusion.

Fatou's own footprint decomposes roughly into a fixed engine cost, the package
index for the workspace's dependency closure, and the open files themselves. The
index is the dominant term, and it scales with how many packages the environment
resolves to — not with how much code you are editing.

### One-shot runs

The language server stays resident; `fatou format`, `fatou lint`, and
`fatou parse` do not. For a CI job or a pre-commit hook what matters is the
high-water mark of a process that lives for a few milliseconds.

{{ memory-cli }}

The whole-tree cases run the files in parallel, so their peak holds several
syntax trees at once. That peak is a function of how many files are in flight
together, not of how long the file list is: pointing Fatou at ten times the code
does not cost ten times the memory.
