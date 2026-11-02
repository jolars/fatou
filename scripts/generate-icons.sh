#!/usr/bin/env bash
# Derive the site icons from the master logo (assets/logo.png).
#
# The Newton fractal has no vector original, so favicon.svg wraps an embedded
# PNG. VitePress links both icon formats from its site configuration.
#
# Outputs (all overwritten):
#   docs/src/public/theme/favicon.png     32x32
#   docs/src/public/theme/favicon.svg     64x64
#   docs/src/public/apple-touch-icon.png  180x180
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

resize 32 docs/src/public/theme/favicon.png
resize 180 docs/src/public/apple-touch-icon.png

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
resize 64 "$tmp/favicon.png"

{
  echo '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" width="64" height="64">'
  printf '  <title>Fatou</title>\n'
  printf '  <image width="64" height="64" href="data:image/png;base64,%s"/>\n' \
    "$(base64 -w0 "$tmp/favicon.png")"
  echo '</svg>'
} >docs/src/public/theme/favicon.svg

echo "wrote docs/src/public/theme/favicon.png docs/src/public/theme/favicon.svg docs/src/public/apple-touch-icon.png"
