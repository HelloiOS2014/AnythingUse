# LAU — Android 端完整方案规划（v2）

> **状态：已按本文推进中**（v2；2026-09-15 增补「观察身份」修订，见 §4 / §5.2 / D7）。v1 经双模型并行审查（自审 + GPT-5.6 Sol，结论 BLOCK）后返工；Phase 2 起任何实现以本文为准，修改需先改文档。**当前实现进度、差距与真机验收结果见 §0。**
> 产品名 **AnythingUse**；`lcu` = Local Computer Use（只管本地电脑）；`lau` = **Local Android Use**（Android 端独立 CLI）。

## 0. 实现现状与差距（2026-09-15：源码核对 + 真机验收）

本节记录**代码里已经存在什么**，以及**真机上实测到了什么**。完整证据（逐条命令 + 原始输出）见
[`evidence/lau/phase2-acceptance-2026-09-15.md`](../evidence/lau/phase2-acceptance-2026-09-15.md)。

**真机验收结果（2026-09-15，Xiaomi 2211133C / Android 16 / serial `<device-serial>`）**

| 断言 | 结果 |
|---|---|
| P2-1 doctor 四态 + 未启用时的引导 blocker | ✅ 两条路径均过（禁用时精确报出引导 blocker，exit 3） |
| P2-2 dump：已知 label / 能力字段 / 包与窗口身份 | 🟡 包名、能力字段、真实 label 均过；**窗口身份取不到**（#22） |
| P2-3 invoke 语义点开（全程无坐标） | ✅ 设置首页 →「我的设备」详情页，代次 15→16 |
| P2-4 set_value 中文并重 dump 断言 | ✅ 重 dump 得到 `value == 你好LCU` |
| P2-5 陈旧 observationId / 能力未声明 | ✅ `stale_observation`、`unsupported_capability`，均 exit 3 |
| P2-6 锁屏/灭屏 + 不自动唤醒 | ✅ `screen_off` / `device_locked`；`mWakefulness` 保持 Asleep |
| P2-7 杀 helper 自动恢复 / 重启手机 | 🟡 **进程会重启，但 HyperOS 把无障碍开关一起关掉**（#17）；重启手机未测 |
| P3-3 getevent 真机双验 | ✅ 真手指 → `paused`(taken_over)；注入动作**不**触发 |
| P3-9 daemon 空闲退出 | ✅ 60s 退出并清理 socket；无状态命令不经 daemon |

**对原文两处结论的更正**：
- P2-2 要求「包/窗口身份」：实测**只有包名可用**，`windowTitle` 恒空且无 `windowId`（#22）；
- P2-7 的「系统自动重启服务（开关在）→ ping 恢复」**只对了一半**：进程确实被系统重启，但 HyperOS 会**自行关掉无障碍开关**，必须人工重开（#17）。

**已实现（源码级 + 真机确认）**

- CLI：`doctor` / `screenshot` / `dump` / `invoke` / `set-value` / `scroll` / `foreground` / `launch`，daemon 侧 `run` / `decide` / `act` / `status` / `result` / `cancel` / `approve`；全局 `--serial`（env `LAU_SERIAL`）。退出码 `0` / `2`(waiting_user) / `3` / `64` / `69` / `70`。
- daemon：按需拉起、空闲 60s 退出（`LAU_IDLE_EXIT_SECS`）、socket `~/.local/share/AnythingUse/lau/lau.sock`、**任务状态仅存内存**（daemon 重启即失）。
- helper：Kotlin AccessibilityService，`localabstract:dev.anythinguse.lau.helper`（ADB 只做 `forward`），op = `ping` / `dump` / `foreground` / `invoke` / `set_value` / `scroll` / `launch`；每次 dump 递增代次并在动作时校验 → `stale_observation`；`set_value` 执行后重读比对；拒绝对自身包名自动化；灭屏/锁屏 → `screen_off` / `device_locked`；节点上限 400。
- 接管检测：`run` 时启动 `adb shell getevent -lt`，捕获 `BTN_TOUCH` / `ABS_MT_TRACKING_ID` 递增触摸纪元；`decide` / `act` 前比对，不一致 → `paused` + `wait_reason=taken_over`。
- **触摸守卫（fail-closed，2026-09-15 补）**：每个设备一条监听、纪元按 serial 隔离；`run` 时监听起不来即拒绝建任务，`decide` / `act` 前监听已断流 → 任务转 `paused` + `wait_reason=watch_unavailable`。
- **`lau resume`（2026-09-15 补）**：解除 `taken_over` / `watch_unavailable` 造成的暂停；恢复时**重建该设备监听**（仍不健康则再次 fail-closed），并作废暂停前的 `observation_id` 与待批准动作。
- **`decide --wait`（2026-09-15 补）**：轮询直到有可决策观察；只等人处理的暂停会继续等（`lau resume` 可解除），遇 consequence 门 / 终态 / 未知任务立即返回；超时 `LAU_DECIDE_WAIT_SECS`（默认 600s）。
- 后果门：`act` 的 `effect` 属 `Destructive` / `ExternalCommunication` / `ExternalSubmit` / `PermissionChange` / `Financial` / `Credential` / `Unknown` 时停为 `wait_reason=consequence`，弹 **Mac** osascript 对话框（默认 Deny），CLI 返回 `waiting_user`（exit 2）；`lau approve <task-id>` 只重新打开该对话框。
- 语义优先：坐标动作（`Targeted`）在 `act` 一律被拒（`semantic_action_required`）；`Navigate` 声明为 Chrome-only。

**未实现 / 与计划的差距（按严重度）**

| # | 差距 | 位置 | 影响 |
|---|---|---|---|
| 1 ✅ | ~~`getevent_ok` 只写不读~~ **已修**：`run` 时监听起不来直接拒绝建任务；`decide`/`act` 前监听已断流 → 任务 `paused`（`wait_reason=watch_unavailable`） | `daemon.rs` | 满足 §6 的 fail-closed，不再静默放行 |
| 2 ✅ | ~~触摸纪元是全局的~~ **已修**：改为 `watches: HashMap<serial, TouchWatch>`，纪元与健康状态按设备隔离 | `daemon.rs` | 双设备的接管判定互不干扰 |
| 3 ✅ | ~~无 `resume` / `pause` / `watch`~~ **部分修**：`lau resume` 已实现（重建监听、作废旧观察与待批动作）；`pause` / `watch` 仍未提供 | `main.rs` / `daemon.rs` | 被接管的暂停现在可以恢复 |
| 4 ✅ | ~~`decide --wait` 被丢弃~~ **已修**：客户端轮询 `LAU_DECIDE_WAIT_SECS`（默认 600s），遇暂停持续等、遇 consequence 门/终态立即返回 | `main.rs` | Agent 侧无需自行轮询 |
| 5 ✅ | ~~无 app_access 门~~ **已实现并全路径真机验证（2026-09-15）**：身份 = 包名 + 签名证书 SHA-256（helper `app_identity`）；按 D8② 拦 `decide`/`act`；三按钮 Mac 对话框；`always_allow` 持久化到 `<数据根>/app_permissions.json`（0600），`lau permissions` 列出、`--revoke <key>` 撤销 | daemon / helper | 门触发、真实身份、allow_once 不落盘、Deny→failed、always_allow 持久化 + 静默放行、revoke 幂等 —— 全部真机通过 |
| 6 | 无 Android 证据层 guard（§7） | `daemon.rs` | **已实现（2026-09-15）**：`crates/lau-cli/src/evidence.rs` 按 §5.6 落地（密码字段/凭证文本 → R4，发送·删除·支付类 → R3，能力未声明/元素不在观察内 → 抬高，声明只能抬不能降），11 个单测通过；helper 已补 `password` 标记。**真机已验证密码框 → R4**（验收 16）；验收 15（谎报 navigate）目前只有单测覆盖 |
| 7 | R4 与 R3 同路：走普通 consequence 对话框，**不是**人工接管 | `daemon.rs` | ✅ **已实现并真机验证（2026-09-15，验收 16）**：R4 → 两步 Mac 对话框（Start takeover → 人自己在手机上做 → Done），**提案被丢弃、从不执行**（密码框 `value` 保持 null）；完成后 `observation_id` 清空，旧令牌 act 被拒为 `stale observation_id` |
| 8 | consequence 授权无 `GateRequest` / `ConsequenceGrant` 绑定、无一次性消费与过期；批准后**重放**已存动作（helper 侧靠代次兜底） | `daemon.rs` | 与 mac 端「批准不重放」不同，属 Android 特有设计，必须在验收中证明安全 |
| 9 | 无 `indeterminate`（§4）：动作超时/响应丢失只当普通错误 | `daemon.rs` | 计划要求的「不确定不重放」语义缺失 |
| 10 ✅ | ~~无 per-serial 队列 / forward 隔离~~ **已实现（2026-09-15）**：`serial_is_busy` + `promote_next_for_serial`，每个请求入口做一次 `sweep_queues`；同设备第二个任务进 `queued`，终态后自动提升 | `daemon.rs` | 真机（单设备）验证通过；转发隔离本就按 serial 键控端口。**双设备并行未验**（只有一台设备） |
| 11 | `dispatchGesture` 坐标兜底未实现（D5）；helper 亦无 `global_back` | helper / daemon | Phase 2 承诺的兜底缺席 |
| 12 | 无 helper peer 凭据校验（§4 威胁模型承诺项） | helper | 本机其他进程仍可触达 forward 端口 |
| 13 | 分发与技能：`install-cli.sh` 不装 `lau`、`package-release.sh` 不打包 lau/helper、无 Android Skill | `scripts/` | 未产品化 |
| 14 | `android-helper` 无测试；`lau-cli` 已有 5 个单测（守卫/纪元）但无端到端 | 全仓 | 回归保护仍薄弱 |

**真机验收新增的差距（2026-09-15，按严重度）**

| # | 差距 | 位置 | 影响 |
|---|---|---|---|
| 15 ✅ | ~~**`observationId` 不持久、无会话身份**~~ **已修（2026-09-15）**：观察令牌改为 `<sessionId>:<generation>`，实例重建即会话失效 | helper / daemon | 已按 §4 实现，验收第 10 条真机通过（数字撞车仍被拒） |
| 16 ✅ | ~~helper **未按 §5.2 复核窗口 ID/包名/bounds/能力**~~ **已修（2026-09-15）**：`resolveNode` 落地 7 步校验（会话/代次/下标+refresh/包名+windowId/bounds 容差 0.5%/能力） | helper | 已按 §5.2 实现，验收第 11、12 条真机通过 |
| 17 | 🔴 **HyperOS 会自行关闭无障碍服务**（机主确认未操作；发生在杀进程之后） | 系统 / 产品 | 命中 §9 风险表；任务无法继续，必须人工重开。**一次复现尝试未成功（`am crash` 后服务仍为 enabled），触发条件未确证** |
| 18 ✅ | ~~**可点元素与 label 分离**~~ **已修（2026-09-15）**：dump 时把子树文字归并到可点行上（§3「label 归属」，深度 ≤3、≤3 段、≤200 字符） | helper | 真机验证：`e10 LinearLayout "蓝牙 已开启"`、`e5 "我的设备"` 等全部带名；按名字直接 `invoke` 打开蓝牙页成功 |
| 19 | 🟠 daemon 响应被**截断在 8192 字节**，`decide` 偶发 exit 70（12+15 次压测未复现，根因未确证） | daemon | **已加埋点（2026-09-15）**：daemon 把每次响应的 `op + bytes` 追加到 `<数据根>/daemon.log`（不含负载，>64 KiB 启动时截断）；真机已验证写入（`decide bytes=6660`）。**等复发抓现场** |
| 20 ✅ | ~~doctor 判据不可靠~~ **已修（2026-09-15）**：`bound` 改为解析 `Bound services:` **块**（该块跨多行、按 `android:label` 列出服务），`enabled` 为唯一门禁；`enabled:false && ping:true` 时在 `notes` 里显式说明是陈旧实例 | `main.rs` | 真机 `bound:true` 正确；禁用态由"真机原文单测"覆盖（§5.5 判据表已写明） |
| 21 | 🟡 `decide`→`act` 之间代次增长极快（输入文字、搜索结果、切页均 bump），元素 id 会重排 | daemon | 观察极易过期，Agent 必须"拿到即用" |
| 22 | 🟡 dump 缺窗口身份：`windowTitle` 取自 `root.contentDescription` 实测恒空、无 `windowId`；§5.3 的时间戳 / 方向(displayId) / 截图可用性也缺 | helper | 验收 P2-2 只完成一半；缺 `target_lost` 类判定依据 |
| 23 ✅ | ~~关屏时 `screenshot` 照常"成功"~~ **已修并验证（2026-09-15）**：按 §5.3 先取屏幕状态，非交互/锁屏即 `screen_off` / `device_locked`（exit 3），成功时 JSON 带 `isInteractive`/`keyguardLocked` | `main.rs` | 真机三条全过：亮屏解锁 → ok 带状态；关屏 → `screen_off`；亮屏锁屏 → `device_locked`（均未唤醒/解锁设备） |
| 24 ✅ | ~~服务被禁用后 socket 仍可连接但返回空响应~~ **已修（2026-09-15）**：报错改为可行动指引（指向 `lau doctor --json` + 重新打开无障碍开关） | `helper.rs` | 用户拿到的是下一步动作，而不是 `empty response` |
| 25 | 🟡 **bounds 复核在列表动画期间会拒绝动作**（真机遇到一次：连续滚动时 `stale_observation: node e1 moved or resized since the dump`） | helper | 设计内的 fail-closed，但会带来"重试一次"的操作成本；已写入 troubleshooting。若实测过于频繁，再评估 D7 的容差或对可滚动容器放宽 |
| 26 ✅ | ~~app access 被拒后，后续 `decide` 会把已 `failed` 的任务**复活**成新的门并再弹一次对话框~~ **已修（2026-09-15，真机发现）**：门在建立前先检查终态，终态任务一律不再产生新门；新增回归单测 | `daemon.rs` | 拒绝是终态，不会被下一次调用推翻 |

**✅ = 2026-09-15 本轮修复**（`cargo test -p lau-cli` 5 项通过；`cargo test --workspace` 全绿；#15/#16 的修复另经真机验收第 10–13 条确认）。

**口径澄清（2026-09-15，已由项目所有者确认）**：审批**只在 Mac**、**禁止任何手机弹窗**（手机弹窗会误触发 getevent 接管，操作者也不在看手机）。§5.4 与 D6 从始至终如此规定；Phase 3 验收第 5 条原先误写为「设备对话框」，已统一为 Mac 对话框。实现 app_access 门时必须遵守这一点。

## 1. 目标与非目标

**目标**
- Agent 通过 `lau` 操作**真实 Android 手机**：观察（截图 + 节点树 + 屏幕状态）、语义执行、任务闭环（run/decide/act/result）。
- 与 mac 端同一设计哲学：语义优先、目标严格、诚实完成、本地优先、人机共存。
- 复用共享**类型与机制**（Action/EffectKind；授权 grant 机制待复用），不复用 mac 特化逻辑——这些类型现已抽到平台中立的 `anything-core`（见 §7 与 §0 #8）。

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
| 环境变量 | `LAU_ADB_BIN`、`LAU_SERIAL`、`LAU_HELPER_PORT`、`LAU_IDLE_EXIT_SECS`、`LAU_DECIDE_WAIT_SECS` |
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
| 坐标兜底 | DirectedInput（需前台） | helper `dispatchGesture`（`canPerformGestures`，API 24+；**不经 ADB**）—— **尚未实现**；当前 `lau act` 直接拒绝坐标动作 |
| 接管检测 | UserInputMonitor（事件标记） | daemon 常听 `getevent -lt`（硬件层有事件、注入无 → 干净区分）—— 已实现，纪元按 serial 隔离；监听断流即 fail-closed；**2026-09-15 真机验证：真手指 → `taken_over`，注入 → 不触发** |
| 审批 | lcu-desktop 菜单栏 GUI | **Mac 对话框**（osascript，人在电脑前点；**不在手机弹**） |

**关键映射**（复用 `anything-core` 类型的依据）：

| `lcu` semantic | AccessibilityNodeInfo | 备注 |
|---|---|---|
| `invoke` | `ACTION_CLICK` | 按节点能力探测；`ACTION_SELECT` 等后续扩展 |
| `set_value` | `ACTION_SET_TEXT` | 原生 unicode/CJK；仅能力已声明时暴露；执行后**重 dump 验证值**，不符 → `verification_failed` |
| `focus` | `ACTION_FOCUS` | 仅能力已声明时；accessibility focus 不混用 |
| `scroll` | `ACTION_SCROLL_{UP,DOWN,LEFT,RIGHT}`（API 23+） | delta 只取轴+符号 → **单页步进 + 重观察**；对角/幅度不承诺，不支持 → `unsupported_capability` |
| （新增）`global_back` | `performGlobalAction(GLOBAL_ACTION_BACK)` | 系统级回退，Phase 3 |

compact `elements[]`（id/role/label/frame/capabilities）与 `lcu decide` 同构。

**label 归属（2026-09-15 修订，针对 §0 #18）**：可点的容器常常自身没有文字，文字挂在其子节点上（实测：`e12 LinearLayout`（可点、无 label）与 `e13 TextView "蓝牙"`（有 label、不可点）是父子）。**dump 必须把 label 归到"能被点的那一行"上**：

- 节点自身有 `text` / `contentDescription` → 用自身的；
- 自身为空 → 从其**子树**（深度 ≤ 3）收集非空 `text` / `contentDescription`，去重后以空格拼接，最多 3 段、总计 200 字符（沿用既有长度上限）；
- 只在 label **为空**时派生，绝不覆盖节点自身的文字。

这样 `invoke` 的目标天然带名字，Agent 不必按坐标猜（对齐 mac 侧由 Runtime 解析 Finder 行的做法，但 lau 把它放在 helper：树就在设备侧，父子关系是精确的，Mac 端只有平铺坐标，只能几何猜测）。

## 4. 传输与协议

- **ADB 只做传输与观察**：装 APK、forward、screencap、getevent 监听（观察流，非注入）。**任何输入注入不经过 ADB**；坐标兜底一律 helper `dispatchGesture`。
- helper 监听 `localabstract:dev.anythinguse.lau.helper`（包名前缀防撞名）；Mac 侧 `adb forward tcp:127.0.0.1:<port>`。
- **forward 生命周期**：adb server 重启 / USB 重插 / transport 替换都会失效。daemon 在**每个会话前**：验证 serial → 检查/重建 forward → `ping` helper → 才执行。端口建议 serial 键控或 `tcp:0` 自动分配。
- **不确定结果不重放**：动作发出后响应丢失/超时 → 返回 `indeterminate`，**强制重观察**，绝不自动重试动作（副作用风险）。只重试幂等的 ping/dump。
- **协议（JSON 行）**：每行一个请求/响应；带 `v`（协议版本）、`id`（请求 ID，响应必须对应）；UTF-8；请求/树/文本有最大尺寸；超时与 exactly-one-response 语义；畸形输入 → 关连接。单时刻只接受一个活跃客户端（CLI 串行）。
- **观察身份（2026-09-15 修订，针对 §0 #15/#16）**：服务实例在 `onServiceConnected` 生成随机 `sessionId`；`observationId` 从裸计数器改为 **`"<sessionId>:<generation>"` 的不透明字符串**，客户端只负责原样带回，不得解析、拼接或自行构造（`ping` 也返回当前 `sessionId`，供诊断）。
  动作校验必须**同时**满足：`sessionId` 与当前实例一致 **且** `generation` 与当前代次一致；任一不符 → `stale_observation`。
  这堵掉了"实例重建后计数器归零、旧 id 与新的数值巧合相等就被当成当前"的窗口（§0 #15 实测：同一进程 PID 不变、代次 22→1）。
- **本地威胁模型（如实写）**：forward 出来的 127.0.0.1 端口本机其他进程可达；设备侧 abstract socket 受 SELinux 约束但仍可能被设备本地进程探测。缓解：包名前缀 socket + 设备侧校验 peer credentials（实测 forward 后的 UID）+ 如需更强再加一次性 session token。**不宣称「无鉴权问题」**。

## 5. Helper APK 设计（Phase 2 主体）

### 5.1 组件
- `LauAccessibilityService`：核心服务。socket 与 accept 循环**直接宿主在 AccessibilityService 生命周期内**（系统绑定、被杀自动重启，不引入额外前台服务/通知）。`accessibilityFlags = FLAG_DEFAULT | FLAG_RETRIEVE_INTERACTIVE_WINDOWS`；**不**请求触摸探索。
- `LauActivity`：一次性引导——服务状态 + 深链无障碍设置 + HyperOS 授权指引（「USB 安装」需小米账号、自启动、电池无限制）。无其他 UI。
- dump：按需新鲜 dump（被动缓存仅记前台窗口包名）；**观察代次**（observationId）随每次 dump 递增，节点快照有界缓存。

### 5.2 执行规则
- 所有元素动作请求带 `observationId`（`<sessionId>:<generation>` 字符串）。helper 的校验顺序如下，**任一不符即 `stale_observation`，绝不静默换节点**：
  1. `sessionId` == 当前实例的 `sessionId`（实例一旦重建，此前所有观察立即作废）；
  2. `generation` == 当前代次；
  3. `elementId` 下标在本次 dump 范围内，且 `refresh()` 成功；
  4. **节点身份复核**：刷新后节点的 `packageName` 与 `windowId` 必须与 dump 时记录的一致；
  5. **bounds 复核**：刷新后节点的归一化 bounds 必须与 dump 时记录的一致（容差取值见 D7）；
  6. **能力复核**：请求的能力必须仍在 dump 时声明的集合内（例如 `set_value` 要求节点仍可编辑），否则 `unsupported_capability`；
  7. 节点包名不得是 helper 自身（既有规则）。
  helper 需为**当前代次**保存 `elementId → {packageName, windowId, bounds, capabilities}` 快照（随代次失效、数量有界）。
- `set_value`：能力未声明 → `unsupported_capability`；执行后重 dump 比对值，不符 → `verification_failed`。
- **helper 永不接收坐标请求之外的解释权**：`dispatchGesture` 仅由 CLI 侧在无语义能力时提出，仍受 `semantic_action_required` 拒绝规则约束（对齐 mac）。
- **helper 拒绝对自己包名的任何自动化动作**。

### 5.3 屏幕与安全状态（诚实观察）
- 观察输出必须携带：`isInteractive`、Keyguard 锁定态、前台包名/窗口、时间戳、方向/display ID、截图可用性。
- 灭屏/锁屏/FLAG_SECURE → 显式 `screen_off` / `device_locked` / `secure_capture_unavailable`；**绝不自动唤醒/解锁**；锁屏画面不满足 app 目标 → `target_lost`。
- **`screenshot` 命令遵循同一条规则（2026-09-15 修订，针对 §0 #23）**：屏幕非交互或已锁屏时**明确失败**（`screen_off` / `device_locked`，exit 3），**不得**返回一张锁屏图却报 `ok`；成功时 JSON 必须带上 `isInteractive` 与 `keyguardLocked`。屏幕状态取自 helper 的 `foreground`（它在任何屏幕状态下都能回答，且不需要唤醒）。
- **不承诺「安全页识别」**（v1 已删）：`isImportantForAccessibility` 与包名 denylist 只是纵深防御；敏感后果一律靠 app_access + effect guard（对齐 mac：mac 也不猜敏感页）。

### 5.4 审批（受信面）

- 人在 **Mac** 前操作，审批也在 Mac。**禁止手机弹窗**（会误触发 getevent 接管，操作者也不在看手机）。
- **三类门，语义互不替代**（2026-09-15 细化）：

  | 门 | 何时 | 选项 | 是否记住 |
  |---|---|---|---|
  | **app access** | 首次控制某个包（触发点见 D8） | Allow once / Always allow / Deny | 只记 `always_allow` |
  | **consequence**（R3） | 单个动作被证据层判为 R3 | Allow / Deny（默认 Deny） | **一次性**，消费即失效 |
  | **takeover**（R4） | 单个动作被判为 R4 | Start takeover →（人自己在手机上做）→ Done / Cancel | 不适用 |

- **app access 的身份** = `包名 + 签名证书摘要`（`GET_SIGNING_CERTIFICATES` 首证书 SHA-256）：应用升级不失效，**换签名即失效**。展示用"应用名（包名）"，判定只认身份串。
- 对话框必须**披露**：将要控制哪个应用、身份、AnythingUse 优先语义动作、以及**该许可不授权任何后果类动作**。
- consequence：CLI `lau act` 立即返回 `waiting_user` / exit 2，Agent 停下；`lau approve <task-id>` 只重新打开对话框，**不能代批**。
- **批准不等于重放（2026-09-15 修订，取代原文"Allow：执行这一次已停住的动作"）**：Allow 只产生**一个一次性授权**；daemon **丢弃**已停住的提案，**强制重新观察**，由同一个 Actor 基于新观察重新提交。授权按"后果身份"（包 + 动作 + 元素身份）匹配并有有效期，**消费一次即失效**；不匹配则作废重来。理由：与 mac 执行契约 §4/§5 一致，并消除 §0 #8 的"批准后重放"分歧。
- R4 走**人工接管**（不是普通审批）：
  - 弹出"Start takeover" → 人**自己在手机上**完成该动作 → 回到 Mac 对话框点 **Done**（或 Cancel）；
  - **Done**：**不执行**已停住的动作，丢弃提案 → 重新观察 → 交回同一个 Actor 继续；
  - **Cancel**：任务 failed；
  - 等待期间任务停在 `waiting_actor` + `wait_reason=consequence`，**不占用**设备执行资源。
- app access 的持久决定可撤销：`lau permissions --json` 列出、`lau permissions revoke <key>` 撤销；**CLI 永远没有"批准"路径**。

### 5.5 构建与分发
- Gradle + Kotlin，`minSdk 24`（dispatchGesture/SCROLL_* 均满足）。`scripts/install-android-helper.sh`（`adb install -r`；含 HyperOS「USB 安装」失败指引）。
- `lau doctor` 四态区分：**installed / enabled / bound / ping-responsive**；验收含杀进程与**重启手机**后恢复。
  **判据与优先级（2026-09-15 修订，针对 §0 #20/#24）**：
  1. `enabled`（读 `settings get secure enabled_accessibility_services`）是**唯一门禁依据** —— 只有它能决定 blocker 与退出码；
  2. `bound` 必须**只解析 `dumpsys accessibility` 的 `Bound services:` 块**（该块**跨多行**，且服务**按 `android:label` 列出**，不是组件名），不得对整篇 dumpsys 做子串匹配（禁用后 `button:{…LauAccessibilityService…}` 条目仍在，会造成恒真的假阳性）；
  3. `ping` 仅作诊断，存在**滞后窗口**（服务已禁用、旧实例 socket 尚未销毁时仍能应答）；`enabled:false` 而 `ping:true` 时必须在报告里显式说明"这是陈旧实例，以 `enabled` 为准"；
  4. 服务禁用后 socket 可连接但返回空响应：CLI 必须给出**可行动的报错**（提示去 `lau doctor` 并重新打开无障碍开关），不得只报 `empty response`。
  5. `lau doctor` 只读已足够；CLI 只有在**能够**做到时才去切换开关（本章暂不允许 CLI 改设备设置）。

### 5.6 Android 证据层（EffectGuard）—— 2026-09-15 新增设计

**位置**：`crates/lau-cli` 内新模块（见 D10）。**复用共享的类型与策略表**（`RiskLevel` / `EffectKind` / `EffectClaim` / `anything_core::effect_guard::effect_policy`），**证据分类自己实现**：`lcu-core` 的 macOS 实现（AXConfirm、桌面 role）不复用（§7），也不去改共享的 `ElementNode`（mac/Chrome 共用，为一个尚未承诺的 Phase 4 统一而耦合它不划算）。
**接口形态**：`judge(elements: &[Value], action: &Action, effect: Option<&EffectClaim>) -> Judgement` —— 直接吃 daemon 手里那份 dump 原始元素 JSON（不构造 `AppObservation`）。
**输出**：`Judgement { risk, rationale, model_claim_overridden, unknown }`（字段对齐 anything-core 的 `EffectJudgement`）。

判定规则（**证据下限，只抬不降**；Actor 声明只能抬高）：

| 证据 | 最低风险 |
|---|---|
| 观察 / 等待 / `focus` | R0 |
| 语义 `invoke` 导航类、`scroll` | R1 |
| `set_value` 且非敏感 | R2 |
| 标签/描述命中「发送 / 提交 / 删除 / 卸载 / 发布 / 确认 / 支付」等对外或不可逆语义 | **R3** |
| 节点 `password=true`，或标签/值命中「密码 / 验证码 / OTP / 信用卡 / CVV / 支付密码」 | **R4** |
| `set_value` 的文本本身像凭证（字母数字混合且 ≥12 位，或 6 位纯数字） | **R4** |
| 元素不在本次观察内 / 能力未声明 / 树为空却要执行动作 | **R3**，或以 `unsupported_capability` 拒绝 |
| 证据互相矛盾（话术像取消、动作像提交） | **unknown → 停下问人** |

- **不按应用名/包名做策略**（对齐 mac 执行契约 §1）：包名只用于 app access 身份与审计，风险一律由**动作证据**判定。
- 坐标动作继续一律拒绝（`semantic_action_required`），直到 D5 决定 `dispatchGesture`。
- 证据层是 **Runtime 侧的下限**：Actor 声明 `navigate` 而证据是「发送」时取 R3，并记 `model_claim_overridden=true`。
- 需要 helper 配合的一处 schema 增补：dump 的每个元素增加 `"password": true`（当 `AccessibilityNodeInfo.isPassword`），仅供证据层使用。

## 6. `lau` daemon（Phase 3，按需非常驻）

- **生命周期**：首个 `lau run` 拉起；`LAU_IDLE_EXIT_SECS`（默认 60）无活动退出。不写 launchd。不用时进程不存在，零消耗；任务期间一个小 Rust 进程（socket + 单任务队列 + getevent 读进程，内存几 MB，空闲 0 CPU）。
- **职责**（跨 CLI 进程持有）：任务状态与队列（**每 serial 一条串行队列**）、app_access/consequence 门、观察代次、forward 会话管理、`getevent -lt` 触摸纪元（getepoch）。
- **接管规则**：任务执行前查 getepoch；真实硬件触摸 → epoch+1 → `paused (taken_over)`。getevent 不可用/断流 → **fail closed**（暂停任务并报告），不宣称共存。注入（performAction/dispatchGesture/input）不产生 getevent —— 需在真机（小米）实测验证两种情形。**当前实现**：断流已 fail-closed（任务转 `paused` 并报告，`lau resume` 会重建监听）；纪元按 serial 隔离（§0 #1/#2 已修）。**真机双验已于 2026-09-15 通过**：真手指触摸 → `paused(taken_over)`；注入动作（helper `performAction`）→ 不触发。节点身份复核已按 §5.2 落地（§0 #15/#16 已修）。
- **诊断日志（2026-09-15 新增，针对 §0 #19）**：daemon 在 `<数据根>/daemon.log` 追加**每次响应的操作名与字节数**（**不含任何负载**，避免泄露屏幕内容），用于定位那次"响应被截断在 8192 字节"的偶发故障；daemon 启动时轮转（超过 64 KiB 即截断，只保留最近一段）。
- CLI 前缀进程（`lau run/decide/act/result/status/resume/cancel`）都是 daemon 的薄客户端；`doctor`/`screenshot`/`dump` 等无状态命令 Phase 2 起即单进程直连。

## 7. 与 mac 端共用/不复用

**复用**：`Action`/`SemanticAction`/`TargetedInput` 类型、`EffectKind`/`EffectClaim`、`semantic_action_required` 语义、exit code 约定（0/2/3/64/69/70）。这些类型现已抽到**平台中立**的 `crates/anything-core`（`lcu-core` 退化为 macOS 层：再导出 + `StaticEffectGuard`），**`lau-cli` 直接依赖 `anything-core`，不依赖 `lcu-core`**。授权 grant 机制尚未复用（§0 #8）。
**不复用、需 Android 化**：`effect_guard` **本体**（内含 mac 硬编码：AXConfirm、桌面 role 大小写、坐标点击 R0 地板）→ 新建 Android 证据层：Android role 规范化、`isPassword`/editable、包/签名身份、全页敏感标签、截图可用性、树完整性；坐标动作在不可达/安全/敏感面上禁止或抬高地板。
**不复用**：`lcu-runtime`（SQLite 持久化）、`lcu-platform-macos`、`apps/lcu-desktop`、`lcu` CLI。

## 8. 分期计划

### Phase 1 — 观察骨架（✅ 完成 + 本轮修复；真机验收已过）
- `lau doctor` / `lau screenshot`（真机验收过：Xiaomi 2211133C / Android 16）。
- **本轮修复（审查 #13）**：doctor 吞掉 serial 解析错误 → 多设备/无效 serial 现为 blocker（`target_unresolved`），显式 `--serial` 校验存在与授权状态。

### Phase 2 — Helper APK（语义执行；无 Mac daemon）—— 验收已执行：5 通过 / 2 部分；`dispatchGesture` 仍未实现（§0）
- 交付：`native/android-helper/` + 安装脚本 + `lau dump / invoke / set_value / scroll / foreground`（CLI 单进程直连；**观察代次状态由 helper 持有**——AccessibilityService 本身设备侧长活）。
- **验收（确定性断言，非元素计数）**：
  1. doctor 四态 + 未启用时的引导 blocker
  2. dump：断言**已知 label**（如「设置」内具体条目文案）、能力字段、包/窗口身份
  3. invoke：语义点开已知条目，断言目标窗口/包切换（同包时断言窗口标题/层级变化），全程无坐标
  4. set_value：原生/Compose 控件输入 `你好LCU`，重 dump 断言值相等；WebView 不支持时返回 `unsupported_capability`（不静默失败）
  5. 陈旧 observationId → `stale_observation`；能力未声明 → `unsupported_capability`
  6. 锁屏/灭屏 → `device_locked`/`screen_off`，不自动唤醒
  7. 杀 helper 进程 → 系统自动重启服务（开关在）→ ping 恢复；**重启手机** → doctor 全绿（含 HyperOS 自启动指引）

### Phase 3 — `lau` daemon + 任务闭环 + 安全模型 —— **部分交付**（daemon / 闭环 / 接管 / fail-closed 守卫 / resume / `decide --wait` 已有；验收第 3、9 条真机通过；安全模型主要缺口见 §0）
- 交付：daemon（§6）+ `lau run --app <package> --actor agent` / `decide --wait --json` / `act` / `result` / `resume` / `cancel` / `approve`；Android 证据层 guard；consequence **Mac 对话框**（§5.4）。
- **验收**：
  1. 全闭环：`lau run "在设置中打开深色模式" --app com.android.settings --actor agent` → decide → act → done 重观察 → `succeeded` —— ✅ **2026-09-15 真机通过（目标真实达成）**：`在设置里打开蓝牙页面`，两步语义动作（返回 → 蓝牙行，step=4）后重观察确认页面已是蓝牙页，再 `done` → `succeeded`。注意：`succeeded` 只证明「显式 Done + 目标可再次观察」，**不判断目标内容**（与 mac 契约一致，诚实性由 Actor 负责）
  2. 中文搜索 + `global_back` 回退（两步）—— 🟡 `global_back` ✅ 2026-09-15 真机通过（`global_back ok`，从蓝牙页退回设置首页）；任务闭环内的"中文搜索"待补（Phase 2 的 P2-4 已单独验证过 CJK `set_value`）
  3. **真机双验**：真实手指触摸 → `paused`；`performAction`/`dispatchGesture` 注入 → **不**触发（getevent 区分）—— ✅ 2026-09-15 通过
  4. getevent 断流 → fail closed（暂停并报告，不装共存）—— ✅ **2026-09-15 真机故障注入通过**：杀掉 `getevent` 进程后 `decide` → `paused` + `wait_reason=watch_unavailable`，`status` 报告 `watch:{healthy:false, dead_reason:"getevent stream ended"}`；`lau resume` 重建监听后 `healthy:true` 恢复
  5. app_access 首次 → **Mac 对话框**（与 §5.4 / D6 一致；**不是**手机弹窗，避免误触发 getevent 接管）；批准 → 重观察继续；helper 自动化触不到对话框
  6. 破坏性效果声明 → `waiting_user`（CLI exit 2，Agent 停）；**把删除/支付标签谎报为 navigate → 仍要到达正确的门**（证据覆盖低报）
  7. 有语义能力时提交坐标 → `semantic_action_required` —— ✅ **2026-09-15 真机通过**：`act` 提交 `targeted/click` → 立即 `semantic_action_required`（exit 3），**不弹任何门**。注意：曾因证据层先判 R3 而错误地弹出后果确认框，已把"坐标一律先拒"提到门之前（D5 未决期间不得让坐标经审批执行）
  8. 双设备接入：per-serial 队列与 forward 各自独立 —— 🟡 **per-serial 队列已实现并真机验证（单设备）**：同设备第二个任务 `state=queued`（`wait_reason=device_queue`），`decide` 返回 `queued` 且不弹门；前一个任务终态后自动提升（`promoted from the device queue`）。**真正的双设备并行未验**（当前只有一台设备）
  9. daemon 空闲退出（60s）→ 下次 `run` 重新拉起，任务状态不丢（队列在 daemon 内存，任务跨 daemon 重启不承诺——单任务内完成）
  10. **会话隔离**（2026-09-15 新增，§0 #15）：重建服务实例（关屏/亮屏，或 `am crash`）后，用**旧** `observationId` 提交动作 → 必须 `stale_observation`，**即使代次数值巧合相同** —— ✅ 2026-09-15 通过（旧会话 `65a49aa8:1` vs 新会话 `185f2569:1`，数字相同仅会话不同，被拒；同元素换新令牌则成功）
  11. **节点身份复核**（2026-09-15 新增，§0 #16）：dump 后让 UI 变化到 `eN` 指向别的元素，再用旧 `observationId` 提交 → 必须 `stale_observation`，**不得**点到新元素 —— ✅ 2026-09-15 通过（`node e2 failed refresh`，页面未被改动）
  12. **能力复核**（2026-09-15 新增）：对 dump 时声明 `set_value`、现已不可编辑的节点执行 `set_value` → `unsupported_capability` 或 `stale_observation` —— ✅ 2026-09-15 通过（`e2 did not advertise set_value`）
  13. **回归**：正常 `decide → act` 流程与 Phase 2 的 P2-3 / P2-4 不受影响 —— ✅ 2026-09-15 通过（`invoke` 与中文 `set_value` 均正常）
  14. **app access**（2026-09-15 新增，§0 #5）：首次控制一个未授权的包 → Mac 对话框；Deny → 任务 failed；Allow once → 继续且**同一任务内**不再问；Always allow → 新任务也不再问；`lau permissions revoke` 之后重新问 —— ✅ **2026-09-15 全路径真机通过**：门触发并取到真实签名身份；`Allow once` 生效且**不落盘**、换任务再问；第二个任务 `Deny` → `failed`（并因此发现并修复 §0 #26）；`Always allow` → 落盘 `app_permissions.json`（0600）、新任务静默放行；`--revoke` → 删除并幂等
  15. **证据覆盖低报**（§0 #6）：把「支付/删除」标签的动作谎报为 `navigate` → 仍到达正确的门，且 `model_claim_overridden=true`
  16. **密码框**（§0 #6）：对 `password=true` 的输入框执行 `set_value` → 走 R4 人工接管，**绝不自动输入**
  17. **不重放**（§0 #8 / D9）：Allow 之后若 UI 已变化，旧提案**不得**被执行；任务必须基于新观察重新决策

### Phase 4 — 并入共享 Runtime（评估，不承诺）
- 触发条件：Phase 3 全过 + 真实跨端单队列需求。届时先补设计文档再动 `lcu-desktop`。

## 9. 风险与对策

| 风险 | 对策 |
|---|---|
| HyperOS「USB 安装」需小米账号/`INSTALL_FAILED_USER_RESTRICTED` | 安装脚本 + 引导页逐步指引；troubleshooting 记录 |
| 无障碍开关手动一次性（等价 macOS 授权） | doctor 引导 blocker + 深链 |
| HyperOS 杀后台/自启动关 → 服务不复活 | **2026-09-15 实测命中（§0 #17）**：系统会**自行关闭无障碍服务**（机主未操作），进程虽被重启但开关不会回来 → **必须人工重开**；对策：doctor 以 `enabled` 为准并给出引导；指引开自启动/电池无限制 |
| SET_TEXT 场景局限（WebView/自定义 View/IME 校验） | 能力探测 + 重 dump 验证 + 显式错误码；验收覆盖 CJK/emoji/Compose |
| dump 时视图滞后（动画中） | 新鲜 dump + 代次 + `stale_observation` |
| forward 失效 / 响应不确定 | 每会话重建 + `indeterminate` 强制重观察，绝不重放 |
| 本机其他进程可触 forward 端口 | 包名 socket + peer credential 校验 + 可选 session token（如实声明威胁模型）；**peer 校验尚未实现（§0 #12）** |
| getevent 断流 | 已修（§0 #1）：断流即 fail-closed 暂停并报告；`lau resume` 重建监听，重建不成功则任务保持暂停 |
| 灭屏/锁屏/FLAG_SECURE 假成功 | 观察带屏幕态 + 显式错误码；不自动唤醒 |
| 多设备 | per-serial 队列/forward/权限；doctor 不再吞歧义 |

## 10. 决策点

- **D1** `lau-cli` 依赖共享契约类型（**已落地**：依赖平台中立的 `anything-core`，不依赖 `lcu-core`；guard 本体不复用，Android 证据层待建）
- **D2** helper 包名 `dev.anythinguse.lau.helper`（socket 名同前缀）
- **D3** ~~进程内单任务~~ → **按需 daemon + 空闲退出**（已并入 §6）
- **D5** 坐标兜底 = helper `dispatchGesture`（不经 ADB，受 `semantic_action_required` 约束）（推荐：是；替代项：完全禁止坐标）
- **D6** 审批 UI = **Mac 对话框**（osascript；`lau approve` 只开会话，不代批）。**禁止手机弹窗（2026-09-15 已确认，见 §0 口径澄清）**。
- **D4** Android skill 名（Phase 3 末定）
- **D7**（2026-09-15 新增，**待定**）bounds 复核的严格度：① 严格相等（最保守，可能因动画/微移把同一元素误判为 stale）；② 归一化容差（**推荐**，如 ≤0.5% 屏宽/高）；③ 只比 `packageName` + `windowId`、不比 bounds
- **D8**（2026-09-15 新增，**已定：②**）app access 的触发点：**同时拦 `decide`**（读屏也是控制，与 mac 一致）；`dump` / `screenshot` 作为无状态调试命令不受门禁约束。备选 ① 只拦 `act`（更松）、③ 连 `dump` 也拦（会让调试命令不可用）
- **D9**（2026-09-15 新增，**已定：不重放**）Allow 只给一次性授权，daemon 丢弃提案并强制重新观察（§5.4 已按此写）；helper 的代次/会话校验作为第二道防线保留
- **D10**（2026-09-15 新增，**已定：`lau-cli` 模块内**）Android 证据层先与 CLI/daemon 同 crate；Phase 4 并入共享 Runtime 时再抽独立 crate

## 11. 仓库布局（完成后）

```
crates/anything-core/           # 共享平台中立契约（lau 与 lcu 共用；已抽取）
crates/lau-cli/                 # lau CLI + daemon
native/android-helper/          # Gradle Kotlin APK
scripts/install-android-helper.sh
docs/lau-android-plan.md        # 本文档
```
