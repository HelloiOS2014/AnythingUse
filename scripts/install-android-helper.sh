#!/usr/bin/env bash
# Build and sideload the LAU AccessibilityService helper.
# Does not enable the service — that is a one-time Settings toggle on the phone.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
app="$root/native/android-helper"
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
java_home="${JAVA_HOME:-$(/usr/libexec/java_home -v 17 2>/dev/null || true)}"
gradle="${LAU_GRADLE:-}"
serial="${LAU_SERIAL:-}"

if [[ -z "$gradle" ]]; then
  gradle="$(find "$HOME/.gradle/wrapper/dists/gradle-9.2.0-bin" -type f -name gradle -perm +111 2>/dev/null | head -n 1 || true)"
fi
if [[ -z "$gradle" || ! -x "$gradle" ]]; then
  echo "gradle not found. Set LAU_GRADLE to a Gradle 8.11+/9 binary." >&2
  exit 64
fi
if [[ ! -d "$sdk/platforms" ]]; then
  echo "Android SDK not found at $sdk. Set ANDROID_HOME." >&2
  exit 64
fi

export ANDROID_HOME="$sdk"
export ANDROID_SDK_ROOT="$sdk"
if [[ -n "$java_home" ]]; then
  export JAVA_HOME="$java_home"
fi

printf 'sdk.dir=%s\n' "$sdk" > "$app/local.properties"

echo "Building helper APK with $gradle"
"$gradle" -p "$app" --quiet assembleDebug

apk="$app/app/build/outputs/apk/debug/app-debug.apk"
if [[ ! -f "$apk" ]]; then
  echo "apk not produced: $apk" >&2
  exit 70
fi

adb=(adb)
if [[ -n "${LAU_ADB_BIN:-}" ]]; then
  adb=("$LAU_ADB_BIN")
fi
if [[ -n "$serial" ]]; then
  adb+=(-s "$serial")
fi

echo "Installing $apk"
if ! "${adb[@]}" install -r "$apk"; then
  cat >&2 <<'EOF'
install failed. On HyperOS/MIUI:
  Developer options → USB debugging ON
  Developer options → USB debugging (Security settings) ON
  Developer options → Install via USB ON  (requires a signed-in Mi account)
Then re-run this script.
EOF
  exit 3
fi

echo "Installed. Enable Accessibility → AnythingUse LAU, then: lau doctor --json"
echo "The helper does not run until that toggle is on."
