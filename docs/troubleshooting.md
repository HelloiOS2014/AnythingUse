# Troubleshooting

## Finder / target keeps stealing frontmost from the Agent TUI

- First check **execute path**, not window matching. Finder sidebar
  「下载」 is `AXStaticText` → `AXCell` → `AXRow` → `AXOutline`. It has no
  `AXPress`. The writable attribute is the outline's `kAXSelectedRowsAttribute`
  (`ax_select: select eN`). Seeing the label in `decide` is not enough.
- Compact `decide` JSON omits raw `actions` but exposes generic `capabilities`.
  `invoke` the labeled `element_id`; Runtime live-checks `AXSelect`. Targeted
  click/type on a semantic-capable element is rejected with
  `semantic_action_required`.
- If `last_action_summary` is `target activated; discarded pre-activation`,
  `auto` already activated because an explicit targeted action required the
  foreground. Semantic `invoke` / `set_value` never hide that fallback.
- To never activate: `--control-mode background_only` (hard fail instead of
  `foreground_activate`). Real user click/key/scroll on the target still pauses.

## Agent wrapped `lcu` in a script / missing `image_path` file

- Call `lcu` **directly** each step. A Python/JS `subprocess` driver is not the
  product surface. `python3 -c 'json.load(...)'` after redirect is still a
  Python script — do not do it. Extract the redirected file with
  `ctx_execute_file` or the harness Read tool.
- `decide` is compact elements, not a full tree. Do not echo `elements`.
- If `image_path` is missing after `decide`, the CLI was likely started with a
  rewritten `TMPDIR` (tool sandbox / `ctx_execute`). `lcu-desktop` inherits
  that dir; the 0600 PNG is deleted with the sandbox. Re-run `lcu` in the
  login user environment, then extract from a redirected JSON file if needed.

## `runtime unavailable` (exit 69)

- `lcu` starts its sibling `lcu-desktop` from the same directory. Product
  install is `~/.local/bin` via `./scripts/install-cli.sh` (both binaries).
  A PATH `lcu` without sibling `lcu-desktop` fails with exit 69. Checkout
  `./target/release/lcu` is debug-only. Rebuild or reinstall if the sibling
  is missing.
- Check socket under Runtime root; must not listen on TCP.

## Permissions denied

- System Settings → Privacy & Security → Screen Recording / Accessibility / Input Monitoring.
- Grant the **macos-window-service** binary and/or the host that spawns it (`lcu-desktop`, Terminal).

## `mac_window_service` disconnected (`lcu doctor`)

- Build: `cd native/macos-window-service && swift build -c release`
- Set `LCU_MACOS_WINDOW_SERVICE` to the binary path if not under `.build/release/`
- Socket: `~/Library/Application Support/AnythingUse/macos-window.sock`

## `chrome_control_host` disconnected

- Run `./native/chrome-control/scripts/install-native-host.sh`
- Load unpacked only from the installer's printed `extension:` path (default: `~/Library/Application Support/AnythingUse/chrome-extension`)
- Chrome must be running so Native Messaging can launch the host
- Socket: `~/Library/Application Support/AnythingUse/chrome-control.sock` (no TCP)

## Task stuck in `waiting_actor`

Older persisted rows may appear as `waiting_user` / `waiting_approval`.

- Run `lcu approve <id>` only opens UI; complete approval in the GUI.
- Agents cannot approve.
- After approval, fetch the fresh continuation with `lcu decide`; the old action
  is never replayed.

## Finder / Chrome not resolved

- Non-Chrome: ensure the app is running with a visible window; try `--app Finder`.
- Chrome: extension + native host must be connected; goals open a **background task tab**, not AX menus.

## `lcu decide` returns `elements: []`

- The target app exposed a screenshot but no usable macOS Accessibility tree.
  Finder on a secondary display can still yield a tree after CG hit-test; do not
  jump to titlebar primers or `foreground_activate` just because the first match failed.
- AnythingUse may attempt one background coordinate click only when AX hit-testing
  can prove an actionable element or the exact editable becomes focused. It never
  follows an unverified click with text or Return/Enter. Finder sidebar rows are
  `AXSelect`, not that click.
- If background delivery is unavailable, `auto` may activate only the exact
  permitted target, discard the old proposal, and re-observe. `background_only`
  fails instead. The agent never switches back.
- Consequences still use separate one-time confirmation or takeover gates. Do
  not add an application-specific workaround or weaken Runtime's evidence floor.

## A Chrome task's `decide` returns the same `observation_id` after an `act`

The worker has not produced the next observation yet, so the call raced it. Compare
`observation_id` across calls and issue `decide` again — never re-submit an action bound
to an observation you already consumed. A ChromeTab observation returns DOM-derived
elements with `el_N` ids (role / label / capabilities) plus a rendered task-tab
screenshot; a blank task tab legitimately reports zero elements before the first
navigation. Verified 2026-09-16: `com.google.Chrome` task → navigate to an https URL →
new observation with one `link` element → `done` → `succeeded`.

## VLM slow or OOM on 16GB

- Cold load ~2–3 minutes on MPS fp16 (warmup absorbs the first-inference compile; the first real propose is then fast).
- Per-propose budget is 180s (`LCU_VLM_MAX_TIME` / `LCU_VLM_PROPOSE_SECS`); the Rust hard timeout is 240s (`LCU_VLM_TIMEOUT_SECS`).
- A failed propose is retried once automatically (VLM failures are frequently transient); two consecutive failures fail the task.
- Generation stops as soon as the emitted JSON action is complete — a truncated action is refused and retried, never executed.
- Close heavy apps; do not run parallel reloads.

## Task shows `paused` after a crash / restart

Expected behavior: a task left non-terminal when `lcu-desktop` died is recovered to `paused` on restart (with its step budget rebuilt) so the user decides — `lcu resume` or `lcu cancel`. Recovered paused tasks do not occupy queue slots.

## Target becomes frontmost

- In `auto`, a permitted target may come to the front when background delivery
  is unavailable. App access discloses this fallback; the agent never switches
  back afterwards.
- macOS control requires Input Monitoring to distinguish real user HID from
  tagged AnythingUse input. Agent waits release generic ownership; continuation
  re-resolves and re-observes the target.
- Activation of any window other than the exact permitted target is a
  control-plane bug: cancel the task. Do not switch back as a workaround.
- Real user HID on the target pauses the task. `lcu pause` / `lcu cancel`
  remain explicit controls.

## Foreground-required input

- `exclusive` is not an Action. Choose `--control-mode auto|background_only`.
- `auto` tries background first and activates only after the backend returns
  `foreground_required`; `background_only` fails instead.
- App access already disclosed this capability fallback. After activation
  Runtime takes a fresh observation and asks the same Actor to propose again.
  It never restores the previous app or emits global HID.

## Android (`lau`)

`lau` is a **separate, source-level** CLI (never `lcu`); `docs/lau-android-plan.md` §0 lists every current gap and known limitation. Common failures:

- **`adb` not found** — set `LAU_ADB_BIN`, or install Android platform-tools.
- **`target_unresolved`** — more than one authorized device (pass `--serial` / `LAU_SERIAL`), or the given serial matches no connected device.
- **`device_unavailable`** — the device is `unauthorized` / `offline`: accept the USB-debugging prompt on the phone, or check the cable.
- **helper blockers** — `lau doctor --json` reports `installed` / `enabled` / `bound` / `ping`. Not installed → `./scripts/install-android-helper.sh`. Not enabled → Settings → Accessibility → “AnythingUse LAU”. HyperOS also needs autostart + unrestricted battery, otherwise the service is not revived after a kill.
- **HyperOS install failure** (`INSTALL_FAILED_USER_RESTRICTED`) — enable “Install via USB” (needs a Xiaomi account) and retry.
- **helper socket not responding** — toggle the accessibility service off/on. The CLI re-creates `adb forward` on every RPC, so a stale forward is not the usual cause.
- **task parked in `waiting_actor` with `wait_reason=consequence`** — a Mac dialog is open, or `lau approve <task-id>` reopens it. The CLI cannot approve.
- **task `paused` with `wait_reason=taken_over`** — real touch on the device paused it. `lau resume <task-id>` clears the pause: it rebuilds the device touch watch and drops the pre-pause observation, so the next `decide` re-observes instead of continuing an old frame. `lau cancel` remains the alternative.
- **`stale_observation: node e3 moved or resized since the dump`** — the node shifted more than the §5.2 bounds tolerance (0.5% of the screen) between the dump and the action; common while a list is still animating, or right after a scroll. Re-run `lau decide` / `lau dump` and submit again against the fresh token. This is the designed fail-closed behaviour, not a bug.
- **task parked with `wait_reason=takeover`** — a high-risk (R4) action was judged unsafe to automate. A two-step Mac dialog handles it: click **Start takeover**, do the action **yourself on the phone**, then click **Done**. AnythingUse never runs that action, and afterwards the task re-observes (the old proposal and observation id are dropped). `lau approve` cannot complete a takeover.
- **`watch_unavailable`** — the device's `getevent` touch watch is not live (spawn failed, or the stream ended). `lau run` refuses to create a task, and a running task is paused rather than acting blind. Fix the ADB/device link, then `lau resume <task-id>`; if the watch still cannot attach, resume fails closed and the task stays paused. `lau status <task-id> --json` reports the watch's `healthy` / `dead_reason`.
- **after a phone reboot: `helper socket not responding` while `enabled` is still true** — expected until the phone is **unlocked once**. The helper lives in credential-encrypted storage, so its process cannot start before the first unlock and the accessibility service cannot bind. Unlock the phone and re-check `lau doctor --json`; it recovers in a couple of seconds. (Verified 2026-09-15: HyperOS did **not** disable the service across a reboot.)
- **`set_value` reports ok but the text does not stick** — you probably targeted a duplicate node (dumps can contain two elements with the same frame, e.g. a wrapper and the real field). Pick the element whose `role` is `EditText` and whose frame matches the visible field, then re-dump to confirm the value; the helper verifies by re-reading the node, so a wrong-but-editable node can pass verification without reaching the UI.
- **task parked with `wait_reason=app_access`** — the first control of that package needs a decision in the **Mac** dialog (Deny / Always allow / Allow once). The CLI cannot answer it; nothing is persisted unless the human picks *Always allow*. `lau permissions --json` lists grants, `lau permissions --revoke <key>` removes one.
- **`{"status":"queued"}` / `task is queued behind another task on this device`** — one serial queue per device. Wait for the current task to end (it is promoted automatically), or `lau cancel` the other task.
- **`indeterminate: the action may or may not have been applied … do not resend it`** — the helper's response was lost or timed out, so the outcome is unknown. The observation was dropped and the task stays steerable: run `decide` for a fresh observation and decide from what you actually see. **Never resend the action.** `lau status --json` shows `indeterminate: true`.
- **`semantic_action_required`** — coordinate input is refused by design (`invoke`/`set_value`/`scroll`/`focus`/`global_back` only). Use the capability the element advertises.
- **`forbidden_peer`** — something other than root/shell tried to reach the helper socket on the device.
- **a Spinner/dropdown cannot be operated** — known limitation (§0 #27): such nodes may advertise `scroll` while `performAction` returns false, and they are not `invoke`-able; with coordinates refused there is no fallback yet. Work around it by reaching the same state through another control (or ask the human to change it).
