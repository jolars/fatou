#!/usr/bin/env bash
#
# Regenerate the pinned JuliaSyntax oracle corpora and Unicode operator table,
# including their provenance. Development snapshots replay their upstream revision;
# released snapshots use the root Julia project. Run inside the devenv shell
# or provide a compatible Julia interpreter on PATH.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source_file="$script_dir/../crates/fatou-parser/tests/fixtures/oracle/.juliasyntax-source"

revision="$(sed -n 's/^juliasyntax_revision=//p' "$source_file")"
if [[ -n "$revision" ]]; then
	syntax_version="$(sed -n 's/^syntax_version=//p' "$source_file")"
	export FATOU_ORACLE_SYNTAX_VERSION="${FATOU_ORACLE_SYNTAX_VERSION:-$syntax_version}"
	exec bash "$script_dir/refresh-juliasyntax-oracle.sh" "$revision"
fi

if ! command -v julia >/dev/null 2>&1; then
	echo "error: julia not found on PATH (enter the devenv shell first)" >&2
	exit 1
fi

exec julia --startup-file=no --project="$script_dir/.." "$script_dir/update-juliasyntax-corpus.jl"
