# Delivery status

Version: **v3.2** (macOS core)  
Branch: `main`  
Product name: **AnythingUse**  
Command codename: **LCU** (`lcu`, `lcu-*`); data root: `~/Library/Application Support/AnythingUse`

## Source-level delivery

This page records source and interface delivery. **Implemented** means the
behavior exists in the source tree; it does not mean that AnythingUse is already
proven mature, stable, or compatible with every real application.

Current macOS-core implementation includes:

- Humans and Agents share the public `lcu` CLI; Agents use the bundled Skill only.
- One serial FIFO queue per login user. `waiting_actor` / paused tasks release the execution slot while an Agent continuation keeps only its strict target reservation; crash leftovers recover to `paused`.
- Three distinct GUI gates: stable app access, task-scoped foreground activation, and one-time consequence confirmation/takeover. Gate completion discards the old proposal and returns a fresh observation to the same Actor.
- macOS: strict window identity (PID + `CGWindowID`); signed identity includes Team ID, signing ID, and code hash (unsigned identity uses canonical path + executable hash); task-level `auto`, `background_only`, or `foreground` control mode; no restore of the previous app; real user HID on the target pauses automatically while focus changes alone do not.
- Foreground Agent turns suspend native ownership. Resume is non-activating and allowed only for the same untouched foreground window; the listen-only HID monitor ignores tagged AnythingUse events and treats real click/key/scroll as takeover.
- Sleep/wake and login-session activation changes invalidate temporary reservations, one-time grants, and control sessions; only affected tasks pause.
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
  proven. It is replaced by the GUI-approved **foreground session**: after one
  approval the exact window may be activated and driven; the agent never
  switches back, and real user input on the target pauses automatically.

Current execution ladder: background semantic → provably isolated background
targeted → task-scoped foreground session → explicit failure. Background is
preferred, never promised. Foreground capability and real consequences use
separate grants; approval never replays a stale action.

The current realignment is source-verified only. Pending one concentrated Gate
1 live acceptance: a background-capable app, a generic screenshot-only target
that requires foreground, and a real Chrome background task tab. No application
name or bundle ID is part of Runtime policy.

Hard product boundaries (not optional):

- No MCP Computer Use control plane.
- No Playwright / independent automation browser.
- No public TCP control surface (private per-user Unix sockets only).

## Outside the current source scope

These are not part of the current macOS-core source scope:

- Top100 multi-app certification suites and long soak / stress programs as release blockers.
- macOS Developer ID signed installer / notarized packaging.
- Windows (or other OS) backends.

## Follow-up (independent tracks)

1. Signed macOS packaging and install/uninstall story.
2. Windows compatibility on the same command-oriented model.

Neither changes the current source-level delivery statement.

## Naming map

| Surface | Name |
|---|---|
| Product / GitHub | AnythingUse |
| CLI binary | `lcu` |
| Desktop host | `lcu-desktop` |
| Agent Skill directory | `skills/local-computer-use` |
| Runtime data directory | `~/Library/Application Support/AnythingUse` |
| Crates / native prefixes | `lcu-*`, historical “Local Computer Use” phrasing in some docs |

“Local Computer Use” in Skill and older headings is the historical product line name; new prose should prefer **AnythingUse** while keeping path and binary names stable.
