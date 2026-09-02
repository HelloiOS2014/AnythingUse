#!/usr/bin/env bash
# Install the AnythingUse skill into Pi (global) and put product binaries on PATH.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v pi >/dev/null 2>&1; then
  echo "pi not found on PATH. Install Pi first." >&2
  exit 1
fi

if [[ ! -f "$root/package.json" ]]; then
  echo "missing $root/package.json (Pi package manifest)" >&2
  exit 1
fi

echo "Installing AnythingUse skill for Pi from $root"
# Absolute path: relative sources resolve against ~/.pi/agent/settings.json.
pi install "$root"

if [[ -x "$root/target/release/lcu" && -x "$root/target/release/lcu-desktop" ]]; then
  "$root/scripts/install-cli.sh"
else
  echo "note: release binaries not built yet; skill is installed, but set LCU_BIN or run:" >&2
  echo "  cargo build -p lcu-cli -p lcu-desktop --release" >&2
  echo "  ./scripts/install-cli.sh" >&2
fi

echo "Done. New Pi sessions should list the local-computer-use skill."
echo "This checkout also autoloads it via .pi/settings.json after the project is trusted."
