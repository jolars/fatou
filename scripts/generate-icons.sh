#!/usr/bin/env bash
# Install a RealFaviconGenerator bundle in the site's public directory.
#
# Usage: scripts/generate-icons.sh [bundle-url]
#
# Generate a new bundle on RealFaviconGenerator before updating the URL.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

command -v curl >/dev/null || {
  echo "error: curl not found" >&2
  exit 1
}

bundle_url=${1:-https://realfavicongenerator.net/files/99f8beba-5fe6-4f37-9504-a1a0b14412f8}
public_folder=docs/src/public
files=(
  favicon.svg
  favicon-96x96.png
  favicon.ico
  apple-touch-icon.png
  web-app-manifest-192x192.png
  web-app-manifest-512x512.png
  site.webmanifest
)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Finish every download before replacing any installed files.
for file in "${files[@]}"; do
  curl --fail --location --silent --show-error --retry 2 \
    --connect-timeout 15 --max-time 60 \
    "${bundle_url%/}/$file" --output "$tmp/$file"
done
for file in "${files[@]}"; do
  cp "$tmp/$file" "$public_folder/$file"
done

# Preserve the previous icon URLs for cached pages and external references.
cp "$public_folder/favicon.svg" "$public_folder/theme/favicon.svg"
cp "$public_folder/favicon-96x96.png" "$public_folder/theme/favicon.png"

echo "Installed the favicon bundle in $public_folder"
