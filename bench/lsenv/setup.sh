#!/usr/bin/env bash
# Provision the two Julia language servers the memory benchmark compares Fatou
# against, each in its own pinned environment.
#
#   LanguageServer.jl  the incumbent: a Julia runtime plus SymbolServer, which
#                      indexes the whole active environment in a child process.
#   JETLS              the successor: real type inference through JET, which
#                      means loading the package into a live Julia session.
#
# They get separate environments because their dependency bounds are not
# co-satisfiable (JETLS tracks JuliaSyntax v2 from the julia repo, which the
# General registry does not carry), and because a shared environment would make
# one server's resolution perturb the other's measurement.
#
# JETLS is not registered, so it is pinned by commit rather than by version, and
# `[sources]` in its Project.toml pins its own dependencies. Its repo ships
# `JETLS_DEV_MODE = true` for contributors, which loads Revise; the benchmark
# overrides that to false through LocalPreferences.toml so we measure what a
# user runs, not what a JETLS developer runs.
#
# Everything here is opt-in and local: this writes into bench/lsenv/ (gitignored)
# and into the shared Julia depot, and needs the network on first run.
set -euo pipefail

# name|version (registered packages)
LANGUAGESERVER_VERSION="5.0.0"
# JETLS is unregistered; pin the checkout by commit.
JETLS_REPO="https://github.com/aviatesk/JETLS.jl"
JETLS_COMMIT="7e01ca583bea6ffd382af1e30b1e10ed1f73628b"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LS_ENV="$SCRIPT_DIR/languageserver"
JETLS_DIR="$SCRIPT_DIR/jetls"
MANIFEST="$SCRIPT_DIR/manifest.json"

# The repo exports JULIA_PROJECT=@., which would otherwise shadow the explicit
# --project flags below for any nested Pkg call.
export JULIA_PROJECT=""

# --- LanguageServer.jl -------------------------------------------------------
mkdir -p "$LS_ENV"
echo "==> lsenv: LanguageServer.jl v$LANGUAGESERVER_VERSION"
julia --startup-file=no --project="$LS_ENV" -e "
    using Pkg
    Pkg.add(PackageSpec(name = \"LanguageServer\", version = \"$LANGUAGESERVER_VERSION\"))
    Pkg.precompile()
"

# --- JETLS -------------------------------------------------------------------
if [[ -d "$JETLS_DIR/.git" ]] &&
  [[ "$(git -C "$JETLS_DIR" rev-parse HEAD 2>/dev/null || true)" == "$JETLS_COMMIT" ]]; then
  echo "==> lsenv: JETLS already at ${JETLS_COMMIT:0:9}"
else
  echo "==> lsenv: cloning JETLS @ ${JETLS_COMMIT:0:9}"
  rm -rf "$JETLS_DIR"
  git clone --quiet "$JETLS_REPO" "$JETLS_DIR"
  git -C "$JETLS_DIR" checkout --quiet "$JETLS_COMMIT"
fi

printf '[JETLS]\nJETLS_DEV_MODE = false\n' >"$JETLS_DIR/LocalPreferences.toml"

echo "==> lsenv: instantiating JETLS (clones JuliaSyntax/JuliaLowering from the julia repo on first run)"
julia --startup-file=no --project="$JETLS_DIR" -e '
    using Pkg
    Pkg.instantiate()
    Pkg.precompile()
'

# --- manifest ----------------------------------------------------------------
# Read the resolved LanguageServer version back out of the environment rather
# than echoing the request, so the artifact records what was actually installed.
ls_version="$(julia --startup-file=no --project="$LS_ENV" -e '
    using Pkg
    for (_, p) in Pkg.dependencies()
        p.name == "LanguageServer" && (print(p.version); break)
    end
')"
jetls_commit="$(git -C "$JETLS_DIR" rev-parse --short HEAD)"
jetls_date="$(git -C "$JETLS_DIR" log -1 --date=short --format=%cd)"

cat >"$MANIFEST" <<EOF
{
  "languageserver": {"version": "$ls_version", "project": "$LS_ENV"},
  "jetls": {"repo": "$JETLS_REPO", "commit": "$jetls_commit", "date": "$jetls_date", "project": "$JETLS_DIR"}
}
EOF

echo "==> lsenv: ready (LanguageServer v$ls_version, JETLS $jetls_commit)"
