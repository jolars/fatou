#!/usr/bin/env bash
#
# Regenerate the pinned JuliaSyntax oracle corpus (`expected.sexpr` files plus
# the `.juliasyntax-source` version sidecar). Thin wrapper around the Julia
# helper; requires the devenv Julia toolchain (which ships JuliaSyntax) on PATH.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v julia >/dev/null 2>&1; then
    echo "error: julia not found on PATH (enter the devenv shell first)" >&2
    exit 1
fi

exec julia --startup-file=no "$script_dir/update-juliasyntax-corpus.jl"
