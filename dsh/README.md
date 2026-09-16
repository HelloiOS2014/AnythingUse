# AnythingUse as a DSH plugin

This directory makes the repository installable as a **[DeepSeek Harness (DSH)](https://github.com/deepseek-ai/deepseek-harness) plugin bundle**: it registers the two first-party Agent skills with the harness's own skill registry, so a DSH session can route to `lcu` (macOS + Chrome) and `lau` (Android) the moment the profile boots.

Nothing is vendored: the adapter resolves the harness's installed
`@deepseek-ai/dsh-skill-filesystem` provider and registers one isolated provider
that scans this repository's `skills/` directory.

## Install

Install with the harness's own command. The bundle comes **from GitHub**, so it
works on any machine and needs no checkout:

```bash
# profile directories are their own pnpm workspace roots, so -w is required
dsh plugin --profile web add -w github:HelloiOS2014/AnythingUse

# if pnpm cannot re-resolve an unrelated dependency range in that profile
# (a peer range such as ^0.1.5-rc.1 against only-prerelease publications),
# resolve from the local store instead:
dsh plugin --profile web add -w --offline github:HelloiOS2014/AnythingUse
```

`dsh plugin add` also lists the bundle in `dsh.profile.bundles`, so nothing else
is needed. `./scripts/install-dsh.sh [profile]` wraps that ladder and prints the
verification commands:

```bash
./scripts/install-dsh.sh              # GitHub -> the `web` profile
./scripts/install-dsh.sh headless     # GitHub -> another profile
./scripts/install-dsh.sh --local      # this checkout (plugin development; live edits)
./scripts/install-dsh.sh --remove web # uninstall
```

**Update:** re-run the same `add` (or the script) so pnpm re-fetches the ref and
moves the pin to the newest commit; a git dependency is a snapshot of the commit
it was installed from, not a moving branch.

A profile that is already running picks a newly added bundle up on its next boot.

## Verify

```bash
# the composed tree contains the adapter row (no boot)
dsh --profile web --dump-config | grep -A2 anythinguse

# the installed copy registers and loads both skills (no model call)
DSH_PLUGIN_ROOT=~/.dsh/profiles/web/node_modules/anythinguse \
  node scripts/test-dsh-plugin.mjs web
```

Expected:

```text
catalog (2): local-android-use, local-computer-use
  local-android-use [anythinguse] 7079 chars — Operate a real USB-connected Android device…
  local-computer-use [anythinguse] 13708 chars — Operate AnythingUse (`lcu`) computer use…
OK: the DSH plugin bundle registers both AnythingUse skills with the harness registry
```

The skills accept `lcu` and `lau`, so the binaries still have to be on `PATH`
(`./scripts/install-cli.sh`); the plugin only teaches DSH that the skills exist.

## How it works

| File | Role |
|---|---|
| `../package.json` → `dsh.bundle.patch` | declares this repository as a DSH bundle |
| `cordis.patch.yml` | inserts one row, `anythinguse-skills`, pointing at the adapter |
| `index.mjs` | the adapter: resolves the harness provider, registers `providerName: anythinguse` with `includeDefaultRoots: false` and `customSkillDirs: [<repo>/skills]` |

Two details worth knowing if you copy this layout:

- the loader anchors a package-relative `name` in a patch to **that patch file's
  directory**, so `name: './index.mjs'` (not `'./dsh/index.mjs'`) is correct when
  the patch lives in a subdirectory;
- `includeDefaultRoots: false` is deliberate: the harness already scans the
  project and user roots, so scanning them again here would list every local
  skill twice.

## Uninstall

```bash
./scripts/install-dsh.sh --remove web
```
