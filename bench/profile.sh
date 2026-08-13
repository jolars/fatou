#!/usr/bin/env bash
# Profile the formatter: one perf recording, two views.
#
# Unlike bench/compare_format.sh, which answers "how fast are we against Runic
# and JuliaFormatter", this answers "where does the time go". It reuses the same
# warm-loop harness (benches/format_compare.rs) as the thing being sampled, so
# the profile describes the same `format(&str)` call the published numbers do,
# with process startup amortized away by the iteration count.
#
# Built with the `profiling` cargo profile: release codegen, symbols kept.
# `[profile.release] strip = "symbols"` would otherwise leave every frame in the
# flamegraph unresolved.
#
# Two artifacts, both gitignored (a profile is a local observation, not a
# tracked result the way bench/results.json is):
#   bench/profile.svg    the flamegraph
#   bench/profile.data   the raw perf recording, for your own `perf report`
#
# Usage:
#   ./bench/profile.sh                       # JuliaSyntax/src/parser.jl
#   ./bench/profile.sh path/to/file.jl       # a file you care about
#   ./bench/profile.sh --dir bench/corpus/DataFrames   # the parallel check path
#
# Env overrides: ITERATIONS, WARMUP, FREQ, TOP.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH="$ROOT/bench"
CORPUS="$BENCH/corpus"

# Defaulted per mode further down: one file is cheap, a whole tree is not.
ITERATIONS="${ITERATIONS:-}"
WARMUP="${WARMUP:-3}"
FREQ="${FREQ:-997}"
TOP="${TOP:-40}"

SVG="$BENCH/profile.svg"
PERFDATA="$BENCH/profile.data"

for tool in perf flamegraph; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "error: $tool not found; it ships in the devenv shell" >&2
    exit 1
  }
done

paranoid="$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 4)"
if [ "$paranoid" -gt 2 ]; then
  echo "error: kernel.perf_event_paranoid=$paranoid blocks user-space sampling." >&2
  echo "       sudo sysctl kernel.perf_event_paranoid=2" >&2
  exit 1
fi

# --- what to profile -------------------------------------------------------

MODE="file"
TARGET=""
if [ "${1:-}" = "--dir" ]; then
  MODE="dir"
  TARGET="${2:?--dir needs a directory}"
elif [ $# -gt 0 ]; then
  TARGET="$1"
else
  TARGET="$CORPUS/JuliaSyntax/src/parser.jl"
  if [ ! -f "$TARGET" ]; then
    echo "error: default corpus file missing. Run: task bench-corpus" >&2
    exit 1
  fi
fi

# perf is run from $BENCH (it drops perf.data in the cwd), so a relative target
# would resolve against the wrong directory and the harness would silently
# measure nothing. Absolutize before anything else touches it.
case "$TARGET" in
  /*) ;;
  *) TARGET="$PWD/$TARGET" ;;
esac

if [ "$MODE" = "dir" ]; then
  [ -d "$TARGET" ] || { echo "error: no such directory: $TARGET" >&2; exit 1; }
  export FATOU_BENCH_DIR="$TARGET"
  unset FATOU_BENCH_FILELIST || true
  # The directory path is rayon-parallel; sampling across worker threads is
  # still meaningful, but each iteration costs a whole tree, so scale down.
  ITERATIONS="${ITERATIONS:-20}"
  label="$(basename "$TARGET") (directory --check path)"
else
  [ -f "$TARGET" ] || { echo "error: no such file: $TARGET" >&2; exit 1; }
  FILELIST="$(mktemp -t fatou-profile-XXXXXX.txt)"
  trap 'rm -f "$FILELIST"' EXIT
  printf '%s\n' "$TARGET" >"$FILELIST"
  export FATOU_BENCH_FILELIST="$FILELIST"
  unset FATOU_BENCH_DIR || true
  ITERATIONS="${ITERATIONS:-300}"
  label="$(basename "$TARGET") ($(wc -c <"$TARGET" | tr -d ' ') bytes)"
fi

export FATOU_BENCH_ITERATIONS="$ITERATIONS"
export FATOU_BENCH_WARMUP="$WARMUP"
FATOU_BENCH_OUTPUT_JSON="$(mktemp -t fatou-profile-XXXXXX.json)"
export FATOU_BENCH_OUTPUT_JSON

# --- build with symbols ----------------------------------------------------

# Frame pointers, not DWARF CFI. Release codegen omits frame pointers, and perf
# is left unwinding via `.eh_frame` — which on this toolchain drops the
# callchain on most samples, silently collapsing every caller-attributed view to
# self time. Forcing frame pointers costs a register and gives complete stacks,
# 50x smaller recordings, and no dropped chunks. Verify with `--call-graph fp`
# below; the two must always be changed together.
export RUSTFLAGS="${RUSTFLAGS:-} -Cforce-frame-pointers=yes"

echo "==> building format_compare (profile: profiling)"
EXE="$(
  cargo build --profile profiling --bench format_compare --message-format=json 2>/dev/null |
    "${PYTHON:-python3}" -c '
import json, sys
exe = None
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    if msg.get("reason") == "compiler-artifact" and msg.get("executable"):
        if msg["target"]["name"] == "format_compare":
            exe = msg["executable"]
print(exe or "")
'
)"
[ -n "$EXE" ] || { echo "error: could not locate the format_compare binary" >&2; exit 1; }

# --- record ----------------------------------------------------------------

echo "==> profiling $label, $ITERATIONS iterations at ${FREQ}Hz"
cd "$BENCH"
rm -f perf.data perf.data.old
flamegraph --deterministic \
  --cmd "record -F $FREQ --call-graph fp" \
  --title "fatou format" --subtitle "$label" \
  -o "$SVG" -- "$EXE" >/dev/null

[ -f perf.data ] && mv -f perf.data "$PERFDATA"
rm -f perf.data.old

# --- report ----------------------------------------------------------------

# `--no-inline` matters in both reports below: with inline frames expanded, perf
# labels them with the short source name, so `formatter::core::format` shows up
# as a bare `format` among dozens of unrelated `format`s and any grep for a
# phase root silently misses.

echo
echo "==> phase split (inclusive, share of total)"
# `awk NR<=n` rather than `head -n`: under `pipefail`, head closing the pipe
# SIGPIPEs perf report and fails the script on an otherwise complete profile.
perf report --stdio --children -g none --no-inline --percent-limit 0.4 \
  -i "$PERFDATA" 2>/dev/null |
  { grep -E 'fatou_(parser|formatter)|rowan' || true; } |
  awk 'NR<=25'

echo
echo "==> top $TOP by self time"
perf report --stdio --no-children -g none --no-inline --percent-limit 0.5 \
  -i "$PERFDATA" 2>/dev/null |
  { grep -vE '^#|^$' || true; } |
  awk -v n="$TOP" 'NR<=n'

echo
echo "flamegraph: $SVG"
echo "raw perf:   $PERFDATA"
echo "            perf report -i $PERFDATA --no-children -g graph,caller"
