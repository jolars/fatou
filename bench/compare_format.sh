#!/usr/bin/env bash
# Warm-loop benchmark: Fatou vs Runic vs JuliaFormatter over JuliaSyntax.jl.
#
# Each tool is timed in a warm loop inside its own runtime (Rust for Fatou, a
# long-lived Julia process for Runic and JuliaFormatter), so process startup and
# first-call JIT are excluded. Results land in bench/results.json, which the docs
# `doc-utils` mdBook preprocessor reads to render the performance page.
#
# Scenarios:
#   single_file  one substantial file all three tools handle (parse_stream.jl),
#                via each tool's pure String -> String formatter.
#   project      the whole JuliaSyntax/src tree via each tool's directory entry
#                point: Fatou's parallel `check_paths` (discovery + read +
#                rayon-parallel format, read-only) and JuliaFormatter's recursive
#                `format(dir; overwrite=false)`. Runic has no in-process
#                directory API, so it is excluded from this scenario.
#   cold_start   the opposite of the warm loop: one fresh process per run on the
#                single file, so process startup and (for the Julia tools) package
#                load and first-call JIT all count. See bench/cold_start.py.
#
# Env overrides: SINGLE_ITERS, PROJECT_ITERS, COLD_ITERS, WARMUP, SINGLE_FILE,
# JULIA_PROJECT.
# JULIA_PROJECT points Julia at an environment that provides Runic and
# JuliaFormatter; leave it unset to use the devenv default env.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH="$ROOT/bench"
CORPUS="$BENCH/corpus/JuliaSyntax"
SRC="$CORPUS/src"

SINGLE_ITERS="${SINGLE_ITERS:-50}"
PROJECT_ITERS="${PROJECT_ITERS:-20}"
COLD_ITERS="${COLD_ITERS:-5}"
WARMUP="${WARMUP:-3}"
SINGLE_FILE="${SINGLE_FILE:-parse_stream.jl}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# --- corpus ------------------------------------------------------------------
"$BENCH/corpus/download.sh"

single_list="$TMP/single.txt"
printf '%s\n' "$SRC/$SINGLE_FILE" > "$single_list"

# --- Fatou (Rust warm harness) ----------------------------------------------
echo "==> building fatou (release)"
cargo build --release --quiet --manifest-path "$ROOT/Cargo.toml"

echo "==> fatou: single file"
FATOU_BENCH_FILELIST="$single_list" \
FATOU_BENCH_ITERATIONS="$SINGLE_ITERS" \
FATOU_BENCH_WARMUP="$WARMUP" \
FATOU_BENCH_OUTPUT_JSON="$TMP/fatou_single.json" \
  cargo bench --quiet --manifest-path "$ROOT/Cargo.toml" --bench format_compare
echo "==> fatou: project (directory)"
FATOU_BENCH_DIR="$SRC" \
FATOU_BENCH_ITERATIONS="$PROJECT_ITERS" \
FATOU_BENCH_WARMUP="$WARMUP" \
FATOU_BENCH_OUTPUT_JSON="$TMP/fatou_project.json" \
  cargo bench --quiet --manifest-path "$ROOT/Cargo.toml" --bench format_compare

# --- Runic + JuliaFormatter (Julia warm harness) -----------------------------
# --threads=auto lets JuliaFormatter's recursive directory mode (Threads.@threads)
# use every core in the project scenario, comparable to Fatou's rayon pool; the
# single-threaded string loop is unaffected.
julia_args=(--startup-file=no --threads=auto)
[[ -n "${JULIA_PROJECT:-}" ]] && julia_args+=(--project="$JULIA_PROJECT")
echo "==> julia tools: single file"
julia "${julia_args[@]}" "$BENCH/julia_bench.jl" \
  "$single_list" "$SINGLE_ITERS" "$WARMUP" "$TMP/julia_single.json"
echo "==> julia tools: project (directory)"
julia "${julia_args[@]}" "$BENCH/julia_bench.jl" \
  "$SRC" "$PROJECT_ITERS" "$WARMUP" "$TMP/julia_project.json" dir

if grep -q '"tool":"runic","available":false' "$TMP/julia_single.json"; then
  echo "WARNING: Runic is not loadable in this Julia environment." >&2
  echo "         Reload the devenv/direnv shell (Runic is in devenv.nix) and re-run." >&2
fi

# --- cold start (fresh-process invocation, single file) ----------------------
# Unlike the warm loops above, this times a full CLI invocation per iteration:
# process startup plus, for the Julia tools, package load and first-call JIT.
echo "==> cold start: single file (fresh process per run)"
cold_project_args=()
[[ -n "${JULIA_PROJECT:-}" ]] && cold_project_args=(--julia-project "$JULIA_PROJECT")
python3 "$BENCH/cold_start.py" \
  --file "$SRC/$SINGLE_FILE" \
  --iterations "$COLD_ITERS" \
  --out "$TMP/cold.json" \
  --fatou "$ROOT/target/release/fatou" \
  --julia julia \
  "${cold_project_args[@]}"

# --- metadata ----------------------------------------------------------------
cpu="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' || echo unknown)"
commit="$(git -C "$CORPUS" rev-parse --short HEAD 2>/dev/null || echo unknown)"
tag="$(git -C "$CORPUS" describe --tags --always 2>/dev/null || echo unknown)"
cat > "$TMP/meta.json" <<EOF
{
  "host": "$(uname -n)",
  "os": "$(uname -s) $(uname -m)",
  "cpu": "$cpu",
  "iterations_single": $SINGLE_ITERS,
  "iterations_project": $PROJECT_ITERS,
  "iterations_cold": $COLD_ITERS,
  "warmup": $WARMUP,
  "single_target": "$SINGLE_FILE",
  "project_target": "JuliaSyntax/src",
  "corpus": {
    "repo": "https://github.com/JuliaLang/JuliaSyntax.jl",
    "tag": "$tag",
    "commit": "$commit"
  }
}
EOF

# --- merge -------------------------------------------------------------------
python3 "$BENCH/merge.py" \
  --fatou-single "$TMP/fatou_single.json" --julia-single "$TMP/julia_single.json" \
  --fatou-project "$TMP/fatou_project.json" --julia-project "$TMP/julia_project.json" \
  --cold "$TMP/cold.json" \
  --meta "$TMP/meta.json" \
  --out "$BENCH/results.json"

echo "==> done"
