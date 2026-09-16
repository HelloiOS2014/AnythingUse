# AnythingUse — User Guide (macOS)

## What this is

**AnythingUse** runs a single-instance Runtime on your Mac (CLI codename `lcu`; data root `~/Library/Application Support/AnythingUse`). Humans and Agents both use the same `lcu` CLI. First-app access is shown automatically in the desktop UI; high-risk actions require you to approve there too.

This guide describes the implemented setup and interface, not a blanket
real-app stability certification. Runtime behavior is verified per target
scenario.

Execution surfaces:

- **macOS apps** — strict window-targeted control via `macos-window-service`. An explicit bundle ID is launched without taking frontmost when it has no existing window. Choose `--control-mode auto|background_only`; `auto` prefers background and may activate the exact permitted target when required, while `background_only` fails instead. The agent never switches back. If the Agent TUI (Pi/iTerm) is frontmost while thinking, the next `act` may activate Finder/the target again — that is the disclosed fallback, not a restore of the TUI. Agent waits release generic ownership and resume from a fresh target observation; real user input on that target pauses automatically. Consequences use separate one-time confirmation or takeover gates.
- **Chrome** — real Chrome Extension + Native Messaging + debugger/CDP on an inactive background task tab (not Playwright, not a second browser). User tabs are not reactivated when a task ends.

Tasks enter one serial FIFO queue. `waiting_actor` and paused tasks release the global execution slot and generic target reservation. An action receipt is not completion: `succeeded` requires explicit `Done` followed by successful target re-observation.

## Requirements

- macOS on Apple Silicon recommended
- Screen Recording + Accessibility + Input Monitoring for the **macos-window-service** / `lcu-desktop` host (Input Monitoring distinguishes real user HID from tagged AnythingUse input)
- Optional Chrome surface: install native host + load unpacked extension (see below)
- Optional: local Qwen3-VL weights under `models/Qwen3-VL-4B-Instruct` for VLM mode

### Local model (optional — needed only for VLM decision mode)

Two steps: Python env, then weights (~8 GB).

```bash
# 1. Python env (Python 3.11+)
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt

# 2. Weights (uses `hf` CLI or huggingface_hub; writes models/Qwen3-VL-4B-Instruct)
./scripts/download_qwen3_vl.sh
```

`lcu-desktop` auto-detects the model (`models/Qwen3-VL-4B-Instruct` relative to
the repo, or `LCU_MODEL_DIR`). Without weights, `lcu` and `lcu-desktop` still
work; only VLM-decided tasks fail — use `--actor agent` instead.

VLM knobs (env vars, defaults shown):

| Var | Default | Meaning |
|---|---|---|
| `LCU_MODEL_DIR` | `<repo>/models/Qwen3-VL-4B-Instruct` | Model weights directory (also honored by the download script) |
| `LCU_MODEL_REPO` | `Qwen/Qwen3-VL-4B-Instruct` | HuggingFace repo for the download script |
| `LCU_VLM_MAX_TIME` / `LCU_VLM_PROPOSE_SECS` | `180` | Per-propose wall budget (seconds) |
| `LCU_VLM_TIMEOUT_SECS` | `240` | Rust-side hard timeout for one propose |
| `LCU_VLM_MAX_IMAGE` | `384` | Screenshot downscale cap (px) |
| `LCU_VLM_MAX_NEW` | `192` | Max generated tokens per propose |

## Install (dev)

```bash
# Rust product binaries
cargo build -p lcu-cli -p lcu-desktop --release

# Native macOS window service (auto-spawned by Runtime when found)
cd native/macos-window-service && swift build -c release && cd ../..

# product PATH entry: sibling `lcu` + `lcu-desktop` in ~/.local/bin
./scripts/install-cli.sh
lcu doctor --json
```

The on-demand host exits after 60 idle seconds when no task or approval is
active. Run `lcu-desktop` explicitly only for a persistent menu-bar host.
Checkout `./target/release/lcu` is a debug fallback only.

### Chrome surface (optional)

```bash
./native/chrome-control/scripts/install-native-host.sh
# Chrome → chrome://extensions → Developer mode → Load unpacked
#   → the installer's printed extension: path
#      (default: ~/Library/Application Support/AnythingUse/chrome-extension)
```

The installer copies the extension to its printed `extension:` path (the
default above). Do not load the repository source directory into Chrome.

## First run

1. Grant Screen Recording, Accessibility, and Input Monitoring when prompted (for the window service binary / host app), then restart AnythingUse after changing TCC permissions.
2. `lcu doctor --json` — check permissions (`screen_recording`/`accessibility`/`input_monitoring`) and surface connectivity in `notes` (`mac_window` / `chrome_tab`).
3. Choose one explicit decision path:
   - Local model installed: `lcu run "Open Downloads in Finder" --app com.apple.finder --actor vlm --wait --json`.
   - External Agent: ask the Agent to use the AnythingUse Skill; it submits with `--actor agent` and drives `lcu decide` / `lcu act`. Pi: `pi install git:github.com/HelloiOS2014/AnythingUse` (or `./scripts/install-pi.sh`). Do not put this checkout in global user packages. Claude Code / Grok / DSH: see the README plugin section (DSH installs with `./scripts/install-dsh.sh [profile]`).
4. If a gate is required, use the menu-bar / desktop UI (not the CLI). App access discloses that `auto` may bring the exact target forward; consequence confirmation/takeover remains separate. Activation or approval discards the old proposal and the same Actor receives a fresh observation.

Persistent app access can be removed from the menu-bar item **Revoke app access…**.

Without `--wait`, `lcu run` returns after queuing the task. Use `lcu status`, `lcu watch`, or `lcu result` to follow it. `waiting_actor` may be waiting for a human gate or Agent continuation; resume a paused task only after the user is ready.

## Decision maker

The decision maker is pluggable; both receive the same data surface (compact elements + scaled screenshot) and their proposals flow through the same safety pipeline.

- **Agent-driven (product default when `LCU_VISION_ACTOR` is unset/`auto`)**: submit with `--actor agent`, then invoke `lcu decide` / `lcu act` **directly** (no driver script). `decide` returns compact elements with generic `capabilities` plus `image_path` (no raw platform `actions`); the caller extracts matching `element_id`s and reads the screenshot. Runtime rejects coordinate input when the target element advertises the equivalent semantic capability. Finder sidebar rows are `AXSelect` on the outline, not `AXPress`. To forbid bringing the target front: `--control-mode background_only`. Executable actions require the shared closed-set effect claim. Do not run `lcu` under a rewritten `TMPDIR`. Decision timeout: `LCU_AGENT_DECISION_TIMEOUT_SECS` (default 600s). See the Agent Skill for the full workflow.
- **Local VLM (optional)**: submit with `--actor vlm`; `lcu-desktop` runs the Qwen3-VL subprocess automatically. Requires the weights under `models/Qwen3-VL-4B-Instruct` (see above).

For Chrome, put the destination in the high-level goal. An external Agent may
submit `{"kind":"semantic","type":"navigate","url":"https://example.com/"}`
with `--effect '{"kind":"navigate","summary":"Open example.com"}'` through
`lcu act` from a current observation; navigation accepts only explicit
`http://` or `https://` URLs with a host and is not a macOS-window action.

Runtime data root override: `LCU_RUNTIME_ROOT` (alias `LCU_RUNTIME_DIR`).

## Android (`lau`) — source-level endpoint

Android is a **separate** CLI (`lau`, never `lcu`) with its own skill
(`skills/local-android-use/`). `./scripts/install-cli.sh` installs it alongside
`lcu` when it has been built, and `package-release.sh` ships it with the helper
APK. It is **source-level**: the acceptance evidence is one device
(2026-09-15), so read the [LAU plan](lau-android-plan.md) §0 before relying on it.

### Setup

```bash
# helper APK: build + install
./scripts/install-android-helper.sh
# then on the phone: Settings → Accessibility → "AnythingUse LAU"  (must be ON)
lau doctor --json          # installed / enabled / bound / ping — `enabled` is the gate
```

HyperOS also needs autostart + unrestricted battery, otherwise the service is not
revived after being killed. **After a reboot, unlock the phone once**: the helper
lives in credential-encrypted storage, so until the first unlock its process
cannot start and `doctor` reports `helper socket not responding`.

### Observe and act

```bash
# operator surface (no task, no gates): read-only dump/screenshot + raw actions
lau dump --json                 # compact elements + observation token
lau screenshot --json           # 0600 temp PNG; refuses on a locked/off screen
lau invoke e7 --observation-id <token> --json
lau set-value --observation-id <token> e4 "你好" --json
lau scroll e1 --observation-id <token> --dy 1 --json

# agent surface (a task, gated): the daemon starts on demand and exits after 60s idle
lau run "在设置中打开蓝牙页面" --app com.android.settings --actor agent --json
lau decide <task-id> --json                                   # fresh observation + token
lau act <task-id> --observation-id <obs> \
  --action '{"kind":"semantic","type":"invoke","element_id":"e10"}' \
  --effect '{"kind":"navigate","summary":"open Bluetooth settings"}'
lau result <task-id> --json                                   # succeeded / failed / cancelled
```

`decide`/`act` are the Agent path and carry the gates; the stateless commands
above are the operator path and are deliberately ungated — never drive the device
with them from an agent.

### The three gates

| Gate | When | What happens |
|---|---|---|
| **app access** | first control of a package | a **Mac** dialog: Deny / Always allow / Allow once. `lau permissions --json` lists persisted grants; `lau permissions --revoke <key>` removes one. |
| **consequence (R3)** | sending, deleting, submitting, paying | task parks as `waiting_user` (exit 2) and a Mac dialog decides. **An approval never replays the parked action** — re-observe and propose again. |
| **takeover (R4)** | credentials, permission changes, financial actions | a two-step Mac dialog: **Start takeover** → do it yourself on the phone → **Done**. AnythingUse never performs that action. |

Approvals are never stored for replay: after any gate the task re-observes.

### Pauses and interruptions

The device touch watch (`getevent`) is mandatory. Real finger input pauses the
task (`taken_over`); a dead watch pauses it too (`watch_unavailable`). `lau
resume <task-id>` rebuilds the watch and drops the stale observation, so the next
`decide` re-observes. `lau decide --wait` polls through a pause (timeout
`LAU_DECIDE_WAIT_SECS`, default 600s) so an agent can wait for the human instead
of failing. One serial queue per device: a second task on the same phone reports
`queued` and starts when the first ends.

Coordinates are refused outright (`semantic_action_required`) — use the
capability an element advertises. ADB is transport and observation only, never
input injection.

## Privacy defaults

- Screenshots are not stored in SQLite and are not printed to stdout.
- Task metadata is local under the Runtime data root.
- The same rules, plus the Android specifics (credential fields are flagged and
  never read, private 0600 screenshots, a payload-free daemon log), are in
  `docs/privacy.md`.

## Stopping

- `lcu cancel <task-id>` (releases target resources, including Chrome debugger/tab ownership)
- Quit `lcu-desktop` to stop the Runtime accept loop
