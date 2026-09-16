#!/usr/bin/env node
/**
 * Self-check for this repository's DSH plugin bundle (`dsh/cordis.patch.yml` +
 * `dsh/index.mjs`).
 *
 * It boots a minimal real composition — cordis + the harness's filesystem
 * backend + the skill registry — and loads this repository's adapter exactly as
 * a profile would. Then it asks the registry for the merged catalog and for each
 * skill body. That covers everything that can silently break: the adapter's
 * provider resolution and registration, the skill directory layout, and the YAML
 * frontmatter.
 *
 * Usage:
 *   node scripts/test-dsh-plugin.mjs [profile]     # default profile: web
 *   DSH_PLUGIN_ROOT=/path/to/installed/anythinguse node scripts/test-dsh-plugin.mjs [profile]
 *   DSH_SKILL_ROOT=/path/to/dsh-skill node scripts/test-dsh-plugin.mjs
 *
 * `DSH_PLUGIN_ROOT` checks an installed copy (for example
 * `~/.dsh/profiles/<profile>/node_modules/anythinguse`) instead of this
 * checkout — that is how a git-installed bundle is verified.
 *
 * Exit code 0 means both skills are in the catalog AND their bodies load.
 */
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = process.env.DSH_PLUGIN_ROOT
  ? resolve(process.env.DSH_PLUGIN_ROOT)
  : resolve(here, '..');
const profile = process.argv[2] ?? 'web';
const dshHome = process.env.DSH_HOME ?? join(homedir(), '.dsh');
const profileDir = join(dshHome, 'profiles', profile);
const EXPECTED = ['local-android-use', 'local-computer-use'];

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

/** Resolve harness packages from the profile first (what a booted profile has), then the dsh install. */
function makeResolver() {
  const anchors = [];
  if (existsSync(join(profileDir, 'package.json'))) {
    anchors.push(join(profileDir, 'package.json'));
  }
  for (const candidate of [
    join(dshHome, 'node_modules', '@deepseek-ai', 'dsh', 'package.json'),
    '/usr/local/lib/node_modules/@deepseek-ai/dsh/package.json',
  ]) {
    if (existsSync(candidate)) anchors.push(candidate);
  }
  const override = process.env.DSH_SKILL_ROOT;
  if (override) anchors.unshift(join(override, 'package.json'));
  return (specifier) => {
    for (const anchor of anchors) {
      try {
        return createRequire(anchor).resolve(specifier);
      } catch {
        /* try the next anchor */
      }
    }
    fail(
      `cannot resolve ${specifier} (tried profile "${profile}" and the dsh installation)`,
    );
  };
}

const resolveFrom = makeResolver();
const load = async (specifier) => import(pathToFileURL(resolveFrom(specifier)).href);

const { Context } = await load('@deepseek-ai/cordis');
const fsLocal = await load('@deepseek-ai/dsh-fs-local');
const skillService = await load('@deepseek-ai/dsh-skill');
const adapter = await import(pathToFileURL(join(repoRoot, 'dsh', 'index.mjs')).href);

const root = new Context();
// A profile-root baseUrl is what the real loader provides; the adapter resolves
// the harness's own provider package through it.
root.baseUrl = pathToFileURL(join(profileDir, '/')).href;

/** A module namespace is not itself a plugin; the loader picks `default` or an `apply` export. */
const asPlugin = (mod) => {
  if (typeof mod === 'function') return mod;
  if (mod?.default && (typeof mod.default === 'function' || mod.default.apply)) return mod.default;
  return mod;
};

await root.plugin(asPlugin(fsLocal), { cwd: repoRoot });
await root.plugin(asPlugin(skillService));
await root.plugin(asPlugin(adapter));

// The adapter registers its provider from an async dynamic import (the same
// shape a real boot uses), so poll briefly instead of racing it.
const deadline = Date.now() + 5000;
let catalog = [];
while (Date.now() < deadline) {
  catalog = await root.skills.list({ cwd: repoRoot });
  if (EXPECTED.every((name) => catalog.some((summary) => summary.name === name))) break;
  await new Promise((wake) => setTimeout(wake, 100));
}

const names = catalog.map((summary) => summary.name).sort();
console.log(`catalog (${names.length}): ${names.join(', ') || '(empty)'}`);

const ours = catalog.filter((summary) => EXPECTED.includes(summary.name));
if (ours.length !== EXPECTED.length) {
  const missing = EXPECTED.filter((name) => !names.includes(name));
  fail(`missing skill(s): ${missing.join(', ')}`);
}

for (const summary of ours) {
  const loaded = await root.skills.get(summary.name, { cwd: repoRoot });
  const body = loaded?.body ?? loaded?.content ?? '';
  if (typeof body !== 'string' || body.length < 200) {
    fail(`skill "${summary.name}" loaded no usable body (${body?.length ?? 0} chars)`);
  }
  console.log(
    `  ${summary.name} [${summary.provider}] ${body.length} chars — ${summary.description.slice(0, 60)}…`,
  );
}

console.log('OK: the DSH plugin bundle registers both AnythingUse skills with the harness registry');
