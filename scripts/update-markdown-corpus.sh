#!/usr/bin/env bash
# Regenerate the pinned Julia Markdown oracle artifacts from the stdlib parser.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v julia >/dev/null 2>&1; then
	echo "error: julia not found on PATH (enter the devenv shell first)" >&2
	exit 1
fi

exec julia --startup-file=no "$script_dir/update-markdown-corpus.jl"
