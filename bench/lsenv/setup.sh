#!/usr/bin/env bash
# Provision the two Julia language servers the memory benchmark compares Fatou
# against, each in its own pinned environment.
#
# Each server gets its own project. Both use the benchmark depot for package
# installation and precompilation.
set -euo pipefail
BENCH_JULIA="${BENCH_JULIA:-julia}"

# name|version (registered packages)
LANGUAGESERVER_VERSION="6.0.1"
# JETLS is unregistered; pin the checkout by commit.
JETLS_REPO="https://github.com/aviatesk/JETLS.jl"
JETLS_COMMIT="40a5e9cb824614ae7d7cf3e3a73f26282c0fa1cd"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export JULIA_DEPOT_PATH="${JULIA_DEPOT_PATH:-$SCRIPT_DIR/depot:}"
LS_ENV="$SCRIPT_DIR/languageserver"
JETLS_DIR="$SCRIPT_DIR/jetls"
MANIFEST="$SCRIPT_DIR/manifest.json"

# The repo exports JULIA_PROJECT=@., which would otherwise shadow the explicit
# --project flags below for any nested Pkg call.
export JULIA_PROJECT=""

# --- LanguageServer.jl -------------------------------------------------------
mkdir -p "$LS_ENV"
echo "==> lsenv: LanguageServer.jl v$LANGUAGESERVER_VERSION"
"$BENCH_JULIA" --startup-file=no --project="$LS_ENV" -e "
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

echo "==> lsenv: instantiating JETLS"
"$BENCH_JULIA" --startup-file=no --project="$JETLS_DIR" -e '
    using Pkg
    Pkg.instantiate()
    Pkg.precompile()
'

# --- manifest ----------------------------------------------------------------
# Read the resolved LanguageServer version back out of the environment rather
# than echoing the request, so the artifact records what was actually installed.
ls_version="$("$BENCH_JULIA" --startup-file=no --project="$LS_ENV" -e '
    using Pkg
    for (_, p) in Pkg.dependencies()
        p.name == "LanguageServer" && (print(p.version); break)
    end
')"
jetls_commit="$(git -C "$JETLS_DIR" rev-parse HEAD)"
jetls_date="$(git -C "$JETLS_DIR" log -1 --date=short --format=%cd)"

cat >"$MANIFEST" <<EOF
{
  "languageserver": {"version": "$ls_version", "manifest_sha256": "$(sha256sum "$LS_ENV/Manifest.toml" | cut -d " " -f1)"},
  "jetls": {"repo": "$JETLS_REPO", "commit": "$jetls_commit", "date": "$jetls_date", "manifest_sha256": "$(sha256sum "$JETLS_DIR/Manifest.toml" | cut -d " " -f1)"}
}
EOF

echo "==> lsenv: ready (LanguageServer v$ls_version, JETLS $jetls_commit)"
