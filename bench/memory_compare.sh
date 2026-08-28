#!/usr/bin/env bash
# Language-server speed and memory benchmark: Fatou against the two Julia
# language servers, plus Fatou's own one-shot CLI memory runs. Results land in
# bench/memory.json, which the docs `doc-utils` mdBook preprocessor reads to
# render the language-server section of the performance page.
#
# Two scopes, because Fatou is used two ways:
#
#   language server  the resident case. Each server opens the same workspace,
#                    is put through the same scripted session, and is sampled
#                    across its whole process tree. See bench/lsp_memory.py.
#   CLI              the one-shot case. Peak RSS of `fatou format`/`lint`/`parse`
#                    over one file and over a whole tree. See bench/cli_memory.py.
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
# Env overrides: SETTLE_TIMEOUT, QUIET_SECONDS, LSP_LATENCY_RUNS,
# LSP_LATENCY_WARMUPS, CLI_RUNS, OPEN_FILE_COUNT, and GNU_TIME for a GNU `time`
# outside PATH (the devenv shell provides one).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH="$ROOT/bench"
CORPUS="$BENCH/corpus"
WORKSPACE="$CORPUS/DataFrames"
LSENV="$BENCH/lsenv"

SETTLE_TIMEOUT="${SETTLE_TIMEOUT:-300}"
QUIET_SECONDS="${QUIET_SECONDS:-5}"
CLI_RUNS="${CLI_RUNS:-5}"
OPEN_FILE_COUNT="${OPEN_FILE_COUNT:-5}"
LSP_LATENCY_RUNS="${LSP_LATENCY_RUNS:-20}"
LSP_LATENCY_WARMUPS="${LSP_LATENCY_WARMUPS:-2}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# --- corpus and environments -------------------------------------------------
"$CORPUS/download.sh"

echo "==> instantiating the workspace ($WORKSPACE)"
JULIA_PROJECT="" julia --startup-file=no --project="$WORKSPACE" -e '
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
  --server "fatou=$ROOT/target/release/fatou lsp" \
  --server "languageserver=julia --startup-file=no --project=$LSENV/languageserver $LSENV/ls_runner.jl $WORKSPACE" \
  --server "jetls=julia --startup-file=no --threads=auto --project=$LSENV/jetls $LSENV/jetls_runner.jl" \
  --settle-timeout "$SETTLE_TIMEOUT" \
  --quiet-seconds "$QUIET_SECONDS" \
  --latency-runs "$LSP_LATENCY_RUNS" \
  --latency-warmups "$LSP_LATENCY_WARMUPS" \
  --stderr-dir "$LSENV" \
  --out "$TMP/lsp.json"

# --- one-shot CLI -------------------------------------------------------------
echo "==> memory: fatou CLI"
cli_args=()
[[ -n "${GNU_TIME:-}" ]] && cli_args=(--time "$GNU_TIME")
python3 "$BENCH/cli_memory.py" \
  --fatou "$ROOT/target/release/fatou" \
  --project "$WORKSPACE" \
  --runs "$CLI_RUNS" \
  "${cli_args[@]}" \
  --out "$TMP/cli.json"

# --- metadata -----------------------------------------------------------------
cpu="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | sed 's/.*: //' || echo unknown)"
mem_kb="$(grep -m1 MemTotal /proc/meminfo 2>/dev/null | awk '{print $2}' || echo 0)"
fatou_version="$("$ROOT/target/release/fatou" --version | awk '{print $2}')"
julia_version="$(julia --startup-file=no --version | awk '{print $3}')"

cat >"$TMP/meta.json" <<EOF
{
  "host": "$(uname -n)",
  "os": "$(uname -s) $(uname -m)",
  "cpu": "$cpu",
  "memory_gb": $((mem_kb / 1024 / 1024)),
  "settle_timeout": $SETTLE_TIMEOUT,
  "quiet_seconds": $QUIET_SECONDS,
  "cli_runs": $CLI_RUNS,
  "lsp_latency_runs": $LSP_LATENCY_RUNS,
  "lsp_latency_warmups": $LSP_LATENCY_WARMUPS,
  "lsp_latency_files": $LSP_LATENCY_FILES,
  "corpora": $(cat "$CORPUS/manifest.json"),
  "servers": $(cat "$LSENV/manifest.json"),
  "versions": {"fatou": "$fatou_version", "julia": "$julia_version"}
}
EOF

python3 "$BENCH/memory_merge.py" \
  --lsp "$TMP/lsp.json" \
  --cli "$TMP/cli.json" \
  --meta "$TMP/meta.json" \
  --out "$BENCH/memory.json"

echo "==> done: $BENCH/memory.json"
