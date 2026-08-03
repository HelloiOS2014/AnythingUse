#!/usr/bin/env bash
# Build and run the per-user macos-window-service.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
swift build -c release
BIN="$ROOT/.build/release/macos-window-service"
exec "$BIN" serve "$@"
