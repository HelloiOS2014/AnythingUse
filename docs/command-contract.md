# `lcu` Command Contract

Schema version: `1.0.0`  
Internal private IPC protocol version: `1`  
Status: product surface (macOS window + Chrome tab)  
Product: **AnythingUse** (binary remains `lcu`)

## Principles

1. Humans and Agents share the same `lcu` binary and flags.
2. Agents never open the private Runtime socket themselves; they only run `lcu`.
3. `--json` / JSONL outputs never include screenshots, full semantic trees, or long model reasoning.
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
  "schema_version": "1.0.0",
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

### `lcu run "<goal>" [--app <bundle_id>] [--wait] [--max-steps N] [--source human|agent] [--source-name <name>] [--json]`

Submit a high-level natural-language task.

`source` and `source-name` are display metadata, not authentication. Tasks enter one serial FIFO queue; `waiting_user` and user-paused tasks release the execution slot.

Task wire states include `queued`, `running`, `waiting_user` (legacy alias `waiting_approval`), `paused` (alias `paused_by_user`), `succeeded`, `failed`, `cancelled`. After GUI approval or user resume, the task returns to `running` (not a new `queued` enqueue).

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

Opens local GUI confirmation bound to:

```text
task_id + observation_id + action_hash + target_app + expires_at + one_time_nonce
```

CLI, Agent, and model credentials must not produce an approval result. Exit code is typically `2` (waiting user).

### `lcu schema [--json]`

Schema and internal protocol versions.

### `lcu decide <task-id> [--wait] [--json]` / `lcu act <task-id> --observation-id <obs> --action <json> [--json]`

Agent decision mode (Runtime started with `LCU_VISION_ACTOR=agent`).

`decide` returns the observation the worker is waiting on: compact elements,
goal/step, and a `image_path` to a 0600 temp screenshot (read it before
submitting — the file is removed once the decision is consumed or times
out). `--wait` polls until a decision is available. `act` submits an action
JSON for that observation; the action then flows through the exact same
pipeline as VLM proposals (validation, EffectGuard, approvals, control
gates). A stale `observation_id` is rejected with exit 3; use `decide` again.
Decision timeout: `LCU_AGENT_DECISION_TIMEOUT_SECS` (default 600s).
Cancel/pause aborts a parked decision immediately (exit 2 semantics preserved).

## Agent rules

1. Run `lcu doctor --json` when environment health is unknown.
2. Submit with `lcu run ... --source agent --source-name <name> --json`.
3. On `waiting_user` / exit `2`, hand control to the human immediately.
4. Never invent mouse/keyboard primitives; never approve high-risk actions.
5. Judge success only from `lcu` result/status fields.

## Non-interference boundary

- macOS tasks do not activate their target window. User focus on the same PID and window pauses the task; a lost target fails it.
- Chrome tasks use an inactive task tab. User takeover detaches the debugger and the Runtime does not reactivate a previous or task tab.
