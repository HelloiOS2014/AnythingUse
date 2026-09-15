#!/usr/bin/env bash
# Install the AnythingUse skill into Pi (global git package) and put product
# binaries on PATH. Do not point user settings at this checkout.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="${LCU_PI_PACKAGE:-git:github.com/HelloiOS2014/AnythingUse}"

if ! command -v pi >/dev/null 2>&1; then
  echo "pi not found on PATH. Install Pi first." >&2
  exit 1
fi

echo "Installing AnythingUse skill for Pi from $src"
pi install "$src"

if [[ -x "$root/target/release/lcu" && -x "$root/target/release/lcu-desktop" ]]; then
  "$root/scripts/install-cli.sh"
else
  echo "note: release binaries not built yet; skill is installed, but set LCU_BIN or run:" >&2
  echo "  cargo build -p lcu-cli -p lcu-desktop --release" >&2
  echo "  ./scripts/install-cli.sh" >&2
fi

echo "Done. New Pi sessions load local-computer-use from the git package"
echo "under ~/.pi/agent/git/ (not this checkout)."
echo "This checkout still autoloads via .pi/settings.json after the project is trusted."
