# LAU — Android 端完整方案规划（v2）

> **状态：待批准（Draft v2）**。v1 经双模型并行审查（自审 + GPT-5.6 Sol，结论 BLOCK）后返工；Phase 2 起任何实现以本文为准，修改需先改文档。
> 产品名 **AnythingUse**；`lcu` = Local Computer Use（只管本地电脑）；`lau` = **Local Android Use**（Android 端独立 CLI）。

## 1. 目标与非目标

**目标**
- Agent 通过 `lau` 操作**真实 Android 手机**：观察（截图 + 节点树 + 屏幕状态）、语义执行、任务闭环（run/decide/act/result）。
- 与 mac 端同一设计哲学：语义优先、目标严格、诚实完成、本地优先、人机共存。
- 复用 `lcu-core` 的**类型与机制**（Action/EffectKind/授权机制），不复用其 mac 特化逻辑（见 §7）。

**非目标**
- ❌ Android 走 `lcu`（无 `lcu --app android`、无 `lcu-android`）
- ❌ Python 任何形式（uiautomator2 / appium 生态不用）
- ❌ ADB 注入任何输入（`input tap` / `input text` / `am start` 一律不出现；ADB 只做传输与观察）
- ❌ 常驻服务（无 launchd/LaunchAgent；daemon 按需拉起、空闲退出，见 §6）
- ❌ Wi-Fi / 云通道；❌ 模拟器专项优化；❌ Phase 4 前动 `lcu-desktop`

## 2. 命名与边界

| 项 | 值 |
|---|---|
| CLI | `lau`（`crates/lau-cli`，薄入口） |
| daemon | `lau` daemon（按需拉起，空闲 60s 退出，对齐 `lcu-desktop` 模式；**非常驻**） |
| helper | `native/android-helper/`（Kotlin APK，sideload） |
| 包名 | `dev.anythinguse.lau.helper`（socket 名带包名前缀，见 §4） |
| 环境变量 | `LAU_ADB_BIN`、`LAU_SERIAL`、`LAU_HELPER_PORT`、`LAU_IDLE_EXIT_SECS` |
| 数据根 | Mac 侧 `~/.local/share/AnythingUse/lau/`；设备侧无持久化 |
| Skill | Android 单独出 skill（不塞 `local-computer-use`），Phase 3 末定名 |

## 3. 总体架构（对照 mac 端）

| 层 | mac | Android |
|---|---|---|
| CLI | `lcu` | `lau` |
| Runtime | `lcu-desktop`（按需+空闲退出） | `lau` daemon（同模式，Phase 3 起） |
| 控制面 | `macos-window-service`（Swift，AX） | `android-helper` APK（Kotlin，AccessibilityService） |
| 通道 | 私有 unix socket | 设备 `localabstract:dev.anythinguse.lau.helper` + `adb forward`（仅 127.0.0.1） |
| 观察 | 窗口截图 + AX 树 + Input Monitoring | `adb screencap` + Accessibility dump + `getevent` 硬件触摸纪元 |
| 语义执行 | AXPress/AXSelect/AXSetValue | `performAction`：CLICK / SET_TEXT / SCROLL / FOCUS |
| 坐标兜底 | DirectedInput（需前台） | helper `dispatchGesture`（`canPerformGestures`，API 24+；**不经 ADB**） |
| 接管检测 | UserInputMonitor（事件标记） | daemon 常听 `getevent -lt`（硬件层有事件、注入无 → 干净区分） |
| 审批 | lcu-desktop 菜单栏 GUI | **Mac 对话框**（osascript，人在电脑前点；**不在手机弹**） |

**关键映射**（复用 `lcu-core` 类型的依据）：

| `lcu` semantic | AccessibilityNodeInfo | 备注 |
|---|---|---|
| `invoke` | `ACTION_CLICK` | 按节点能力探测；`ACTION_SELECT` 等后续扩展 |
| `set_value` | `ACTION_SET_TEXT` | 原生 unicode/CJK；仅能力已声明时暴露；执行后**重 dump 验证值**，不符 → `verification_failed` |
| `focus` | `ACTION_FOCUS` | 仅能力已声明时；accessibility focus 不混用 |
| `scroll` | `ACTION_SCROLL_{UP,DOWN,LEFT,RIGHT}`（API 23+） | delta 只取轴+符号 → **单页步进 + 重观察**；对角/幅度不承诺，不支持 → `unsupported_capability` |
| （新增）`global_back` | `performGlobalAction(GLOBAL_ACTION_BACK)` | 系统级回退，Phase 3 |

compact `elements[]`（id/role/label/frame/capabilities）与 `lcu decide` 同构。

## 4. 传输与协议

- **ADB 只做传输与观察**：装 APK、forward、screencap、getevent 监听（观察流，非注入）。**任何输入注入不经过 ADB**；坐标兜底一律 helper `dispatchGesture`。
- helper 监听 `localabstract:dev.anythinguse.lau.helper`（包名前缀防撞名）；Mac 侧 `adb forward tcp:127.0.0.1:<port>`。
- **forward 生命周期**：adb server 重启 / USB 重插 / transport 替换都会失效。daemon 在**每个会话前**：验证 serial → 检查/重建 forward → `ping` helper → 才执行。端口建议 serial 键控或 `tcp:0` 自动分配。
- **不确定结果不重放**：动作发出后响应丢失/超时 → 返回 `indeterminate`，**强制重观察**，绝不自动重试动作（副作用风险）。只重试幂等的 ping/dump。
- **协议（JSON 行）**：每行一个请求/响应；带 `v`（协议版本）、`id`（请求 ID，响应必须对应）；UTF-8；请求/树/文本有最大尺寸；超时与 exactly-one-response 语义；畸形输入 → 关连接。单时刻只接受一个活跃客户端（CLI 串行）。
- **本地威胁模型（如实写）**：forward 出来的 127.0.0.1 端口本机其他进程可达；设备侧 abstract socket 受 SELinux 约束但仍可能被设备本地进程探测。缓解：包名前缀 socket + 设备侧校验 peer credentials（实测 forward 后的 UID）+ 如需更强再加一次性 session token。**不宣称「无鉴权问题」**。

## 5. Helper APK 设计（Phase 2 主体）

### 5.1 组件
- `LauAccessibilityService`：核心服务。socket 与 accept 循环**直接宿主在 AccessibilityService 生命周期内**（系统绑定、被杀自动重启，不引入额外前台服务/通知）。`accessibilityFlags = FLAG_DEFAULT | FLAG_RETRIEVE_INTERACTIVE_WINDOWS`；**不**请求触摸探索。
- `LauActivity`：一次性引导——服务状态 + 深链无障碍设置 + HyperOS 授权指引（「USB 安装」需小米账号、自启动、电池无限制）。无其他 UI。
- dump：按需新鲜 dump（被动缓存仅记前台窗口包名）；**观察代次**（observationId）随每次 dump 递增，节点快照有界缓存。

### 5.2 执行规则
- 所有元素动作请求带 `observationId`；helper 校验代次 → `refresh()` 节点 → 复核窗口 ID/包名/bounds/能力 → 任一不符 → `stale_observation`，绝不静默换节点。
- `set_value`：能力未声明 → `unsupported_capability`；执行后重 dump 比对值，不符 → `verification_failed`。
- **helper 永不接收坐标请求之外的解释权**：`dispatchGesture` 仅由 CLI 侧在无语义能力时提出，仍受 `semantic_action_required` 拒绝规则约束（对齐 mac）。
- **helper 拒绝对自己包名的任何自动化动作**。

### 5.3 屏幕与安全状态（诚实观察）
- 观察输出必须携带：`isInteractive`、Keyguard 锁定态、前台包名/窗口、时间戳、方向/display ID、截图可用性。
- 灭屏/锁屏/FLAG_SECURE → 显式 `screen_off` / `device_locked` / `secure_capture_unavailable`；**绝不自动唤醒/解锁**；锁屏画面不满足 app 目标 → `target_lost`。
- **不承诺「安全页识别」**（v1 已删）：`isImportantForAccessibility` 与包名 denylist 只是纵深防御；敏感后果一律靠 app_access + effect guard（对齐 mac：mac 也不猜敏感页）。

### 5.4 审批（受信面）
- 人在 **Mac** 前操作，审批也在 Mac。**禁止手机弹窗**（会误触发 getevent 接管，操作者也不在看手机）。
- consequence 门 → daemon 弹出 **osascript 对话框**（Allow / Deny，默认 Deny）。CLI `lau act` 立即返回 `waiting_user` / exit 2，Agent 停下；人点 Mac 对话框。`lau approve <task-id>` 只负责再次打开该对话框，不能代点 Allow。
- Allow：执行**这一次**已停住的动作（grant 消费），然后重观察、交回 Actor。Deny：任务 failed。
- R4（不可逆高危）→ **人工接管**，不是普通审批。

### 5.5 构建与分发
- Gradle + Kotlin，`minSdk 24`（dispatchGesture/SCROLL_* 均满足）。`scripts/install-android-helper.sh`（`adb install -r`；含 HyperOS「USB 安装」失败指引）。
- `lau doctor` 四态区分：**installed / enabled / bound / ping-responsive**；验收含杀进程与**重启手机**后恢复。

## 6. `lau` daemon（Phase 3，按需非常驻）

- **生命周期**：首个 `lau run` 拉起；`LAU_IDLE_EXIT_SECS`（默认 60）无活动退出。不写 launchd。不用时进程不存在，零消耗；任务期间一个小 Rust 进程（socket + 单任务队列 + getevent 读进程，内存几 MB，空闲 0 CPU）。
- **职责**（跨 CLI 进程持有）：任务状态与队列（**每 serial 一条串行队列**）、app_access/consequence 门、观察代次、forward 会话管理、`getevent -lt` 触摸纪元（getepoch）。
- **接管规则**：任务执行前查 getepoch；真实硬件触摸 → epoch+1 → `paused (taken_over)`。getevent 不可用/断流 → **fail closed**（暂停任务并报告），不宣称共存。注入（performAction/dispatchGesture/input）不产生 getevent —— 需在真机（小米）实测验证两种情形。
- CLI 前缀进程（`lau run/decide/act/result/status/resume/cancel`）都是 daemon 的薄客户端；`doctor`/`screenshot`/`dump` 等无状态命令 Phase 2 起即单进程直连。

## 7. 与 mac 端共用/不复用

**复用**：`Action`/`SemanticAction`/`TargetedInput` 类型、`EffectKind`/`EffectClaim`、授权 grant 机制、`semantic_action_required` 语义、exit code 约定（0/2/3/64/69/70）。
**不复用、需 Android 化**：`effect_guard` **本体**（内含 mac 硬编码：AXConfirm、桌面 role 大小写、坐标点击 R0 地板）→ 新建 Android 证据层：Android role 规范化、`isPassword`/editable、包/签名身份、全页敏感标签、截图可用性、树完整性；坐标动作在不可达/安全/敏感面上禁止或抬高地板。
**不复用**：`lcu-runtime`（SQLite 持久化）、`lcu-platform-macos`、`apps/lcu-desktop`、`lcu` CLI。

## 8. 分期计划

### Phase 1 — 观察骨架（✅ 完成 + 本轮修复）
- `lau doctor` / `lau screenshot`（真机验收过：Xiaomi 2211133C / Android 16）。
- **本轮修复（审查 #13）**：doctor 吞掉 serial 解析错误 → 多设备/无效 serial 现为 blocker（`target_unresolved`），显式 `--serial` 校验存在与授权状态。

### Phase 2 — Helper APK（语义执行；无 Mac daemon）
- 交付：`native/android-helper/` + 安装脚本 + `lau dump / invoke / set_value / scroll / foreground`（CLI 单进程直连；**观察代次状态由 helper 持有**——AccessibilityService 本身设备侧长活）。
- **验收（确定性断言，非元素计数）**：
  1. doctor 四态 + 未启用时的引导 blocker
  2. dump：断言**已知 label**（如「设置」内具体条目文案）、能力字段、包/窗口身份
  3. invoke：语义点开已知条目，断言目标窗口/包切换（同包时断言窗口标题/层级变化），全程无坐标
  4. set_value：原生/Compose 控件输入 `你好LCU`，重 dump 断言值相等；WebView 不支持时返回 `unsupported_capability`（不静默失败）
  5. 陈旧 observationId → `stale_observation`；能力未声明 → `unsupported_capability`
  6. 锁屏/灭屏 → `device_locked`/`screen_off`，不自动唤醒
  7. 杀 helper 进程 → 系统自动重启服务（开关在）→ ping 恢复；**重启手机** → doctor 全绿（含 HyperOS 自启动指引）

### Phase 3 — `lau` daemon + 任务闭环 + 安全模型
- 交付：daemon（§6）+ `lau run --app <package> --actor agent` / `decide --wait --json` / `act` / `result` / `resume` / `cancel` / `approve`；Android 证据层 guard；consequence **Mac 对话框**（§5.4）。
- **验收**：
  1. 全闭环：`lau run "在设置中打开深色模式" --app com.android.settings --actor agent` → decide → act → done 重观察 → `succeeded`
  2. 中文搜索 + `global_back` 回退（两步）
  3. **真机双验**：真实手指触摸 → `paused`；`performAction`/`dispatchGesture` 注入 → **不**触发（getevent 区分）
  4. getevent 断流 → fail closed（暂停并报告，不装共存）
  5. app_access 首次 → 设备对话框；批准 → 重观察继续；helper 自动化触不到对话框
  6. 破坏性效果声明 → `waiting_user`（CLI exit 2，Agent 停）；**把删除/支付标签谎报为 navigate → 仍要到达正确的门**（证据覆盖低报）
  7. 有语义能力时提交坐标 → `semantic_action_required`
  8. 双设备接入：per-serial 队列与 forward 各自独立
  9. daemon 空闲退出（60s）→ 下次 `run` 重新拉起，任务状态不丢（队列在 daemon 内存，任务跨 daemon 重启不承诺——单任务内完成）

### Phase 4 — 并入共享 Runtime（评估，不承诺）
- 触发条件：Phase 3 全过 + 真实跨端单队列需求。届时先补设计文档再动 `lcu-desktop`。

## 9. 风险与对策

| 风险 | 对策 |
|---|---|
| HyperOS「USB 安装」需小米账号/`INSTALL_FAILED_USER_RESTRICTED` | 安装脚本 + 引导页逐步指引；troubleshooting 记录 |
| 无障碍开关手动一次性（等价 macOS 授权） | doctor 引导 blocker + 深链 |
| HyperOS 杀后台/自启动关 → 服务不复活 | 验收含杀进程+重启；指引开自启动/电池无限制 |
| SET_TEXT 场景局限（WebView/自定义 View/IME 校验） | 能力探测 + 重 dump 验证 + 显式错误码；验收覆盖 CJK/emoji/Compose |
| dump 时视图滞后（动画中） | 新鲜 dump + 代次 + `stale_observation` |
| forward 失效 / 响应不确定 | 每会话重建 + `indeterminate` 强制重观察，绝不重放 |
| 本机其他进程可触 forward 端口 | 包名 socket + peer credential 校验 + 可选 session token（如实声明威胁模型） |
| 灭屏/锁屏/FLAG_SECURE 假成功 | 观察带屏幕态 + 显式错误码；不自动唤醒 |
| 多设备 | per-serial 队列/forward/权限；doctor 不再吞歧义 |

## 10. 决策点

- **D1** `lau-cli` 依赖 `lcu-core` 类型（推荐：是；guard 本体不复用，另建 Android 证据层）
- **D2** helper 包名 `dev.anythinguse.lau.helper`（socket 名同前缀）
- **D3** ~~进程内单任务~~ → **按需 daemon + 空闲退出**（已并入 §6）
- **D5** 坐标兜底 = helper `dispatchGesture`（不经 ADB，受 `semantic_action_required` 约束）（推荐：是；替代项：完全禁止坐标）
- **D6** 审批 UI = **Mac 对话框**（osascript；`lau approve` 只开会话，不代批）。禁止手机弹窗。
- **D4** Android skill 名（Phase 3 末定）

## 11. 仓库布局（完成后）

```
crates/lau-cli/                 # lau CLI + daemon（已有 CLI）
native/android-helper/          # Gradle Kotlin APK
scripts/install-android-helper.sh
docs/lau-android-plan.md        # 本文档
```
