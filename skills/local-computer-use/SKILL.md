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
5. Submit **high-level natural-language goals**. In Agent decision mode, propose only actions bound to the fresh observation returned by `lcu decide`.
6. Claim completion only when `lcu status` / `lcu result` reports `succeeded`. Queued state, step count, or an action receipt is not success; Runtime success requires explicit `Done` and target re-observation.
7. Do not claim OS sandboxing of the model worker unless the project reports prove it.

## Commands (sole external surface)

| Command | Purpose |
|---|---|
| `lcu doctor [--json]` | Permissions (`screen_recording`/`accessibility`/`input_monitoring`), runtime reachability, private entry, surface connectivity in `notes` (`mac_window` / `chrome_tab`) |
| `lcu run "<goal>" [--app <id>] [--wait] [--max-steps N] [--source human\|agent] [--source-name <name>] [--actor vlm\|agent] [--control-mode auto\|background_only\|foreground] [--json]` | Submit a task (`--actor` picks the decision maker; `--control-mode` picks the task execution policy) |
| `lcu list [--json]` | List global queue tasks |
| `lcu status <task-id> [--json]` | Task state |
| `lcu watch <task-id> [--seconds N]` | Incremental JSONL status lines |
| `lcu result <task-id> [--json]` | Result summary (no screenshots) |
| `lcu pause <task-id> [--json]` | User pause |
| `lcu resume <task-id> [--json]` | Resume after pause |
| `lcu cancel <task-id> [--json]` | Cancel a task |
| `lcu approve <approval-id> [--json]` | **Only opens GUI**; never completes approval |
| `lcu schema [--json]` | Schema / protocol versions |
| `lcu decide <task-id> [--wait] [--json]` | **Agent decision mode**: fetch the observation the worker is waiting on (compact elements + screenshot path + goal/step). The task must use `--actor agent` |
| `lcu act <task-id> --observation-id <obs> --action '<json>' [--effect '<json>'] [--json]` | Submit an observation-bound decision. Every executable action requires the shared closed-set `effect` claim |

Agent decision mode workflow (decision maker = the agent itself, data surface identical to the local VLM):

```bash
# 1. Submit a goal with this task's decision maker set to the agent
#    (no Runtime restart needed; --actor overrides the process default)
lcu run "Open Downloads in Finder" --app com.apple.finder --actor agent --json

# 2. Decision loop: fetch observation → decide → submit
lcu decide <task-id> --wait --json   # elements + image_path (read the image BEFORE submitting)
lcu act <task-id> --observation-id <obs> \
  --action '{"kind":"semantic","type":"invoke","element_id":"e1"}' \
  --effect '{"kind":"navigate","summary":"Open the selected item"}'
# ... repeat until done
lcu act <task-id> --observation-id <obs> --action '{"kind":"done","summary":"Goal verified complete"}'
```

Always pass `--app <bundle_id>` (or `--app pid:NNNN`) with `lcu run`: the
Runtime never infers the target app from the goal text and never falls back to
the frontmost or largest window. Without an explicit selector the task fails
with a usage error.

Decision timeout: `LCU_AGENT_DECISION_TIMEOUT_SECS` (default 600s). A stale
`observation_id` is rejected — fetch a fresh decision. Cancel/pause aborts a
parked decision immediately.

Stable exit codes: `0` ok, `2` waiting user, `3` task failed, `4` permission, `64` usage, `69` runtime unavailable, `70` internal.

## Recommended workflow

1. Run `lcu doctor --json` on first use or after permission changes.
2. Follow the Agent decision workflow above. Submit with `--actor agent`
   **without** `--wait`, because the Agent must remain free to call
   `lcu decide` / `lcu act`.
3. If the command returns `status: waiting_user` / exit `2`, stop for the human;
   the task state itself is `waiting_actor`.
4. Claim completion only after `lcu result <task-id> --json` reports
   `succeeded`; otherwise continue the loop or cancel.

There is **one serial FIFO queue** for the macOS login user. `waiting_actor` and user-paused tasks release the execution slot; a waiting Agent task keeps only its strict target reservation. Do not invent client secrets, `RegisterClient`, MCP, or Playwright.

macOS execution ladder: background semantic → provably isolated background targeted → **foreground session** (task-scoped GUI grant) → explicit failure. Background is preferred, never promised. When foreground is required, Runtime discards the old proposal, activates only the exact approved target, takes a fresh observation, and returns control to the same Actor; it never restores the previous app. Agent think time suspends native ownership; no-activation resume requires the same untouched foreground window. Consequences (send/delete/pay/…) use separate one-time confirmation or takeover gates. Real user HID on the target ends the session and pauses the task automatically; focus changes alone do not (macOS control requires Input Monitoring). Chrome tasks use an inactive background task tab and do not reactivate user or task tabs on completion. Do **not** drive Chrome via page DOM, CDP, or Playwright from the agent — only `lcu run …`.

## Chrome setup and navigation

Do not install or reload the Chrome extension yourself. If `lcu doctor` says
the Chrome surface is disconnected, ask the human to run
`./native/chrome-control/scripts/install-native-host.sh` and load unpacked
only from the installer's printed `extension:` path (default:
`~/Library/Application Support/AnythingUse/chrome-extension`). The repository
extension directory is not a second load target.

For a Chrome task, express the destination in the high-level goal. When a
current observation calls for navigation, submit only the shared action JSON
`{"kind":"semantic","type":"navigate","url":"https://example.com/"}` with
`--effect '{"kind":"navigate","summary":"Open example.com"}'` through `lcu act`.
It is Chrome-only and accepts only explicit `http://` or `https://`
URLs with a host; never call the native-host method directly.

## What you must not do

- Read `~/Library/Application Support/LocalComputerUse` task DBs or screenshot dirs for “cheating” evidence.
- Bypass the three GUI gates: app access, task-scoped foreground activation, and one-time consequence confirmation/takeover.
- Open Runtime / chrome-control / macos-window sockets yourself.
- Start Windows-specific tooling.

## References

- `docs/status.md` — delivery boundary (v3.2)
- `docs/command-contract.md` — full contract
- `docs/user-guide.md` — human setup (including Chrome extension install)
- `docs/privacy.md` — data retention
- `docs/troubleshooting.md` — common failures
- `docs/architecture.md` — components and lifecycle
