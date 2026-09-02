---
name: local-computer-use
description: Operate AnythingUse (`lcu`) computer use on the user's real macOS apps and installed Chrome. Use when the user asks to control the desktop, click UI, drive Finder/Chrome/apps, run lcu, or do local Computer Use. Pi, Claude Code, Grok Build, and other CLI agents share this skill. Only the lcu CLI; no Playwright, DOM, MCP, or private sockets.
---

# AnythingUse Skill (`lcu`)

Skill directory name `local-computer-use` is historical. Product name is **AnythingUse**; the only agent control surface is the **`lcu`** CLI.

## When to use

Use this skill when the user wants an agent to operate **real** desktop applications on this machine via AnythingUse / `lcu`.

## Binary

Invoke the real `lcu` binary only (never private sockets). Resolve it in this order:

1. `$LCU_BIN` if set and executable
2. `lcu` on `PATH` (product install: `lcu` and sibling `lcu-desktop` in `~/.local/bin`)
3. `./target/release/lcu` from an AnythingUse checkout working directory (**debug fallback only**)
4. `<this-skill>/../../target/release/lcu` when the skill is loaded from the repo (**debug fallback only**)

`lcu` starts `lcu-desktop` from the **same directory** as itself. PATH/`~/.local/bin` is the product entry; do not keep depending on the git checkout. Install siblings with `./scripts/install-cli.sh` (Pi also runs this from `./scripts/install-pi.sh`).

If none exist, ask the human to build (`cargo build -p lcu-cli -p lcu-desktop --release`) then `./scripts/install-cli.sh`. In examples below, `lcu` means that resolved binary.

## How you call `lcu` (do not wrap it)

You are the decision maker. Each step is **one direct `lcu` invocation** (`run` / `decide` / `act` / `status` / `result`). Do **not** write a Python, JavaScript, or shell **driver** that `subprocess`/`exec`s `lcu` to run the loop. That is a second client and is not the product surface.

`lcu decide --json` is already the compact observation, not a full AX/DOM tree:

- `observation_id`, `goal`, `step`, `target`, compact `elements[]` (`id` / `role` / `label` / `frame` / generic `capabilities`), `image_path`
- Runtime will not give you a fuller tree. Do not ask for one. Do not paste the `elements` array into the chat.

**Caller extracts.** After `decide`, pull only the fields you need (window title, matching labels, `element_id`s). If the harness must keep JSON out of the conversation, redirect `lcu decide --json` to a file, then extract from **that already-written file** with the harness file tool (`ctx_execute_file` when context-mode is on). Extraction is not a reason to wrap `lcu`, and it is **not** a reason to write Python.

When `image_path` is present, read that image with the harness Read/image tool **before** `lcu act`. The file is mode `0600` under the process temp dir and is removed when the decision is consumed or times out.

Run `lcu` in the login user's environment. Do **not** start `lcu run` / `lcu decide` / `lcu act` (or let them spawn `lcu-desktop`) inside a tool sandbox that rewrites `TMPDIR` (including context-mode `ctx_execute`). `lcu-desktop` inherits `TMPDIR`; a sandbox temp is deleted and `image_path` is gone. Context-mode, if active, is only for **extracting** a decide JSON file (`ctx_execute_file`) after `lcu` has already written it — never the Computer Use client.

Redirect is not a driver: `lcu decide … --json > /path/decide.json` then extract **that file**. Forbidden even as “just parsing”:

- `python3 -c '...'`
- a `.py` / `.js` / `.sh` helper
- `subprocess` / `exec` / `osascript` around `lcu`

If you need compact fields, use `ctx_execute_file` (or the harness Read) on the redirected JSON. `lcu` itself stays a single direct argv.

## Observe vs execute

Seeing an element in `decide` does **not** mean a background click/Press exists.

Compact `elements[]` omits raw platform `actions`, but includes generic
`capabilities` (`invoke` / `set_value` / `focus` / `scroll`). Use the advertised
semantic capability; Runtime rejects coordinate click/type substitutes with
`semantic_action_required`. Runtime `invoke` then live-checks the native AX tree:

- Finder sidebar rows (`AXStaticText` → `AXCell` → `AXRow` → `AXOutline`): **`AXSelect`** via the outline's writable `kAXSelectedRowsAttribute`. Success summary looks like `ax_select: select e30`.
- Real buttons: `AXPress`.
- Coordinate input is explicit last-resort, only when no matching semantic
  capability is advertised, and usually needs the exact window frontmost.

`invoke` and `set_value` never hide a coordinate/keyboard fallback. If the
semantic capability cannot execute, they fail explicitly. A separately proposed
targeted action may make `auto` return `foreground_required` and activate the
target; that is not a substitute for an advertised semantic capability.

To prove **no focus steal**, submit with `--control-mode background_only`. Pass: user's frontmost app unchanged, Finder title becomes 下载/Downloads, `last_action_summary` is `ax_select:…`, `lcu result` is `succeeded`, no `target activated; discarded…`.

## Hard rules (never break)

1. **Only** invoke the `lcu` binary, **directly**. Do not wrap it in a driver script. Do not write `python3 -c` (or any `.py`) to call `lcu` **or** to parse its JSON. Do not open Runtime sockets, invent HTTP APIs, or add MCP tools for Computer Use control.
2. Do **not** use Playwright, browser remote debugging, or page DOM access.
3. **Never** approve high-risk (R3/R4) actions. If `lcu` returns exit code `2` or JSON `status: waiting_user`, stop and ask the human.
4. Do **not** request screenshots, full semantic trees, or model chain-of-thought. `decide` already returns compact elements; extract, do not echo the array.
5. Submit **high-level natural-language goals**. In Agent decision mode, propose only actions bound to the fresh observation returned by `lcu decide`.
6. Claim completion only when `lcu status` / `lcu result` reports `succeeded`. Queued state, step count, or an action receipt is not success; Runtime success requires explicit `Done` and target re-observation.
7. Do not claim OS sandboxing of the model worker unless the project reports prove it.
8. Do not rewrite `TMPDIR` or run `lcu` inside a sandbox that does.
9. Never replace an advertised semantic capability with targeted input. On
   `semantic_action_required`, use the returned element and capability.

## Commands (sole external surface)

| Command | Purpose |
|---|---|
| `lcu doctor [--json]` | Permissions (`screen_recording`/`accessibility`/`input_monitoring`), runtime reachability, private entry, surface connectivity in `notes` (`mac_window` / `chrome_tab`) |
| `lcu run "<goal>" [--app <id>] [--wait] [--max-steps N] [--source human\|agent] [--source-name <name>] [--actor vlm\|agent] [--control-mode auto\|background_only] [--json]` | Submit a task (`--actor` picks the decision maker; `--control-mode` picks the task execution policy) |
| `lcu list [--json]` | List global queue tasks |
| `lcu status <task-id> [--json]` | Task state |
| `lcu watch <task-id> [--seconds N]` | Incremental JSONL status lines |
| `lcu result <task-id> [--json]` | Result summary (no screenshots) |
| `lcu pause <task-id> [--json]` | User pause |
| `lcu resume <task-id> [--json]` | Resume after pause |
| `lcu cancel <task-id> [--json]` | Cancel a task |
| `lcu approve <approval-id> [--json]` | **Only opens GUI**; never completes approval |
| `lcu schema [--json]` | Schema / protocol versions |
| `lcu decide <task-id> [--wait] [--json]` | **Agent decision mode**: compact observation (`elements` + `image_path` + goal/step). Caller extracts; not a full tree. Task must use `--actor agent` |
| `lcu act <task-id> --observation-id <obs> --action '<json>' [--effect '<json>'] [--json]` | Submit an observation-bound decision. Every executable action requires the shared closed-set `effect` claim |

Agent decision mode workflow (decision maker = the agent itself, data surface identical to the local VLM):

```bash
# 1. Submit a goal with this task's decision maker set to the agent
#    (no Runtime restart needed; --actor overrides the process default)
#    --source-name is display-only: pi | claude-code | grok | ...
lcu run "Open Downloads in Finder" --app com.apple.finder --actor agent --source agent --source-name pi --control-mode background_only --json

# 2. Direct lcu calls. Extract key fields from decide JSON; do not wrap lcu.
lcu decide <task-id> --wait --json   # compact elements + image_path (read image, then extract ids)
lcu act <task-id> --observation-id <obs> \
  --action '{"kind":"semantic","type":"invoke","element_id":"e1"}' \
  --effect '{"kind":"navigate","summary":"Open the selected item"}'
# ... repeat until done
lcu act <task-id> --observation-id <obs> --action '{"kind":"done","summary":"Goal verified complete"}'
```

Always pass `--app <bundle_id>` (or `--app pid:NNNN`) with `lcu run`: the
Runtime never infers the target app from the goal text and never falls back to
the frontmost or largest window. Without an explicit selector the task fails
with a usage error. On macOS, an explicit bundle ID launches the installed app
without taking frontmost when it has no existing window; a PID selector never
launches another process. The Runtime automatically presents the first-app
access dialog; the Agent waits for the human decision and then continues.

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

There is **one serial FIFO queue** for the macOS login user. `waiting_actor` and user-paused tasks release the execution slot and generic target reservation. Do not invent client secrets, `RegisterClient`, MCP, or Playwright.

macOS execution ladder: background semantic → provably isolated background targeted → disclosed exact-target foreground fallback in `auto` → explicit failure in `background_only`. App access discloses the fallback. Runtime discards the old proposal before activation, takes a fresh observation, and returns control to the same Actor; it never restores the previous app. Agent waits release generic target ownership.

If `last_action_summary` is `target activated; discarded pre-activation proposal and re-observing`, `auto` already brought the exact target front because **background delivery failed**. For Finder sidebar that usually means the agent/Runtime skipped `AXSelect` and tried a click. Do not treat that as success. Re-observe; if the control is a list row, `invoke` the labeled id (Runtime selects the parent outline). Use `--control-mode background_only` when the user must not lose their frontmost app — then click/activate is a hard fail, not a fallback.

Consequences (send/delete/pay/…) use separate one-time confirmation or takeover gates. Real user HID on the target pauses the task automatically. Chrome tasks use an inactive background task tab and do not reactivate user or task tabs on completion. Do **not** drive Chrome via page DOM, CDP, or Playwright from the agent — only `lcu run …`.

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

- Wrap `lcu` in a Python/JS/shell driver (`python3 -c`, `.py`, `subprocess`, `execFile`, a `run()` helper that owns the loop, or `osascript` sampling in the same script as `lcu act`). Parsing `decide.json` with Python is the same violation.
- Echo compact `elements` into the chat, or ask for a full semantic/AX/DOM tree.
- Assume compact JSON includes raw `actions`. It exposes only generic
  `capabilities`; Finder rows still `invoke` by `element_id`, and Runtime applies
  `AXSelect`.
- Run `lcu` under a rewritten `TMPDIR` / `ctx_execute` sandbox (screenshot path dies with the sandbox).
- Invent titlebar primers, `appKitDefined` activation, event taps, or a session wrapper that clicks the target to “warm” it.
- Treat `target activated; discarded…` on a Finder sidebar task as the intended path.
- Read `~/Library/Application Support/LocalComputerUse` or AnythingUse task DBs / screenshot dirs for “cheating” evidence.
- Bypass app access or one-time consequence confirmation/takeover.
- Open Runtime / chrome-control / macos-window sockets yourself.
- Start Windows-specific tooling.

## References

- `docs/status.md` — delivery boundary (v3.2)
- `docs/execution-contract.md` — normative execution contract
- `docs/command-contract.md` — current implemented CLI contract
- `docs/user-guide.md` — human setup (including Chrome extension install)
- `docs/privacy.md` — data retention
- `docs/troubleshooting.md` — common failures
- `docs/architecture.md` — components and lifecycle
