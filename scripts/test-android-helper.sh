#!/usr/bin/env bash
# Run the Android helper's JVM unit tests (pure rules; no device needed).
#
# The helper is a Gradle project without a wrapper in this repo, so the script
# locates a Gradle distribution the same way install-android-helper.sh does.
#
# Usage:
#   ./scripts/test-android-helper.sh
#   LAU_GRADLE=/path/to/gradle ./scripts/test-android-helper.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
helper="$root/native/android-helper"

gradle="${LAU_GRADLE:-}"
if [[ -z "$gradle" ]]; then
  for candidate in "$HOME"/.gradle/wrapper/dists/gradle-*-bin/*/gradle-*/bin/gradle; do
    [[ -x "$candidate" ]] && gradle="$candidate"
  done
fi
if [[ -z "$gradle" || ! -x "$gradle" ]]; then
  echo "gradle not found. Install Gradle (or set LAU_GRADLE=/path/to/gradle)." >&2
  exit 1
fi

echo "Running helper unit tests with $gradle"
"$gradle" -p "$helper" :app:testDebugUnitTest --console=plain "$@"

echo
echo "Results:"
for f in "$helper"/app/build/test-results/testDebugUnitTest/*.xml; do
  [[ -f "$f" ]] || continue
  grep -oE 'tests="[0-9]+" skipped="[0-9]+" failures="[0-9]+" errors="[0-9]+"' "$f" |
    sed "s|^|  $(basename "$f" .xml): |"
done
