#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# build-release.sh — local release build for the current platform
# Usage: ./scripts/build-release.sh [--target <triple>]
# ─────────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TARGET="${1:-}"
ARGS=""
if [ -n "$TARGET" ]; then
  ARGS="--target $TARGET"
  rustup target add "$TARGET" 2>/dev/null || true
fi

echo "▶ Installing frontend dependencies…"
(cd src-frontend && npm ci)

# On Linux: ensure libayatana-appindicator3-dev is installed (required for tray icon)
if [[ "$(uname)" == "Linux" ]]; then
  if ! dpkg -s libayatana-appindicator3-dev &>/dev/null && ! dpkg -s libappindicator3-dev &>/dev/null; then
    echo "▶ Installing appindicator library (required for tray icon)…"
    sudo apt-get install -y libayatana-appindicator3-dev 2>/dev/null \
      || sudo apt-get install -y libappindicator3-dev 2>/dev/null \
      || echo "Warning: could not install appindicator library — tray icon may not work"
  fi
fi

echo "▶ Building Tauri release${TARGET:+ for $TARGET}…"
cargo tauri build $ARGS

echo ""
echo "✓ Build complete. Artifacts in:"
find src-tauri/target -maxdepth 4 -name "*.AppImage" -o -name "*.deb" \
  -o -name "*.dmg"  -o -name "*.exe"  -o -name "*.msi" \
  2>/dev/null | grep -v "\.d$" | sort
