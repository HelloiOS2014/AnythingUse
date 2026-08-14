#!/usr/bin/env bash
# Package AnythingUse v0.1.0 for distribution: release binaries, Chrome
# control installer, skill, and docs in one zip (and optional dmg).
#
# Not code-signed: macOS will require right-click → Open on first launch.
set -euo pipefail

VERSION="${1:-0.1.0}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE_ROOT="$(mktemp -d)"
trap 'rm -rf "$STAGE_ROOT"' EXIT
STAGE="$STAGE_ROOT/AnythingUse-$VERSION"

echo "==> Assembling $STAGE"
mkdir -p "$STAGE/bin" "$STAGE/scripts" "$STAGE/skills" "$STAGE/native-host"

cp "$ROOT/target/release/lcu" "$STAGE/bin/"
cp "$ROOT/target/release/lcu-desktop" "$STAGE/bin/"
cp "$ROOT/native/macos-window-service/.build/release/macos-window-service" "$STAGE/bin/"
cp -R "$ROOT/native/chrome-control/extension" "$STAGE/extension"
cp "$ROOT/native/chrome-control/native-host/host.mjs" "$STAGE/native-host/"
cp "$ROOT/native/chrome-control/native-host/com.lcu.chrome_control.json.template" "$STAGE/native-host/"
cp "$ROOT/requirements.txt" "$STAGE/"
cp "$ROOT/native/chrome-control/scripts/install-native-host.sh" "$STAGE/scripts/"
cp "$ROOT/native/chrome-control/scripts/uninstall-native-host.sh" "$STAGE/scripts/"
cp -R "$ROOT/skills/local-computer-use" "$STAGE/skills/"
cp "$ROOT/README.md" "$ROOT/README.zh-CN.md" "$ROOT/AGENTS.md" "$STAGE/"
cp "$ROOT/docs/command-contract.md" "$ROOT/docs/troubleshooting.md" "$STAGE/"

cat >"$STAGE/README-RELEASE.txt" <<EOF
AnythingUse v$VERSION — local-first control plane for macOS apps and Chrome.

Components:
  bin/lcu                public CLI
  bin/lcu-desktop        Runtime host (start this first)
  bin/macos-window-service  Swift window service (auto-spawned by desktop)
  extension/             Chrome extension source (install via scripts/install-native-host.sh)
  skills/                agent skill (install via Claude/Grok plugin marketplace)
  AGENTS.md              agent notes

Quick start:
  ./bin/lcu-desktop &
  ./bin/lcu doctor --json

External Agent (default decision path):
  ./bin/lcu run "Open Downloads in Finder" --app com.apple.finder --actor agent --json
  ./bin/lcu decide <task-id> --wait --json
  ./bin/lcu act <task-id> --observation-id <obs> --action '<json>'
  # Repeat decide/act; finish with a done action and confirm lcu result is succeeded.

Optional local VLM (local model resources required):
  ./bin/lcu run "Open Downloads in Finder" --app com.apple.finder --actor vlm --wait --json

Chrome surface:
  ./scripts/install-native-host.sh
  # chrome://extensions → Load unpacked → the extension: path printed above

Not code-signed: macOS shows Gatekeeper prompts on first run (right-click → Open).
Documentation: docs/ on GitHub (https://github.com/HelloiOS2014/AnythingUse).
EOF

echo "==> Building zip"
cd "$(dirname "$STAGE")"
zip -qr "AnythingUse-$VERSION.zip" "AnythingUse-$VERSION"
mv "AnythingUse-$VERSION.zip" "$ROOT/dist/" 2>/dev/null || { mkdir -p "$ROOT/dist"; mv "AnythingUse-$VERSION.zip" "$ROOT/dist/"; }

if command -v hdiutil >/dev/null 2>&1; then
  echo "==> Building dmg"
  hdiutil create -volname "AnythingUse-$VERSION" -srcfolder "AnythingUse-$VERSION" \
    -ov -format UDZO "$ROOT/dist/AnythingUse-$VERSION.dmg" >/dev/null
fi

echo "==> Done"
ls -lh "$ROOT/dist/"
