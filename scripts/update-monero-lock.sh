#!/usr/bin/env bash
# Bump the bundled monero-wallet-rpc: fetch hashes.txt, verify its GPG signature against
# binaryFate's key (fingerprint pinned below), then rewrite scripts/monero-cli.lock.
# Usage: scripts/update-monero-lock.sh v0.18.5.4
set -euo pipefail

VERSION="${1:?usage: $0 <version, e.g. v0.18.5.4>}"
FINGERPRINT="81AC591FE9C4B65C5806AFC3F0AF4D462A0BDF92"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOCK="$ROOT/scripts/monero-cli.lock"
PLATFORMS="linux-x64.tar.bz2 linux-armv8.tar.bz2 mac-armv8.tar.bz2 mac-x64.tar.bz2 win-x64.zip"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
export GNUPGHOME="$WORK/gnupg"
mkdir -m 700 "$GNUPGHOME"

curl -fsSL -o "$WORK/hashes.txt" https://www.getmonero.org/downloads/hashes.txt
curl -fsSL -o "$WORK/binaryfate.asc" \
  https://raw.githubusercontent.com/monero-project/monero/master/utils/gpg_keys/binaryfate.asc

gpg --quiet --import "$WORK/binaryfate.asc"
gpg --list-keys --with-colons | grep -q "^fpr:::::::::$FINGERPRINT:" \
  || { echo "Imported key does not match the pinned fingerprint" >&2; exit 1; }
gpg --status-fd 1 --verify "$WORK/hashes.txt" 2>/dev/null | grep -q "VALIDSIG $FINGERPRINT" \
  || { echo "hashes.txt signature is not valid for $FINGERPRINT" >&2; exit 1; }
echo "hashes.txt signature OK"

{
  sed -n '1,/^# Update with/p' "$LOCK"
  echo "version=$VERSION"
  for p in $PLATFORMS; do
    archive="monero-${p%%.*}-$VERSION.${p#*.}"
    hash="$(awk -v a="$archive" '$2 == a { print $1 }' "$WORK/hashes.txt")"
    [ -n "$hash" ] || { echo "$archive missing from hashes.txt" >&2; exit 1; }
    echo "$archive=$hash"
  done
} > "$WORK/lock"
mv "$WORK/lock" "$LOCK"
echo "Updated $LOCK to $VERSION. Rebuild the app and run the RPC tests before releasing."
