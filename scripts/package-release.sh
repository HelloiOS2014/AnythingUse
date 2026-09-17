#!/usr/bin/env bash
# Package AnythingUse for distribution: release binaries, Chrome control
# installer, both Agent skills, and (when built) the Android endpoint plus the
# helper APK, all in one zip (and optional dmg).
#
# Not code-signed: macOS will require right-click → Open on first launch.
set -euo pipefail

VERSION="${1:-0.2.1}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE_ROOT="$(mktemp -d)"
trap 'rm -rf "$STAGE_ROOT"' EXIT
STAGE="$STAGE_ROOT/AnythingUse-$VERSION"

echo "==> Assembling $STAGE"
mkdir -p "$STAGE/bin" "$STAGE/scripts" "$STAGE/skills" "$STAGE/native-host"

cp "$ROOT/target/release/lcu" "$STAGE/bin/"
cp "$ROOT/target/release/lcu-desktop" "$STAGE/bin/"
cp "$ROOT/native/macos-window-service/.build/release/macos-window-service" "$STAGE/bin/"

# Android endpoint: separate CLI (`lau`, never `lcu`) plus the helper APK the
# user sideloads onto their phone.
if [[ -x "$ROOT/target/release/lau" ]]; then
  cp "$ROOT/target/release/lau" "$STAGE/bin/"
else
  echo "warn: target/release/lau missing — build with: cargo build -p lau-cli --release" >&2
fi
lau_apk="$ROOT/native/android-helper/app/build/outputs/apk/debug/app-debug.apk"
if [[ -f "$lau_apk" ]]; then
  mkdir -p "$STAGE/android"
  cp "$lau_apk" "$STAGE/android/anythinguse-lau-helper.apk"
else
  echo "warn: helper APK missing — build with: ./scripts/install-android-helper.sh" >&2
fi
cp "$ROOT/scripts/install-android-helper.sh" "$STAGE/scripts/"
cp -R "$ROOT/native/chrome-control/extension" "$STAGE/extension"
cp "$ROOT/native/chrome-control/native-host/host.mjs" "$STAGE/native-host/"
cp "$ROOT/native/chrome-control/native-host/com.lcu.chrome_control.json.template" "$STAGE/native-host/"
cp "$ROOT/requirements.txt" "$STAGE/"
cp "$ROOT/native/chrome-control/scripts/install-native-host.sh" "$STAGE/scripts/"
cp "$ROOT/native/chrome-control/scripts/uninstall-native-host.sh" "$STAGE/scripts/"
cp -R "$ROOT/skills/local-computer-use" "$STAGE/skills/"
cp -R "$ROOT/skills/local-android-use" "$STAGE/skills/"
cp "$ROOT/docs/lau-android-plan.md" "$STAGE/"
cp "$ROOT/README.md" "$ROOT/README.zh-CN.md" "$ROOT/AGENTS.md" "$STAGE/"
cp "$ROOT/docs/command-contract.md" "$ROOT/docs/troubleshooting.md" "$STAGE/"

cat >"$STAGE/README-RELEASE.txt" <<EOF
AnythingUse v$VERSION — local-first control plane for macOS apps and Chrome.

Components:
  bin/lcu                public CLI (macOS apps + Chrome)
  bin/lcu-desktop        Runtime host (started on demand by lcu)
  bin/macos-window-service  Swift window service (auto-spawned by desktop)
  bin/lau                Android endpoint CLI (source-level; see the LAU plan)
  android/anythinguse-lau-helper.apk  AccessibilityService helper to sideload
  extension/             Chrome extension source (install via scripts/install-native-host.sh)
  skills/                agent skill (install via Claude/Grok plugin marketplace)
  AGENTS.md              agent notes

Android (`lau`, work in progress — see the LAU plan on GitHub):
  ./scripts/install-android-helper.sh          # builds + adb-installs the APK
  # then enable Settings → Accessibility → AnythingUse LAU
  ./bin/lau doctor --json
  ./bin/lau run "<goal>" --app <package> --actor agent --json
  ./bin/lau decide <task-id> --json ; ./bin/lau act <task-id> --observation-id <obs> --action '<json>'
  # The first control of an app needs a decision in the Mac dialog; consequences
  # and R4 takeovers are gated separately.

Quick start:
  ./bin/lcu doctor --json
  # Run ./bin/lcu-desktop yourself only for a persistent menu-bar host.

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
