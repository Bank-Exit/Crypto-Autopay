#!/usr/bin/env bash
# Download the pinned monero-wallet-rpc for a Rust target triple, check its SHA-256
# against scripts/monero-cli.lock, and place it where Tauri expects the sidecar:
#   src-tauri/binaries/autopay-wallet-rpc-<triple>[.exe]
# Renamed so the .deb (which installs sidecars in /usr/bin) never clashes with a distro monero package.
# Usage: scripts/fetch-wallet-rpc.sh [target-triple]   (default: host triple)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOCK="$ROOT/scripts/monero-cli.lock"
OUT_DIR="$ROOT/src-tauri/binaries"
TRIPLE="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
VERSION="$(sed -n 's/^version=//p' "$LOCK")"

case "$TRIPLE" in
  x86_64-unknown-linux-gnu)  PLATFORM=linux-x64;   EXT=tar.bz2 ;;
  aarch64-unknown-linux-gnu) PLATFORM=linux-armv8; EXT=tar.bz2 ;;
  aarch64-apple-darwin)      PLATFORM=mac-armv8;   EXT=tar.bz2 ;;
  x86_64-apple-darwin)       PLATFORM=mac-x64;     EXT=tar.bz2 ;;
  x86_64-pc-windows-msvc)    PLATFORM=win-x64;     EXT=zip ;;
  *) echo "Unsupported target: $TRIPLE" >&2; exit 1 ;;
esac

ARCHIVE="monero-$PLATFORM-$VERSION.$EXT"
EXPECTED="$(sed -n "s/^$ARCHIVE=//p" "$LOCK")"
[ -n "$EXPECTED" ] || { echo "No pinned hash for $ARCHIVE in $LOCK" >&2; exit 1; }

BIN="monero-wallet-rpc"; [ "$EXT" = zip ] && BIN="monero-wallet-rpc.exe"
DEST="$OUT_DIR/autopay-wallet-rpc-$TRIPLE"; [ "$EXT" = zip ] && DEST="$DEST.exe"

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "Downloading $ARCHIVE"
curl -fsSL --retry 3 -o "$WORK/$ARCHIVE" "https://downloads.getmonero.org/cli/$ARCHIVE"

ACTUAL="$(sha256 "$WORK/$ARCHIVE")"
if [ "$ACTUAL" != "$EXPECTED" ]; then
  echo "SHA-256 mismatch for $ARCHIVE: expected $EXPECTED, got $ACTUAL" >&2
  exit 1
fi
echo "SHA-256 OK"

mkdir -p "$WORK/x"
if [ "$EXT" = zip ]; then
  if command -v unzip >/dev/null; then
    unzip -q "$WORK/$ARCHIVE" -d "$WORK/x"
  else
    powershell -NoProfile -Command "Expand-Archive -Path '$WORK/$ARCHIVE' -DestinationPath '$WORK/x'"
  fi
else
  tar -xjf "$WORK/$ARCHIVE" -C "$WORK/x"
fi

FOUND="$(find "$WORK/x" -type f -name "$BIN" | head -n 1)"
[ -n "$FOUND" ] || { echo "$BIN not found in $ARCHIVE" >&2; exit 1; }

mkdir -p "$OUT_DIR"
cp "$FOUND" "$DEST"
chmod +x "$DEST"
echo "Installed $DEST ($VERSION)"
