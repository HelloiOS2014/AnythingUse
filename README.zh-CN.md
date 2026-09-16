<div align="center">

# AnythingUse

<p align="center">
  <strong>本地优先、原生语义的计算机与移动端控制平面 (Computer Use Control Plane)</strong><br>
  赋予 AI Agent 真正操作真实桌面、浏览器与移动端应用的能力 —— 基于系统原生无障碍树，100% 本机静默后台运行，受不可逾越的人类安全门禁保护。
</p>

<p align="center">
  <a href="#"><img src="https://img.shields.io/badge/平台-macOS%20%7C%20Chrome%20%7C%20Android-007AFF?style=for-the-badge&logo=apple&logoColor=white" alt="支持平台" /></a>
  <a href="#"><img src="https://img.shields.io/badge/核心语言-Rust%20%7C%20Swift%20%7C%20Kotlin-orange?style=for-the-badge&logo=rust&logoColor=white" alt="编程语言" /></a>
  <a href="#"><img src="https://img.shields.io/badge/隐私-100%25%20本地%20%7C%20零云端通信-34C759?style=for-the-badge" alt="本地优先" /></a>
  <a href="#"><img src="https://img.shields.io/badge/安全-系统级强制安全门-red?style=for-the-badge" alt="安全体系" /></a>
  <a href="#"><img src="https://img.shields.io/badge/Agent-Claude%20%7C%20Pi%20%7C%20Grok-5856D6?style=for-the-badge" alt="支持的 Agent" /></a>
</p>

<p align="center">
  <a href="#-为什么选择-anythinguse">核心优势</a> •
  <a href="#-核心能力">核心能力</a> •
  <a href="#-系统架构">系统架构</a> •
  <a href="#-快速上手">快速上手</a> •
  <a href="#-agent-集成与技能">Agent 技能</a> •
  <a href="#-多端控制表面">多端表面</a> •
  <a href="#-安全与人在回路">安全体系</a> •
  <a href="#-文档索引">文档索引</a>
</p>

<p align="center">
  <a href="README.md">English</a> | <strong>简体中文</strong>
</p>

---

</div>

## ⚡ 为什么选择 AnythingUse？

市面上大部分 Computer Use 方案依赖截取全屏图片、发往云端多模态大模型盲猜屏幕像素坐标 `(x, y)`。

在实际生产应用中，这种“截图+盲猜”模式存在致命缺陷：
- 🐢 **迟缓且昂贵**：每一步都需要截取 4K/1080p 大图并等待 3–5 秒的视觉推理，消耗海量 Vision Token。
- 🎯 **像素极其脆弱**：窗口移动、跨屏幕 DPI 缩放、深色模式切换都会导致坐标偏移，极易发生误触甚至毁损数据。
- 🚫 **暴力霸占屏幕**：Agent 强行夺取鼠标光标并在屏幕上乱点，用户在 Agent 工作期间完全无法使用电脑。
- 🔑 **无痕空壳浏览器**：大部分工具只能新开一个无 Cookie、无登录态的空白无头浏览器，遇到 2FA 和常用网站必须反复重新登录。

**AnythingUse 从底层架构进行了彻底的范式重构：**

| 核心维度 | 传统视觉方案 (Anthropic / OSWorld) | AnythingUse |
|---|---|---|
| **UI 识别基准** | 像素截图 + 坐标盲猜 `(x, y)` | **操作系统原生无障碍树** (`AXUIElement` / `AccessibilityNodeInfo`) |
| **单步执行延迟** | 2,000ms – 5,000ms (云端视觉链路) | **低于 50ms** 原生语义级瞬时响应 |
| **Token 消耗** | 巨大（每一步都要上传全屏大图） | 语义操作**零视觉 Token**；极小结构化 JSON 交互 |
| **用户共存状态** | 强行霸占前台屏幕、抢夺物理鼠标光标 | **真正静默的后台执行**（定向窗口 / 独立后台标签页） |
| **浏览器环境** | 临时的空壳隔离浏览器，丢失所有登录态 | **复用日常登录的真实 Chrome**（保留已登录会话、Cookie 与 2FA） |
| **移动端支持** | 无支持，或依赖脆弱的 ADB 坐标模拟点击 | **原生 Android 辅助服务** (`lau` + USB 直连 + 真机触摸硬件感知) |
| **安全机制** | 依赖软性 Prompt 提示词引导（极易被越狱/幻觉突破） | **操作系统级强制门禁**：独立风险底线、防重放令牌、人类审批闭环 |
| **人工介入接管** | 用户的鼠标操作与 Agent 互抢光标冲突 | **物理级即刻让路**：触摸键盘、移动鼠标或触摸手机屏瞬间自动暂停 |

---

## 🌟 核心能力

### 🎯 原生语义控制（彻底告别像素盲猜）
直接对接操作系统原生无障碍接口。Agent 依托清晰的语义标识、标签名与 UI 角色（如 `"发送" 按钮`、`"搜索" 输入框`、`"确认" 对话框`）精准驱动界面。无视屏幕分辨率、DPI 缩放比例与窗口遮挡，点击永远精准。

### 🖥️ 真正静默的后台执行（不抢鼠标、不霸占桌面）
在 macOS 上基于 PID 与窗口 ID 实施精准的后台事件投递，全程不移动物理鼠标指针、不抢夺桌面窗口焦点。Agent 在后台执行自动化任务时，你可以继续流畅地写代码、看视频或处理邮件。

### 🌐 驱动真实 Chrome（复用真实配置与已登录会话）
通过专用 MV3 扩展与 Native Messaging 机制，在日常使用的 **真实 Chrome 配置** 中建立独立、非激活的后台任务标签页。无需在无头浏览器中重新输入密码，直接无缝复用已登录的 GitHub、Gmail、Jira、Slack 和内部系统。

### 📱 原生 Android 直控表面 (`lau`)
通过 USB 连接直控任何 Android 设备。基于机载 AccessibilityService 辅助服务实现语义元素提取、原生文字录入与页面滑动；集成硬件级触摸监听 (`getevent`)，手指触碰手机屏幕的瞬间任务即刻让出控制权。

### 🛡️ 坚不可摧的人在回路（Human-in-the-Loop）安全门
安全性不是提示词层面的建议，而是不可逾越的底层架构约束：
- **独立证据底线**：运行时在执行前独立核验 UI 节点的真实语义。即使 Agent 将某个危险动作声称为“无害浏览”，若目标实际为破坏性操作（如“删除数据”），系统强制升格至 R3 后果门禁，并在 Mac 上弹出原生系统对话框等待人类审批。
- **代际绑定的防伪令牌**：每个动作严格绑定生成时的观察令牌。若界面发生弹窗、标签切换或布局改变，陈旧动作立即 Fail-Closed 拒绝执行。
- **绝对凭证隔离**：密码框与敏感输入字段在系统底层自动打标，其明文绝不被读取、绝不进入截图、绝不上送模型。遇到凭证填写自动触发 R4 人工接管。

### 🤝 一流的 Agent 原生集成
开箱支持 **Claude Code**、**Pi**、**Grok Build**、**DeepSeek Harness（DSH）** 等主流 Agent CLI 工具，并为自定义 Agent 框架提供机器友好的标准化 CLI 接口。

---

## 🏗️ 系统架构

AnythingUse 将**决策者（AI Agent / 本地模型）**与**底座执行/安全策略**进行严格分层隔离：

```text
                     ┌──────────────────────────────────────────────┐
                     │         AI Agents (Claude / Pi / Grok)       │
                     │              或人类操作者 (CLI)              │
                     └───────────────────────┬──────────────────────┘
                                             │
                               ┌──────────────┴──────────────┐
                               ▼                             ▼
                        ┌──────────────┐              ┌──────────────┐
                        │   lcu CLI    │              │   lau CLI    │
                        └──────┬───────┘              └──────┬───────┘
                               │ (Unix Socket: 0600)         │ (本地 Daemon)
                               ▼                             ▼
                ┌─────────────────────────────┐       ┌──────────────┐
                │     AnythingUse 运行时      │       │  lau daemon  │
                │  队列 · 策略 · AX 语义引擎  │       └──────┬───────┘
                └──────┬───────────────┬──────┘              │ (adb forward)
                       │               │                     ▼
                       ▼               ▼              ┌──────────────┐
                ┌─────────────┐ ┌─────────────┐       │Android 真机  │
                │macOS 目标窗 │ │ 真实 Chrome │       │Accessibility │
                │ (静默后台)  │ │ (任务标签页)│       │   Service    │
                └─────────────┘ └─────────────┘       └──────────────┘
                       ▲               ▲                     ▲
                       └───────────────┴─────────────────────┘
                             系统级安全门禁与硬件输入监听
```

### 确定性执行闭环

```text
    ┌───────────┐         紧凑元素列表与观察令牌         ┌───────────┐
    │           │ ─────────────────────────────────────> │           │
    │  运行时   │                                        │ AI Agent  │
    │           │ <───────────────────────────────────── │           │
    └───────────┘          动作提案与意图影响声明        └───────────┘
```

1. **观察 (`decide`)**：运行时提取当前可见无障碍节点，生成轻量紧凑的结构化列表与唯一的代际防伪令牌。
2. **提案 (`act`)**：Agent 提交原子化语义动作（`invoke`、`set_value`、`scroll` 等），并附带闭集 `--effect` 意图声明。
3. **守卫核验**：运行时比对真实 UI 语义、强制核定风险底线，确认合规后原子化执行。
4. **重新核验证明 (`result`)**：任务的成功必须由 Agent 提交显式 `done` 并在**重新观察验证目标界面确已达成预期**后方可确认为 `succeeded`。

---

## 🚀 快速上手

### 1. 编译与安装

**环境要求**：Apple Silicon Mac、Rust 1.85+、Xcode Command Line Tools。

```bash
# 克隆仓库
git clone https://github.com/HelloiOS2014/AnythingUse.git
cd AnythingUse

# 编译核心 CLI 与原生服务
cargo build -p lcu-cli -p lcu-desktop -p lau-cli --release
(cd native/macos-window-service && swift build -c release)

# 安装可执行文件至 ~/.local/bin
./scripts/install-cli.sh
```

### 2. 环境自检

运行 `lcu doctor` 命令，快速检查系统权限与组件状态：

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

> **系统权限说明**：
> - **屏幕录制 (Screen Recording)**：用于目标窗口状态快照与视觉验证。
> - **辅助功能 (Accessibility)**：用于无障碍树解析与语义动作触发。
> - **输入监控 (Input Monitoring)**：用于感知人类对目标窗口的物理键鼠操作，实现瞬时让路。

### 3. 执行你的第一个任务

```bash
# 1. 在后台静默操作 macOS 桌面应用
lcu run "打开下载文件夹并选中最新的 PDF 文档" --app com.apple.finder

# 2. 在独立标签页中驱动你日常登录的真实 Chrome
lcu run "查看最新的 GitHub 通知" --app com.google.Chrome
```

### 4. 连接 Android 真机 (`lau`)

```bash
# 将无障碍辅助服务 APK 安装至 USB 连接的 Android 手机
./scripts/install-android-helper.sh

# 检查 ADB 连接与辅助服务授权状态
lau doctor

# 提取当前屏幕的完整语义无障碍树（毫秒级，无需截图）
lau dump

# 在手机上提交自动化任务
lau run "打开系统设置并检查电池健康度" --package com.android.settings
```

---

## 🤖 Agent 集成与技能

AnythingUse 为主流 Agent CLI 提供了官方原生 Skill 插件：

### 一键安装技能

| Agent 宿主 | 安装命令 |
|---|---|
| **Claude Code** | `claude plugin marketplace add HelloiOS2014/AnythingUse && claude plugin install anythinguse` |
| **Pi** | `pi install git:github.com/HelloiOS2014/AnythingUse` *(或执行 `./scripts/install-pi.sh`)* |
| **Grok Build** | `grok plugin install https://github.com/HelloiOS2014/AnythingUse --trust` |
| **DeepSeek Harness（DSH）** | `dsh plugin --profile <profile> add -w <repo>` *（或 `./scripts/install-dsh.sh [profile]`；见 [`dsh/README.md`](dsh/README.md)）* |

### 标准化 Agent 执行循环

Agent 通过直接调用 CLI 标准输入输出与 AnythingUse 交互（机器友好 JSON）：

```bash
# 1. 提交目标任务
lcu run "打开下载文件夹" --app com.apple.finder --actor agent --json

# 2. 获取紧凑观察结果（返回 elements 与 observation token）
lcu decide <task-id> --wait --json

# 3. 提交绑定令牌的语义动作
lcu act <task-id> --observation-id <obs-token> \
  --action '{"kind":"semantic","type":"invoke","element_id":"e12"}' \
  --effect '{"kind":"navigate","summary":"open Downloads"}'

# 4. 获取验证后的最终任务结果
lcu result <task-id> --json
```

---

## 🎛️ 多端控制表面

AnythingUse 针对不同操作系统表面深度定制了控制机制：

| 能力维度 | macOS 桌面应用 (`lcu`) | 真实 Chrome 浏览器 (`lcu`) | Android 移动端 (`lau`) |
|---|---|---|---|
| **控制范围** | 目标进程 PID + 确切窗口 ID | 绑定登录配置的独立后台标签页 | 目标应用包名 + Activity |
| **底层实现** | macOS 原生无障碍 (`AXUIElement`) | Chrome 扩展 + CDP 协议 | 原生 `AccessibilityService` 助手 |
| **后台运行** | 静默后台事件投递，不抢占焦点 | 独立后台标签页静默交互 | 手机前台应用直控 |
| **物理接管** | 触碰物理鼠标/打字即刻暂停 | 切换至该标签页即刻暂停 | 手指触摸手机屏幕即刻暂停 |
| **操作注入** | 窗口级定向事件投递 | 专用标签页 CDP 事件注入 | 无障碍节点原生 Action（绝不用 ADB 坐标模拟） |
| **安全门禁** | 应用授权 • 后果门 (R3) • 接管门 (R4) | 域名策略 • 后果门 (R3) | 应用授权 • 后果门 (R3) • 接管门 (R4) |

---

## 🛡️ 安全与人在回路

安全性是 AnythingUse 不可动摇的工程基石：

1. **100% 本地 IPC**：CLI、运行时与底层服务之间完全通过权限为 `0600`（父目录 `0700`）的私有 Unix Domain Socket 通信。绝无任何公网 TCP 端口暴露，零云端隐私遥测。
2. **独立证据风险底线**：运行时在执行前独立核验 UI 节点。即便 Agent 声称动作是“只读导航”，若目标按钮实际为危险操作（如“清空数据”），系统强制升格为 R3 后果门，暂停等待人类审批。
3. **代际绑定的防伪令牌**：每个操作提案严格锁定于生成时的观察令牌。若界面弹窗、页面刷新或布局跳变，旧令牌立刻失效并拒止执行。
4. **单次授权绝不跨步复用**：所有人类核准均在 Mac 端通过原生对话框（`osascript`）完成，且严格仅对当前单步原子操作生效。核准决不落盘、决不跨步重放。
5. **绝对凭据保护**：密码框与敏感输入字段被系统标记隔离，其明文绝不被读取、绝不进入提示词。凭证录入强制触发 R4 人工接管。
6. **物理硬件级让路 (HID Watch)**：任何时刻只要你在 Mac 上移动鼠标敲击键盘，或在手机上滑动屏幕，正在运行的任务会在毫秒级内自动让出控制权并暂停。

---

## 🚦 退出码与规范

`lcu` 与 `lau` 遵循完全统一的机器可读退出码体系：

| 退出码 | 状态代码 | 状态释义 | Agent 处理指引 |
|---:|---|---|---|
| **0** | `ok` | 命令或动作执行成功 | 继续下一步骤 |
| **2** | `waiting_user` | 阻断于人类安全门（需审批或人工接管） | 暂停并提示人类处理 |
| **3** | `failed` | 任务失败或操作被拒 | 处理错误或重新规划 |
| **4** | `permission_denied` | 系统权限被拒绝或缺失 | 提示操作者授予系统权限 |
| **64** | `usage_error` | 命令行参数或请求格式错误 | 修正命令调用参数 |
| **69** | `unavailable` | 运行时或底层守护进程不可达 | 启动运行时 (`lcu-desktop`) 或自检 |
| **70** | `internal_error` | 无法恢复的底层系统或驱动错误 | 记录错误日志并重启服务 |

---

## 📂 仓库结构

```text
├── crates/
│   ├── anything-core/           # 跨端通用基础契约（动作、效果、风险、退出码）
│   ├── lcu-*/                   # macOS 运行时：CLI、队列、策略、Chrome 与原生后端
│   └── lau-cli/                 # Android 控制 CLI 与本地守护进程
├── apps/
│   └── lcu-desktop/             # 菜单栏常驻运行时宿主与审批交互界面
├── native/
│   ├── macos-window-service/    # Swift 编写的原生窗口定向观察与输入服务
│   ├── chrome-control/          # 真实 Chrome 浏览器扩展与 Native Messaging 宿主
│   └── android-helper/          # Kotlin 编写的 Android 无障碍辅助服务 APK
├── skills/
│   ├── local-computer-use/      # 面向 macOS 与 Chrome 的官方 Agent 技能 (lcu)
│   └── local-android-use/       # 面向 Android 的官方 Agent 技能 (lau)
├── docs/                        # 详细规范说明、系统架构、执行契约与用户指南
└── scripts/                     # 构建、安装、打包与发布自动化脚本
```

---

## 📚 文档索引

### 核心规范与契约
- **[执行契约 (Execution Contract)](docs/execution-contract.md)** —— 严格的不变量规则、安全门与执行闭环规范。
- **[`lcu` 命令契约 (Command Contract)](docs/command-contract.md)** —— CLI 命令格式、JSON 信封与 Schema 规范。
- **[系统架构规格 (Architecture)](docs/architecture.md)** —— 核心组件设计、IPC 拓扑与关键技术决策。

### 指南与各端实现
- **[用户指南 (User Guide)](docs/user-guide.md)** —— 完整的安装部署与端到端使用指南。
- **[Android 端方案规划 (LAU Plan)](docs/lau-android-plan.md)** —— Android 端无障碍控制架构与真机验收记录。
- **[交付状态说明 (Delivery Status)](docs/status.md)** —— 源码对齐边界与真机验证记录。
- **[隐私与数据边界 (Privacy)](docs/privacy.md)** —— 凭据隔离与数据保护规范。
- **[故障排查手册 (Troubleshooting)](docs/troubleshooting.md)** —— 常见问题排查与诊断技巧。
- **[Agent 协作说明 (AGENTS.md)](AGENTS.md)** —— 面向自治 Agent 调用的强制规则与决策规范。
