#!/usr/bin/env bash
# Regenerate the oracle with the locked Julia binary and matching JuliaSyntax.
# Only --update resolves a new nightly and changes the lock.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ $# -gt 1 ]]; then
	echo "usage: $0 [--update | locked-julia-commit]" >&2
	exit 1
fi

if [[ "${1:-}" == --update ]]; then
	python3 "$script_dir/julia-oracle.py" update
fi
revision="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["commit"])' "$script_dir/julia-oracle.lock.json")"
if [[ $# -eq 1 && "$1" != --update && "$1" != "$revision" ]]; then
	echo "error: requested revision does not match julia-oracle.lock.json; restore its matching lock first" >&2
	exit 1
fi

install_args=()
if [[ -n "${FATOU_ORACLE_JULIA:-}" ]]; then
	install_args+=(--julia "$FATOU_ORACLE_JULIA")
fi
oracle_bin_dir="$(python3 "$script_dir/julia-oracle.py" install "${install_args[@]}")"

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
source_dir="$work_dir/julia"
git init --quiet "$source_dir"
git -C "$source_dir" remote add origin https://github.com/JuliaLang/julia.git
git -C "$source_dir" sparse-checkout set JuliaSyntax
git -C "$source_dir" fetch --quiet --depth=1 --filter=blob:none origin "$revision"
git -C "$source_dir" checkout --quiet --detach FETCH_HEAD

export FATOU_ORACLE_REVISION
FATOU_ORACLE_REVISION="$(git -C "$source_dir" rev-parse HEAD)"
export FATOU_ORACLE_SYNTAX_VERSION
FATOU_ORACLE_SYNTAX_VERSION="$(cat "$source_dir/VERSION")"
export FATOU_ORACLE_BINARY_SOURCE
FATOU_ORACLE_BINARY_SOURCE=archive
if [[ -n "${FATOU_ORACLE_JULIA:-}" ]]; then
	FATOU_ORACLE_BINARY_SOURCE=source-build
	unset FATOU_ORACLE_ARCHIVE_SHA256
else
	export FATOU_ORACLE_ARCHIVE_SHA256
	FATOU_ORACLE_ARCHIVE_SHA256="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["sha256"])' "$script_dir/julia-oracle.lock.json")"
fi
# Explicit activation prevents JULIA_PROJECT=@. and a user's LOAD_PATH from
# selecting the released package in the root environment.
export JULIA_LOAD_PATH="@:@stdlib"
oracle_project="$source_dir/JuliaSyntax"
# A temporary checkout cannot reuse package precompilation. Staying in this
# process also prevents precompile workers from bypassing the NixOS loader.
"$oracle_bin_dir/julia" --startup-file=no --compiled-modules=no --project="$oracle_project" "$script_dir/update-juliasyntax-corpus.jl"
"$oracle_bin_dir/julia" --startup-file=no --compiled-modules=no --project="$oracle_project" "$script_dir/harvest-juliasyntax-corpus.jl"
