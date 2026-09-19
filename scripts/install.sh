#!/bin/sh
# diet-code installer for Linux/macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/Mohammad-Palla/diet-code/master/scripts/install.sh | sh
#
# Detects OS/arch, downloads the matching prebuilt binary from GitHub
# Releases, verifies its SHA-256 checksum, and installs it into user space
# (~/.local/bin by default; no sudo required). Override with:
#   DIET_CODE_VERSION=v0.1.0 DIET_CODE_INSTALL_DIR=/custom/bin
set -eu

REPO="Mohammad-Palla/diet-code"
VERSION="${DIET_CODE_VERSION:-latest}"
INSTALL_DIR="${DIET_CODE_INSTALL_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)  platform="linux" ;;
  Darwin) platform="darwin" ;;
  *) echo "diet-code: unsupported OS '$os'. Use npm (npm i -g diet-code) or build from source." >&2; exit 1 ;;
esac

case "$arch" in
  x86_64|amd64) arch="x64" ;;
  arm64|aarch64) arch="arm64" ;;
  *) echo "diet-code: unsupported arch '$arch'. Use npm (npm i -g diet-code) or build from source." >&2; exit 1 ;;
esac

key="$platform-$arch"
asset=""
case "$key" in
  linux-x64)   asset="diet-code-x86_64-unknown-linux-musl" ;;
  linux-arm64) asset="diet-code-aarch64-unknown-linux-musl" ;;
  darwin-x64)  asset="diet-code-x86_64-apple-darwin" ;;
  darwin-arm64) asset="diet-code-aarch64-apple-darwin" ;;
  *) echo "diet-code: no prebuilt binary for $key. Use npm or build from source." >&2; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

mkdir -p "$INSTALL_DIR"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

echo "diet-code: downloading $asset ..."
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$base/$asset" -o "$tmp"
  curl -fsSL "$base/checksums.txt" -o "$tmp.sums"
else
  wget -q "$base/$asset" -O "$tmp"
  wget -q "$base/checksums.txt" -O "$tmp.sums"
fi

# Verify the checksum (fail closed — never install an unverified binary).
expected="$(grep " $asset\$" "$tmp.sums" | awk '{print $1}' | head -1 || true)"
if [ -z "$expected" ]; then
  echo "diet-code: no checksum listed for $asset — refusing to install." >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp" | awk '{print $1}')"
fi
if [ "$actual" != "$expected" ]; then
  echo "diet-code: checksum mismatch for $asset (expected $expected, got $actual)." >&2
  exit 1
fi

install -m 0755 "$tmp" "$INSTALL_DIR/diet-code"
rm -f "$tmp.sums"

echo "diet-code: installed to $INSTALL_DIR/diet-code"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo "diet-code: add it to your PATH:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac
"$INSTALL_DIR/diet-code" --version || true
