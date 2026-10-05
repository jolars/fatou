#!/usr/bin/env bash
set -euo pipefail

book_dir=${1:-docs/book}
test -f "$book_dir/html/index.html"
test -f "$book_dir/markdown/introduction.md"

python3 scripts/clean-docs-markdown.py "$book_dir/markdown"
cp -a "$book_dir/html/." "$book_dir/"
cp -a "$book_dir/markdown/." "$book_dir/"
cp "$book_dir/introduction.md" "$book_dir/index.md"
rm -rf "$book_dir/html" "$book_dir/markdown"
