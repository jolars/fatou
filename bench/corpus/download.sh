#!/usr/bin/env bash
# Fetch the real-world Julia corpora for the formatter benchmark.
#
# Two projects, picked to pull in opposite directions:
#
#   JuliaSyntax.jl  the parser Fatou targets for parity -- dense branching,
#                   large token tables, and the code Fatou is best equipped to
#                   handle. Home turf.
#   DataFrames.jl   ordinary library code of the kind users actually format:
#                   docstring-heavy, macro-heavy, built around a large indexing
#                   DSL, and roughly 2.6x the size of the JuliaSyntax tree.
#
# Checkouts are pinned to a tag for reproducibility and are git-ignored (not
# vendored). `bench/compare_format.sh` defines which files and trees inside them
# are measured.
set -euo pipefail

# name|repo|tag
CORPORA=(
  "JuliaSyntax|https://github.com/JuliaLang/JuliaSyntax.jl|v0.4.10"
  "DataFrames|https://github.com/JuliaData/DataFrames.jl|v1.8.2"
)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SCRIPT_DIR/manifest.json"

for entry in "${CORPORA[@]}"; do
  IFS='|' read -r name repo tag <<<"$entry"
  dest="$SCRIPT_DIR/$name"

  if [[ -d "$dest/.git" ]]; then
    current="$(git -C "$dest" describe --tags --always 2>/dev/null || echo "")"
    if [[ "$current" == "$tag" ]]; then
      echo "corpus: $name already at $tag ($(git -C "$dest" rev-parse --short HEAD))"
      continue
    fi
    echo "corpus: refreshing $name checkout (was '$current', want '$tag')"
    rm -rf "$dest"
  fi

  echo "corpus: cloning $repo @ $tag"
  git clone --depth 1 --branch "$tag" "$repo" "$dest"
  echo "corpus: $name at $tag ($(git -C "$dest" rev-parse --short HEAD))"
done

# A manifest of what is actually checked out, so `compare_format.sh` can record
# the pins in results.json without duplicating the table above. The resolved
# commit is read back from the checkout rather than assumed, since a tag may
# point at an annotated object rather than the commit itself.
{
  echo "["
  sep=""
  for entry in "${CORPORA[@]}"; do
    IFS='|' read -r name repo tag <<<"$entry"
    commit="$(git -C "$SCRIPT_DIR/$name" rev-parse --short HEAD 2>/dev/null || echo unknown)"
    printf '%s  {"name": "%s", "repo": "%s", "tag": "%s", "commit": "%s"}\n' \
      "$sep" "$name" "$repo" "$tag" "$commit"
    sep=","
  done
  echo "]"
} >"$MANIFEST"
