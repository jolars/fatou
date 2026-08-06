#!/usr/bin/env bash
# Warm-loop benchmark: Fatou vs Runic vs JuliaFormatter over two real-world
# Julia corpora.
#
# Each tool is timed in a warm loop inside its own runtime (Rust for Fatou, a
# long-lived Julia process for Runic and JuliaFormatter), so process startup and
# first-call JIT are excluded. Results land in bench/results.json, which the docs
# `doc-utils` mdBook preprocessor reads to render the performance page.
#
# Corpora (see bench/corpus/download.sh): JuliaSyntax.jl, the parser Fatou
# targets for parity, and DataFrames.jl, ordinary docstring- and macro-heavy
# library code about 2.6x its size.
#
# Scenarios (see SCENARIOS below):
#   single_*   one file through each tool's pure String -> String formatter,
#              chosen to span both size and shape: dense parser internals
#              (parse_stream.jl), a large flat macro/data table (kinds.jl), and
#              a docstring-heavy application file (abstractdataframe.jl).
#   project_*  a whole source tree via each tool's directory entry point:
#              Fatou's parallel `check_paths` (discovery + read + rayon-parallel
#              format, read-only) and JuliaFormatter's recursive
#              `format(dir; overwrite=false)`. Runic has no in-process directory
#              API, so it is excluded from these scenarios.
#   cold_start the opposite of the warm loop: one fresh process per run on
#              parse_stream.jl, so process startup and (for the Julia tools)
#              package load and first-call JIT all count. See bench/cold_start.py.
#
# Env overrides: SINGLE_ITERS, PROJECT_ITERS, COLD_ITERS, WARMUP, COLD_FILE,
# JULIA_PROJECT.
# JULIA_PROJECT points Julia at an environment that provides Runic and
# JuliaFormatter; leave it unset to use the devenv default env.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH="$ROOT/bench"
CORPUS="$BENCH/corpus"

SINGLE_ITERS="${SINGLE_ITERS:-50}"
PROJECT_ITERS="${PROJECT_ITERS:-20}"
COLD_ITERS="${COLD_ITERS:-5}"
WARMUP="${WARMUP:-3}"
COLD_FILE="${COLD_FILE:-JuliaSyntax/src/parse_stream.jl}"

# key|label|target (relative to bench/corpus)|mode
# Order here is the order the docs render scenarios in.
SCENARIOS=(
  "single_parse_stream|Single file: parse_stream.jl|JuliaSyntax/src/parse_stream.jl|file"
  "single_kinds|Single file: kinds.jl|JuliaSyntax/src/kinds.jl|file"
  "single_abstractdataframe|Single file: abstractdataframe.jl|DataFrames/src/abstractdataframe/abstractdataframe.jl|file"
  "project_juliasyntax|Project: JuliaSyntax|JuliaSyntax/src|dir"
  "project_dataframes|Project: DataFrames|DataFrames/src|dir"
)

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# --- corpus ------------------------------------------------------------------
"$CORPUS/download.sh"

# --- Fatou (Rust warm harness) ----------------------------------------------
echo "==> building fatou (release)"
cargo build --release --quiet --manifest-path "$ROOT/Cargo.toml"

# The Julia harness is one process per scenario; both harnesses emit the same
# per-file JSON schema so merge.py can pair them up scenario by scenario.
# --threads=auto lets JuliaFormatter's recursive directory mode (Threads.@threads)
# use every core in the project scenarios, comparable to Fatou's rayon pool; the
# single-threaded string loop is unaffected.
julia_args=(--startup-file=no --threads=auto)
[[ -n "${JULIA_PROJECT:-}" ]] && julia_args+=(--project="$JULIA_PROJECT")

merge_args=()
first_file_key=""
for entry in "${SCENARIOS[@]}"; do
  IFS='|' read -r key label rel mode <<<"$entry"
  target="$CORPUS/$rel"

  if [[ ! -e "$target" ]]; then
    echo "ERROR: scenario '$key' target is missing: $target" >&2
    exit 1
  fi

  echo "==> $key ($mode): $rel"
  if [[ "$mode" == dir ]]; then
    iters="$PROJECT_ITERS"
    FATOU_BENCH_DIR="$target" \
    FATOU_BENCH_ITERATIONS="$iters" \
    FATOU_BENCH_WARMUP="$WARMUP" \
    FATOU_BENCH_OUTPUT_JSON="$TMP/fatou_$key.json" \
      cargo bench --quiet --manifest-path "$ROOT/Cargo.toml" --bench format_compare
    julia "${julia_args[@]}" "$BENCH/julia_bench.jl" \
      "$target" "$iters" "$WARMUP" "$TMP/julia_$key.json" dir
  else
    iters="$SINGLE_ITERS"
    printf '%s\n' "$target" >"$TMP/list_$key.txt"
    FATOU_BENCH_FILELIST="$TMP/list_$key.txt" \
    FATOU_BENCH_ITERATIONS="$iters" \
    FATOU_BENCH_WARMUP="$WARMUP" \
    FATOU_BENCH_OUTPUT_JSON="$TMP/fatou_$key.json" \
      cargo bench --quiet --manifest-path "$ROOT/Cargo.toml" --bench format_compare
    julia "${julia_args[@]}" "$BENCH/julia_bench.jl" \
      "$TMP/list_$key.txt" "$iters" "$WARMUP" "$TMP/julia_$key.json"
    [[ -n "$first_file_key" ]] || first_file_key="$key"
  fi

  merge_args+=(--scenario "$key" "$label" "$rel" "$TMP/fatou_$key.json" "$TMP/julia_$key.json")
done

# Runic is reported unavailable in `dir` mode by design, so only a single-file
# scenario can tell a missing package apart from an excluded one.
if [[ -n "$first_file_key" ]] &&
  grep -q '"tool":"runic","available":false' "$TMP/julia_$first_file_key.json"; then
  echo "WARNING: Runic is not loadable in this Julia environment." >&2
  echo "         Run inside the devenv shell (which sets JULIA_PROJECT to the repo's" >&2
  echo "         pinned project, where Runic lives), or pass --project=., and re-run." >&2
fi

# --- cold start (fresh-process invocation, single file) ----------------------
# Unlike the warm loops above, this times a full CLI invocation per iteration:
# process startup plus, for the Julia tools, package load and first-call JIT.
echo "==> cold start: $COLD_FILE (fresh process per run)"
cold_project_args=()
[[ -n "${JULIA_PROJECT:-}" ]] && cold_project_args=(--julia-project "$JULIA_PROJECT")
python3 "$BENCH/cold_start.py" \
  --file "$CORPUS/$COLD_FILE" \
  --iterations "$COLD_ITERS" \
  --out "$TMP/cold.json" \
  --fatou "$ROOT/target/release/fatou" \
  --julia julia \
  "${cold_project_args[@]}"

# --- metadata ----------------------------------------------------------------
cpu="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' || echo unknown)"
cat >"$TMP/meta.json" <<EOF
{
  "host": "$(uname -n)",
  "os": "$(uname -s) $(uname -m)",
  "cpu": "$cpu",
  "iterations_single": $SINGLE_ITERS,
  "iterations_project": $PROJECT_ITERS,
  "iterations_cold": $COLD_ITERS,
  "warmup": $WARMUP,
  "corpora": $(cat "$CORPUS/manifest.json")
}
EOF

# --- merge -------------------------------------------------------------------
python3 "$BENCH/merge.py" \
  "${merge_args[@]}" \
  --cold "$TMP/cold.json" \
  --meta "$TMP/meta.json" \
  --out "$BENCH/results.json"

echo "==> done"
