# AnythingUse

> One local control plane for anything an agent can operate.

**中文版：[README.zh-CN.md](README.zh-CN.md)**

AnythingUse is a local-first control layer for humans and Agents. **Today:** real macOS applications and the user's installed Chrome. **Future:** Windows and other endpoint types through the same command-oriented model.

**Naming:** product name is **AnythingUse**. The public CLI is still `lcu`; crates, sockets, and the CLI keep the historical **LCU** codename (`lcu`, `lcu-*`); the data root is `~/Library/Application Support/AnythingUse`.

## Why AnythingUse

Computer-use systems often take over the foreground desktop, move the real pointer, or force Agents into a browser-specific API. AnythingUse takes a different approach:

- **Shared interface:** humans and Agents use the same `lcu` command.
- **Local-first:** task state, screenshots, model inference, and approvals stay on the machine by default.
- **Coexists with the user:** background work targets a specific window or Chrome task tab instead of taking over the whole desktop.
- **Honest completion:** an action receipt is not success; a task succeeds only after explicit completion and target re-observation.
- **Endpoint-oriented:** the Runtime routes goals to a platform backend, so future endpoints do not need a new public Agent protocol.

## What works today

| Area | Current capability |
|---|---|
| macOS applications | Operate a specific application window without activating it |
| Chrome | The user's real Chrome profile via extension + Native Messaging on an inactive task tab |
| Decision maker | Pluggable: local Qwen3-VL subprocess (default) or the external Agent itself (`lcu decide` / `lcu act`) |
| Scheduling | One serial FIFO queue with pause, resume, cancel, and crash recovery |
| Safety | Effect-based risk checks, GUI-bound approval, takeover detection, fail-closed window isolation |
| Persistence | Local SQLite task and event state |

The current implementation is a developer build for Apple Silicon macOS (no signed installer yet).

## Architecture

```mermaid
flowchart LR
    H["Human"] --> CLI["lcu CLI"]
    A["Agent + Skill"] --> CLI
    CLI --> IPC["Private Unix socket"]
    IPC --> RT["Runtime<br/>queue · state · approval · decision loop"]
    RT --> MAC["macOS window backend"]
    RT --> CHROME["Chrome tab backend"]
    MAC --> APP["Target app window"]
    CHROME --> TAB["Inactive task tab"]
    USER["User input"] -. "same-target takeover" .-> RT
```

The loop is `observe → decide → guard → act → observe`. `decide` is the pluggable step: either the local VLM proposes an action, or — with `lcu run --actor agent` — an external Agent fetches the observation (`lcu decide`) and submits one (`lcu act`). Both flow through the same validation, risk, and approval gates. See [Architecture](docs/architecture.md) for details.

## Quick start

```bash
# build
cargo build -p lcu-cli -p lcu-desktop --release
(cd native/macos-window-service && swift build -c release)

# start the Runtime and diagnose
./target/release/lcu-desktop &
./target/release/lcu doctor --json

# submit a task (--source is display-only metadata)
./target/release/lcu run "Open Downloads in Finder" \
  --app com.apple.finder --source agent --source-name codex --wait --json
```

Optional: Chrome surface (`./native/chrome-control/scripts/install-native-host.sh`, then `chrome://extensions` → Developer mode → **Load unpacked** → `native/chrome-control/extension`); local VLM weights (`./scripts/download_qwen3_vl.sh`, `models/Qwen3-VL-4B-Instruct`).

## Install the Skill as a plugin

The `local-computer-use` skill ships as a plugin from this repo. **Install** (one of):

```bash
# Claude Code
claude plugin marketplace add HelloiOS2014/AnythingUse
claude plugin install anythinguse

# Grok Build
grok plugin install https://github.com/HelloiOS2014/AnythingUse --trust
```

**Update**:

```bash
claude plugin update anythinguse    # or: grok plugin update
```

Skill updates ride the repo; the `lcu` binaries stay a separate build.

## Non-interference model

Coexistence is not "pause whenever the target app is frontmost":

- A macOS task is bound to a specific PID and window; background actions never activate it.
- If the user takes over that exact window, the task pauses and releases execution.
- Chrome work runs in an inactive task tab and never reactivates the user's tabs.
- If strict target identity cannot be maintained, the operation fails rather than guessing another window.

This is a product invariant. Switching to the target and switching back is not non-interference.

## Queue and completion

One serial FIFO queue per macOS login user; approval-waiting and user-paused tasks release the execution slot. A task reaches `succeeded` only when the model emits explicit `Done` **and** the target can be observed again. Repeated actions, step count, or a successful write do not complete the goal.

## Roadmap

- **Now (v3.2, on `main`):** macOS window control, real Chrome control, pluggable decision maker, CLI, Agent Skill. See [Delivery status](docs/status.md).
- **Next:** signed macOS packaging and Windows compatibility.
- **Later:** additional computers, mobile devices, remote hosts, or other controllable endpoints with a strict target and safe action model.

MCP, Playwright, public TCP, and long Top100/soak gates are **not** current product surfaces or freeze blockers.

## Repository map

```text
apps/lcu-desktop/             Runtime host and approval UI
crates/lcu-cli/               Public command surface
crates/lcu-core/              Shared contracts (actions, risk, task state, protocol)
crates/lcu-platform/          PlatformBackend trait + null backend
crates/lcu-runtime/           Queue, state, policy, and execution loop
crates/lcu-model/             Decision actors (VLM subprocess / AgentActor) + validation
crates/lcu-platform-macos/    Rust adapter for macOS control
crates/lcu-chrome/            Chrome backend adapter
native/macos-window-service/  Swift window-targeted service
native/chrome-control/        Extension and Native Messaging host
skills/local-computer-use/    Agent-facing Skill (directory name historical)
scripts/                      Model download and helper scripts
```

## Documentation

- [Delivery status](docs/status.md) · [Architecture](docs/architecture.md) · [User guide](docs/user-guide.md) · [`lcu` command contract](docs/command-contract.md) · [Privacy](docs/privacy.md) · [Troubleshooting](docs/troubleshooting.md)
- [Agent Skill](skills/local-computer-use/SKILL.md) · [macOS window service](native/macos-window-service/README.md) · [Chrome control](native/chrome-control/README.md)
