/**
 * DSH skill-bundle adapter for AnythingUse.
 *
 * Loaded from this package's cordis.patch.yml as a package-relative path, which
 * the loader anchors to a file URL. The patch itself cannot compute the package
 * root: every bundle layer is applied in the profile-root context, so `baseUrl`
 * there points at the profile rather than at this package.
 *
 * The bundle ships the two first-party Agent skills (`skills/local-computer-use`
 * for `lcu` and `skills/local-android-use` for `lau`) to a DSH profile:
 *
 *   dsh plugin --profile <profile> add <path to this repo>
 *   # then add "anythinguse" to dsh.profile.bundles in that profile
 *
 * The adapter resolves the running harness's own skill provider instead of
 * vendoring a copy, so there is exactly one provider implementation and one
 * skill registry per session.
 */
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const name = 'anythinguse-skills';
// No static inject list: a hard dependency on the skills service would fail the
// whole plugin tree on a profile that does not mount one. Registration below
// waits for the service instead, so such a profile simply gets no skills from
// this bundle rather than failing to start.

const HERE = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = resolve(HERE, '..');

/**
 * Resolve the skill provider from the running harness instead of vendoring a
 * second copy. The profile directory is reachable through the loader's baseUrl;
 * walking up from there reaches the harness installation.
 */
async function loadProviderModule(ctx) {
  const anchors = [];
  const baseUrl = ctx?.baseUrl;
  if (typeof baseUrl === 'string') {
    let dir = fileURLToPath(baseUrl);
    for (let i = 0; i < 6; i += 1) {
      anchors.push(join(dir, 'package.json'));
      const parent = dirname(dir);
      if (parent === dir) break;
      dir = parent;
    }
  }
  anchors.push(join(PACKAGE_ROOT, 'package.json'));

  for (const anchor of anchors) {
    if (!existsSync(anchor)) continue;
    try {
      return await import(
        pathToFileURL(createRequire(anchor).resolve('@deepseek-ai/dsh-skill-filesystem')).href
      );
    } catch {
      /* try the next anchor */
    }
  }
  throw new Error(
    'anythinguse-skills: cannot resolve @deepseek-ai/dsh-skill-filesystem from the running harness',
  );
}

/**
 * This repository keeps its skills at `skills/` beside the adapter. The sibling
 * layout is accepted too, so a packer that copies only `dsh/` still works when
 * `skills/` sits next to the package directory.
 */
function locateSkillsRoot() {
  const candidates = [join(PACKAGE_ROOT, 'skills'), resolve(PACKAGE_ROOT, '..', 'skills')];
  return candidates.find((candidate) => existsSync(candidate));
}

export function apply(ctx, config = {}) {
  const skillsRoot = locateSkillsRoot();
  if (skillsRoot === undefined) {
    throw new Error(`anythinguse-skills: no skills/ directory found near ${PACKAGE_ROOT}`);
  }

  ctx.inject(['skills'], (serviceCtx) => {
    void (async () => {
      const { FileSystemSkillProvider } = await loadProviderModule(serviceCtx);
      serviceCtx.skills.registerProvider((control) =>
        new FileSystemSkillProvider(serviceCtx, control, {
          providerName: config.providerName ?? 'anythinguse',
          // An isolated provider: the project and user roots are already covered
          // by the harness's own filesystem provider, so scanning them again
          // here would surface every local skill twice.
          includeDefaultRoots: false,
          customSkillDirs: [skillsRoot],
          watch: config.watch ?? false,
        }),
      );
    })().catch((error) => {
      // Report rather than throw: a missing provider must not fail the boot.
      ctx.logger?.error?.('anythinguse-skills: ' + (error?.message ?? String(error)));
    });
  });
}
