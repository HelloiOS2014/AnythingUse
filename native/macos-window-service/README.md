# macos-window-service (D2)

Productized P1 macOS window control: **per-user private Unix socket + JSON lines**.

## Role

- Resolve/observe by `MacWindow(pid, window_id)`
- Window screenshot (ScreenCaptureKit / CG fallback) + AX element tree
- AX semantic actions + PID-directed input (`CGEvent.postToPid`)
- Same-window user takeover → `taken_over`; process/window gone → `target_lost`
- Does **not** move the real mouse, does **not** activate the target app
- No TextEdit AppleScript special cases; no frontmost-app conflict model

## Build / run

```bash
cd native/macos-window-service
swift build -c release
./.build/release/macos-window-service serve
# or
./scripts/run_service.sh
```

Default socket:

```text
~/Library/Application Support/LocalComputerUse/macos-window.sock
```

Override: `--socket PATH` or `LCU_MACOS_WINDOW_SOCK`.

Socket mode `0600`, parent directory `0700`. No TCP.

## Wire protocol

Newline-delimited JSON:

```json
{"id":"1","method":"observe","params":{"pid":123,"window_id":456}}
{"id":"1","ok":true,"result":{...}}
```

Methods: `ping`, `permissions`, `list`, `resolve`, `observe`, `semantic`, `targeted`, `detect_conflict` (aliases: `detect_control_state`, `session_health`).

## Permissions

1. System Settings → Privacy & Security → **Accessibility** → `macos-window-service`
2. System Settings → Privacy & Security → **Screen & System Audio Recording** → same binary
