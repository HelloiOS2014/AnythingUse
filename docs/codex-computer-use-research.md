# Codex Computer Use 事实核查与方向纠偏

日期：2026-07-30  
核查对象：本机 Codex/ChatGPT Computer Use `26.721.1000502`、Chrome 扩展 `1.2.27221.15725`、OpenAI 最新 Codex Manual

## 结论

当前 LocalComputerUse 方案的执行层方向错误。

Codex 并不是通过“检测用户正在使用目标应用，然后暂停 Agent”实现无干扰，也不是仅依赖普通 AX action。macOS Computer Use 使用按应用、进程和窗口定向的观察与事件注入、合成焦点和虚拟光标，让目标应用接收操作而尽量不抢走用户当前的系统焦点和真实鼠标。

Chrome 又是另一条专用控制面：官方 Chrome 扩展使用 Chrome Debugger、Native Messaging、任务标签组和标签页 lease，在用户真实 Chrome 登录态中后台工作。

因此，下面三条旧约束不能同时成立：

1. 精确复刻 Codex 的 Chrome 后台行为；
2. 禁止所有浏览器专用控制、CDP 和 Playwright 风格接口；
3. 只用通用 AX/前台冲突检测操作 Chrome。

MCP 与此无关。LocalComputerUse 仍可只向人类和 Agent 提供 `lcu` 命令与 Skill，内部通过私有 Unix socket/Native Messaging 连接执行服务。

## 1. Codex 实际存在三条控制面

### `@Computer`

- 操作 macOS/Windows 桌面应用。
- macOS 官方明确支持 scoped task 在后台运行，用户继续在别处工作。
- Windows 明确运行在 active desktop 前台，会移动鼠标、输入并接管当前桌面。
- 两个平台不是同一种交互语义。

官方来源：[Computer Use](https://learn.chatgpt.com/docs/computer-use)

### `@Chrome`

- 使用用户现有 Chrome profile、登录态、标签页和扩展。
- 每个任务的页面放入任务标签组。
- 官方说明可在后台跨标签页工作而不接管用户浏览器。
- 用户已有标签页需要显式 claim；Agent 创建的标签页有 session/lease 生命周期，结束时关闭、交付或 handoff。

官方来源：[Chrome extension](https://learn.chatgpt.com/docs/chrome-extension)

### `@Browser`

- ChatGPT 桌面端内置浏览器。
- 使用与常规浏览器分离的 profile，不自动共享用户已有标签页或登录会话。
- 需要用户现有 Chrome 上下文时切换到 `@Chrome`。

官方来源：[Browser](https://learn.chatgpt.com/docs/browser)

## 2. macOS Computer Use 的本机实现证据

### 进程与 IPC

插件不是直接在 Agent 进程内操作系统：

```text
Skill / JS client
        ↓
SkyComputerUseClient
        ↓  本地 JSON-RPC Unix socket
SkyComputerUseService
        ↓
macOS Accessibility / ScreenCaptureKit / CoreGraphics
```

本机组件：

- `~/.codex/computer-use/Codex Computer Use.app`
- `SkyComputerUseService`
- `SkyComputerUseClient`
- bundle ID：`com.openai.sky.CUAService`

客户端每一个动作都携带 `app`：

```text
get_app_state(app)
click(app, element/coordinate)
set_value(app, element, value)
press_key(app, key)
type_text(app, text)
```

Computer Use Skill 明确说明：

- `press_key` 和 `type_text` 定向到指定应用，不能触发全局快捷键；
- `get_app_state` 可以在后台启动应用；
- 动作后重新读取目标应用状态和 AX 树。

### 窗口级观察

本机服务包含：

- `ApplicationWindow`
  - `cgWindow`
  - `axWindow`
  - `axApplication`
  - `windowID`
  - `pid`
- `SkyshotOperation`
- `SCScreenshotManager`
- `RefetchableSkyshotAXTree`
- `WindowOrderingObserver`

这说明它把 CGWindow、AXWindow、应用 PID 和 window ID 合并成同一个操作目标，按目标窗口截图并维护可重新获取的 AX 树，而不是只看“哪个 App 在前台”。

### 定向事件

本机二进制符号确认存在：

- `CGEventAPI.postToPid`
- `ApplicationUIElement.MouseEventTarget`
  - `pid`
  - `windowID`
  - `windowBounds`
  - `element`
- `ApplicationUIElement.KeyboardEventTarget`
  - `pid`
  - `element`
- `SynthesizedEvent.send(to: pid)`
- `SynthesizedEvent.click(... inWindow: windowID ...)`
- `SynthesizedEvent.scroll(... inWindow: windowID ...)`
- `SynthesizedEvent.type(string)`

这是 macOS 后台无干扰的核心：事件被合成为目标应用/窗口事件并发送给目标 PID，而不是默认发送到系统当前前台和用户真实键鼠。

### 合成焦点与虚拟光标

本机二进制同时包含：

- `SyntheticAppFocusEnforcer`
  - 让目标应用/窗口在处理合成事件时拥有所需的内部 active/focused 状态；
  - 记录“应用是否真实 active”与“应用是否认为自己 active/focused”。
- `SystemFocusStealPreventer`
  - 监听并抑制不应发生的焦点争抢；
  - 支持 target gained/lost focus handler。
- `ComputerUseCursor`
  - `targetWindowID`
  - `correspondingApplicationPID`
  - 独立 overlay window
  - `VirtualCursor`

因此用户看到的 Agent 光标是绑定目标窗口的虚拟覆盖层，不等于系统真实指针被移动。

## 3. Chrome 的本机实现证据

官方扩展：

```text
扩展 ID：hehggadaopoacecdllhhajmbjkdcmajg
版本：1.2.27221.15725
```

`manifest.json` 权限包括：

- `debugger`
- `nativeMessaging`
- `tabGroups`
- `tabs`
- `scripting`
- `webNavigation`
- `history`
- `downloads`

Native Messaging Host：

```text
com.openai.codexextension
→ ChatGPT for Chrome
```

扩展代码确认存在：

- `chrome.debugger.attach/sendCommand`
- CDP target/session 管理
- `sessionId` 与 turn
- tab lease
- managed agent tab groups
- agent tab / claimed user tab
- deliverable / handoff / cleanup
- 用户或扩展接管时中断 browser control

Codex 的 Chrome Agent API 同时提供：

- Playwright 风格的 DOM locator API；
- DOM CUA；
- 视觉坐标 CUA；
- screenshot；
- tab/session/claim/finalize。

所以“Codex 直接操作我的 Chrome”与“内部使用 Chrome 扩展、Debugger/CDP 和 Playwright 风格接口”并不矛盾。它操作的确实是用户真实 Chrome，不是另起 Playwright Chromium。

## 4. 旧方案错在哪里

### 错误一：把前台 App 等同于用户接管

当前实现：

```text
target PID == frontmost PID
→ UserActiveInTarget
→ PAUSED
```

这与 Codex 的实现相反。Codex 的重点是把事件定向到目标 PID/window，并合成目标应用内部焦点，同时保护用户系统焦点。

应该删除 `frontmost == target → pause` 这一核心不变量。

### 错误二：把 AX 当成完整执行器

AX 只是元素发现、语义动作和状态读取的一部分。Codex 同时使用：

- AX element/action；
- window-aware synthetic events；
- process-targeted CGEvent；
- synthetic app focus；
- virtual cursor；
- window screenshot。

当前“AX 做不到就 unsupported，只有独占模式才能输入”的能力边界不等价于 Codex Computer Use。

### 错误三：用通用 macOS AX 操作 Chrome

Codex 对 Chrome 使用专用扩展控制面。当前不断为 Chrome 菜单、历史、地址栏增加 AX 特例，是在复刻错误的层。

### 错误四：把用户无干扰设计成停机

正确语义应是：

- 用户在别处工作：Agent 继续后台任务；
- 用户在同一 App 的其他窗口/标签页工作：Agent 继续控制自己的目标；
- 用户接管 Agent 正在控制的同一窗口/标签页：中断或 handoff 该控制 session；
- 不能安全定向的系统级操作：才请求前台/独占控制。

不是“用户一用目标 App，整个任务暂停”。

## 5. 修正后的最小架构

```text
人类 / Agent
     ↓
lcu CLI + Agent Skill
     ↓
Rust Runtime
  队列 / 状态 / 风险 / 审批 / 持久化 / VLM
     ↓ Surface Router
     ├── macOS Native Computer Service
     │     Swift + AX + ScreenCaptureKit
     │     window-aware event synthesis
     │     CGEvent postToPid
     │     synthetic focus + virtual cursor
     │
     └── Chrome Extension
           Native Messaging
           tab groups + tab lease
           Chrome Debugger/CDP
           DOM CUA + visual CUA
```

对外仍然只有命令和 Skill，不需要 MCP。

### 技术栈修正

- Rust：保留 Runtime、队列、状态机、SQLite、风控、CLI、模型编排。
- Swift/macOS：实现真正的 macOS 窗口选择、定向事件、合成焦点、虚拟光标和 ScreenCaptureKit。
- TypeScript/JavaScript：实现 Chrome 扩展和 Native Messaging client。

纯 Rust macOS 后端不应再被当作必须条件。Codex 的关键 macOS 能力本身就是原生 Swift/AppKit/Accessibility/CoreGraphics 组合。

## 6. 仓库处理建议

### 保留

- `lcu` 命令与 Agent Skill；
- 全局队列和单自动任务限制；
- Runtime 状态机、SQLite、崩溃恢复；
- 风险判断、用户确认与接管；
- VLM 的观察/动作/重观察循环；
- AX 树读取与语义动作代码；
- macOS 先行。

### 推翻或降级

- app 前台即暂停的 `UserActiveInTarget` 策略；
- app 级 lease 作为用户冲突模型；
- Chrome AX 作为主控制路径；
- “非 AX 输入只能全局独占”的假设；
- 用 heuristic/应用特例证明通用 Computer Use；
- 当前 TextEdit 用户并行验收设计。

## 7. 正确的前期验证顺序

在继续 VLM 和业务开发前，只做两个小型技术 Spike：

1. macOS Native Spike  
   在用户持续操作 App A 时，后台对 App B 的指定 window 执行点击、输入、滚动；用户系统焦点和真实鼠标不变，目标窗口状态成功变化。

2. Chrome Extension Spike  
   在用户继续使用当前 Chrome 标签页时，扩展创建任务标签组，在后台标签页完成导航、点击、输入和验证；不切换用户当前标签页。

两个 Spike 成功后，才把现有 Rust Runtime 和 VLM 接入。失败时直接暴露底层能力缺口，不再用 heuristic、AppleScript 特例或更多文档掩盖。

