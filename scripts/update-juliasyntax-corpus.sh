#!/usr/bin/env bash
#
# Regenerate the pinned JuliaSyntax oracle corpus (`expected.sexpr` files plus
# the `.juliasyntax-source` version sidecar). Thin wrapper around the Julia
# helper. Development snapshots replay their recorded upstream revision;
# released snapshots use the root Julia project. Run inside the devenv shell
# or provide a compatible Julia interpreter on PATH.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source_file="$script_dir/../crates/fatou-parser/tests/fixtures/oracle/.juliasyntax-source"

if ! command -v julia >/dev/null 2>&1; then
	echo "error: julia not found on PATH (enter the devenv shell first)" >&2
	exit 1
fi

revision="$(sed -n 's/^juliasyntax_revision=//p' "$source_file")"
if [[ -n "$revision" ]]; then
	exec bash "$script_dir/refresh-juliasyntax-oracle.sh" "$revision"
fi

exec julia --startup-file=no --project="$script_dir/.." "$script_dir/update-juliasyntax-corpus.jl"
