#!/usr/bin/env bash
# Fetch the real-world Julia corpus for the formatter benchmark.
#
# We benchmark against JuliaSyntax.jl, the parser Fatou targets for parity, so
# its own source is the corpus Fatou is best equipped to handle. The checkout is
# pinned to a tag for reproducibility and is git-ignored (not vendored).
#
#   Single-file target: bench/corpus/JuliaSyntax/src/parser.jl
#   Project target:     bench/corpus/JuliaSyntax/src/
set -euo pipefail

REPO="https://github.com/JuliaLang/JuliaSyntax.jl"
TAG="v0.4.10"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="$SCRIPT_DIR/JuliaSyntax"

if [[ -d "$DEST/.git" ]]; then
  current="$(git -C "$DEST" describe --tags --always 2>/dev/null || echo "")"
  if [[ "$current" == "$TAG" ]]; then
    echo "corpus: JuliaSyntax already at $TAG ($(git -C "$DEST" rev-parse --short HEAD))"
    exit 0
  fi
  echo "corpus: refreshing JuliaSyntax checkout (was '$current', want '$TAG')"
  rm -rf "$DEST"
fi

echo "corpus: cloning $REPO @ $TAG"
git clone --depth 1 --branch "$TAG" "$REPO" "$DEST"
echo "corpus: JuliaSyntax at $TAG ($(git -C "$DEST" rev-parse --short HEAD))"
