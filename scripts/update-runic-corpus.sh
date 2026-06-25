#!/usr/bin/env bash
#
# Regenerate the pinned Runic formatter oracle corpus (`expected.jl` files plus
# the `.runic-source` version sidecar). Thin wrapper around the Julia helper;
# requires the devenv Julia toolchain (which ships Runic) on PATH.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v julia >/dev/null 2>&1; then
    echo "error: julia not found on PATH (enter the devenv shell first)" >&2
    exit 1
fi

exec julia --startup-file=no "$script_dir/update-runic-corpus.jl"
