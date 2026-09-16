---
name: local-android-use
description: Operate a real USB-connected Android device through AnythingUse's `lau` CLI — semantic Accessibility actions, never ADB input injection, never coordinates. Use when the user asks to drive a phone, tap/type/scroll on Android, or run `lau`. Only the `lau` CLI; no ADB input, no private sockets, no MCP, no coordinates.
---

# AnythingUse Skill — Android (`lau`)

`lau` = **Local Android Use**. It is a **separate** CLI from `lcu` (macOS apps + Chrome); the two share action/effect contracts and nothing else.

**Status: source-level, still in development.** The safety model (application access, consequence confirmation, R4 takeover, evidence floor) is implemented and device-verified, but this is not a signed, broadly-certified release. When behaviour is unclear, stop and ask the human instead of improvising.

## Binary

Resolve `lau` in this order:

1. `$LAU_BIN` if set and executable
2. `lau` on `PATH` (product install: `./scripts/install-cli.sh` puts `lcu`, `lcu-desktop` **and** `lau` in `~/.local/bin`)
3. `./target/release/lau` from an AnythingUse checkout (**debug fallback**)

`lau` starts its own on-demand daemon (`lau daemon`, exits after 60 idle seconds). It never starts `lcu-desktop`.

## Device requirements

- Helper APK installed and its Accessibility service **enabled**: `./scripts/install-android-helper.sh`, then Settings → Accessibility → "AnythingUse LAU". HyperOS also needs autostart + unrestricted battery, otherwise the service is not revived after a kill.
- Start with `lau doctor --json`. The four states are `installed / enabled / bound / ping`. **`enabled` is the only gate**; `bound` and `ping` are diagnostics (a live socket with the service disabled is a stale instance).
- **ADB is transport and observation only.** Never inject input through ADB (`input tap`, `input text`, `am start`), never toggle the accessibility service, never wake or unlock the device.

## The loop (you are the decision maker)

```bash
lau run "<goal>" --app <package> --actor agent --json
lau decide <task-id> --json                 # compact elements + image_path + observation token
lau act <task-id> --observation-id <token> --action '<json>' --effect '<json>'
# repeat decide → act; finish with {"kind":"done","summary":"..."}
lau result <task-id> --json                 # succeeded / failed / cancelled
```

- `decide` returns compact `elements[]` (`id` / `role` / `label` / `frame` / `capabilities`, plus `password: true` on credential fields) and an `image_path` (0600 temp file, deleted when the decision is consumed). Extract what you need; do not echo the array.
- Clickable rows now carry the label of their contents (`"蓝牙 已开启"`), so **pick the target by label**, not by position.
- Every action is bound to the observation token `<sessionId>:<generation>`. **Do not parse, store or rebuild it** — act on the decision you just fetched, and fetch a new one after every action.
- Executable actions need the closed-set `--effect` object, e.g. `{"kind":"navigate","summary":"open Bluetooth settings"}`. Runtime raises the risk floor from evidence; your claim can never lower it.
- Actions: `{"kind":"semantic","type":"invoke","element_id":"eN"}`, `set_value` (+`value`), `scroll` (+`element_id`, `delta_x`, `delta_y`), `focus`, and `{"kind":"semantic","type":"global_back"}` for the system back; control actions are `observe`, `wait`, `done`, `fail`, `request_user`.
- **Coordinates are refused outright** (`semantic_action_required`, exit 3). There is no tap-by-position fallback: use the capability the element advertises.
- Exit codes: `0` ok · `2` waiting for the human · `3` failed or refused · `64` usage · `69` runtime unavailable · `70` internal.

## Gates you must never try to bypass

- **App access** — the first control of a package pops a **Mac** dialog (Deny / Always allow / Allow once). `lau` cannot answer it: stop and tell the human. `lau permissions --json` lists persisted grants; `lau permissions --revoke <key>` revokes one.
- **Consequence (R3)** — sending, deleting, submitting, paying parks the task as `waiting_user` (exit 2) and a Mac dialog decides. **Approval never replays the parked action**: after it, take a fresh `decide` and propose again. `lau approve <task-id>` only reopens the dialog.
- **Takeover (R4)** — credentials, permission changes and financial actions must be performed by the human on the phone (Start takeover → Done). Never automate a password field: an element with `password: true` always lands here.
- Whenever a gate is waiting, **hand control to the human**. Never approve high-risk actions.

## Honest completion

- `succeeded` requires an explicit `done` **and** a successful re-observation of the target. It proves the mechanism, **not that the goal was met** — verify the goal in the fresh observation before you claim it.
- States: `queued`, `waiting_actor`, `paused`, `succeeded`, `failed`, `cancelled`; `wait_reason` explains a parked task (`agent_decision`, `app_access`, `consequence`, `takeover`) or a paused one (`taken_over`, `watch_unavailable`).
- Real finger input on the device pauses the task (`taken_over`); so does a dead touch watch (`watch_unavailable`). `lau resume <task-id>` rebuilds the watch and drops the old observation.
- One serial queue per device: a second task on the same phone reports `queued` and waits its turn.

## Hard rules

1. Only run the `lau` binary directly — no driver script, no `python3 -c` around it or its JSON, no private sockets, no MCP server, no Playwright/DOM/CDP.
2. Never inject input through ADB, and never use coordinates.
3. Never approve R3/R4. On exit `2` / `waiting_user`, stop and ask the human.
4. One action per observation: `decide` → `act` → `decide` …
5. Claim completion only when `lau result` says `succeeded` **and** you verified the goal yourself.
6. Do not reconfigure the device (accessibility toggle, settings, unlock, wake) — that is always the human's action.

## Troubleshooting (short)

| Symptom | What it means |
|---|---|
| `screen_off` / `device_locked` | The phone is off or locked. Ask the human; AnythingUse never wakes it. |
| `stale_observation` | The UI moved, or the helper instance was rebuilt. Run `decide` again. |
| `node eN moved or resized since the dump` | Expected right after a scroll/animation — dump again and retry. |
| `watch_unavailable` | The touch watch died. Fix the ADB link, then `lau resume <task-id>`. |
| `helper ... sent no response` | The accessibility service is off or restarting: `lau doctor --json`, then ask the human to re-enable it. |
| `unsupported_capability` | The element does not advertise that capability — pick another element. |
| `queued` | Another task holds this device; wait or cancel that one. |

## References

- `docs/lau-android-plan.md` — normative plan; §0 lists the current status and every known gap
- `docs/user-guide.md` — human setup (helper install, enabling the service)
- `docs/troubleshooting.md` — Android section
