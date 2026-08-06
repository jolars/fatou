#!/usr/bin/env bash
# Derive the site icons from the master logo (assets/logo.png).
#
# The logo is a Newton fractal, and its basin boundary is infinitely intricate
# -- generate-logo.jl renders newton mode raster-only, so there is no vector
# original. favicon.svg is therefore a thin wrapper around a small embedded
# PNG: mdBook always links `rel="icon"` at favicon.svg, so shipping one is the
# only way to keep its own default icon from winning over ours.
#
# Outputs (all overwritten):
#   docs/theme/favicon.png         32x32    mdBook `rel="shortcut icon"`
#   docs/theme/favicon.svg         64x64    mdBook `rel="icon"`
#   docs/src/apple-touch-icon.png  180x180  iOS home screen, linked from head.hbs
#
# Usage: scripts/generate-icons.sh
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

command -v magick >/dev/null || {
  echo "error: magick not found (install imagemagick)" >&2
  exit 1
}

logo=assets/logo.png

# <size> <out>
resize() {
  magick "$logo" -resize "$1x$1" -strip -define png:compression-level=9 "$2"
}

resize 32 docs/theme/favicon.png
resize 180 docs/src/apple-touch-icon.png

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
resize 64 "$tmp/favicon.png"

{
  echo '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">'
  printf '  <title>Fatou</title>\n'
  printf '  <image width="64" height="64" href="data:image/png;base64,%s"/>\n' \
    "$(base64 -w0 "$tmp/favicon.png")"
  echo '</svg>'
} >docs/theme/favicon.svg

echo "wrote docs/theme/favicon.png docs/theme/favicon.svg docs/src/apple-touch-icon.png"
