# Privacy

Product: **AnythingUse**. Runtime data root on macOS defaults to `~/Library/Application Support/AnythingUse` (override with `LCU_RUNTIME_ROOT`). The Android endpoint (`lau`) keeps its own root at `~/.local/share/AnythingUse/lau/`.

## Data that stays local

| Kind | Location | Notes |
|---|---|---|
| Task metadata | Runtime SQLite | No screenshot blobs |
| Events | SQLite `events` table | Compact messages only |
| Decision screenshots (if any) | private 0600 files in the OS temp directory | one pending observation only; removed on consume/replace/timeout/pause/terminal, startup prunes owned leftovers older than 1 hour |
| Model weights | `models/` (user provided) | Offline inference preferred |

## Android endpoint (`lau`) — data that stays local

Root: `~/.local/share/AnythingUse/lau/`. The device is reached over the USB ADB
link only (`adb forward` to an abstract socket inside the helper app); **no
network port is opened, nothing is uploaded, and there is no account or
telemetry**.

| Kind | Location | Notes |
|---|---|---|
| Daemon log | `daemon.log`, mode 0644 | One line per response: `op=<name> bytes=<n>` — **no payloads, no element text**. Rotated once past 64 KiB. |
| Persisted app access | `app_permissions.json`, mode 0600 | Written **only** when the human picks "Always allow" in the Mac dialog: package name + signing-certificate SHA-256 + timestamp. Revoke with `lau permissions --revoke <key>`. |
| Decision screenshots | private **0600** PNG in the OS temp directory | One pending observation per task; deleted when the decision is consumed/replaced/timed out or the task ends, and leftovers older than 1 hour are swept at daemon start. |
| Observations | daemon memory only | Not written to disk; dropped at the next decision. |

### Credentials are flagged, never read

The helper marks credential fields (`password: true`) so the Android evidence
layer routes them to the **R4 human-takeover gate** — AnythingUse never types a
credential. The same rule governs reading: a password field's text is **never**
put into an observation's `value`, **never** contributes to its own or an
ancestor's label, and is never echoed back by `set_value` (`<redacted>`). Only
the *hint* (`contentDescription`, e.g. "密码") may appear. That rule lives in
`PureRules` and is covered by unit tests.

### What the helper can see and do

It is an AccessibilityService: it reads the active window's node tree (package,
window id/title, screen state, and per element role/label/frame/capabilities)
and performs semantic actions (`invoke`, `set_value`, `scroll`, `focus`,
system back). It **cannot** inject input through ADB, use coordinates (refused
with `semantic_action_required`), wake or unlock the device, or draw anything on
the phone. Its socket accepts only root/shell peers.

## What Agents see

Only `lcu` JSON fields: task id, goal, state, summary/error, step count, app selector, and doctor flags.  
Agents do **not** receive screenshots outside the explicit Agent decision mode, full AX trees, model chain-of-thought, or approval internals.

**Agent decision mode exception** (`--actor agent`): when the external Agent
is the decision maker, `lcu decide` deliberately hands it the same data
surface the local VLM gets — a compact element tree, the goal/step context,
and a scaled screenshot via a 0600 temp file (deleted when the decision is
consumed or times out; never embedded in IPC JSON). This is the explicit
price of agent-driven decisions and only applies while agent mode is
enabled.

## What we do not do by default

- Upload telemetry to the network
- Complete high-risk approvals without GUI user presence
- Read credentials (see the Android rule above), inject input through ADB, act on
  coordinates, or wake/unlock a device

## Model helper isolation

The Python VLM worker is a subprocess. An isolation probe may show that the OS does **not** block network or home-directory writes. Do not claim full sandboxing unless seatbelt/sandbox evidence is recorded.
