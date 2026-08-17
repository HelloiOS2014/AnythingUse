# Troubleshooting

## `runtime unavailable` (exit 69)

- `lcu` normally starts its sibling `lcu-desktop` automatically. Rebuild or
  reinstall if that binary is missing.
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
- AnythingUse may attempt one background coordinate click only when AX hit-testing
  can prove an actionable element or the exact editable becomes focused. It never
  follows an unverified click with text or Return/Enter.
- If background delivery is unavailable, `auto` may activate only the exact
  permitted target, discard the old proposal, and re-observe. `background_only`
  fails instead. The agent never switches back.
- Consequences still use separate one-time confirmation or takeover gates. Do
  not add an application-specific workaround or weaken Runtime's evidence floor.

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
