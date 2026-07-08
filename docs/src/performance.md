# Performance

Fatou is a compiled Rust formatter competing with two Julia-native tools,
[Runic](https://github.com/fredrikekre/Runic.jl) and
[JuliaFormatter](https://github.com/domluna/JuliaFormatter.jl). This page
compares their raw formatting throughput.

## Methodology

All three tools expose a pure `String -> String` formatter
(`fatou::formatter::format`, `Runic.format_string`,
`JuliaFormatter.format_text`). We measure that function directly in a **warm
loop**: the tool is loaded once, run through a few warmup calls, and then timed
over many iterations. This deliberately **excludes process startup and
first-call JIT compilation** for the Julia tools, which would otherwise dominate
and obscure the actual formatting cost. In other words, these numbers reflect a
long-lived editor or language-server session, not the cold `julia -e ...`
command-line invocation.

Because each tool runs in its own runtime, we report **throughput in MB/s**,
which normalizes for byte count and stays comparable even when tools cover
different files. Each tool formats with **its own default style**; we are
measuring speed, not comparing output. A file counts for a tool only if that
tool formats it without error, and any skips are reported.

The corpus is [JuliaSyntax.jl](https://github.com/JuliaLang/JuliaSyntax.jl) (the
parser Fatou targets for parity), pinned to a tag. Two scenarios: a single
substantial source file, and the whole `src/` directory.

Reproduce with `task bench` (after reloading the devenv shell so `Runic` is on
the Julia path). Results are written to `bench/results.json`.

## Results

{{#include performance-table.md}}
