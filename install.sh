#!/bin/sh
# dlog installer — downloads a prebuilt `dlog` binary from the GitHub Releases
# and installs it onto your PATH. No clone, no Rust/C toolchain required.
#
#   curl -fsSL https://raw.githubusercontent.com/nonokia/dlog/main/install.sh | sh
#
# Environment overrides:
#   DLOG_VERSION   release tag to install (default: latest)
#   DLOG_BIN_DIR   install directory  (default: $HOME/.local/bin)
#
# Asset layout (must match .github/workflows/release.yml): each release carries
# `dlog-<target>.tar.gz` containing a `dlog-<target>/` dir with the `dlog` binary.

set -eu

REPO="nonokia/dlog"
BIN_DIR="${DLOG_BIN_DIR:-$HOME/.local/bin}"

err() { printf 'install: %s\n' "$1" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || err "required command not found: $1"; }

need uname
need tar
need mkdir

# A downloader: prefer curl, fall back to wget.
if command -v curl >/dev/null 2>&1; then
  dl() { curl -fsSL "$1" -o "$2"; }
  dl_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
  dl() { wget -qO "$2" "$1"; }
  dl_stdout() { wget -qO- "$1"; }
else
  err "need curl or wget to download"
fi

# Map the host OS/arch to a release target triple.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)  os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  *) err "unsupported OS: $os (build from source: cargo install --git https://github.com/$REPO)" ;;
esac
case "$arch" in
  x86_64 | amd64) arch_part="x86_64" ;;
  arm64 | aarch64) arch_part="aarch64" ;;
  *) err "unsupported architecture: $arch (build from source: cargo install --git https://github.com/$REPO)" ;;
esac
target="${arch_part}-${os_part}"

# Resolve the version: explicit tag, or follow the 'latest' redirect.
version="${DLOG_VERSION:-}"
if [ -z "$version" ]; then
  version="$(dl_stdout "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$version" ] || err "could not determine the latest version; set DLOG_VERSION"
fi

asset="dlog-${target}.tar.gz"
url="https://github.com/$REPO/releases/download/${version}/${asset}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

printf 'install: downloading dlog %s (%s)\n' "$version" "$target" >&2
dl "$url" "$tmp/$asset" || err "download failed: $url"

# Verify the checksum when its companion file is published.
if dl "${url}.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
  if command -v sha256sum >/dev/null 2>&1; then
    sum="$(sha256sum "$tmp/$asset" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    sum="$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)"
  else
    sum=""
  fi
  if [ -n "$sum" ]; then
    want="$(cut -d' ' -f1 < "$tmp/$asset.sha256")"
    [ "$sum" = "$want" ] || err "checksum mismatch for $asset"
    printf 'install: checksum ok\n' >&2
  fi
fi

tar -xzf "$tmp/$asset" -C "$tmp"
bin="$tmp/dlog-${target}/dlog"
[ -f "$bin" ] || err "archive did not contain the expected binary: dlog-${target}/dlog"

mkdir -p "$BIN_DIR"
install -m 0755 "$bin" "$BIN_DIR/dlog" 2>/dev/null || { cp "$bin" "$BIN_DIR/dlog"; chmod 0755 "$BIN_DIR/dlog"; }

printf 'install: dlog %s installed to %s/dlog\n' "$version" "$BIN_DIR" >&2
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) printf 'install: note: %s is not on your PATH — add it, e.g.\n  export PATH="%s:$PATH"\n' "$BIN_DIR" "$BIN_DIR" >&2 ;;
esac
