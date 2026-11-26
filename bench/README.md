# Benchmarks

## Reproduce the results

Run this command from the repository root in the devenv shell:

```sh
task bench
```

Run `task bench-format` for the formatter benchmarks or `task bench-lsp` for the
language-server benchmarks. Devenv supplies stable Julia for both suites. The
tasks write `bench/results.json` and `bench/memory.json`, respectively. These
committed files supply the charts, machine details, and tool versions on the
performance page. The formatter suite measures cold-start peak RSS with GNU
`time`. Building the documentation reads the artifacts without running the
benchmarks.

Set `BENCH_OUT` to write an exploratory language-server comparison to a separate
file. The current harness also measures completion after an unsaved edit and
diagnostics after introducing and repairing a syntax error. Only validated
responses contribute to the latency distributions.

## Measure Fatou CLI memory during development

Run `task bench-cli-memory` to measure Fatou’s formatting, linting, and parsing
on the pinned DataFrames corpus. The task writes `bench/cli-memory.tmp.json`.

## Compare two Fatou builds

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
