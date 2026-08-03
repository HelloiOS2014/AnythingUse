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
- Socket: `~/Library/Application Support/LocalComputerUse/macos-window.sock`

## `chrome_control_host` disconnected

- Run `./native/chrome-control/scripts/install-native-host.sh`
- Load unpacked extension from `native/chrome-control/extension`
- Chrome must be running so Native Messaging can launch the host
- Socket: `~/Library/Application Support/LocalComputerUse/chrome-control.sock` (no TCP)

## Task stuck in `waiting_approval`

- Run `lcu approve <id>` only opens UI; complete approval in the GUI.
- Agents cannot approve.

## Finder / Chrome not resolved

- Non-Chrome: ensure the app is running with a visible window; try `--app Finder`.
- Chrome: extension + native host must be connected; goals open a **background task tab**, not AX menus.

## VLM slow or OOM on 16GB

- Cold load ~2–3 minutes on MPS fp16.
- Close heavy apps; do not run parallel reloads.

## Target unexpectedly becomes frontmost

- Cancel the task and treat it as a control-plane bug; foreground activation is not an acceptable success path.
- If the user takes over the exact target window, the expected state is paused, not background input into that window.
- Do not work around the issue by activating the target and switching back.

## Exclusive input

- Exclusive global HID is not part of the window/Chrome surfaces. Prefer semantic / directed targeted input.
