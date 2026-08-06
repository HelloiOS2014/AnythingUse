# Chrome control (AnythingUse)

Product Chrome backend for AnythingUse / LCU: **real user Chrome**, **MV3 extension + `chrome.debugger`/CDP**, **Native Messaging**, **task tab group + tab lease**, mapped to `ChromeTab` / `ControlState`.

(Historical milestone label: Wave D3 — not a separate product.)

## Invariants

| Rule | Implementation |
|---|---|
| Real Chrome | Unpacked MV3 extension in the user profile |
| No Playwright | No dependency; no secondary automation browser |
| No AX / AppleScript | DOM/CDP only; history/downloads/address-bar AX special cases are **not** on this path |
| Control plane | Unix socket under Runtime private entry only — **never TCP** |
| Tab lease | claim / handoff / release / force cleanup |
| Control state | `none` / `taken_over` / `target_lost` |
| Start / end task | Create task tab with `active:false`; **never** `tabs.update({active:true})` on the user tab (claim-time restore and end-task restore both steal focus) |

## Layout

```text
native/chrome-control/
  extension/           MV3 extension (stable id via manifest key)
  native-host/         Native Messaging host
  scripts/             install / uninstall
  README.md
```

Stable extension id:

```text
glcqmejd6hdgz7lkorqygtzonjxogz4b
```

Native host name:

```text
com.lcu.chrome_control
```

Private control socket (created when Chrome launches the host):

```text
~/Library/Application Support/LocalComputerUse/chrome-control.sock
```

Override root with `LCU_RUNTIME_ROOT` (same as Rust `RuntimePaths`).

## Install

### Prerequisites

- macOS with **Google Chrome**
- **Node.js** 18+ on `PATH`

### 1. Install native messaging host

```bash
./native/chrome-control/scripts/install-native-host.sh
```

Writes:

```text
~/Library/Application Support/Google/Chrome/NativeMessagingHosts/com.lcu.chrome_control.json
~/Library/Application Support/LocalComputerUse/chrome-control-host-wrapper.sh
```

### 2. Load the extension

1. Open `chrome://extensions`
2. Enable **Developer mode**
3. **Load unpacked** → `native/chrome-control/extension`
4. Confirm id `glcqmejd6hdgz7lkorqygtzonjxogz4b`

Chrome 150+ blocks CLI `--load-extension`; interactive load is required.

## Runtime methods

Extension dispatch (via native host Unix socket):

- `ping` / `get_state`
- `claim` / `start_task` — background task tab (`https://example.com/` default; never `chrome://` — debugger cannot attach); navigation via VLM actions; always `active:false`
- `handoff` — rebind task id without detaching
- `observe` / `act` / `navigate` / `type` / `click` / `read`
- `release` / `end_task` — detach debugger + drop lease (optional close tab)
- `cleanup` — force detach + drop lease (cancel / crash / disconnect)

Takeover (user activates the task tab or cancels debugger) detaches the debugger immediately and never re-activates a previously recorded user tab.
