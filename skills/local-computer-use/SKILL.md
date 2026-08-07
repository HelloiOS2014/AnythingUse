---
name: local-computer-use
description: Operate AnythingUse (local Computer Use) on the user's real Mac desktop apps and installed Chrome only through the `lcu` CLI. No Playwright, no DOM control, no MCP control plane, no private socket access.
---

# AnythingUse Skill (`lcu`)

Skill directory name `local-computer-use` is historical. Product name is **AnythingUse**; the only agent control surface is the **`lcu`** CLI.

## When to use

Use this skill when the user wants an agent to operate **real** desktop applications on this machine via AnythingUse / `lcu`.

## Hard rules (never break)

1. **Only** invoke the `lcu` binary. Do not open Runtime sockets, invent HTTP APIs, or add MCP tools for Computer Use control.
2. Do **not** use Playwright, browser remote debugging, or page DOM access.
3. **Never** approve high-risk (R3/R4) actions. If `lcu` returns exit code `2` or JSON `status: waiting_user`, stop and ask the human.
4. Do **not** request screenshots, full semantic trees, or model chain-of-thought on stdout.
5. Submit **high-level natural-language goals** only. Do not invent low-level click/type primitives.
6. Claim completion only when `lcu status` / `lcu result` reports `succeeded`. Queued state, step count, or an action receipt is not success; Runtime success requires explicit `Done` and target re-observation.
7. Do not claim OS sandboxing of the model worker unless the project reports prove it.

## Commands (sole external surface)

| Command | Purpose |
|---|---|
| `lcu doctor [--json]` | Permissions (`screen_recording`/`accessibility`/`input_monitoring`), runtime reachability, private entry, surface connectivity in `notes` (`mac_window` / `chrome_tab`) |
| `lcu run "<goal>" [--app <id>] [--wait] [--max-steps N] [--source human\|agent] [--source-name <name>] [--actor vlm\|agent] [--json]` | Submit a task (`--actor` picks the decision maker per task; default follows the Runtime) |
| `lcu list [--json]` | List global queue tasks |
| `lcu status <task-id> [--json]` | Task state |
| `lcu watch <task-id> [--seconds N]` | Incremental JSONL status lines |
| `lcu result <task-id> [--json]` | Result summary (no screenshots) |
| `lcu pause <task-id> [--json]` | User pause |
| `lcu resume <task-id> [--json]` | Resume after pause |
| `lcu cancel <task-id> [--json]` | Cancel a task |
| `lcu approve <approval-id> [--json]` | **Only opens GUI**; never completes approval |
| `lcu schema [--json]` | Schema / protocol versions |
| `lcu decide <task-id> [--wait] [--json]` | **Agent decision mode**: fetch the observation the worker is waiting on (compact elements + screenshot path + goal/step). Requires `lcu-desktop` started with `LCU_VISION_ACTOR=agent` |
| `lcu act <task-id> --observation-id <obs> --action '<json>' [--json]` | Submit an agent decision (action JSON). Same safety pipeline as VLM proposals: EffectGuard + approvals still apply |

Agent decision mode workflow (decision maker = the agent itself, data surface identical to the local VLM):

```bash
# 1. Submit a goal with this task's decision maker set to the agent
#    (no Runtime restart needed; --actor overrides the process default)
lcu run "Open Downloads in Finder" --app com.apple.finder --actor agent --json

# 2. Decision loop: fetch observation → decide → submit
lcu decide <task-id> --wait --json   # elements + image_path (read the image BEFORE submitting)
lcu act <task-id> --observation-id <obs> --action '{"kind":"semantic","type":"invoke","element_id":"e1"}'
# ... repeat until done
lcu act <task-id> --observation-id <obs> --action '{"kind":"done","summary":"Goal verified complete"}'
```

Decision timeout: `LCU_AGENT_DECISION_TIMEOUT_SECS` (default 600s). A stale
`observation_id` is rejected — fetch a fresh decision. Cancel/pause aborts a
parked decision immediately.

Stable exit codes: `0` ok, `2` waiting user, `3` task failed, `4` permission, `64` usage, `69` runtime unavailable, `70` internal.

## Recommended workflow

```bash
# 1. Health (first use or after permission changes)
lcu doctor --json

# 2. Submit (desktop Runtime must be running: lcu-desktop)
lcu run "Open Downloads in Finder" --app com.apple.finder --wait --json

# Agent-originated tasks only tag display source (no secrets / registration):
lcu run "Open Downloads in Finder" --app com.apple.finder --source agent --source-name codex --wait --json

# 3. Track (if not using --wait)
lcu status <task-id> --json
lcu watch <task-id> --seconds 60

# 4. If waiting_user / exit 2 → human must approve in the desktop GUI
#    Agents must NOT call any path that completes approval.

# 5. Finish
lcu result <task-id> --json
# or cancel
lcu cancel <task-id> --json
```

There is **one serial FIFO queue** for the macOS login user. `waiting_user` and user-paused tasks release the execution slot. Do not invent client secrets, `RegisterClient`, MCP, or Playwright.

macOS tasks do not activate the target window; user takeover of the same window pauses the task. Chrome tasks use an inactive background task tab and do not reactivate user or task tabs on completion. Do **not** drive Chrome via page DOM, CDP, or Playwright from the agent — only `lcu run …`.

## What you must not do

- Read `~/Library/Application Support/LocalComputerUse` task DBs or screenshot dirs for “cheating” evidence.
- Bypass `lcu approve` GUI binding (task + observation + action hash + target + expiry + nonce).
- Open Runtime / chrome-control / macos-window sockets yourself.
- Start Windows-specific tooling.

## References

- `docs/status.md` — delivery boundary (v3.2)
- `docs/command-contract.md` — full contract
- `docs/user-guide.md` — human setup (including Chrome extension install)
- `docs/privacy.md` — data retention
- `docs/troubleshooting.md` — common failures
- `docs/architecture.md` — components and lifecycle
