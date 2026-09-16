#!/usr/bin/env bash
# Put AnythingUse product binaries on PATH (~/.local/bin by default).
#
# `lcu` starts a sibling `lcu-desktop` from the same directory
# (`std::env::current_exe()` + file name). Both must be installed together.
# `macos-window-service` is installed beside them when the Swift build exists.
#
# Development default: symlink to this checkout's release builds so rebuilds
# are picked up. Production packaging (`scripts/package-release.sh`) copies the
# same sibling set into a distribution `bin/` directory.
#
# Usage:
#   ./scripts/install-cli.sh
#   LCU_INSTALL_COPY=1 ./scripts/install-cli.sh
#   LCU_BIN_DIR=/custom/bin ./scripts/install-cli.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="${LCU_BIN_DIR:-$HOME/.local/bin}"
copy="${LCU_INSTALL_COPY:-0}"

lcu="$root/target/release/lcu"
desktop="$root/target/release/lcu-desktop"
window="$root/native/macos-window-service/.build/release/macos-window-service"

if [[ ! -x "$lcu" || ! -x "$desktop" ]]; then
  echo "missing release binaries. Build first:" >&2
  echo "  cargo build -p lcu-cli -p lcu-desktop --release" >&2
  exit 1
fi

mkdir -p "$dest"

install_one() {
  local src="$1"
  local name="$2"
  local target="$dest/$name"
  if [[ "$copy" == "1" ]]; then
    cp "$src" "$target"
    chmod +x "$target"
    echo "copied $target"
  else
    ln -sfn "$src" "$target"
    echo "linked $target -> $src"
  fi
}

install_one "$lcu" "lcu"
install_one "$desktop" "lcu-desktop"

# Android endpoint (`lau`): a separate CLI, never `lcu`. Installed when built.
lau="$root/target/release/lau"
if [[ -x "$lau" ]]; then
  install_one "$lau" "lau"
else
  echo "note: lau not built; the Android endpoint stays unavailable until:" >&2
  echo "  cargo build -p lau-cli --release" >&2
fi

if [[ -x "$window" ]]; then
  install_one "$window" "macos-window-service"
else
  echo "note: macos-window-service not built; window surface stays on the compile-time checkout path if present:" >&2
  echo "  (cd native/macos-window-service && swift build -c release)" >&2
fi

case ":$PATH:" in
  *":$dest:"*) ;;
  *)
    echo "note: $dest is not on PATH in this shell; open a new terminal or export PATH=\"$dest:\$PATH\"" >&2
    ;;
esac
