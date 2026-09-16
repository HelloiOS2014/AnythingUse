# AnythingUse as a DSH plugin

This directory makes the repository installable as a **[DeepSeek Harness (DSH)](https://github.com/deepseek-ai/deepseek-harness) plugin bundle**: it registers the two first-party Agent skills with the harness's own skill registry, so a DSH session can route to `lcu` (macOS + Chrome) and `lau` (Android) the moment the profile boots.

Nothing is vendored: the adapter resolves the harness's installed
`@deepseek-ai/dsh-skill-filesystem` provider and registers one isolated provider
that scans this repository's `skills/` directory.

## Install

```bash
# into the `web` profile (default)
./scripts/install-dsh.sh

# into another profile
./scripts/install-dsh.sh headless
```

The script adds `anythinguse` to the profile's dependencies, lists it in
`dsh.profile.bundles`, and prints the verification command. A profile that is
already running picks the bundle up on its next boot.

## Verify

```bash
# the composed tree contains the adapter row (no boot)
dsh --profile web --dump-config | grep -A2 anythinguse

# the harness's own provider discovers and loads both skills (no model call)
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
