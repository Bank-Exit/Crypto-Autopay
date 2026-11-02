#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# build-release.sh — local release build on the current OS
# Usage: ./scripts/build-release.sh [--target <triple>]... [--skip-npm]
#
#   Linux   : x86_64-unknown-linux-gnu (default), aarch64-unknown-linux-gnu
#   macOS   : aarch64-apple-darwin, x86_64-apple-darwin (both buildable on one Mac)
#   Windows : x86_64-pc-windows-msvc (run from Git Bash)
#
# Tauri cannot build Windows or macOS installers from Linux: use the Release workflow
# (.github/workflows/release.yml, triggered by a v* tag) or a machine of that OS.
# Works with the bash 3.2 shipped with macOS.
# ─────────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TARGETS=()
SKIP_NPM=0
while [ $# -gt 0 ]; do
  case "$1" in
    --target) [ $# -ge 2 ] || { echo "--target needs a triple" >&2; exit 1; }; TARGETS+=("$2"); shift 2 ;;
    --skip-npm) SKIP_NPM=1; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "Unknown argument: $1 (see --help)" >&2; exit 1 ;;
  esac
done

case "$(uname -s)" in
  Linux)                HOST_OS=linux ;;
  Darwin)               HOST_OS=macos ;;
  MINGW*|MSYS*|CYGWIN*) HOST_OS=windows ;;
  *) echo "Unsupported host: $(uname -s)" >&2; exit 1 ;;
esac

HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
[ ${#TARGETS[@]} -gt 0 ] || TARGETS=("$HOST_TRIPLE")

for t in "${TARGETS[@]}"; do
  case "$t" in
    *-linux-*)   os=linux ;;
    *-apple-*)   os=macos ;;
    *-windows-*) os=windows ;;
    *) echo "Unsupported target: $t" >&2; exit 1 ;;
  esac
  if [ "$os" != "$HOST_OS" ]; then
    echo "Cannot build $t on $HOST_OS: Tauri bundles only for the OS it runs on." >&2
    echo "Push a v* tag to run the Release workflow, or build on a $os machine." >&2
    exit 1
  fi
done

command -v cargo-tauri >/dev/null || cargo tauri --version >/dev/null 2>&1 || {
  echo "Tauri CLI missing: cargo install tauri-cli --version '^2' --locked" >&2; exit 1; }

if [ "$SKIP_NPM" -eq 0 ]; then
  echo "▶ Installing frontend dependencies…"
  (cd src-frontend && npm ci)
fi

# On Linux: ensure libayatana-appindicator3-dev is installed (required for tray icon)
if [ "$HOST_OS" = linux ]; then
  if ! dpkg -s libayatana-appindicator3-dev &>/dev/null && ! dpkg -s libappindicator3-dev &>/dev/null; then
    echo "▶ Installing appindicator library (required for tray icon)…"
    sudo apt-get install -y libayatana-appindicator3-dev 2>/dev/null \
      || sudo apt-get install -y libappindicator3-dev 2>/dev/null \
      || echo "Warning: could not install appindicator library — tray icon may not work"
  fi
fi

BUNDLE_DIRS=()
for t in "${TARGETS[@]}"; do
  rustup target add "$t" >/dev/null

  # Bundled monero-wallet-rpc, pinned and SHA-256 checked (same script as CI).
  sidecar="src-tauri/binaries/autopay-wallet-rpc-$t"
  case "$t" in *-windows-*) sidecar="$sidecar.exe" ;; esac
  if [ ! -f "$sidecar" ]; then
    echo "▶ Fetching monero-wallet-rpc for $t…"
    scripts/fetch-wallet-rpc.sh "$t"
  fi

  echo "▶ Building Tauri release for $t…"
  cargo tauri build --target "$t"
  BUNDLE_DIRS+=("target/$t/release/bundle")
done

echo ""
echo "✓ Build complete. Artifacts (SHA-256):"
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
for d in "${BUNDLE_DIRS[@]}"; do
  find "$d" -maxdepth 2 -type f \( -name "*.AppImage" -o -name "*.deb" -o -name "*.rpm" \
    -o -name "*.dmg" -o -name "*.msi" -o -name "*-setup.exe" \) 2>/dev/null | sort | while read -r f; do
    sha256 "$f"
  done
done
echo "Next: sign the release, see docs/SIGNING_RELEASES.md"
