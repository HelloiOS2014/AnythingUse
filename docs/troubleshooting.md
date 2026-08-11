# Troubleshooting

## `runtime unavailable` (exit 69)

- Start `lcu-desktop`, or set `LCU_EMBEDDED_RUNTIME=1` for local smoke only (debug builds).
- Check socket under Runtime root; must not listen on TCP.

## Permissions denied

- System Settings → Privacy & Security → Screen Recording / Accessibility.
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

## Task stuck in `waiting_user`

Wire name is `waiting_user` (legacy alias `waiting_approval` may appear in older logs).

- Run `lcu approve <id>` only opens UI; complete approval in the GUI.
- Agents cannot approve.

## Finder / Chrome not resolved

- Non-Chrome: ensure the app is running with a visible window; try `--app Finder`.
- Chrome: extension + native host must be connected; goals open a **background task tab**, not AX menus.

## `lcu decide` returns `elements: []`

- The target app exposed a screenshot but no usable macOS Accessibility tree.
- AnythingUse keeps coordinate click and Return/Enter at R3; it does not silently weaken the gate to automate an unknown Send/Delete/Pay control.
- In the current `1.0.0` build, cancel the task if no safe semantic action is available. Enterprise WeChat is one known example. The planned `1.1.0` fix is the generic screenshot-bound macOS operator; do not add an application-specific workaround.

## VLM slow or OOM on 16GB

- Cold load ~2–3 minutes on MPS fp16 (warmup absorbs the first-inference compile; the first real propose is then fast).
- Per-propose budget is 180s (`LCU_VLM_MAX_TIME` / `LCU_VLM_PROPOSE_SECS`); the Rust hard timeout is 240s (`LCU_VLM_TIMEOUT_SECS`).
- A failed propose is retried once automatically (VLM failures are frequently transient); two consecutive failures fail the task.
- Generation stops as soon as the emitted JSON action is complete — a truncated action is refused and retried, never executed.
- Close heavy apps; do not run parallel reloads.

## Task shows `paused` after a crash / restart

Expected behavior: a task left non-terminal when `lcu-desktop` died is recovered to `paused` on restart (with its step budget rebuilt) so the user decides — `lcu resume` or `lcu cancel`. Recovered paused tasks do not occupy queue slots.

## Target unexpectedly becomes frontmost

- Cancel the task and treat it as a control-plane bug; foreground activation is not an acceptable success path.
- If the user takes over the exact target window, the expected state is paused, not background input into that window.
- Do not work around the issue by activating the target and switching back.

## Exclusive input

- Exclusive global HID is not part of the window/Chrome surfaces. Prefer semantic / directed targeted input.
