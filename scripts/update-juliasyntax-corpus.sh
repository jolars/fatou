#!/usr/bin/env bash
#
# Regenerate the pinned JuliaSyntax oracle corpus (`expected.sexpr` files plus
# the `.juliasyntax-source` version sidecar). Thin wrapper around the Julia
# helper; run inside the devenv shell, which provides `julia` and sets
# `JULIA_PROJECT=@.` so the repo's pinned JuliaSyntax (root `Project.toml`)
# resolves. The script itself just `using JuliaSyntax` from the active env.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v julia >/dev/null 2>&1; then
    echo "error: julia not found on PATH (enter the devenv shell first)" >&2
    exit 1
fi

exec julia --startup-file=no "$script_dir/update-juliasyntax-corpus.jl"
