# `lcu` Command Contract

Schema version: `1.1.0`
Internal private IPC protocol version: `1`  
Status: product surface (macOS window + Chrome tab)  
Product: **AnythingUse** (binary remains `lcu`)

This is the implemented CLI/interface contract, not a blanket assertion of
stable runtime behavior across all target applications.

> **Scope.** This document covers `lcu` (macOS windows + Chrome). The Android
> endpoint is a **separate binary with its own surface**: see the
> [LAU Android plan](lau-android-plan.md) §2. It shares the `anything-core`
> action/effect types but not this CLI contract. In short: the Agent surface is
> `lau run` / `decide` / `act` / `result` / `cancel` / `resume` (gated), while
> `lau doctor` / `dump` / `invoke` / `set-value` / `scroll` / `screenshot` /
> `foreground` / `launch` / `permissions` form the **operator** surface and carry
> no task context, so no gate applies to them.

The target execution semantics are frozen in the
[Execution Contract](execution-contract.md). Until [Delivery status](status.md)
records migration completion, this file describes the current wire surface.

## Principles

1. Humans and Agents share the same `lcu` binary and flags.
2. Agents never open the private Runtime socket themselves; they only run `lcu` **directly** (no Python/JS/shell driver that subprocess-calls `lcu`).
3. `--json` / JSONL outputs never include screenshots, full semantic trees, or long model reasoning. `decide` returns **compact** `elements`; the caller extracts key fields and must not echo the array or ask for a fuller tree.
4. `lcu approve` only opens the desktop confirmation UI. It never completes approval in the CLI.
5. Exit codes are stable and machine-readable.
6. No second Agent protocol (no MCP Computer Use control plane).
7. No Playwright / DOM browser control channel.
8. No public TCP service.

## Exit codes

| Code | Meaning |
|---:|---|
| 0 | Success |
| 2 | Waiting for user (approval / takeover) |
| 3 | Task failed |
| 4 | Permission denied |
| 64 | Usage / invalid request |
| 69 | Runtime unavailable / protocol mismatch |
| 70 | Internal error |

## JSON envelope

```json
{
  "schema_version": "1.1.0",
  "status": "ok | waiting_user | failed | permission_denied | unavailable",
  "error": { "code": "snake_case", "message": "..." },
  "data": {}
}
```

`error` is omitted on success. `data` is omitted on pure errors.

## Commands

### `lcu doctor [--json]`

Platform, arch, private entry, permissions, surface connectivity, blockers, notes.

Private entry invariants:

- kind: `unix_socket` (macOS V1)
- `listens_tcp` must be `false`
- directory mode `0700`, socket mode `0600`

Surface permissions (online `doctor`, i.e. Runtime reachable):

- `permissions` lists the three TCC/OS flags: `screen_recording`, `accessibility`, `input_monitoring`
- surface connectivity appears in `notes` as `mac_window` (Swift service, `macos-window.sock`) and `chrome_tab` (Chrome host, `chrome-control.sock`)

When the Runtime is unreachable, offline `doctor` reports `mac_window_service` / `chrome_control_host` socket presence instead.

### `lcu run "<goal>" [--app <bundle_id>] [--wait] [--max-steps N] [--source human|agent] [--source-name <name>] [--actor vlm|agent] [--control-mode auto|background_only] [--json]`

Submit a high-level natural-language task.

`source` and `source-name` are display metadata, not authentication. `--actor vlm|agent` selects the decision maker for this task (default: the Runtime process setting, `LCU_VISION_ACTOR`; unset/`auto` selects Agent). `--control-mode` selects disclosed background-first automatic fallback or background-only. Tasks of either Actor kind share one serial FIFO queue; `waiting_actor` and user-paused tasks release the global execution slot and generic target reservation.

Task wire states include `queued`, `running`, `waiting_actor` (older persisted aliases: `waiting_user` / `waiting_approval`), `paused` (alias `paused_by_user`), `succeeded`, `failed`, `cancelled`. A gate decision discards the old proposal, re-observes the target, and returns the task to the same Actor through `waiting_actor` continuation.

Task success requires an explicit model `Done` and a successful re-observation of the target. A successful action, repeated action, step count, or queued state is not completion.

### `lcu list [--json]`

List tasks known to the current Runtime.

### `lcu status <task-id> [--json]`

Task state without screenshots.

### `lcu result <task-id> [--json]`

Result summary (`summary` / `error` / terminal state). No screenshots.

### `lcu watch <task-id> [--seconds N] [--interval-ms M]`

Prints **incremental** JSON lines when state or step_count changes. Does not dump trees or images.

### `lcu pause <task-id> [--json]` / `lcu resume <task-id> [--json]`

User pause / resume.

### `lcu cancel <task-id> [--json]`

Cancel and release target resources.

### `lcu approve <approval-id> [--json]`

Opens the local GUI for one of three distinct gates:

- app access: stable signed application identity;
- consequence confirmation/takeover: task + stable app identity + effect kind + Runtime-derived consequence identity + expiry + one-time nonce.

Screenshot-only consequence requests additionally retain the original
`observation_id + image_hash + action_hash` as audit/exact-frame evidence, but
the old action is never stored for replay.

CLI, Agent, and model credentials must not produce an approval result. Exit code is typically `2` (waiting user).

### `lcu schema [--json]`

Schema and internal protocol versions.

### `lcu decide <task-id> [--wait] [--json]` / `lcu act <task-id> --observation-id <obs> --action <json> [--effect <json>] [--json]`

Agent decision mode for a task submitted with `--actor agent`. No Runtime
restart is needed; the task-level actor overrides the process default.

`decide` returns the strict target, capture transform/hash, **compact**
elements (id/role/label/frame plus platform-neutral `capabilities`; not a full
AX/DOM tree and not raw platform `actions`), goal/step, and an `image_path` to
a 0600 temp screenshot. The
**caller extracts** matching ids from that payload; do not paste `elements`
into the chat or wrap `lcu` in a driver because the JSON is large. Redirect
to a file and extract with `ctx_execute_file` / Read — never `python3 -c`
or a helper script, even if that helper does not spawn `lcu`. Finder outline rows are
executed as `AXSelect` (`kAXSelectedRowsAttribute`) inside Runtime `invoke`,
not as a coordinate click. Read `image_path` before submitting — the file is
removed once the decision is consumed or times out. Do not run `lcu` under a
rewritten `TMPDIR` (sandbox `ctx_execute` included): `lcu-desktop` inherits
it and the screenshot path dies with the sandbox. `--wait` polls until a
decision is available. `act` submits an action JSON for that observation; the
action then flows through the exact same pipeline as VLM proposals
(validation, EffectGuard, approvals, control gates). A stale
`observation_id` is rejected with exit 3; use `decide` again.
Targeted click/type on an element that advertises the equivalent semantic
capability is rejected with `semantic_action_required`; use that element's
`invoke` or `set_value` capability instead.
Decision timeout: `LCU_AGENT_DECISION_TIMEOUT_SECS` (default 600s).
Cancel/pause aborts a parked decision immediately (exit 2 semantics preserved).

Every executable semantic/targeted action requires the same closed-set
`effect` object used by the local VLM. Runtime independently raises the risk
floor from the fresh observation and action; the Actor claim is never an
authorization and cannot lower that floor. Screenshot coordinates remain
bound to the returned observation. A one-time confirmation is consumed only
by an equivalent Runtime consequence identity, or for screenshot-only input by
an exact fresh `image_hash + action_hash` match.

For a Chrome task, an Agent may submit browser navigation through the shared
action contract: `{"kind":"semantic","type":"navigate","url":"https://example.com/"}`.
It accepts only explicit `http://` or `https://` URLs with a host, is ChromeTab-only,
and still passes the normal validation and risk gates. Do not call the native
host method directly.

## Agent rules

1. Run `lcu doctor --json` when environment health is unknown.
2. Submit with `lcu run ... --actor agent --source agent --source-name <name> --json`.
3. On `waiting_user` / exit `2`, hand control to the human immediately.
4. Use only fresh observation-bound actions; never approve high-risk actions.
5. Judge success only from `lcu` result/status fields.

## Foreground fallback

Background execution is preferred, not promised. App access discloses that
`auto` may activate the exact target when the backend reports
`foreground_required`; `background_only` fails instead. Activation verifies
the exact PID and window, discards the old proposal, and never restores the
previous app. Continuation starts from a fresh observation. Consequences
(send/delete/pay/…) still use separate one-time confirmation or takeover gates.
Real user HID on the target pauses the task; `pause` / `cancel` remain explicit.

## Source-level non-interference contract

- macOS tasks prefer background semantic/targeted input and never guess another window when identity fails. Only the exact app-access-permitted target may be activated by `auto`. Real user HID on the controlled target is takeover; focus/frontmost changes alone are not. A lost target fails the task.
- Chrome tasks use an inactive task tab. User takeover detaches the debugger and the Runtime does not reactivate a previous or task tab.
