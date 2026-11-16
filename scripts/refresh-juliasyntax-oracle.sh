#!/usr/bin/env bash
# Regenerate a candidate oracle from Julia's development tree without changing
# the root Julia environment. Pass a commit from .juliasyntax-source to replay it.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
revision="${1:-master}"
if [[ $# -gt 1 || "$revision" == -* ]]; then
	echo "usage: $0 [julia-revision]" >&2
	exit 1
fi

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
# Explicit activation prevents JULIA_PROJECT=@. and a user's LOAD_PATH from
# selecting the released package in the root environment.
export JULIA_LOAD_PATH="@:@stdlib"
oracle_project="$source_dir/JuliaSyntax"
julia --startup-file=no --project="$oracle_project" "$script_dir/update-juliasyntax-corpus.jl"
julia --startup-file=no --project="$oracle_project" "$script_dir/harvest-juliasyntax-corpus.jl"
