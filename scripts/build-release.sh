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

echo "▶ Building Tauri release${TARGET:+ for $TARGET}…"
cargo tauri build $ARGS

echo ""
echo "✓ Build complete. Artifacts in:"
find src-tauri/target -maxdepth 4 -name "*.AppImage" -o -name "*.deb" \
  -o -name "*.dmg"  -o -name "*.exe"  -o -name "*.msi" \
  2>/dev/null | grep -v "\.d$" | sort
