#!/usr/bin/env bash
# Language-server speed and memory benchmark: Fatou against the two Julia
# language servers. Results land in bench/memory.json. Each server opens the
# same workspace, runs a scripted editing session, and is sampled across its
# process tree. See bench/lsp_memory.py.
#
# Unlike the throughput benchmark, this one reports absolute megabytes as well as
# ratios: for memory the absolute figure is the thing a user feels, and the tools
# are not separated by a startup floor here so much as by what they load. The
# comparison is emphatically **not** like-for-like work -- JETLS runs real type
# inference and LanguageServer.jl indexes the environment through a Julia
# runtime, while Fatou is static and has no Julia runtime at all. What it
# measures is what an editor session costs, not the price of equivalent analysis.
#
# Workspace: the pinned DataFrames.jl corpus checkout, instantiated so the Julia
# servers see a resolvable environment (this writes a Manifest.toml into the
# gitignored checkout and populates the shared Julia depot).
#
# Env overrides: BENCH_JULIA, SETTLE_TIMEOUT, QUIET_SECONDS, LSP_LATENCY_RUNS,
# LSP_LATENCY_WARMUPS, and OPEN_FILE_COUNT.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH="$ROOT/bench"
CORPUS="$BENCH/corpus"
WORKSPACE="$CORPUS/DataFrames"
LSENV="$BENCH/lsenv"

export BENCH_JULIA="${BENCH_JULIA:-julia}"
export JULIA_DEPOT_PATH="$LSENV/depot:"
LSP_SESSIONS="${LSP_SESSIONS:-6}"
SETTLE_TIMEOUT="${SETTLE_TIMEOUT:-300}"
BENCH_OUT="${BENCH_OUT:-$BENCH/memory.json}"
BENCH_JULIA="$(realpath "$(command -v "$BENCH_JULIA")")"
export PATH="$(dirname "$BENCH_JULIA"):$PATH"
QUIET_SECONDS="${QUIET_SECONDS:-5}"
OPEN_FILE_COUNT="${OPEN_FILE_COUNT:-5}"
LSP_LATENCY_RUNS="${LSP_LATENCY_RUNS:-20}"
LSP_LATENCY_WARMUPS="${LSP_LATENCY_WARMUPS:-2}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# --- corpus and environments -------------------------------------------------
"$CORPUS/download.sh"

echo "==> instantiating the workspace ($WORKSPACE)"
JULIA_PROJECT="" "$BENCH_JULIA" --startup-file=no --project="$WORKSPACE" -e '
    using Pkg
    Pkg.instantiate()
    Pkg.precompile()
'

"$LSENV/setup.sh"

echo "==> building fatou (release)"
cargo build --release --quiet --manifest-path "$ROOT/Cargo.toml"

# --- the files each server opens ---------------------------------------------
# The largest files in the tree, which is where a per-file cost actually shows
# above the noise. Sorted by size and then by path so the selection is a
# property of the pinned checkout, not of directory iteration order.
mapfile -t OPEN_FILES < <(
  find "$WORKSPACE/src" -name '*.jl' -printf '%s\t%p\n' |
    sort -k1,1rn -k2,2 |
    head -n "$OPEN_FILE_COUNT" |
    cut -f2
)
if [[ ${#OPEN_FILES[@]} -eq 0 ]]; then
  echo "ERROR: no .jl files found under $WORKSPACE/src" >&2
  exit 1
fi
echo "==> opening ${#OPEN_FILES[@]} files per server"
LSP_LATENCY_FILES=${#OPEN_FILES[@]}
if ((LSP_LATENCY_FILES > 3)); then
  LSP_LATENCY_FILES=3
fi

# --- language servers ---------------------------------------------------------
# Every server sees the same JULIA_PROJECT, so they agree on which environment
# the workspace belongs to: Fatou indexes that environment's packages, and the
# Julia servers resolve against it.
#
# JETLS gets --threads=auto because that is what its own [apps.jetls] julia_flags
# declare; LanguageServer.jl's clients run it single-threaded, and both are left
# at the configuration their users actually get.
JULIA_PROJECT="$WORKSPACE" python3 "$BENCH/lsp_memory.py" \
  --project "$WORKSPACE" \
  --files "${OPEN_FILES[@]}" \
  --server "fatou=\"$ROOT/target/release/fatou\" lsp" \
  --server "languageserver=\"$BENCH_JULIA\" --startup-file=no --project=\"$LSENV/languageserver\" \"$LSENV/ls_runner.jl\" \"$WORKSPACE\"" \
  --server "jetls=\"$BENCH_JULIA\" --startup-file=no --history-file=no --threads=auto --project=\"$LSENV/jetls\" -m JETLS serve" \
  --sessions "$LSP_SESSIONS" \
  --settle-timeout "$SETTLE_TIMEOUT" \
  --quiet-seconds "$QUIET_SECONDS" \
  --latency-runs "$LSP_LATENCY_RUNS" \
  --latency-warmups "$LSP_LATENCY_WARMUPS" \
  --stderr-dir "$LSENV" \
  --out "$TMP/lsp.json"

# --- metadata -----------------------------------------------------------------
cpu="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' || echo unknown)"
mem_kb="$(grep -m1 MemTotal /proc/meminfo 2>/dev/null | awk '{print $2}' || echo 0)"
fatou_version="$("$ROOT/target/release/fatou" --version | awk '{print $2}')"
julia_version="$("$BENCH_JULIA" --startup-file=no --version | awk '{print $3}')"

cat >"$TMP/meta.json" <<EOF
{
  "host": "$(uname -n)",
  "os": "$(uname -s) $(uname -m)",
  "cpu": "$cpu",
  "memory_gb": $((mem_kb / 1024 / 1024)),
  "settle_timeout": $SETTLE_TIMEOUT,
  "quiet_seconds": $QUIET_SECONDS,
  "lsp_latency_runs": $LSP_LATENCY_RUNS,
  "lsp_latency_warmups": $LSP_LATENCY_WARMUPS,
  "lsp_latency_files": $LSP_LATENCY_FILES,
  "corpora": $(cat "$CORPUS/manifest.json"),
  "servers": $(cat "$LSENV/manifest.json"),
  "fatou_commit": "$(git -C "$ROOT" rev-parse HEAD)",
  "versions": {"fatou": "$fatou_version", "julia": "$julia_version"}
}
EOF

python3 "$BENCH/memory_merge.py" \
  --lsp "$TMP/lsp.json" \
  --meta "$TMP/meta.json" \
  --out "$BENCH_OUT"

echo "==> done: $BENCH_OUT"
