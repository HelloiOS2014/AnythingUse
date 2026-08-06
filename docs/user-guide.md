# AnythingUse — User Guide (macOS)

## What this is

**AnythingUse** (CLI / data root still branded **LCU** / `LocalComputerUse`) runs a single-instance Runtime on your Mac. Humans and Agents both use the same `lcu` CLI. High-risk actions require you to approve in the desktop UI.

Execution surfaces:

- **macOS apps** — strict window-targeted control via `macos-window-service`. The Runtime does not activate the target window; user takeover of the same window pauses the task.
- **Chrome** — real Chrome Extension + Native Messaging + debugger/CDP on an inactive background task tab (not Playwright, not a second browser). User tabs are not reactivated when a task ends.

Tasks enter one serial FIFO queue. `waiting_user` and user-paused tasks release the execution slot. An action receipt is not completion: `succeeded` requires an explicit model `Done` followed by a successful re-observation of the target.

## Requirements

- macOS on Apple Silicon recommended
- Screen Recording + Accessibility for the **macos-window-service** / `lcu-desktop` host
- Optional Chrome surface: install native host + load unpacked extension (see below)
- Optional: local Qwen3-VL weights under `models/Qwen3-VL-4B-Instruct` for VLM mode

### Local model weights (optional)

```bash
# requires `hf` CLI or python package huggingface_hub
./scripts/download_qwen3_vl.sh
# override: LCU_MODEL_DIR=... LCU_MODEL_REPO=...
```

Without weights, doctor and CLI still work; VLM propose path needs the model tree and a Python env with the project’s vision stack (see `.venv` / model worker).

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
#   → native/chrome-control/extension
```

## First run

1. Grant Screen Recording and Accessibility when prompted (for the window service binary / host app).
2. `lcu doctor --json` — check permissions (`screen_recording`/`accessibility`/`input_monitoring`) and surface connectivity in `notes` (`mac_window` / `chrome_tab`).
3. `lcu run "Open Downloads in Finder" --app com.apple.finder --wait --json`
4. If approval is required, use the menu-bar / desktop confirmation UI (not the CLI).

Without `--wait`, `lcu run` returns after queuing the task. Use `lcu status`, `lcu watch`, or `lcu result` to follow it. `waiting_user` requires human action; resume a user-paused task only after the user is ready.

## Privacy defaults

- Screenshots are not stored in SQLite and are not printed to stdout.
- Task metadata is local under the Runtime data root.
- See `docs/privacy.md`.

## Stopping

- `lcu cancel <task-id>` (releases target resources, including Chrome debugger/tab ownership)
- Quit `lcu-desktop` to stop the Runtime accept loop
