# AnythingUse — User Guide (macOS)

## What this is

**AnythingUse** runs a single-instance Runtime on your Mac (CLI codename `lcu`; data root `~/Library/Application Support/AnythingUse`). Humans and Agents both use the same `lcu` CLI. High-risk actions require you to approve in the desktop UI.

This guide describes the implemented setup and interface, not a blanket
real-app stability certification. Runtime behavior is verified per target
scenario.

Execution surfaces:

- **macOS apps** — strict window-targeted control via `macos-window-service`. The Runtime does not activate the target window; user takeover of the same window pauses the task.
- **Chrome** — real Chrome Extension + Native Messaging + debugger/CDP on an inactive background task tab (not Playwright, not a second browser). User tabs are not reactivated when a task ends.

Tasks enter one serial FIFO queue. `waiting_user` and user-paused tasks release the execution slot. An action receipt is not completion: `succeeded` requires an explicit model `Done` followed by a successful re-observation of the target.

## Requirements

- macOS on Apple Silicon recommended
- Screen Recording + Accessibility for the **macos-window-service** / `lcu-desktop` host
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

# start desktop-owned Runtime (production path)
./target/release/lcu-desktop &

# health (surfaces + permissions)
./target/release/lcu doctor --json
```

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

1. Grant Screen Recording and Accessibility when prompted (for the window service binary / host app).
2. `lcu doctor --json` — check permissions (`screen_recording`/`accessibility`/`input_monitoring`) and surface connectivity in `notes` (`mac_window` / `chrome_tab`).
3. Choose one explicit decision path:
   - Local model installed: `lcu run "Open Downloads in Finder" --app com.apple.finder --actor vlm --wait --json`.
   - External Agent: ask the Agent to use the AnythingUse Skill; it submits with `--actor agent` and drives `lcu decide` / `lcu act`.
4. If approval is required, use the menu-bar / desktop confirmation UI (not the CLI).

Without `--wait`, `lcu run` returns after queuing the task. Use `lcu status`, `lcu watch`, or `lcu result` to follow it. `waiting_user` requires human action; resume a user-paused task only after the user is ready.

## Decision maker

The decision maker is pluggable; both receive the same data surface (compact elements + scaled screenshot) and their proposals flow through the same safety pipeline.

- **Agent-driven (product default when `LCU_VISION_ACTOR` is unset/`auto`)**: submit with `--actor agent`, then drive the loop with `lcu decide <task-id> --wait --json` (fetch observation) and `lcu act <task-id> --observation-id <obs> --action '<json>'` (submit a decision). Decision timeout: `LCU_AGENT_DECISION_TIMEOUT_SECS` (default 600s). See the Agent Skill for the full workflow.
- **Local VLM (optional)**: submit with `--actor vlm`; `lcu-desktop` runs the Qwen3-VL subprocess automatically. Requires the weights under `models/Qwen3-VL-4B-Instruct` (see above).

For Chrome, put the destination in the high-level goal. An external Agent may
submit `{"kind":"semantic","type":"navigate","url":"https://example.com/"}`
through `lcu act` from a current observation; navigation accepts only explicit
`http://` or `https://` URLs with a host and is not a macOS-window action.

Runtime data root override: `LCU_RUNTIME_ROOT` (alias `LCU_RUNTIME_DIR`).

## Privacy defaults

- Screenshots are not stored in SQLite and are not printed to stdout.
- Task metadata is local under the Runtime data root.
- See `docs/privacy.md`.

## Stopping

- `lcu cancel <task-id>` (releases target resources, including Chrome debugger/tab ownership)
- Quit `lcu-desktop` to stop the Runtime accept loop
