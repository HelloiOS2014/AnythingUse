# macos-window-service

AnythingUse **macOS window** control surface: **per-user private Unix socket + JSON lines**.

(Historical milestone labels: Wave D2 / P1 spike — not separate products.)

## Role

- Resolve/observe by `MacWindow(pid, window_id)`
- Window screenshot (ScreenCaptureKit / CG fallback) + AX element tree
- AX semantic actions + PID-directed input (`CGEvent.postToPid`)
- Real user click/key/scroll on the reserved window → `taken_over`; focus changes alone do not; process/window gone → `target_lost`
- Does **not** move the real mouse; does **not** activate the target app except
  inside a GUI-approved foreground session (`foreground_activate` is the only
  `NSRunningApplication.activate` entry)
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

## Foreground session (GUI-approved)

When background semantic/targeted delivery is unavailable, Runtime may place a
**single-slot foreground session** (one serial FIFO):

- `set_foreground_session` `{pid, window_id, active}` — Runtime records the
  GUI-approved session; `active=false` clears only the matching slot (pid 0
  clears crash leftovers).
- `foreground_activate` `{pid, window_id}` — the only `NSRunningApplication.activate`
  entry. Requires the matching approved session, raises or uniquely proves the
  exact window before activation, then re-proves it after activation and before input. Never restores the previous app.
- `suspend_foreground_session` / `resume_foreground_session` — release native
  ownership while the Actor thinks; resume never activates and succeeds only
  for the same untouched exact foreground window.

The listen-only HID monitor ignores tagged AnythingUse events. A real user
click, key, or scroll on the target is `taken_over`, including while suspended.

## Wire protocol

Newline-delimited JSON:

```json
{"id":"1","method":"observe","params":{"pid":123,"window_id":456}}
{"id":"1","ok":true,"result":{...}}
```

Methods: `ping`, `permissions`, `list`, `resolve`, `observe`, `semantic`, `targeted`, `set_takeover_watch`, `set_foreground_session`, `suspend_foreground_session`, `resume_foreground_session`, `foreground_activate`, `detect_conflict` (aliases: `detect_control_state`, `session_health`).

## Permissions

1. System Settings → Privacy & Security → **Accessibility** → `macos-window-service`
2. System Settings → Privacy & Security → **Screen & System Audio Recording** → same binary
3. **Input Monitoring** → same binary (real-user takeover detection)
