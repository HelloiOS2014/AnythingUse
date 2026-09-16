# LAU Phase 2 真机验收 — 2026-09-15

- 设备：Xiaomi 2211133C（``）· Android 16 · serial `<device-serial>`（USB）
- 主机：macOS · adb 1.0.41 (35.0.2) · `lau` 由 `70ac8ce` 构建（debug，含本轮 P0 修复）
- 依据：`docs/lau-android-plan.md` §8「Phase 2 验收（确定性断言，非元素计数）」
- 纪律：**全程不使用 ADB 输入注入**（无 `input tap/text`、无 `am start`）；所有动作都经 helper 的 AccessibilityService

## 结果总览

| # | 断言 | 结果 | 证据摘要 |
|---|---|---|---|
| 1 | doctor 四态 + 未启用时的引导 blocker | ✅ 两条路径均通过 | 正常：四态全绿、exit 0；**禁用后**：`enabled:false` + 精确引导 blocker + exit 3（见发现 G） |
| 2 | dump：已知 label / 能力字段 / 包与窗口身份 | 🟡 部分通过 | 包名 ✓、能力字段 ✓、已知 label ✓；**窗口身份缺失**（发现 B） |
| 3 | invoke：语义点开已知条目、无坐标、断言层级变化 | ✅ 通过 | `invoke e7` → `{"performed":"invoke"}`；代次 15→16；label 集由设置首页变为「我的设备」详情页 |
| 4 | set_value：输入 `你好LCU` 并重 dump 断言值相等 | ✅ 通过 | `{"performed":"set_value","value":"你好LCU"}`；重 dump 该 EditText `value == 你好LCU` |
| 5 | 陈旧 observationId → stale；能力未声明 → unsupported | ✅ 通过 | 见下（两条均 exit 3） |
| 6 | 锁屏/灭屏 → `device_locked`/`screen_off`，不自动唤醒 | ✅ 通过 | 关屏 → `screen_off`；亮屏锁屏 → `device_locked`；`mWakefulness` 保持 `Asleep`，未被唤醒/解锁 |
| 7 | 杀 helper → 服务自动恢复；重启手机 → doctor 全绿 | 🟡 **前半成立，但被 HyperOS 反制** | `am crash` 后 PID 17820→32256、≤2s 内 ping 恢复；**随后系统自行把无障碍服务关掉（机主确认未操作）**，必须人工重开（见发现 I）；重启未做 |

## 追加：Phase 3 验收第 3 条（getevent 真机双验）— ✅ 通过

```
$ lau run "在设置中打开深色模式" --app com.android.settings --actor agent --json
{"data":{"state":"waiting_actor","task_id":"task_1789469160358181000"},"status":"ok"}   exit=0
   ↑ 本轮新增的 ensure_watch 在真机通过：getevent 监听成功挂载（挂不上会直接拒绝建任务）

$ lau status <task-id> --json   → "watch":{"healthy":true,"dead_reason":null}
$ lau decide <task-id> --json   → {"status":"ok","obs":"obs_2","n":34}   exit=0

[机主用手指在屏幕上滑了一下]

$ lau decide <task-id> --json   → {"error":"taken_over","status":"paused"}   exit=3
   任务：state=paused, wait_reason=taken_over, last_action_summary="paused: real touch on device"
   → 真实触摸被正确识别为接管 ✅

$ lau resume <task-id> --json   → state=waiting_actor, observation_id=null   exit=0   ← resume 真机验证 ✅
$ lau decide <task-id> --json   → obs_3（旧 obs_2 已作废）
$ lau act <task-id> --observation-id obs_3 \
      --action '{"kind":"semantic","type":"invoke","element_id":"e1"}' \
      --effect '{"kind":"navigate","summary":"返回上一页"}'
  → {"last_action_summary":"invoke ok","step":1,"state":"waiting_actor"}   exit=0
$ lau decide <task-id> --json   → status ok（**不是** taken_over）
   → 注入动作不触发接管 ✅
```

旁证：`adb shell getevent -lt` 能正常打开设备事件流（`fts` 触摸设备在列），说明该 ROM 确实会吐触摸事件 ——
这证伪了我此前标注为"纸上测不出来"的那个假设风险（fail-closed 只有半截的风险），**该实现成立**。

## 逐步证据

### #1 doctor 四态

```
$ lau doctor --json
{"data":{...,"helper":{"blockers":[],"bound":true,"enabled":true,"installed":true,"ping":true},
 "target":"<device-serial>","model":"2211133C","android_version":"16"},"status":"ok"}   exit=0
```

### #2 dump

```
$ lau dump --json | jq '.data | keys'
["elements","isInteractive","keyguardLocked","observationId","packageName","screenHeight","screenWidth","windowTitle"]

$ jq '.data | {observationId, packageName, windowTitle, isInteractive, keyguardLocked}' dump.json
{"observationId":15,"packageName":"com.android.settings","windowTitle":"","isInteractive":true,"keyguardLocked":false}

$ jq '.data.elements | length'   → 30
$ jq '[.data.elements[].capabilities[]] | unique'  → ["focus","invoke","scroll"]
```

已知 label 断言成功（`搜索系统设置项`、`我的设备`、`WLAN`、`蓝牙`、`锁屏`… 均为「设置」真实条目）。

### #3 invoke（语义、无坐标）

```
$ lau dump --json          → observationId 15；首页
$ lau invoke e7 --observation-id 15 --json
{"data":{"performed":"invoke"},"status":"ok"}   exit=0
$ lau dump --json          → observationId 16，packageName 仍为 com.android.settings
   label 集变化：WLAN/蓝牙/移动网络…（首页）
              → 设备名称/<phone-name>/存储空间/108.3GB/128GB/OS版本/保修期/处理器/运行内存（详情页）
```

同包内层级变化成立（规划对同包场景要求的正是「窗口标题/层级变化」）。

### #4 set_value（中文）

```
$ lau dump --json                    → observationId 18；搜索页 EditText e8 [invoke,set_value,focus]
$ lau set-value e8 "你好LCU" --observation-id 18 --json
{"data":{"performed":"set_value","value":"你好LCU"},"status":"ok"}   exit=0
$ lau dump --json | jq '[.data.elements[]|select(.value=="你好LCU")]|length'  → 1（PASS）
```

### #5 两条拒绝路径

```
$ lau invoke e2 --observation-id 15 --json
{"error":"stale_observation: observation 15 is not current (20)","status":"error"}   exit=3

$ lau set-value e2 "x" --observation-id 15 --json
{"error":"stale_observation: observation 15 is not current (20)","status":"error"}   exit=3

$ lau invoke e8 --observation-id 15 --json      # e8 是 TextView，无 invoke 能力
{"error":"unsupported_capability: e8 has no invoke","status":"error"}                 exit=3
```

### #7 杀 helper（前半）

```
$ adb shell kill -9 17820
/system/bin/sh: kill: 17820: Operation not permitted     ← 第一次尝试失败（SELinux/UID），helper 未死，不计入
$ adb shell am crash dev.anythinguse.lau.helper
t=2s  pid=32256  {"blockers":[],"bound":true,"enabled":true,"installed":true,"ping":true}   ← 真正死亡并自动恢复
```

PID 变化证明进程确实被换掉，而非"没杀成"。

### #6 锁屏 / 灭屏

```
[机主按电源键关屏]
$ lau dump --json    → {"error":"screen_off: display is not interactive","status":"error"}   exit=3
$ lau foreground     → {"isInteractive":false,"keyguardLocked":true,"packageName":"com.android.systemui"}  exit=0
$ lau screenshot     → {"bytes":15580,"image_path":"…/lau-….png","sha256":"c35bac…"}        exit=0   ← 见发现 F
$ adb shell dumpsys power | grep mWakefulness   → mWakefulness=Asleep                        ← 未被唤醒

[机主按电源键亮屏，停在锁屏]
$ lau dump --json    → {"error":"device_locked: device is locked","status":"error"}          exit=3
$ lau foreground     → {"isInteractive":true,"keyguardLocked":true,"packageName":"com.android.systemui"}
$ adb shell dumpsys  → 锁屏仍锁定（未自动解锁）
```

## 新发现（规划未覆盖，需回写 §0）

### D. `observationId` 不持久，且没有会话身份 → 旧观察可能"复活"

- `generation` 是 **服务实例内的计数器**（`LauAccessibilityService.kt:33`），无持久化、无 boot/session 标识。
- 实测：`am crash` 后（PID 32256）代次一路涨到 22；**机主关屏再亮屏后，代次回到 2**，而 PID 仍是 32256 —— 即系统重建了 AccessibilityService 实例（`onServiceConnected` 再次执行），计数器归零。
- 含义：只要计数器爬回同一个数值，**旧观察的 id 会与新的"当前代次"相等**，`requireNode` 的代次校验就会放行；而节点是按**新 dump 的下标** `e{N}` 取的 —— 于是动作可能落在**与当初观察毫无关系的另一个元素**上。
- 触发条件很日常：关屏/亮屏、ROM 重新绑定服务、force-stop 后重开、崩溃重启（验收 7 自己就会触发）。

### E. helper 没有做规划 §5.2 要求的"复核窗口 ID/包名/bounds/能力"

```kotlin
private fun requireNode(req: JSONObject): AccessibilityNodeInfo {
    … if (obs != generation) throw stale_observation          // ① 代次
      val idx = eid.removePrefix("e")…; if (idx !in nodes.indices) throw element_not_found  // ② 下标
      val node = nodes[idx]; if (!node.refresh()) throw stale_observation                   // ③ 节点还在
      if (node.packageName == packageName) throw forbidden_package                          // ④ 自身包名
      return node
}
```

规划 §5.2 写的是「校验代次 → `refresh()` → **复核窗口 ID/包名/bounds/能力** → 任一不符 → `stale_observation`」，实际只做了 ①②③④，**没有复核窗口/包名/bounds/能力与观察时是否一致**。与发现 D 叠加后，就是上面那条"动作可能落到别的元素"的路径。

### F. 关屏时 `screenshot` 照常"成功"

关屏状态下 `lau screenshot` 仍返回 PNG（锁屏内容，15580 字节，exit 0），**没有屏幕状态校验**。Agent 若只看截图、不看 `screen_off`，会拿到一张无意义的图并据此决策。



### A. 可点元素与 label 分离
- 可 `invoke` 的元素（`LinearLayout`/`FrameLayout`/`RecyclerView`）**没有 label**；
- 带 label 的元素（`TextView`/`Button`）**大多没有 invoke 能力**（如 `e13 TextView 蓝牙 []` 对 `e12 LinearLayout (无label) [invoke,focus]`）。
- 后果：Agent 无法把「点开蓝牙」从 dump 直接映射为一个 element id，必须自行做几何包含解析（本次验收就是这么做的：按 frame 包含关系找到行）。
- mac 侧同类问题在 Runtime/Swift 解决（Finder 行 → outline 的 `kAXSelectedRowsAttribute`）；**lau 侧没有等价机制**，helper 只做 `requireNode` + `ACTION_CLICK`。

### B. 窗口身份缺失
- helper 的 `windowTitle` 取自 `root.contentDescription`（`LauAccessibilityService.kt:194`），实测恒为空串；dump 中**没有 `windowId`**。
- §5.3 要求的 时间戳 / 方向或 display ID / 截图可用性 **均未提供**；实有字段为 `isInteractive`、`keyguardLocked`、`screenWidth/Height`。
- 因此验收 #2 的「包/窗口身份」只完成包名那一半。

### C. 代次增长快 → decide/act 之间极易过期
- 一次会话内 generation 15→20→21→22：输入文字、搜索结果出现、页面切换都会 bump。
- 元素 id 随代次重排（同一输入框：`e8` → `e2`）。
- 含义：Phase 3 的 `decide` → `act` 之间只要 UI 有任何变化就会 `stale_observation`，Agent 必须"拿到即用"。

### G. doctor 的 `bound` / `ping` 判据不可靠（假阳性）

服务被禁用期间实测：

```
$ lau doctor --json   → {"helper":{"bound":true,"enabled":false,"ping":true}}   ← bound/ping 均误报为真
（约 1 分钟后）
$ lau doctor --json   → {"helper":{"bound":true,"enabled":false,"ping":false}}  ← ping 才跟上；bound 仍是 true
```

- `bound` 的判据是 `dumpsys accessibility` 输出里是否包含 `LauAccessibilityService`；禁用后 dumpsys 里仍保留
  `button:{dev.anythinguse.lau.helper/…LauAccessibilityService, …}`（无障碍快捷按钮条目）→ **恒为真的假阳性**。
- `ping` 存在滞后窗口：服务已禁用，但旧实例的 socket 尚未销毁，仍能应答，直到实例销毁才转为 false。
- 结论：**四态里只有 `enabled`（读 `settings get secure enabled_accessibility_services`）可信**；`bound`/`ping` 只能当诊断信息，不能当门禁。

### H. 服务禁用后 socket 一度可连但返回空响应

```
$ lau dump --json   → {"error":"helper returned an empty response","status":"error"}   exit=3
```

连接被接受后立即关闭、无响应；CLI 的报错对用户没有指导性（既不像 `screen_off` 那样说明原因，也不提示去开无障碍）。
`localabstract:dev.anythinguse.lau.helper` 随即消失，而 `adb forward --list` 仍留着过期映射（每次 RPC 会重建 forward，无实际影响）。

### I. 【更正验收 7】HyperOS 会自行关闭无障碍服务

时间线（机主全程只按过电源键、解锁、进设置页，未动过开关）：

1. `am crash` → PID 17820→32256，t=2s 时 `ping:true`（进程确已重启）；
2. 随后锁屏/解锁测试期间 helper 一直正常应答（能返回 `screen_off`/`device_locked`）；
3. 机主进入设置的无障碍页时，发现 **「AnythingUse LAU」已处于关闭状态**，且
   `settings get secure enabled_accessibility_services` 里已不含我们的服务（master `accessibility_enabled` 仍为 1）；
4. 机主手动重新打开 → `doctor` 四态立刻恢复全绿，`dump` 恢复正常（PID 仍是 32256，代次回到 1）。

含义：规划 Phase 2 验收第 7 条「杀 helper 进程 → 系统自动重启服务（开关在）→ ping 恢复」**只对了一半** ——
进程确实会被系统重启，但 **HyperOS 会把无障碍开关一起关掉**，需要人工重新开启才能继续工作。
这正是规划 §9 风险表里「HyperOS 杀后台/自启动关 → 服务不复活」那一条的真实命中。

**未确证**：是 `am crash` 触发的自动禁用，还是关屏/解锁过程触发的；logcat 已被覆盖，未留下证据。要定论需做一次受控复现。

### J. 【补强发现 D】实例重建不需要进程重启

重新打开无障碍服务后：

```
$ adb shell pidof dev.anythinguse.lau.helper   → 32256      ← 进程与崩溃后相同，从未变化
$ lau dump --json                              → gen = 1    ← 计数器却是全新的
```

即：**同一个进程内，服务实例被销毁重建即可把 `generation` 归零**。这比"进程重启才归零"更容易发生（关屏/重绑/开关切换都会触发），
使发现 D 的"旧 observationId 复活"从理论风险变成日常风险。

### K. 【新缺陷 · 未复现】daemon 响应被截断在 8192 字节 → decide 偶发 exit 70

真机跑 Phase 3 双验时实测到一次：

```
$ lau decide task_1789469160358181000 --json
lau: daemon response is not JSON: EOF while parsing a string at line 1 column 8192     exit=70
```

- **同一条命令立刻重试即成功**；随后 12 次 + 15 次连续 `decide` 全部成功（响应 6.5–7.4 KB）。
- 发生时机：紧跟一次成功 `act` 之后、页面切换尚未稳定时。
- `8192` = 8 KiB，与 `BufReader` / `BufferedWriter` 默认缓冲一致，但**根因未确证**：
  - helper 侧 `BufferedWriter`（默认 8 KiB）每次 `write()` 后都 `flush()`，不像它；
  - daemon 侧用 `write_all` 整体写出；CLI 侧用 `read_line`（按语义不应截断）。
- 我尝试把响应顶过 8 KiB 来复现，但设置界面各页只有 31–36 个元素（6–7 KB），**未能复现**。
- 影响：Agent 的 `decide`/`act` 循环会偶发中断（exit 70 = 内部错误）；`decide --wait` **不会**重试该错误（它是硬错误，不是"暂停"）。
- 建议（**待评审，未实施**）：传输改为显式分帧（长度前缀或读到 EOF）；或在 daemon 侧记录每次响应的字节数，便于下次复发时定位。

## 追加：#15 / #16 修复后的真机验收（2026-09-15 晚）

修复内容：helper 生成随机会话 + 观察令牌改为 `<sessionId>:<generation>`；`resolveNode` 落地 §5.2 的 7 步校验（含 bounds 容差 0.5%）。
helper APK 重新构建并安装（`scripts/install-android-helper.sh` → Success），Rust 侧 `cargo test -p lau-cli` 5/5 通过。

| 断言 | 关键操作 | 结果 |
|---|---|---|
| 10 会话隔离 | `am crash` 重建实例：PID 32256→19168、会话 `65a49aa8`→`185f2569`、**代次又是 1**；用旧令牌 `65a49aa8:1` 提交 | ✅ `stale_observation: observation belongs to session 65a49aa8, current is 185f2569`，exit 3 |
| 10 对照 | 同一元素换新令牌 `185f2569:1` | ✅ `{"performed":"invoke"}` exit 0（证明拒绝确因会话，而非其他原因） |
| 11 节点身份复核 | dump → `invoke` 改变 UI → 用同一旧令牌再对 `e2` 提交 | ✅ `stale_observation: node e2 failed refresh`，exit 3，且页面未被改动 |
| 12 能力复核 | 对只声明 `focus` 的 `e2` 调 `set_value` | ✅ `unsupported_capability: e2 did not advertise set_value`，exit 3 |
| 13 回归 | 正常 `invoke`（返回） | ✅ exit 0 |
| 13 回归 | 搜索页 `set_value` 写入 `你好LCU` | ✅ exit 0；重 dump 得到 `value == 你好LCU` |
| 附加 | 畸形令牌 `garbage` | ✅ `protocol_error: malformed observationId (want <session>:<generation>)`，exit 3 |

### 过程事故（如实记录）

- 寻找"设置搜索框"时，我按**位置**选了设置首页第一个可 `invoke` 的元素，实际打开的是**小米账号**登录页（`com.xiaomi.account`），紧接着 `set_value` 把测试文本 `你好LCU` 写进了**账号输入框**。
- **未造成提交**：`下一步`（e8）与"已阅读并同意…"（e7）**从未被 invoke**（当时查找的"清空/取消"元素不存在，两次 invoke 都没有执行）；随后 `invoke 返回` 退出，页面已无残留输入，前台回到 `com.android.settings`。
- 根因正是发现 #18（可点行没有 label，只能按位置猜）—— 这条发现由此获得一次真实事故佐证。

### #17 复现尝试（未成功）

- 本次 `am crash` 之后，无障碍服务**没有被系统关闭**（`enabled: true`，doctor 四态全绿，会话正常切换）。
- 因此 #17 的触发条件**仍未确证**：之前那次"自行关闭"既可能由 `am crash` 引起，也可能由关屏/亮屏或设置页交互引起。要定论需要更可控的复现（或 logcat 权限）。

## 追加：#18 修复后的真机验证（2026-09-15 晚）

修复：helper dump 时把子树文字归并到"能被点的那一行"（plan §3「label 归属」；深度 ≤3、≤3 段、≤200 字符；不覆盖节点自身的文字）。

```
$ lau dump --json | jq -r '.data.elements[]|select(.capabilities|index("invoke"))|"\(.id)\t\(.role)\t\(.label // "(无)")"'
e2   LinearLayout   登录小米账号 享受更多小米服务
e5   LinearLayout   我的设备
e7   LinearLayout   WLAN <office-wifi>
e10  LinearLayout   蓝牙 已开启            ← 修复前这里是 "(无 label)"
e13  LinearLayout   移动网络
e17  LinearLayout   个人热点 已关闭
...
```

功能验证（**按名字点，不用坐标几何**）：

```
$ EID=$(jq -r '[.data.elements[]|select(.label|test("蓝牙"))]|.[0].id' dump.json)   # e10
$ lau invoke e10 --observation-id 29947a87:1 --json
{"data":{"performed":"invoke"},"status":"ok"}
$ lau dump → 页面变为蓝牙设置：返回 | 蓝牙 | 设备名称 <phone-name> | 蓝牙版本 有新版本 …   ✅
```

APK 更新后的一个附带观察：安装新 APK 后 5 秒内 `ping` 曾为 false（服务尚未重新绑定），随后**自动恢复**（新 PID 21838），无需人工切换；doctor 当时给出的 blocker 文案（"toggle AnythingUse LAU off/on"）是准确的但偏保守。

## 追加：#20 / #24 修复与验证（2026-09-15 晚）

**#20 doctor 判据**（`crates/lau-cli/src/main.rs`）

真机抓到的 `dumpsys accessibility` 结构（这台 ROM）：

```
     button:{dev.anythinguse.lau.helper/…LauAccessibilityService, com.android.settings/…AccessibilityMenuService}
     Bound services:{Service[label=无障碍功能菜单, feedbackType[FEEDBACK_GENERIC], capabilities=8, …], 
                     Service[label=AnythingUse LAU, feedbackType[FEEDBACK_GENERIC], capabilities=33, …]}
     Enabled services:{…}
     Binding services:{}
```

- 关键细节：`Bound services:` **跨多行**，且服务是**按 `android:label` 列出**（不是组件名）——只读第一行、或只找组件名都会漏。
- 修复：`bound_from_dumpsys` 扫描该块（遇到 `Enabled services:` / `Binding services:` 结束），同时匹配组件名与 label；`enabled` 作为唯一门禁；`enabled:false && ping:true` 时在 `notes` 里说明"陈旧实例"。
- 验证：真机（服务已启用）`doctor` → `{installed:true, enabled:true, bound:true, ping:true, notes:[], blockers:[]}` exit 0 ✅；禁用态由**用真机原文写的单测**覆盖（`bound_comes_from_the_bound_services_block_only`，6/6 通过）。

**#24 空响应报错**（`crates/lau-cli/src/helper.rs`）

```
旧：error: helper returned an empty response
新：error: helper accepted the connection but sent no response — the AccessibilityService is
     probably disabled or restarting; run `lau doctor --json` and re-enable
     Settings → Accessibility → AnythingUse LAU
```

（连接被拒/中断的另一条路径也带上了同一句指引。）**未做实机复现**：该错误只在"服务已禁用、旧实例 socket 尚未销毁"的窗口期出现，复现需要关掉无障碍开关。

## 追加：#23 / #19 修复与验证（2026-09-15 晚）

**#23 `screenshot` 屏幕状态门禁**（`crates/lau-cli/src/main.rs`）—— 三条路径真机全过：

```
亮屏解锁：lau screenshot → {"status":"ok","data":{"bytes":183661,"isInteractive":true,"keyguardLocked":false}}   exit=0  ✅
关屏    ：lau screenshot → {"error":"screen_off: display is not interactive"}                                  exit=3  ✅
          同时 adb dumpsys power → mWakefulness=Asleep（未被唤醒）
亮屏锁屏：lau screenshot → {"error":"device_locked: device is locked"}                                        exit=3  ✅
          同时 foreground → {isInteractive:true, keyguardLocked:true}（未被自动解锁）
对照    ：同一状态下 lau dump 返回相同的 screen_off / device_locked，行为一致
```

修复前：关屏时 `screenshot` 会返回一张锁屏 PNG 并报 `ok`（exit 0）。

**#19 daemon 诊断埋点**（`crates/lau-cli/src/daemon.rs`）

```
$ rm -f ~/.local/share/AnythingUse/lau/daemon.log   # 并杀掉旧 daemon 以确保用新二进制
$ lau run … ; lau status … ; lau decide … ; lau cancel …
$ cat ~/.local/share/AnythingUse/lau/daemon.log
1789530125 op=ping   bytes=33
1789530125 op=run    bytes=82
1789530126 op=status bytes=331
1789530126 op=decide bytes=6660      ← 距出事阈值 8192 只差约 1.5 KB
1789530127 op=cancel bytes=279
```

只记录 `op + bytes`，**不含负载**；启动时若超过 64 KiB 即截断（`rotate_log`）。等 8192 截断复发时，这条日志能直接给出"出事那次响应多大"。

## 追加：Phase 3 安全模型真机验收（2026-09-15 深夜）

**验收 16（密码框 → R4 人工接管）— ✅ 通过**

```
# 手机停在「设置 → 指纹、面部与密码 → 设置锁屏密码」，页面上是密码输入框
$ lau dump --json | jq -c '.data.elements[]|select(.password==true)'
{"id":"e4","label":"6位数字密码","password":true,"role":"EditText","capabilities":["set_value","focus"]}
        ↑ helper 新增的密码证据在真机上生效

$ lau run "查看锁屏密码设置" --app com.android.settings --actor agent --json   → waiting_actor
$ lau decide <task> --json                                                     → obs=e530b0da:12

# 用**低声明**（local_edit）尝试输入，看证据层是否会把它抬到 R4
$ lau act <task> --observation-id e530b0da:12 \
    --action '{"kind":"semantic","type":"set_value","element_id":"e4","value":"123456"}' \
    --effect '{"kind":"local_edit","summary":"输入一个数字"}' --json
{"status":"waiting_user","error":"waiting_user"}                                 exit=2
   task: state=waiting_actor, wait_reason=takeover, takeover=true
        ↑ 没有被当成普通审批：走的是 R4 接管门

# 关键断言：什么都没有被输入
$ lau dump --json | jq -c '[.data.elements[]|select(.password==true)][0]'
{"id":"e4","label":"6位数字密码","value":null}                                   ← 仍为空

# 机主在 Mac 上点了 Start takeover → Done
$ lau status <task> --json   → {state:waiting_actor, wait_reason:agent_decision,
                                takeover:false, observation_id:null,
                                last_action_summary:"takeover done by the human; re-observe"}

# 不重放：拿旧令牌再提交一次
$ lau act <task> --observation-id e530b0da:12 --action '…invoke e4…'
{"error":"stale observation_id","status":"error"}                                exit=3   ✅
```

**回归（必须不受影响）**：普通导航动作（`invoke` 蓝牙行，R1）仍然直接执行成功。

**过程新发现（已记为 §0 #25）**：连续滚动时有一次被拒为
`stale_observation: node e1 moved or resized since the dump` —— 列表还在动，节点位移超过 0.5% 容差。
这是设计内的 fail-closed；操作成本是"重新 dump 再试一次"，已写入 troubleshooting。

**尚未真机验证**：验收 15（把「删除/支付」标签谎报为 navigate 仍要到达正确的门）目前只有单测覆盖；
验收 14（app access 门）尚未实现。

## 追加：验收 14（app access 门）真机结果 — 主路径通过

```
# 首次控制 com.android.settings
$ lau run "查看设置首页" --app com.android.settings --actor agent --json   → waiting_actor
$ lau decide <task> --json
{"status":"waiting_user","error":"waiting_user"}                            exit=2
   task: wait_reason=app_access, app_allowed=false
         app_key   = com.android.settings#c9009d01ebf9f5d0302bc71b2fe9aa9a47a432bba17308a3111b75d7b2149025
                     └ 包名 + 签名证书 SHA-256（helper 新增 app_identity op 从真机取得）
         app_label = 设置
        ↑ Mac 上弹出三按钮对话框（Deny / Always allow / Allow once）

# 机主点 Allow once
$ lau status <task> --json → {state:waiting_actor, wait_reason:agent_decision,
                              app_allowed:true, last_action_summary:"app access: allow_once"}
$ lau permissions --json   → []                  ← 没有落盘
app_permissions.json       → 未创建               ← allow_once 只作用于该任务
$ lau decide <task> --json → {"status":"ok","obs":"55d5806b:1","n":20}   ← 门放行

# 第二个任务（验证 allow_once 不跨任务）
$ lau run … ; lau decide <task2> --json → {"status":"waiting_user","wait_reason":"app_access"}
        ↑ 又被拦下并再次弹窗 ✅

# 机主点 Deny
$ lau status <task2> --json → {state:failed, error:"app access denied by the user"}   ✅
```

**真机发现并已修的 bug（§0 #26）**：被拒绝的任务处于 `failed`，但对它再调一次 `decide` 时，
app access 门**先把任务复活成 `waiting_actor` 并又弹了一次对话框**，而不是报终态。
根因：门（`ensure_app_access`）被放在 `decide`/`act` 的最前面，却没有先检查终态。
修复：建立门前先判终态（`succeeded`/`failed`/`cancelled` 一律不再产生新门），并加了回归单测。

**尚未真机验证**：`always_allow` 持久化 + `lau permissions --revoke` 撤销（需要再点两次对话框）。

## 追加：Phase 3 验收 1（全闭环 run→decide→act→done→succeeded）— ✅ 真机通过，目标真实达成

```
TASK=task_1789534738103122000    goal="在设置里打开蓝牙页面"    app=com.android.settings
（app access 已持久化 always_allow，本次未再弹窗 —— 验收 14 的持久化路径同时得到验证）

① decide                        → obs 55d5806b:10（页面=指纹页）
② act invoke e1 (返回)           → {"last_action_summary":"invoke ok","step":1}
   decide                       → 页面回到设置首页（通知与状态栏 / 桌面 / 显示与亮度 …）✅
③ act scroll dy=+1 / dy=-1       → scroll ok（step 2）
   其中一次被拒：stale_observation: node e1 moved or resized since the dump  ← 发现 #25 的现场
   重新观察后重试成功（step 3）
④ act invoke e10 ("蓝牙 已开启")  → {"last_action_summary":"invoke ok","step":4}
   decide                       → obs …:15，页面已是蓝牙页
                                  （返回 / 蓝牙 / 设备名称 <phone-name> / 蓝牙版本 有新版本）✅ 目标达成
⑤ act done                      → {"state":"succeeded","summary":"蓝牙设置页面已打开并经重新观察确认"}
⑥ result                        → {state:succeeded, step:4}
```

daemon.log 中 act/decide 交替的 op 序列与上述步骤一一对应。

### 顺带确认的两件事

- **持久化 app access**：新任务不再弹窗（`app_allowed:true` 来自 `<数据根>/app_permissions.json`，0600）。
- **`done` 的诚实边界（重要）**：更早一次闭环里，中间那次 invoke 因**脚本 bug 根本没发出**
  （daemon.log 无 `op=act`，两次 decide 的响应字节数完全相同、页面未变），但随后的 `done`
  仍让任务进入 `succeeded`。原因是 daemon 的完成契约只要求「显式 Done + 目标可再次观察」，
  它**不判断目标内容** —— 这与 mac 执行契约一致（诚实性由 Actor 负责）。
  含义：**`succeeded` 只证明机制，不证明目标**；上层若要知道"事办成了没"，必须自己核对重新观察到的内容。

## 追加：Phase 3 批量真机验证（2026-09-15 深夜，一轮跑完 4 条）

同一台设备、同一个任务会话内完成（app access 放行后无需再点击）：

```
① 观察：obs e89aeae6:1（蓝牙页）

② global_back（验收 2 的后半）
$ lau act <t> --observation-id e89aeae6:1 --action '{"kind":"semantic","type":"global_back"}' \
      --effect '{"kind":"navigate","summary":"系统返回"}'
{"last_action_summary":"global_back ok","step":1}
$ lau decide → obs e89aeae6:2；页面从蓝牙页退回设置首页 ✅

③ 坐标拒绝（验收 7）
$ lau act <t> --observation-id e89aeae6:2 \
      --action '{"kind":"targeted","type":"click","x":0.5,"y":0.5}' \
      --effect '{"kind":"navigate","summary":"坐标点击"}'
{"status":"error","err":"semantic_action_required: use invoke/set_value/scroll, not coordinate input"}  exit=3
   ↑ 立即拒绝、无弹窗
   ⚠ 修复前：证据层先把它判成 R3 → 弹了后果确认框（机主误点 Allow）。
     由于 D9「批准不重放」，那次点击**没有执行任何坐标动作**；且修复后 Targeted 在门之前就被拒，
     该授权永远不会被消费（5 分钟后过期）。已把"坐标一律先拒"提到门之前。

④ per-serial 队列（验收 8 的结构部分）
$ lau run 第二个任务 → {state:"queued", wait_reason:"device_queue"}
$ lau decide <t2>  → {"status":"queued","err":"task is queued behind another task on this device"}（无弹窗）
$ lau cancel <t1>  → 队列清扫：<t2> {state:"waiting_actor", last_action_summary:"promoted from the device queue"} ✅

⑤ getevent 断流 → fail closed（验收 4，故障注入）
$ pkill -f "getevent -lt"
$ lau decide <t1> → {"status":"paused","err":"watch_unavailable"}
$ lau status <t1> → {state:"paused", wait_reason:"watch_unavailable",
                     watch:{healthy:false, dead_reason:"getevent stream ended"}}   ✅
$ lau resume <t1> → {state:"waiting_actor"}；watch:{healthy:true, dead_reason:null}  ✅ 监听重建
```

**过程发现（安全设计问题，已修）**：app access 对话框原先把 `Allow once` 设为**默认按钮**，
回车/误点即授权（机主两次遇到）。已改为 `default button "Deny"`，与后果门的"默认 Deny"一致。
注意：**已运行的 daemon 仍持有旧对话框脚本**，需重启 daemon 才生效。

**本批未验**：任务闭环内的中文搜索（Phase 2 P2-4 已单独验过 CJK `set_value`）；真正的双设备并行（只有一台设备）。

## 追加：Phase 3 安全验收（indeterminate / 证据覆盖低报 / 破坏性门）

```
# 前置：任务 task_1789540146893734000（com.android.settings，app access 已放行）

① 中文搜索（分项）—— 本次**未串成任务闭环**
   设置首页当前视图的树里没有搜索框（可能已滚出可视区），未强凑。
   分项均已单独验证：CJK `set_value` 见 Phase 2 的 P2-4；`global_back` 见本文件上一节。

② indeterminate（§4 不确定结果不重放）—— ✅
$ adb shell am crash dev.anythinguse.lau.helper      # 动作途中让 helper 消失
$ lau act <t> --observation-id a14a123c:5 --action '{"kind":"semantic","type":"invoke","element_id":"e5"}' …
{"error":"indeterminate: the action may or may not have been applied — re-observe with `decide`
          before acting again, and do not resend it (helper accepted the connection but sent no
          response — the AccessibilityService is probably disabled or restarting; …)"}   exit=3
   task: {state:waiting_actor, wait_reason:agent_decision, indeterminate:true, observation_id:null}
        ↑ 不自动重试；观察被作废强制重观察；结果未知这一点被显式标记
   恢复：helper 2 秒后自动重启；`decide` 立刻可用（新会话 a14a123c）

③ 证据覆盖低报（验收 15/6）—— ✅ 最关键的一条
   路径：设置 → 应用设置 → 应用列表 → 百度地图详情页 → e32「卸载」
$ lau act <t> --observation-id a14a123c:4 \
      --action '{"kind":"semantic","type":"invoke","element_id":"e32"}' \
      --effect '{"kind":"navigate","summary":"查看这个项目"}'      # 故意低报
{"status":"waiting_user","wait_reason":"consequence"}              exit=2
        ↑ 谎报 navigate 仍到达 R3 后果门，没有被静默执行
   机主点 Deny → task {state:failed, error:"user denied on Mac dialog"}
$ adb shell pm path com.baidu.BaiduMap
package:/data/app/~~59m0w30BrUf7k1EHFoNscw==/com.baidu.BaiduMap-…/base.apk   ← 应用还在，没有被卸载 ✅

④ 过程记录：daemon 传输异常第二次出现
$ lau permissions --json
lau: daemon response is not JSON: EOF while parsing a value at line 1 column 0     ← 0 字节空响应
   重试即成功；daemon.log 显示 `op=permissions_list bytes=38`（该次调用其实成功）。
   与 §0 #19 的 8192 字节截断同族，但这次是 0 字节 → **削弱了"固定 8 KiB 边界"的假设**，
   根因仍未确证，继续靠 daemon.log 的字节数埋点等待现场。
```

**顺带的产品化改进**：被门拦下时，任务的 `last_action_summary` 现在会记录原因
（`gated (R3): …` / `takeover (R4): …`），`lau status --json` 即可审计"为什么被拦"。

## 追加：#22 窗口身份 + §5.3 字段（P2-2 补齐）— ✅ 真机通过

修复：helper 的 dump 改用 `AccessibilityWindowInfo.title` 与 `root.windowId`（原来取 `root.contentDescription`，
本平台恒空），并补齐 `capturedAtMs` / `rotation` / `displayId`；daemon 的 `decide` 增加 `screenshot: bool`。

```
$ lau dump --json | jq -c '{packageName,windowId,windowTitle,capturedAtMs,rotation,displayId,isInteractive,keyguardLocked}'
{"packageName":"com.miui.securitycenter","windowId":9381,"windowTitle":"应用信息",
 "capturedAtMs":1789540498654,"rotation":0,"displayId":0,
 "isInteractive":true,"keyguardLocked":false}
```

至此 Phase 2 的 P2-2（「已知 label / 能力字段 / 包与窗口身份」）全部通过，§5.3 要求的字段清单
（`isInteractive`、Keyguard 态、前台包名/窗口、时间戳、方向/display、截图可用性）齐全。

## 未完成项（需机主配合）

1. #17 的复现/定论：需要再来一次受控复现（关屏亮屏各一次 + `am crash` 各一次，分别观察 `enabled`）。
2. #7 后半：`adb reboot` 后 doctor 全绿（重启会中断手机使用，需明确同意）。
