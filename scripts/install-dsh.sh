#!/usr/bin/env bash
# Install AnythingUse's Agent skills into a DSH profile as a plugin bundle.
#
# The bundle is fetched from GitHub by default, so it works on any machine and
# updates with a normal pnpm/dsh plugin update:
#
#   dsh plugin --profile web add -w github:HelloiOS2014/AnythingUse
#
# This repository declares the DSH bundle (`dsh.bundle.patch` in package.json plus
# dsh/cordis.patch.yml), so installing only adds a dependency and a bundles entry
# to the profile. `dsh plugin add` registers the bundle itself; the steps below
# stay idempotent and cover the paths where pnpm cannot run at all.
#
# Usage:
#   ./scripts/install-dsh.sh                     # GitHub -> the `web` profile
#   ./scripts/install-dsh.sh headless            # GitHub -> another profile
#   ./scripts/install-dsh.sh --local             # this checkout (plugin development)
#   ./scripts/install-dsh.sh --remove [profile]  # uninstall
#   ANYTHINGUSE_DSH_SOURCE=github:you/fork ./scripts/install-dsh.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
dsh_home="${DSH_HOME:-$HOME/.dsh}"
source_spec="${ANYTHINGUSE_DSH_SOURCE:-github:HelloiOS2014/AnythingUse}"

remove=0
use_local=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --remove) remove=1; shift ;;
    --local) use_local=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) break ;;
  esac
done
profile="${1:-web}"
profile_dir="$dsh_home/profiles/$profile"

if [[ "$use_local" == "1" ]]; then
  # A checkout dependency is a link, so edits to the adapter and the skills are
  # live. Use it while developing the plugin; use GitHub for real installs.
  source_spec="$root"
fi

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

echo "==> Adding $source_spec to profile '$profile'"
# The documented path is `dsh plugin --profile <p> <pnpm args>`, which forwards to
# pnpm in the profile directory. A profile is its own pnpm workspace root, so the
# dependency needs -w.
installed=0
if dsh plugin --profile "$profile" add -w "$source_spec"; then
  installed=1
elif dsh plugin --profile "$profile" add -w --offline "$source_spec"; then
  # pnpm re-resolves the whole graph on every add. A profile whose other
  # dependencies no longer resolve from the registry (peer ranges such as
  # `^0.1.5-rc.1` against published versions) fails there for reasons unrelated
  # to this bundle; --offline resolves from the local store and lockfile instead.
  echo "note: resolved the profile from the local store (--offline)" >&2
  installed=1
elif [[ "$use_local" == "1" ]]; then
  # Only a checkout can be wired by hand: a git dependency has to be fetched.
  echo "note: pnpm could not update the profile; linking the checkout instead" >&2
  python3 - "$profile_dir/package.json" "$root" <<'PY'
import json, pathlib, sys
manifest_path = pathlib.Path(sys.argv[1])
root = sys.argv[2]
manifest = json.loads(manifest_path.read_text())
manifest.setdefault("dependencies", {})["anythinguse"] = f"link:{root}"
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
link = manifest_path.parent / "node_modules" / "anythinguse"
link.parent.mkdir(parents=True, exist_ok=True)
if link.is_symlink() or link.exists():
    link.unlink()
link.symlink_to(root)
print(f"linked {link} -> {root}")
PY
  installed=1
fi
if [[ "$installed" != "1" ]]; then
  echo "pnpm could not install $source_spec in profile '$profile'." >&2
  echo "Fix that profile's dependency resolution, or use --local to develop against this checkout." >&2
  exit 1
fi

echo "==> Ensuring the bundle is listed in $profile_dir/package.json"
# `dsh plugin add` usually does this already; this keeps the step explicit and
# idempotent for the fallback path.
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
echo "    DSH_PLUGIN_ROOT=$profile_dir/node_modules/anythinguse node scripts/test-dsh-plugin.mjs $profile"
echo
echo "Restart the profile if it is already running, then the session catalog"
echo "gains local-computer-use and local-android-use."
