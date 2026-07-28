#!/usr/bin/env bash
#
# SessionStart hook for Claude Code on the web.
#
# Local development uses devenv/Nix (`devenv.nix`), which already provides both
# the Rust and Julia toolchains. The hosted web container has neither, so this
# script provisions them.
#
# Rust is a hard requirement (build, test, clippy). Julia is *optional*: it is
# needed only to regenerate the pinned JuliaSyntax oracle corpus via
# `scripts/update-juliasyntax-corpus.sh`. `cargo test` runs the oracle against
# the committed `expected.sexpr` files and needs no Julia at all, so a failure
# to provision Julia warns and leaves the session usable.
set -euo pipefail

REPO="${CLAUDE_PROJECT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/fatou"
NIX_VERSION="2.24.10"

# A nixpkgs snapshot pinned by URL. `github:NixOS/nixpkgs` is unusable here: the
# web container's GitHub access is scoped to this repository, so flake inputs
# resolving through api.github.com return 403. releases.nixos.org serves the
# same tree as a plain tarball and is reachable, so fetch it from there. This
# snapshot carries julia-bin 1.12.6, matching the pinned oracle toolchain.
NIXPKGS_URL="https://releases.nixos.org/nixos/unstable/nixos-26.11pre1042126.624af665418d/nixexprs.tar.xz"

log() { printf '[session-start] %s\n' "$*" >&2; }

# Only the hosted container needs this; a local devenv shell already has it all.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  log "not a remote session; leaving the toolchain to devenv"
  exit 0
fi

mkdir -p "$CACHE"

# --- Rust -------------------------------------------------------------------
# Warm the cargo registry so the first build in the session is not a cold fetch.
# The container image is snapshotted after this hook, so the download is paid
# once rather than per session.
log "fetching cargo dependencies"
cargo fetch --manifest-path "$REPO/Cargo.toml"

# --- Julia (optional) -------------------------------------------------------
# Versions come from the corpus sidecar so this cannot silently drift away from
# what the committed `expected.sexpr` files were generated with.
SOURCE_FILE="$REPO/tests/fixtures/oracle/.juliasyntax-source"
JULIA_WANT="$(sed -n 's/^julia_version=//p' "$SOURCE_FILE" 2>/dev/null || true)"
JS_WANT="$(sed -n 's/^juliasyntax_version=//p' "$SOURCE_FILE" 2>/dev/null || true)"

provision_julia() {
  set +e
  [ -n "$JULIA_WANT" ] && [ -n "$JS_WANT" ] || {
    log "could not read pinned versions from $SOURCE_FILE"
    return 1
  }

  local marker="$CACHE/julia-$JULIA_WANT.path"
  local julia_path
  julia_path="$(cat "$marker" 2>/dev/null || true)"

  # Fast path: a previous run already built Julia and the store path survives in
  # the container snapshot, so skip Nix evaluation entirely.
  if [ -z "$julia_path" ] || [ ! -x "$julia_path/bin/julia" ]; then
    install_nix || return 1
    export PATH="$NIX_BIN:$PATH"

    log "building julia-bin $JULIA_WANT from the pinned nixpkgs snapshot (this can take a few minutes)"
    julia_path="$(nix build --impure --no-link --print-out-paths \
      --expr "with import (fetchTarball \"$NIXPKGS_URL\") {}; julia-bin")" || {
      log "nix build of julia-bin failed"
      return 1
    }
    printf '%s\n' "$julia_path" > "$marker"
    printf 'export PATH="%s:$PATH"\n' "$NIX_BIN" >> "$CACHE/env.sh.tmp"
  fi

  local julia_have
  julia_have="$("$julia_path/bin/julia" --version | awk '{print $3}')"
  if [ "$julia_have" != "$JULIA_WANT" ]; then
    log "julia $julia_have does not match the pinned $JULIA_WANT; regenerating the corpus would produce spurious diffs"
    return 1
  fi

  # JuliaSyntax is a dependency-free pure-Julia package, so a git checkout on
  # JULIA_LOAD_PATH is enough — no Pkg resolution and no package registry.
  local pkgdir="$CACHE/pkgdir"
  local jsdir="$pkgdir/JuliaSyntax"
  mkdir -p "$pkgdir"
  if [ ! -d "$jsdir/.git" ]; then
    log "cloning JuliaSyntax.jl v$JS_WANT"
    git -c advice.detachedHead=false clone --quiet --depth 1 --branch "v$JS_WANT" \
      https://github.com/JuliaLang/JuliaSyntax.jl.git "$jsdir" || {
      log "clone of JuliaSyntax.jl v$JS_WANT failed"
      rm -rf "$jsdir"
      return 1
    }
  fi

  {
    printf 'export PATH="%s/bin:$PATH"\n' "$julia_path"
    printf 'export JULIA_LOAD_PATH="%s:@stdlib"\n' "$pkgdir"
    printf 'export JULIA_DEPOT_PATH="%s/depot"\n' "$CACHE"
  } >> "$CACHE/env.sh.tmp"

  # Precompile once so the cost lands in the cached container image rather than
  # in the first command the agent runs.
  log "precompiling JuliaSyntax"
  JULIA_LOAD_PATH="$pkgdir:@stdlib" JULIA_DEPOT_PATH="$CACHE/depot" \
    "$julia_path/bin/julia" --startup-file=no -e 'using JuliaSyntax' >/dev/null 2>&1 || {
    log "precompiling JuliaSyntax failed"
    return 1
  }

  return 0
}

install_nix() {
  # Written unconditionally: /nix can survive in a cached container image while
  # /etc is rebuilt, and without this config nix refuses to build (the container
  # runs as root with no nixbld group, so single-user mode must be selected).
  mkdir -p /etc/nix
  cat > /etc/nix/nix.conf <<'CONF'
build-users-group =
experimental-features = nix-command flakes
substituters = https://cache.nixos.org
sandbox = false
CONF

  NIX_BIN="$(echo /nix/store/*-nix-"$NIX_VERSION"/bin 2>/dev/null | tr ' ' '\n' | head -1)"
  if [ -x "$NIX_BIN/nix" ]; then
    return 0
  fi

  # The usual installer scripts (nixos.org/nix/install, install.determinate.systems)
  # are not reachable from this container, but the release tarballs on
  # releases.nixos.org are. Install from the tarball instead.
  log "installing nix $NIX_VERSION"
  local tmp
  tmp="$(mktemp -d)"
  curl -sSL --retry 3 --max-time 300 \
    -o "$tmp/nix.tar.xz" \
    "https://releases.nixos.org/nix/nix-$NIX_VERSION/nix-$NIX_VERSION-x86_64-linux.tar.xz" || {
    log "downloading nix failed"
    return 1
  }
  tar -xf "$tmp/nix.tar.xz" -C "$tmp" || return 1

  "$tmp/nix-$NIX_VERSION-x86_64-linux/install" --no-daemon --no-channel-add >/dev/null 2>&1 || true
  rm -rf "$tmp"

  NIX_BIN="$(echo /nix/store/*-nix-"$NIX_VERSION"/bin 2>/dev/null | tr ' ' '\n' | head -1)"
  if [ ! -x "$NIX_BIN/nix" ]; then
    log "nix install did not produce a usable binary"
    return 1
  fi
  return 0
}

: > "$CACHE/env.sh.tmp"
if provision_julia; then
  log "julia $JULIA_WANT + JuliaSyntax $JS_WANT ready"
else
  log "julia unavailable — 'cargo test' still runs the oracle against the pinned corpus;"
  log "only scripts/update-juliasyntax-corpus.sh needs Julia"
  : > "$CACHE/env.sh.tmp"
fi

# Publish whatever we managed to provision to the session environment.
if [ -n "${CLAUDE_ENV_FILE:-}" ] && [ -s "$CACHE/env.sh.tmp" ]; then
  while IFS= read -r line; do
    grep -qxF "$line" "$CLAUDE_ENV_FILE" 2>/dev/null || printf '%s\n' "$line" >> "$CLAUDE_ENV_FILE"
  done < "$CACHE/env.sh.tmp"
fi
rm -f "$CACHE/env.sh.tmp"
