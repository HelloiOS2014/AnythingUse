# macos-window-service

AnythingUse **macOS window** control surface: **per-user private Unix socket + JSON lines**.

(Historical milestone labels: Wave D2 / P1 spike — not separate products.)

## Role

- Resolve/observe by `MacWindow(pid, window_id)`
- Window screenshot (ScreenCaptureKit / CG fallback) + AX element tree
- AX semantic actions + PID-directed input (`CGEvent.postToPid`)
- Real user click/key/scroll on the reserved window → `taken_over`; focus changes alone do not; process/window gone → `target_lost`
- Does **not** move the real mouse; `foreground_activate` is the only AppKit
  activation entry and accepts only an exact target
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
~/Library/Application Support/AnythingUse/macos-window.sock
```

Override: `--socket PATH` or `LCU_MACOS_WINDOW_SOCK`.

Socket mode `0600`, parent directory `0700`. No TCP.

## Foreground fallback

When background delivery is unavailable, Runtime may call
`foreground_activate` for the exact app-access-permitted target in `auto` mode.
The service raises or uniquely proves that window, activates the app, and
re-proves the exact window. Runtime discards the rejected action and observes
again before any later input. The previous app is never restored.

The listen-only HID monitor ignores tagged AnythingUse events. A real user
click, key, or scroll on the target is `taken_over`, including while suspended.

Developer probe: start a fresh Runtime with
`LCU_MACOS_EXPERIMENTAL_WINDOW_ROUTING=1` to try window-addressed background
left clicks. It is off by default and does not enable background typing,
scrolling, activation primers, or focus-event suppression.

## Wire protocol

Newline-delimited JSON:

```json
{"id":"1","method":"observe","params":{"pid":123,"window_id":456}}
{"id":"1","ok":true,"result":{...}}
```

Methods: `ping`, `permissions`, `list`, `resolve`, `launch`, `observe`, `semantic`, `targeted`, `set_takeover_watch`, `foreground_activate`, `detect_conflict` (aliases: `detect_control_state`, `session_health`).

## Permissions

1. System Settings → Privacy & Security → **Accessibility** → `macos-window-service`
2. System Settings → Privacy & Security → **Screen & System Audio Recording** → same binary
3. **Input Monitoring** → same binary (real-user takeover detection)
