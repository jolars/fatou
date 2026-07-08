#!/usr/bin/env bash
# Warm-loop benchmark: Fatou vs Runic vs JuliaFormatter over JuliaSyntax.jl.
#
# Each tool's pure String -> String formatter is timed in a warm loop inside its
# own runtime (Rust for Fatou, a long-lived Julia process for Runic and
# JuliaFormatter), so process startup and first-call JIT are excluded. Results
# land in bench/results.json and docs/src/performance-table.md.
#
# Scenarios:
#   single_file  one substantial file all three tools handle (parse_stream.jl)
#   project      every .jl in JuliaSyntax/src (per-tool coverage; skips reported)
#
# Env overrides: SINGLE_ITERS, PROJECT_ITERS, WARMUP, SINGLE_FILE, JULIA_PROJECT.
# JULIA_PROJECT points Julia at an environment that provides Runic and
# JuliaFormatter; leave it unset to use the devenv default env.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH="$ROOT/bench"
CORPUS="$BENCH/corpus/JuliaSyntax"
SRC="$CORPUS/src"

SINGLE_ITERS="${SINGLE_ITERS:-50}"
PROJECT_ITERS="${PROJECT_ITERS:-20}"
WARMUP="${WARMUP:-3}"
SINGLE_FILE="${SINGLE_FILE:-parse_stream.jl}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# --- corpus ------------------------------------------------------------------
"$BENCH/corpus/download.sh"

single_list="$TMP/single.txt"
project_list="$TMP/project.txt"
printf '%s\n' "$SRC/$SINGLE_FILE" > "$single_list"
ls "$SRC"/*.jl > "$project_list"

# --- Fatou (Rust warm harness) ----------------------------------------------
echo "==> building fatou (release)"
cargo build --release --quiet --manifest-path "$ROOT/Cargo.toml"

run_fatou() {
  local list="$1" iters="$2" out="$3"
  FATOU_BENCH_FILELIST="$list" \
  FATOU_BENCH_ITERATIONS="$iters" \
  FATOU_BENCH_WARMUP="$WARMUP" \
  FATOU_BENCH_OUTPUT_JSON="$out" \
    cargo bench --quiet --manifest-path "$ROOT/Cargo.toml" --bench format_compare
}
echo "==> fatou: single file"
run_fatou "$single_list" "$SINGLE_ITERS" "$TMP/fatou_single.json"
echo "==> fatou: project"
run_fatou "$project_list" "$PROJECT_ITERS" "$TMP/fatou_project.json"

# --- Runic + JuliaFormatter (Julia warm harness) -----------------------------
julia_args=(--startup-file=no)
[[ -n "${JULIA_PROJECT:-}" ]] && julia_args+=(--project="$JULIA_PROJECT")
echo "==> julia tools: single file"
julia "${julia_args[@]}" "$BENCH/julia_bench.jl" \
  "$single_list" "$SINGLE_ITERS" "$WARMUP" "$TMP/julia_single.json"
echo "==> julia tools: project"
julia "${julia_args[@]}" "$BENCH/julia_bench.jl" \
  "$project_list" "$PROJECT_ITERS" "$WARMUP" "$TMP/julia_project.json"

if grep -q '"tool":"runic","available":false' "$TMP/julia_single.json"; then
  echo "WARNING: Runic is not loadable in this Julia environment." >&2
  echo "         Reload the devenv/direnv shell (Runic is in devenv.nix) and re-run." >&2
fi

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
  --meta "$TMP/meta.json" \
  --out "$BENCH/results.json" \
  --table "$ROOT/docs/src/performance-table.md"

echo "==> done"
