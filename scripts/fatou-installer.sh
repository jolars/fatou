#!/usr/bin/env sh
set -eu

REPO="${FATOU_REPO:-jolars/fatou}"
INSTALL_DIR="${FATOU_INSTALL_DIR:-$HOME/.local/bin}"
TAG="${FATOU_TAG:-}"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
Linux)
  case "$arch" in
  x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
  aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
  *)
    echo "Unsupported Linux architecture: $arch" >&2
    exit 1
    ;;
  esac
  ;;
Darwin)
  case "$arch" in
  x86_64 | amd64) target="x86_64-apple-darwin" ;;
  arm64 | aarch64) target="aarch64-apple-darwin" ;;
  *)
    echo "Unsupported macOS architecture: $arch" >&2
    exit 1
    ;;
  esac
  ;;
*)
  echo "Unsupported operating system: $os" >&2
  exit 1
  ;;
esac

asset="fatou-${target}.tar.gz"

resolve_download_url() {
  if [ -n "$TAG" ]; then
    case "$TAG" in
    v* | fatou-v*)
      tag_candidates="$TAG"
      ;;
    *)
      tag_candidates="v${TAG} fatou-v${TAG}"
      ;;
    esac

    for tag_candidate in $tag_candidates; do
      candidate_url="https://github.com/${REPO}/releases/download/${tag_candidate}/${asset}"
      if curl --proto '=https' --tlsv1.2 -fsSLI "$candidate_url" >/dev/null 2>&1; then
        printf '%s\n' "$candidate_url"
        return 0
      fi
    done

    echo "Could not find release asset ${asset} for FATOU_TAG='${TAG}' in ${REPO}" >&2
    exit 1
  fi

  api_url="https://api.github.com/repos/${REPO}/releases?per_page=100"
  resolved_url="$(
    curl --proto '=https' --tlsv1.2 -fsSL "$api_url" \
      | tr ',' '\n' \
      | grep 'browser_download_url' \
      | grep -F "/${asset}\"" \
      | sed -E 's/.*"browser_download_url"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/' \
      | sed 's#\\/#/#g' \
      | sed -n '1p'
  )"

  if [ -z "$resolved_url" ]; then
    echo "Could not find a release asset named ${asset} in ${REPO}" >&2
    exit 1
  fi

  printf '%s\n' "$resolved_url"
}

url="$(resolve_download_url)"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT INT TERM

echo "Downloading ${asset}..."
curl --proto '=https' --tlsv1.2 -fLsS "$url" -o "$tmpdir/$asset"

tar -xzf "$tmpdir/$asset" -C "$tmpdir"
mkdir -p "$INSTALL_DIR"
install -m 755 "$tmpdir/fatou" "$INSTALL_DIR/fatou"

echo "Installed fatou to $INSTALL_DIR/fatou"
case ":$PATH:" in
*":$INSTALL_DIR:"*) ;;
*)
  echo "Note: $INSTALL_DIR is not on PATH."
  ;;
esac
