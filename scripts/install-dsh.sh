#!/usr/bin/env bash
# Install AnythingUse's Agent skills into a DSH profile as a DSH plugin bundle.
#
# This repository is the bundle: it declares `dsh.bundle.patch` in package.json,
# and dsh/cordis.patch.yml inserts an adapter that registers
# `skills/local-computer-use` (lcu) and `skills/local-android-use` (lau) with the
# harness's own filesystem skill provider. Installing only adds a dependency and
# a bundles entry to the profile — nothing under ~/.dsh is rewritten by hand.
#
# Usage:
#   ./scripts/install-dsh.sh                 # install into the `web` profile
#   ./scripts/install-dsh.sh <profile>       # install into another profile
#   ./scripts/install-dsh.sh --remove [profile]
#
# After installing into a running profile, restart that profile (e.g. `dsh web`)
# so the bundle layer is composed.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
dsh_home="${DSH_HOME:-$HOME/.dsh}"

remove=0
if [[ "${1:-}" == "--remove" ]]; then
  remove=1
  shift
fi
profile="${1:-web}"
profile_dir="$dsh_home/profiles/$profile"

if ! command -v dsh >/dev/null 2>&1; then
  echo "dsh not found on PATH; install the DeepSeek Harness CLI first." >&2
  exit 1
fi

if [[ "$remove" == "1" ]]; then
  if [[ ! -d "$profile_dir" ]]; then
    echo "no such profile: $profile_dir" >&2
    exit 1
  fi
  dsh plugin --profile "$profile" remove anythinguse || true
  python3 - "$profile_dir/package.json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as fh:
    manifest = json.load(fh)
bundles = manifest.setdefault("dsh", {}).setdefault("profile", {}).get("bundles", [])
if "anythinguse" in bundles:
    bundles.remove("anythinguse")
    with open(path, "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")
    print("removed 'anythinguse' from dsh.profile.bundles")
else:
    print("'anythinguse' was not listed in dsh.profile.bundles")
PY
  exit 0
fi

echo "==> Adding $root to profile '$profile'"
# A profile is its own pnpm workspace root, so the dependency must be declared
# explicitly with -w; `dsh plugin` forwards these args to pnpm verbatim.
if ! dsh plugin --profile "$profile" add -w "$root"; then
  # pnpm resolves the whole graph on every add, so a profile whose other
  # dependencies no longer resolve (e.g. a range that only prereleases satisfy)
  # fails here for reasons unrelated to this bundle. A `link:` dependency plus a
  # symlink is exactly what pnpm would have written, without touching the rest of
  # the graph.
  echo "note: pnpm could not update the profile; wiring the bundle directly instead" >&2
  python3 - "$profile_dir/package.json" "$root" <<'PY'
import json, os, pathlib, sys
manifest_path = pathlib.Path(sys.argv[1])
root = sys.argv[2]
manifest = json.loads(manifest_path.read_text())
deps = manifest.setdefault("dependencies", {})
deps["anythinguse"] = f"link:{root}"
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
link = manifest_path.parent / "node_modules" / "anythinguse"
link.parent.mkdir(parents=True, exist_ok=True)
if link.is_symlink() or link.exists():
    link.unlink()
link.symlink_to(root)
print(f"linked {link} -> {root}")
PY
fi

echo "==> Listing the bundle in $profile_dir/package.json"
python3 - "$profile_dir/package.json" <<'PY'
import json, sys
path = sys.argv[1]
with open(path) as fh:
    manifest = json.load(fh)
bundles = manifest.setdefault("dsh", {}).setdefault("profile", {}).setdefault("bundles", [])
if "anythinguse" not in bundles:
    bundles.append("anythinguse")
    with open(path, "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")
    print("added 'anythinguse' to dsh.profile.bundles")
else:
    print("'anythinguse' already listed")
PY

echo
echo "==> Verify the composed tree (no boot):"
echo "    dsh --profile $profile --dump-config | grep -i anythinguse"
echo
echo "Restart the profile if it is already running, then the session catalog"
echo "gains local-computer-use and local-android-use."
