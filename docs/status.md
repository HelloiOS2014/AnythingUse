# Delivery status

Version: **v3.2** (macOS core)  
Branch: `main`  
Product name: **AnythingUse**  
Command codename: **LCU** (`lcu`, `lcu-*`); data root: `~/Library/Application Support/AnythingUse`

Target contract: [AnythingUse Execution Contract](execution-contract.md),
frozen 2026-08-14. The source is aligned; concentrated live acceptance passed
2026-08-17. This page does not turn that evidence into a broad compatibility claim.

## Source-level delivery

This page records source and interface delivery. **Implemented** means the
behavior exists in the source tree; it does not mean that AnythingUse is already
proven mature, stable, or compatible with every real application.

Current macOS-core implementation includes:

- Humans and Agents share the public `lcu` CLI; Agents use the bundled Skill only.
- One serial FIFO queue per login user. `waiting_actor` / paused tasks release the execution slot and generic target reservation; crash leftovers recover to `paused`.
- GUI decisions cover stable app access and one-time consequence confirmation/takeover. App access discloses the possible foreground fallback. Any activation or approval discards the old proposal and returns a fresh observation to the same Actor.
- macOS: strict window identity (PID + `CGWindowID`); signed identity includes Team ID, signing ID, and code hash (unsigned identity uses canonical path + executable hash); task-level `auto` or `background_only`; no restore of the previous app; real user HID on the target pauses automatically while focus changes alone do not.
- Agent waits release generic native ownership. Continuation re-resolves and re-observes; the listen-only HID monitor ignores tagged AnythingUse events and treats real click/key/scroll on the controlled target as takeover.
- Sleep/wake and login-session activation changes invalidate temporary reservations and one-time grants; only affected tasks pause.
- Runtime startup removes terminal SQLite tasks/events older than 30 days or beyond the newest 1,000; active tasks are untouched. Agent/VLM screenshots are removed on consume/pause/terminal and crash leftovers older than one hour are pruned.
- Chrome: real user Chrome via extension + Native Messaging + inactive task tab (not Playwright), bound to a profile-local stable ID, tab lease, current page scope, and real screenshot hash.
- The menu-bar app lists persistent app permissions and can revoke an `always_allow` decision.
- Pluggable decision maker: when `LCU_VISION_ACTOR` is unset/`auto`, tasks default to the external Agent (`lcu decide`/`lcu act`); `--actor vlm` selects the optional local Qwen3-VL subprocess per task. Both submit the same observation-bound Action plus closed-set `effect` through one validation, risk, gate, and execution path.
- Task `succeeded` only after explicit model `Done` and successful re-observation of the target.

## Runtime verification boundary

`Runtime-verified` requires a recorded live run on the target surface. The
source-level items above are not a substitute for verifying a specific app,
Chrome profile, permissions state, model, or user-coexistence scenario. No
broad stability or compatibility claim is made by this status page.

Earlier live evidence recorded before the current realignment:

- TextEdit: semantic `set_value` followed by explicit `Done` succeeded without making TextEdit frontmost.
- The experimental SkyLight SPI input path (both the rejected focus-without-raise
  spike and the opt-in target-only transport) was **removed** (2026-08-12):
  focus records disrupted the user's keyboard focus and delivery could not be
  proven. It is replaced by the app-access-disclosed foreground fallback: in
  `auto`, the exact window may be activated, the old proposal is discarded,
  and the agent never switches back.

Current execution ladder: background semantic → provably isolated background
targeted → disclosed exact-target foreground fallback in `auto` → explicit
failure in `background_only`. Real consequences retain their own one-time
confirmation; activation and approval never replay a stale action.

Concentrated Gate 1 live acceptance passed 2026-08-17:

- Chrome task tab: Example Domain was observed and the task reached `succeeded`
  without operating the user's existing tab (`task_453ecc07-b518-4cbb-b716-a205e60cef24`).
- Background semantic macOS path: Xcode selected Project Navigator, re-observed it,
  survived a real-user takeover pause without losing task-scoped app access, and
  reached `succeeded` (`task_b4abd6e3-806c-41b6-9c2c-0b16e11a0b2b`).
- Foreground fallback: a screenshot-only enterprise WeChat observation requested
  foreground, activated the exact PID/window through AppKit, discarded the old
  targeted proposal, re-observed a fresh AX tree, entered and cleared the search
  text semantically, and reached `succeeded` without opening a contact or sending
  a message (`task_04d43088-f5be-4b01-a250-ad3a9e2a2be8`).

No application name or bundle ID is part of Runtime policy. These three runs prove
the execution ladder, not compatibility with every macOS application.

Hard product boundaries (not optional):

- No MCP Computer Use control plane.
- No Playwright / independent automation browser.
- No public TCP control surface (private per-user Unix sockets only).

## Parallel endpoint: Android (`lau`) — source-level, device-verified

Android is a **separate** CLI (`lau`, never `lcu`) with its own plan
([LAU Android plan](lau-android-plan.md)) and its own skill
(`skills/local-android-use/`). It is **not** part of the v3.2 macOS-core delivery
claim above, and it makes **no broad compatibility claim**: the evidence is one
device (Xiaomi 2211133C / Android 16) on 2026-09-15, recorded in
`evidence/lau/phase2-acceptance-2026-09-15.md`.

- Source exists for `lau doctor`/`screenshot`/`dump`/`invoke`/`set_value`/`scroll`/`foreground`/`launch`, for an on-demand daemon, and for `run`/`decide`/`act`/`status`/`result`/`cancel`/`resume`/`approve`/`permissions`.
- The helper APK builds from `native/android-helper/` (Kotlin AccessibilityService). ADB stays transport/observation only — no ADB input injection, and coordinate actions are refused outright (`semantic_action_required`).
- The safety model is implemented and device-verified: application access (package + signing-certificate identity, Mac dialog, persisted `always_allow`, `lau permissions --revoke`), consequence confirmation, R4 human takeover, an Android evidence layer (credential fields → R4, send/delete/pay labels → R3, actor claims can only raise the floor) and approvals that are **never replayed**. The hardware-touch watch is fail-closed (a dead `getevent` stream pauses the task; `lau resume` rebuilds it), and each device has one serial queue.
- Observations are session-scoped (`<sessionId>:<generation>`) and re-verified before every action (session, generation, node identity, bounds, capability). Section 0 of the plan lists every remaining gap.
- Partly productized: `./scripts/install-cli.sh` also installs `lau`, and `package-release.sh` ships `bin/lau`, the helper APK and both skills. Not signed or notarized.

Shared platform-neutral contracts now live in `crates/anything-core`
(`lcu-core` re-exports them and keeps the macOS evidence guard); `lau-cli`
depends on `anything-core`, not on `lcu-core`. That refactor is source-level and
does not change the macOS delivery statement above.

## Outside the current source scope

These are not part of the current macOS-core source scope:

- Top100 multi-app certification suites and long soak / stress programs as release blockers.
- macOS Developer ID signed installer / notarized packaging.
- Windows (or other OS) backends.

## Follow-up (independent tracks)

1. Signed macOS packaging and install/uninstall story.
2. Windows compatibility on the same command-oriented model.
3. Finishing the Android endpoint (`lau`): app-access gate, Android evidence layer, packaging, Skill.

None of these changes the current source-level delivery statement.

## Naming map

| Surface | Name |
|---|---|
| Product / GitHub | AnythingUse |
| CLI binary | `lcu` |
| Android CLI | `lau` (`crates/lau-cli`); helper APK `dev.anythinguse.lau.helper` |
| Desktop host | `lcu-desktop` |
| Agent Skill directory | `skills/local-computer-use` |
| Pi package | `package.json` (`pi.skills` → `./skills`); project autoload `.pi/settings.json` |
| Runtime data directory | `~/Library/Application Support/AnythingUse` |
| Crates / native prefixes | `lcu-*`, historical “Local Computer Use” phrasing in some docs |

“Local Computer Use” in Skill and older headings is the historical product line name; new prose should prefer **AnythingUse** while keeping path and binary names stable.
