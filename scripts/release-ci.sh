#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# release-ci.sh — start the GitHub Release workflow (Windows, macOS, Linux)
# Usage: ./scripts/release-ci.sh <version> [--yes]     e.g. ./scripts/release-ci.sh 0.3.0
#
#   1. checks: on master, no uncommitted change, tag not taken, up to date with origin
#   2. bumps the version everywhere (Cargo, tauri.conf.json, package.json) and commits it
#   3. runs clippy, tests and the frontend build
#   4. creates the v<version> tag (signed when git has a signing key), asks, then pushes
#      master + tag: .github/workflows/release.yml builds a draft release
#
# Then: download the draft artifacts, sign SHA256SUMS, publish (docs/SIGNING_RELEASES.md).
# ─────────────────────────────────────────────────────────────
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-}"; VERSION="${VERSION#v}"
YES=0; [ "${2:-}" = "--yes" ] && YES=1
TAG="v$VERSION"
REMOTE=origin
BRANCH=master

die() { echo "✗ $*" >&2; exit 1; }

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || die "Usage: $0 <x.y.z> [--yes]"
[ "$(git rev-parse --abbrev-ref HEAD)" = "$BRANCH" ] || die "Not on $BRANCH"
[ -z "$(git status --porcelain --untracked-files=no)" ] || die "Uncommitted changes: commit or stash them first"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null && die "Tag $TAG already exists"

echo "▶ Fetching $REMOTE…"
git fetch -q "$REMOTE" "$BRANCH" --tags
git ls-remote --exit-code --tags "$REMOTE" "refs/tags/$TAG" >/dev/null 2>&1 && die "Tag $TAG already exists on $REMOTE"
git merge-base --is-ancestor "$REMOTE/$BRANCH" HEAD || die "$REMOTE/$BRANCH has commits you do not have: pull first"

CURRENT="$(sed -n 's/^  "version": "\(.*\)",$/\1/p' src-tauri/tauri.conf.json | head -n 1)"
if [ "$CURRENT" != "$VERSION" ]; then
  echo "▶ Version $CURRENT → $VERSION"
  # First match only (no /g): the [package] / [workspace.package] version, the top-level JSON key.
  for f in Cargo.toml src-tauri/Cargo.toml; do
    V="$VERSION" perl -0pi -e 's/^version = "[^"]*"/version = "$ENV{V}"/m' "$f"
  done
  V="$VERSION" perl -0pi -e 's/^  "version": "[^"]*",/  "version": "$ENV{V}",/m' src-tauri/tauri.conf.json
  (cd src-frontend && npm version "$VERSION" --no-git-tag-version --allow-same-version >/dev/null)
  cargo update --workspace --offline -q 2>/dev/null || cargo update --workspace -q
  git add Cargo.toml Cargo.lock src-tauri/Cargo.toml src-tauri/tauri.conf.json \
    src-frontend/package.json src-frontend/package-lock.json
  git commit -q -m "Release $TAG"
fi

echo "▶ Checks (clippy, tests, frontend build)…"
cargo clippy -q --all-targets -- -D warnings
cargo test -q >/dev/null
(cd src-frontend && npm run build >/dev/null)

if git config user.signingkey >/dev/null; then
  git tag -s "$TAG" -m "Crypto AutoPay $TAG"
else
  echo "  (no git signing key: annotated tag, not signed)"
  git tag -a "$TAG" -m "Crypto AutoPay $TAG"
fi

echo ""
echo "Ready to push $BRANCH and $TAG to $REMOTE ($(git remote get-url "$REMOTE")):"
git log --oneline "$REMOTE/$BRANCH..HEAD"
if [ "$YES" -ne 1 ]; then
  read -r -p "Push now? [y/N] " answer
  case "$answer" in
    y|Y|yes|o|O|oui) ;;
    *) echo "Not pushed. Undo the tag with: git tag -d $TAG"; exit 0 ;;
  esac
fi

git push "$REMOTE" "$BRANCH"
git push "$REMOTE" "$TAG"

URL="$(git remote get-url "$REMOTE" | sed -E 's#^git@github.com:#https://github.com/#; s#\.git$##')"
echo ""
echo "✓ Release workflow started: $URL/actions/workflows/release.yml"
echo "  Draft release (when done): $URL/releases"
echo "  Then sign SHA256SUMS: docs/SIGNING_RELEASES.md"
