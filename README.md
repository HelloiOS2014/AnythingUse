<div align="center">

# AnythingUse

<p align="center">
  <strong>The Local-First, Native-Semantic Computer Use Control Plane</strong><br>
  Empower AI agents to reliably operate desktop, browser, and mobile applications — powered by OS accessibility trees, executed 100% on-device in the background, and guarded by non-negotiable human safety gates.
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/Platform-macOS%20%7C%20Chrome%20%7C%20Android-007AFF?style=for-the-badge&logo=apple&logoColor=white" alt="Platforms" /></a>
  <a href="#"><img src="https://img.shields.io/badge/Core-Rust%20%7C%20Swift%20%7C%20Kotlin-orange?style=for-the-badge&logo=rust&logoColor=white" alt="Languages" /></a>
  <a href="#"><img src="https://img.shields.io/badge/Privacy-100%25%20Local%20%7C%20Zero--Cloud%20IPC-34C759?style=for-the-badge" alt="Local-First" /></a>
  <a href="#"><img src="https://img.shields.io/badge/Safety-OS--Enforced%20Gates-red?style=for-the-badge" alt="Safety" /></a>
  <a href="#"><img src="https://img.shields.io/badge/Agents-Claude%20%7C%20Pi%20%7C%20Grok-5856D6?style=for-the-badge" alt="Agents" /></a>
</p>

<p align="center">
  <a href="#-why-anythinguse">Why AnythingUse</a> •
  <a href="#-key-capabilities">Key Capabilities</a> •
  <a href="#-architecture">Architecture</a> •
  <a href="#-quickstart">Quickstart</a> •
  <a href="#-agent-integration--skills">Agent Skills</a> •
  <a href="#-control-surfaces">Control Surfaces</a> •
  <a href="#-security--safety-gates">Security & Gates</a> •
  <a href="#-documentation">Documentation</a>
</p>

<p align="center">
  <strong>English</strong> | <a href="README.zh-CN.md">简体中文</a>
</p>

---

</div>

## ⚡ Why AnythingUse?

Most computer-use systems take full-screen screenshots, stream high-resolution images to cloud vision models, and blindly guess pixel coordinates `(x, y)`.

In the real world, this approach breaks down:
- 🐢 **Slow & Expensive**: Every action requires a 3–5s VLM inference roundtrip and thousands of vision tokens.
- 🎯 **Pixel Fragility**: Moving a window, changing display scaling, or toggling Dark Mode can cause coordinate drift, leading to misclicks or accidental data deletion.
- 🚫 **Desktop Hijacking**: The agent takes over your screen and cursor, preventing you from using your machine while the agent works.
- 🔑 **The Empty Browser Trap**: Launching an unauthenticated, throwaway browser loses your logins, cookies, and active sessions.

**AnythingUse reimagines computer use from the ground up:**

| Dimension | Traditional Vision Computer Use | AnythingUse |
|---|---|---|
| **UI Grounding** | Pixel screenshots & coordinate guessing `(x, y)` | **Native OS Accessibility Tree** (`AXUIElement` / `AccessibilityNodeInfo`) |
| **Execution Latency** | 2,000ms – 5,000ms per step | **Sub-50ms** native semantic resolution |
| **Token Cost** | Massive (full 4K/1080p image tokens on every step) | **Zero vision tokens** for semantic actions; compact JSON payloads |
| **User Coexistence** | Hijacks your screen and steals mouse cursor | **Silent Background Execution** (targeted window / inactive task tab) |
| **Browser Environment** | Headless / throwaway profile (requires constant re-login) | **Real User Chrome Profile** (retains active logins, cookies, and 2FA) |
| **Mobile Control** | Brittle ADB coordinate injection (`input tap x y`) | **Native Android Service** via USB (`lau` helper + real hardware touch detection) |
| **Safety Invariants** | Soft LLM system prompts (easily jailbroken or hallucinated) | **OS-Level Gates**: Independent risk floor, non-replayable tokens, human approval |
| **Human Interruption** | Agent fights you for the mouse pointer | **Instant Physical Takeover**: Task immediately pauses on physical mouse/touch |

---

## 🌟 Key Capabilities

### 🎯 Native Semantic Control (Zero Pixel Guessing)
Directly reads the operating system's accessibility tree. Agents interact with meaningful UI elements by identity, label, and role (`"Send" button`, `"Search" field`, `"Confirm" dialog`) rather than guessing pixels. It works identically regardless of screen resolution, DPI scaling, or window positioning.

### 🖥️ True Silent Background Execution
Operates directly on targeted windows via PID/Window ID routing on macOS without moving your mouse cursor or stealing focus. You can continue writing code, watching videos, or drafting emails while the agent works in the background.

### 🌐 Real Authenticated Chrome (Dedicated Task Tab)
Drives an inactive background task tab inside your **actual, daily-driver Chrome browser** via a native MV3 extension and Chrome DevTools Protocol. Seamlessly reuses your already-authenticated GitHub, Gmail, Jira, and Slack sessions without credential sharing or headless browser hurdles.

### 📱 Native Mobile Control Plane (`lau`)
Control any USB-connected Android device through a dedicated on-device AccessibilityService helper. Features semantic element dumping, native text entry, and physical touch monitoring (`getevent`) that yields control the millisecond your finger touches the screen.

### 🛡️ Ironclad Human-in-the-Loop Safety Gates
Safety is an OS-enforced invariant, not a prompt suggestion:
- **Independent Evidence Floor**: The runtime inspects actual UI semantics. Even if an agent claims an action is "harmless browsing", targeting a destructive button automatically escalates risk to Level 3 (Consequence Gate) and requires human confirmation via a native macOS dialog.
- **Anti-Replay Token Binding**: Actions are cryptographically bound to the exact observation token they were decided on. If the UI shifts or a popup appears, stale actions fail closed immediately.
- **Absolute Credential Privacy**: Passwords and sensitive input fields are masked at the OS layer. Secret text is never read, logged, or injected into LLM context.

### 🤝 First-Class AI Agent Integration
Out-of-the-box skills and plugins for **Claude Code**, **Pi**, **Grok Build**, and **DeepSeek Harness (DSH)**, accompanied by a clean, deterministic CLI interface for any custom agent harness.

---

## 🏗️ Architecture

AnythingUse enforces a strict separation between **decision makers** (AI agents or local VLMs) and **runtime execution/safety policy**:

```text
                     ┌──────────────────────────────────────────────┐
                     │         AI Agents (Claude / Pi / Grok)       │
                     │             or Human Operator CLI            │
                     └───────────────────────┬──────────────────────┘
                                             │
                               ┌──────────────┴──────────────┐
                               ▼                             ▼
                        ┌──────────────┐              ┌──────────────┐
                        │   lcu CLI    │              │   lau CLI    │
                        └──────┬───────┘              └──────┬───────┘
                               │ (Unix Socket: 0600)         │ (Local Daemon)
                               ▼                             ▼
                ┌─────────────────────────────┐       ┌──────────────┐
                │     AnythingUse Runtime     │       │  lau daemon  │
                │  Queue · Policy · AX Engine │       └──────┬───────┘
                └──────┬───────────────┬──────┘              │ (adb forward)
                       │               │                     ▼
                       ▼               ▼              ┌──────────────┐
                ┌─────────────┐ ┌─────────────┐       │Android Device│
                │macOS Windows│ │ Real Chrome │       │Accessibility │
                │(Background) │ │ (Task Tab)  │       │   Service    │
                └─────────────┘ └─────────────┘       └──────────────┘
                       ▲               ▲                     ▲
                       └───────────────┴─────────────────────┘
                           OS-Enforced Safety Gates & HID Watch
```

### The Deterministic Execution Loop

```text
    ┌───────────┐      compact elements & obs token      ┌───────────┐
    │           │ ─────────────────────────────────────> │           │
    │  Runtime  │                                        │ AI Agent  │
    │           │ <───────────────────────────────────── │           │
    └───────────┘       action proposal + effect claim   └───────────┘
```

1. **Observe (`decide`)**: The runtime extracts visible accessibility nodes into a compact, token-efficient representation bound to a unique observation token.
2. **Propose (`act`)**: The agent submits a semantic action (`invoke`, `set_value`, `scroll`) paired with a declared `--effect` intent.
3. **Guard & Verify**: The runtime verifies the action against live OS accessibility semantics, enforces the risk floor, and executes atomically.
4. **Re-Observe & Prove (`result`)**: Completion is only granted after an explicit `done` action **and** a fresh observation verifying the final state.

---

## 🚀 Quickstart

### 1. Build & Install

**Prerequisites**: macOS on Apple Silicon, Rust 1.85+, Xcode Command Line Tools.

```bash
# Clone the repository
git clone https://github.com/HelloiOS2014/AnythingUse.git
cd AnythingUse

# Build release binaries for macOS and Android CLIs
cargo build -p lcu-cli -p lcu-desktop -p lau-cli --release
(cd native/macos-window-service && swift build -c release)

# Install binaries (lcu, lau, lcu-desktop) into ~/.local/bin
./scripts/install-cli.sh
```

### 2. Environment Verification

Run `lcu doctor` to verify your environment and system permissions:

```bash
lcu doctor
```

```text
status: ok
permissions:
  ✓ screen_recording    (granted)
  ✓ accessibility       (granted)
  ✓ input_monitoring    (granted)
runtime: ready
```

> **macOS Permissions**:
> - **Screen Recording**: Captures window visuals for observation.
> - **Accessibility**: Reads AX element hierarchy and executes semantic clicks.
> - **Input Monitoring**: Detects physical keyboard/mouse input to yield control immediately.

### 3. Run Your First Task

```bash
# 1. Drive a macOS desktop app in the background
lcu run "Open Downloads and select the newest PDF" --app com.apple.finder

# 2. Drive your authenticated Chrome browser on an isolated task tab
lcu run "Check recent GitHub notifications" --app com.google.Chrome
```

### 4. Connect an Android Device (`lau`)

```bash
# Install the Accessibility helper APK to your connected Android phone
./scripts/install-android-helper.sh

# Verify ADB connection and helper authorization
lau doctor

# Dump the current screen's semantic tree (no screenshot needed)
lau dump

# Run an agent task on your phone
lau run "Open Settings and check Battery health" --package com.android.settings
```

---

## 🤖 Agent Integration & Skills

AnythingUse provides first-party skills for leading agent CLI environments:

### One-Click Installation

| Agent Harness | Installation Command |
|---|---|
| **Claude Code** | `claude plugin marketplace add HelloiOS2014/AnythingUse && claude plugin install anythinguse` |
| **Pi** | `pi install git:github.com/HelloiOS2014/AnythingUse` *(or `./scripts/install-pi.sh`)* |
| **Grok Build** | `grok plugin install https://github.com/HelloiOS2014/AnythingUse --trust` |
| **DeepSeek Harness (DSH)** | `./scripts/install-dsh.sh` *(or `./scripts/install-dsh.sh <profile>`; see [`dsh/README.md`](dsh/README.md))* |

### Standardized Agent Loop

Agents interact with AnythingUse by invoking the CLI directly with structured JSON:

```bash
# 1. Submit task
lcu run "Open Downloads folder" --app com.apple.finder --actor agent --json

# 2. Fetch compact observation (returns elements + observation token)
lcu decide <task-id> --wait --json

# 3. Submit semantic action bound to the observation token
lcu act <task-id> --observation-id <obs-token> \
  --action '{"kind":"semantic","type":"invoke","element_id":"e12"}' \
  --effect '{"kind":"navigate","summary":"open Downloads"}'

# 4. Check verified result
lcu result <task-id> --json
```

---

## 🎛️ Control Surfaces

AnythingUse provides specialized surfaces tailored to each target environment:

| Feature | macOS Applications (`lcu`) | Real Chrome Browser (`lcu`) | Android Device (`lau`) |
|---|---|---|---|
| **Target Scope** | Process PID + Window ID | Profile-scoped Inactive Task Tab | Package Name + Activity |
| **Underlying Layer** | macOS Accessibility (`AXUIElement`) | Chrome Extension + CDP via Native Host | Android `AccessibilityService` |
| **Background Execution** | Silent background event delivery | Inactive tab (never steals active tab) | Foreground app on connected device |
| **Physical Takeover** | Immediate pause on physical mouse/keys | Immediate pause on tab switch | Immediate pause on physical screen touch |
| **Action Delivery** | Directed window event synthesis | Chrome DevTools Protocol events | Semantic Accessibility actions (No ADB taps) |
| **Safety Gates** | App Access • Consequence (R3) • Takeover (R4) | Domain Policy • Consequence (R3) | App Access • Consequence (R3) • Takeover (R4) |

---

## 🛡️ Security & Safety Gates

AnythingUse is built around strict security and privacy guarantees:

1. **100% Local IPC**: All communications between CLI, runtime, and native services run over local Unix domain sockets restricted to `0600` permissions (`0700` parent directory). Zero public TCP listeners, zero external network telemetry.
2. **Independent Risk Floor**: The runtime independently inspects UI element semantics before executing any action. If an agent mislabels a destructive action (such as "Delete Account") as a benign navigation, the runtime automatically escalates the risk to R3 and halts for human approval.
3. **Cryptographic Anti-Replay Tokens**: Actions are strictly bound to their observation generation. If a modal opens, layout shifts, or a tab changes, the previous action token becomes invalid and fails closed.
4. **Non-Replayable Human Approvals**: Confirmations are requested via native macOS dialogs (`osascript`) and apply strictly to the single pending atomic action. An approval is **never cached or replayed** for subsequent actions.
5. **Masked Credential Isolation**: Password input fields and authentication views are automatically detected. Text content is masked and never read, screenshotted, or passed into model prompts. Credential entry triggers human takeover (R4).
6. **Hardware-Level Takeover (HID Watch)**: The moment you move your physical mouse or type on your keyboard (macOS), or touch the phone screen (Android), the running task yields control immediately and enters a paused state.

---

## 🚦 Exit Codes & Schema

AnythingUse CLIs (`lcu` and `lau`) return consistent, machine-readable exit codes:

| Exit Code | Status | Meaning | Agent Action |
|---:|---|---|---|
| **0** | `ok` | Command or action succeeded | Proceed to next step |
| **2** | `waiting_user` | Blocked on human gate (approval or takeover) | Yield to human operator |
| **3** | `failed` | Task or operation failed | Handle error or propose retry |
| **4** | `permission_denied` | OS permission rejected or missing | Request operator grant permission |
| **64** | `usage_error` | Command syntax or argument invalid | Correct command invocation |
| **69** | `unavailable` | Runtime or helper daemon unreachable | Start runtime (`lcu-desktop`) / check helper |
| **70** | `internal_error` | Unrecoverable system or driver error | File issue or restart service |

---

## 📂 Repository Map

```text
├── crates/
│   ├── anything-core/           # Shared platform-neutral types (actions, effects, risk, exit codes)
│   ├── lcu-*/                   # macOS runtime: CLI, queue, policy, Chrome & native backends
│   └── lau-cli/                 # Android control CLI & local daemon
├── apps/
│   └── lcu-desktop/             # Resident menu-bar runtime host and approval UI
├── native/
│   ├── macos-window-service/    # Swift window-targeted observation & input service
│   ├── chrome-control/          # Real Chrome extension & Native Messaging host
│   └── android-helper/          # Kotlin AccessibilityService helper APK
├── skills/
│   ├── local-computer-use/      # First-party agent skill for macOS & Chrome (lcu)
│   └── local-android-use/       # First-party agent skill for Android (lau)
├── docs/                        # Normative specifications, contracts, and guides
└── scripts/                     # Build, install, packaging, and release automation
```

---

## 📚 Documentation

### Core Contracts & Specifications
- **[Execution Contract](docs/execution-contract.md)** — Normative invariants, gate rules, and execution loop.
- **[`lcu` Command Contract](docs/command-contract.md)** — Complete CLI specification, JSON envelopes, and schemas.
- **[Architecture Specification](docs/architecture.md)** — Deep dive into system components, IPC, and design decisions.

### Guides & Platform Details
- **[User Guide](docs/user-guide.md)** — Comprehensive end-to-end setup and usage guide.
- **[Android Plan & Reference](docs/lau-android-plan.md)** — Architecture, protocols, and implementation for `lau`.
- **[Delivery Status & Verification](docs/status.md)** — Verification records and implementation boundaries.
- **[Privacy & Security Guide](docs/privacy.md)** — Credential protection and data isolation invariants.
- **[Troubleshooting Guide](docs/troubleshooting.md)** — Common error resolution and diagnostic tips.
- **[Agent Integration Notes](AGENTS.md)** — Guidelines and rules for autonomous AI agents.
