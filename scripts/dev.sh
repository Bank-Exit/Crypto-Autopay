#!/usr/bin/env bash
# dev.sh — clean dev startup: kills stale instances before launching
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "▶ Killing any stale monero-autopay processes…"
pkill -f 'target/debug/monero-autopay' 2>/dev/null && echo "  Killed stale monero-autopay" || true
pkill -f 'target/release/monero-autopay' 2>/dev/null && sleep 0.5 || true

echo "▶ Freeing port 5173 if busy…"
lsof -ti:5173 | xargs kill -9 2>/dev/null && echo "  Killed stale vite on :5173" || true

# Wait briefly for processes to die
sleep 0.5

echo "▶ Starting cargo tauri dev…"
exec cargo tauri dev
