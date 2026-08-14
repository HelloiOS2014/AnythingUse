# AnythingUse Computer Use 纠偏契约与迁移计划

状态：**目标契约，Gate 0 已冻结；源码迁移完成，待 Gate 1 集中真机验收**

日期：2026-08-13
适用范围：macOS 先行；Windows 在 macOS 产品闭环后再适配

本文是本轮纠偏的唯一设计依据。源码实现状态以 `docs/status.md` 为准；企微、
TextEdit 或 Chrome 都只能作为通用能力的验收路径，不能进入产品策略。

## 1. 结论

AnythingUse 保留现有产品边界：

- 人类与 Agent 共用公开 `lcu` 命令面；Agent 通过 Skill 使用；
- 外部 Agent 与本地 VLM 是按任务选择的两条决策线；
- 两条决策线共用一个 Runtime、串行队列、观察/动作契约、安全策略和执行层；
- macOS 原生应用和真实 Chrome 是当前两个执行 surface；
- 不增加 MCP 或 Playwright 控制面。

必须推翻的是当前审批模型，而不是上述产品边界：

- **不能按输入原语审批。** 普通坐标点击、按键、搜索、打开、选择不是天然高风险；
- **审批真实后果。** 发送、提交、删除、上传、权限变更、支付、凭据操作才进入确认或接管；
- **审批后绝不重放旧动作。** Runtime 丢弃旧 proposal，重新观察，再让 Actor 决定下一步；
- **Runtime 不恢复业务流程。** 它不认识搜索框、联系人、企微或任何应用业务；
- **应用授权与后果授权分离。** 允许控制一个应用，不等于允许代表用户发送、删除或付款。

当前问题不是审批窗口是否抢焦点。任何审批 UI、用户操作或目标应用自身都可能改变
界面；产品循环必须天然接受这种变化。

## 2. 外部参考形成的约束

这里只提取共同产品约束，不复制任何产品的传输、依赖或后端。

| 参考 | 可直接采用的约束 | 不采用的部分 |
|---|---|---|
| [Codex / ChatGPT Computer Use](https://learn.chatgpt.com/docs/computer-use) | 应用访问单独授权，可记为 Always allow；普通任务持续执行；敏感或破坏性动作另行询问；macOS 可后台或跨应用 | 其插件/MCP 形态、内部实现和私有协议 |
| [OpenAI Computer Use API](https://developers.openai.com/api/docs/guides/tools-computer-use) | 安全步骤尽量先做，只在下一步产生真实外部风险时确认；确认发送、删除、权限、金融等后果 | API 传输和云端模型绑定 |
| [Gemini Computer Use](https://ai.google.dev/gemini-api/docs/computer-use) | Actor 输出动作意图和 allowed / require_confirmation / blocked 安全判断；用户确认后继续循环并取得最新屏幕 | Playwright 示例和 Gemini API 依赖 |
| [Anthropic Computer Use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool) | 客户端执行截图/动作循环；对有现实后果和明确同意要求的动作做人类确认 | 容器参考实现和 Anthropic API 绑定 |
| [OpenClaw Computer Use](https://docs.openclaw.ai/nodes/computer-use) | 一次一个动作；坐标绑定最近截图 frame；动作后返回新截图；能力启用与逐动作确认是不同层 | Gateway、Node、CUA Driver 和其无逐动作审批策略 |
| [UI-TARS Desktop SDK](https://github.com/bytedance/UI-TARS-desktop/blob/main/docs/sdk.md) | model/operator 分离、循环上限、可取消 | SDK、模型和 Operator 实现 |
| [Peekaboo](https://github.com/openclaw/Peekaboo) | macOS 截图、Accessibility 与原生输入是可组合的底层能力 | CLI/MCP 产品面和直接依赖 |

这些参考的共同循环只有六步：

1. 观察真实目标；
2. Actor 基于当前观察提出一个动作及其预期后果；
3. Runtime 校验目标、观察绑定和安全策略；
4. 若后果需要确认，先停在风险点；否则执行；
5. 执行、确认、用户输入或界面变化之后都重新观察；
6. Actor 基于新观察继续，不复用旧像素、旧元素或旧业务假设。

## 3. 业务契约

### 3.1 使用者与职责

| 角色 | 负责 | 不负责 |
|---|---|---|
| 人类用户 | 下达目标、授予应用访问、确认后果、暂停/取消/接管 | 给 Agent 传私有 socket 或维护执行细节 |
| 外部 Agent | 阅读观察、规划、输出一个动作和后果分类、判断完成 | 自批高风险动作、绕过 Runtime |
| 本地 VLM | 与外部 Agent 使用同一 proposal 契约 | 使用另一套风险或执行路径 |
| Runtime | 队列、任务状态、授权、校验、执行调度、审计、清理 | 搜索联系人、恢复菜单、理解某个应用业务 |
| Platform Operator | 精确截图、语义树、目标绑定、输入投递、能力错误 | 规划任务或决定是否批准后果 |

### 3.2 应用访问与前台能力

第一次控制目标应用前，Runtime 请求应用访问决定：

- `allow_once`：只对当前任务有效；
- `always_allow`：本地持久保存，可在设置中撤销；
- `deny`：当前任务不能控制该应用。

授权绑定稳定的应用身份，而不是显示名称。macOS 使用 bundle ID、签名 Team ID 和必要的
路径/代码身份校验；身份随任务观察保存，持久决定存入本地 SQLite，并由桌面设置提供撤销。
代码身份变化使旧决定失效。切换到新应用必须重新检查该应用的访问决定。

应用访问只允许 Runtime 观察和进行可证明的后台普通交互，**不永久授权抢占前台**。
若某应用不能后台操作，Runtime 另行请求当前任务、当前应用的一次 `ForegroundGrant`；
它只授权把精确目标窗口带到前台，不授权任何业务后果，也不能持久化为 Always allow。
实现不维护应用名单或 bundle ID 特判。

支持三个通用控制模式：

- `auto`（默认）：后台优先，能力不足时请求一次任务级前台授权；
- `background_only`：无法后台控制就返回明确错误；
- `foreground`：任务开始时请求一次任务级前台授权。

模式是用户/任务选择，不是应用硬编码。前台授权完成后先激活并重新观察，绝不重试激活前
的动作。用户在同一目标上产生真实 HID 输入时，任务始终 `taken_over` 并释放前台能力；
ForegroundGrant 不能吞掉用户接管。用户在别处工作时，后台能力继续运行。

Chrome 的持久访问主体是已连接的扩展实例和浏览器 profile，不是 bundle ID 本身；单个
任务仍绑定 Runtime 创建的 tab lease 和当前 origin，后果确认还必须绑定目的 origin。

### 3.3 动作后果与确认

Actor 的每个可执行 proposal 必须声明一个闭集后果：

| `effect.kind` | 示例 | 默认策略 |
|---|---|---|
| `observe` | 截图、等待 | 直接执行 |
| `navigate` | 打开、搜索、选择、滚动、切页 | 直接执行 |
| `local_edit` | 在本地草稿/文档输入非敏感内容 | 直接执行 |
| `external_communication` | 发送消息、邮件、发布内容 | 动作发生前确认 |
| `external_submit` | 提交表单、创建记录、上传文件 | 动作发生前确认 |
| `destructive` | 删除、覆盖、清空、撤销访问 | 动作发生前确认 |
| `permission_change` | 修改共享、账户、系统权限 | 人工接管 |
| `financial` | 购买、支付、转账 | 人工接管 |
| `credential` | 密码、验证码、密钥、安全验证 | 人工接管 |
| `unknown` | Actor 无法判断真实后果 | 停止并请求用户 |

坐标、元素、鼠标、键盘只是动作表达，不决定风险。`effect` 是所选决策 Actor 的结构化
安全分类，**不是用户授权**；用户目标、应用访问和后果 grant 才是授权。与 Gemini 等
截图型 Computer Use 一样，screenshot-only 场景允许安全 Actor 把普通点击分类为
`navigate`，否则产品无法操作没有 Accessibility 的应用。

这是明确的信任边界：用户选择的 decision Actor 是截图语义与安全分类组件，Runtime 不
虚构自己能独立理解所有像素。Actor 无法可靠判断时必须返回 `unknown`；不信任该 Actor 就
不能授权它执行 Computer Use。

Runtime 必须使用新观察计算独立风险下限：语义标签中的 send/delete/pay/submit，输入中的
密码、验证码、密钥和卡号，URL 的敏感参数与 origin，以及目标权限不匹配都必须升高或
阻止；Actor 声明不能降低这些证据。Runtime 没有证据反驳 screenshot-only proposal 时，
使用所选 Actor 的闭集分类；Actor 返回 `unknown`、缺失或非法分类时停止。用户原始目标和
屏幕上的第三方文字不能充当授权；用户目标只授权任务范围，不能证明某个像素的真实后果。

外部 Agent 和本地 VLM 都必须输出同一 `effect`。缺失或非法值在信任边界拒绝，不能落到
一套隐藏的默认业务策略。

### 3.4 确认授权

后果确认生成一次性 `ConsequenceGrant`，绑定：

- `task_id`；
- 稳定目标应用身份；
- `effect.kind`；
- Runtime 从实际执行描述符和观察提取的操作、对象、目的地、内容、数量、账户/origin 与
  稳定目标摘要；任何会改变用户确认判断的字段变化都必须改变摘要哈希；
- 向用户展示的后果摘要（仅展示和审计，不参与匹配）；
- 过期时间和一次性 nonce。

它不授权动作序列，也不授权为完成该后果而产生的其他高风险后果。确认请求可保留原
`observation_id + image_hash + action_hash` 作为“画面完全未变”的精确匹配证据，但 Runtime
不保存一条可在批准后直接重放的 Action。

应用授权、后果确认或前台激活后的统一行为：

1. 记录 transition result；若是后果确认，将对应 grant 标记为已批准；
2. 删除等待时保存的旧 proposal；
3. 对原目标做新观察；
4. 任务进入统一的 `waiting_actor`，把 `transition_result + fresh observation` 交给同一 actor
   kind；应用授权通过、后果确认通过和前台激活完成都走这一入口，不得为 VLM 另开续跑
   路径；continuation 绑定 `task_id + caller identity + transition observation_id`，不是只绑定
   actor kind；本地 VLM 自动继续，外部 Agent 通过下一次 `lcu decide` 重新连接并取得它；
5. Actor 自行决定继续、重建界面、安全失败或完成；
6. 新 proposal 重新经过独立风险下限；Runtime 提取的 `kind + operation + object +
   destination/content digest` 与 grant 一致时消费一次；确认 UI 必须展示这组 Runtime 匹配
   身份，Actor 的 `summary` 只能补充说明；两者冲突时阻断确认；
7. screenshot-only 动作只有在新 proposal 与原 `image_hash + action_hash` 精确一致时才能
   消费；同一截图上指向同一语义元素，或同类输入仍落在原坐标的保守命中范围内，都属于
   原高风险候选：Actor 即使改变坐标或降级 `effect.kind`，也必须按原 grant 风险重新确认或
   接管，不得走普通 allowed。只有明确落在该候选范围外的真正不同 proposal 才先作废旧
   grant、再独立判定，绝不保留旧 grant 等待以后消费；画面变化且 Runtime 无法提取后果
   身份时必须重新确认或请求接管，不能猜测；
8. 外部 Agent 在 actor reconnect TTL 内未回来，任务暂停并通知用户；任何未消费 grant 在
   自身过期、任务暂停、目标变化、Runtime 重启、系统会话失效或取消时作废；已消费 grant
   不退还，恢复只能重新走对应门槛。

Runtime 不自动重新搜索、不重新打开菜单、不重输文本，也不自动点击“相似位置”。

### 3.5 完成

只有 Actor 在新观察上显式返回 `done`，且 Runtime 能再次确认目标仍存在，任务才进入
`succeeded`。动作执行成功、审批通过、找到一个同名文本都不是任务完成。

## 4. 技术契约

### 4.1 组件边界

```text
Human CLI / Agent Skill
          |
          v
Runtime: queue + task + app permission + consequence policy + audit + cleanup
          |
          +------ decision actor: external Agent | local VLM
          |
          +------ operator: macOS application | real Chrome
```

Runtime 是唯一状态所有者。Actor 只决定下一步；Operator 只执行当前动作。任何一层都不得
通过 bundle ID、进程名、窗口标题、联系人名、目标文本或验收字符串选择执行策略。
Chrome surface 必须由显式 target/surface 解析，不能从 goal 中搜索 “Chrome/浏览器”关键词。

### 4.2 观察

继续复用当前 `AppObservation` 基础字段：

- `observation_id`、时间戳；
- app / PID / window ID；
- 窗口 frame、模型图尺寸、scale、`transform_id`、`image_hash`；
- screenshot；
- 可选 semantic elements；
- 上一动作结果、授权结果和 `ui_state`。

`elements=[]` 仅表示走截图决策，不是失败，也不是自动审批理由。每个新观察使旧元素 ID 和
旧坐标 proposal 失效。

### 4.3 Proposal

两条 Actor 线共用最小 proposal：

```json
{
  "observation_id": "obs_...",
  "action": {"kind":"targeted","type":"click","x":0.25,"y":0.20,"button":"left"},
  "effect": {
    "kind": "navigate",
    "summary": "打开当前搜索结果"
  }
}
```

`effect.summary` 用于向用户解释和审计，不能包含凭据或完整敏感文本。Runtime 对动作和
后果分别校验；后果不是任意字符串，CLI/VLM 都使用同一枚举。

### 4.4 单步循环

```text
resolve stable target identity
  -> app access gate
  -> observe
  -> actor.propose(current observation)
  -> validate target + observation + action + effect
  -> independent risk floor
  -> consequence gate
      -> allowed: execute one action
      -> confirmation: park without retaining executable Action
      -> handoff/blocked: stop
  -> settle (bounded)
  -> fresh observe
  -> actor.propose(...)
```

坐标动作执行前仍校验 target、frame 和 transform。批准后只能执行 Actor 在新观察上重新
提交、且满足 §3.4 grant 匹配的动作；Runtime 不自行调用旧 pending action。前台激活也被
视为界面变化：激活完成后重新观察，再交 Actor，不能在 `foreground_required` 后重试原动作。
`waiting_actor` 的续跑 proposal 必须从上图的 validate 重新进入，没有直通执行；caller 或
observation 不匹配就拒绝，目标/前台/观察已变化就重新观察，需要再次激活则重新申请一次性
ForegroundGrant。非审批路径保持“一次观察、一个动作、一次新观察”。

### 4.5 前台与后台

Operator 按能力选择后台语义、后台定向或前台输入。它只能报告通用能力结果：

- `executed`；
- `foreground_required`；
- `stale_observation`；
- `taken_over`；
- `target_lost`；
- `unsupported_capability`；
- `permission_denied`。

Runtime 根据任务控制模式处理结果。Operator 不弹审批、不改变策略、不恢复用户先前应用。
物理指针不移动；无法证明目标就失败。Chrome 继续通过现有扩展和 Native Messaging 控制
真实 Chrome，不从 Agent 侧直连 DOM/CDP。

前台激活后、输入前必须再次证明精确窗口：优先使用已证明的 AX window raise/key-window
identity；无 AX 时只接受 CGWindowID 与唯一 topmost same-PID window 一致，多个候选或无法
证明就失败。仅证明 PID frontmost 不足以输入。

### 4.6 队列与用户接管

- UI 执行队列保持串行，避免大量任务同时操作电脑；
- 等待应用授权、后果确认或用户输入的任务释放执行槽；
- `waiting_actor` 同样释放全局执行槽和 native foreground session，但保留不可执行的目标
  reservation；已用于激活的一次性 ForegroundGrant 同时消费，不得借等待状态钉住前台；
  其它目标可继续，同一目标不可并行。TTL、pause、taken_over、取消或重启时释放
  reservation、未消费 ForegroundGrant 和 ConsequenceGrant；应用访问撤销、grant 过期、
  系统休眠/登出同样失效任务级 reservation/grant，但不删除仍有效的持久 AppPermission；
- 同一目标同时只有一个控制 owner；
- Agent 产生的输入带内部 owner 标记；任何非 owner 的真实用户 HID 在目标上都触发
  `taken_over`，包括前台授权期间；
- `resume` 必须从新观察继续；
- pause、taken_over、取消、失败、完成都清除 pending observation、未消费 grant、控制
  session/current target、native foreground slot 和 Chrome tab/debugger lease。

## 5. 当前污染清单

以下是必须删除或改义的错误设计，不允许继续兼容两套模型：

| 污染 | 当前位置 | 迁移结果 |
|---|---|---|
| 所有坐标 Click / KeyCombo 固定 R3 | `crates/lcu-core/src/effect_guard.rs` | 删除输入原语地板；按 Actor effect + Runtime 强制风险下限判定 |
| `intent` / `effect_claim` / `expected_effect` 多套可选字段 | core/model/CLI/Qwen prompt | 只保留闭集 `effect`，两条 Actor 共用一个 parser |
| `Action::Exclusive` 让 Actor 选择抢前台且固定 R3 | core/model/Runtime/Qwen/Skill | 从 Actor 动作词汇删除；前台只由 task control mode + ForegroundGrant 决定 |
| 审批绑定并重放 `observation_id + action_hash` | `crates/lcu-core/src/approval.rs` | 拆成 AppPermission、ForegroundGrant、ConsequenceGrant；旧 hash 仅作精确画面匹配证据 |
| PendingAction 保存旧动作并在批准后重放 | `crates/lcu-runtime/src/worker.rs` | pending 只保存确认请求；批准后 fresh observe 回 Actor |
| foreground session 与具体失败动作/动作审批耦合 | `crates/lcu-runtime/src/lib.rs`, `worker.rs` | 前台能力由 task control mode + task-scoped ForegroundGrant 决定 |
| Runtime 试图在批准后验证并继续旧坐标 | Runtime worker | 删除；旧 proposal 在任何确认路径失效 |
| `foreground_required` 激活后重试原动作 | Runtime worker | 激活、fresh observe、回 Actor，永不重试原动作 |
| session 吞掉用户接管 | native `Takeover` / `ForegroundSession` | 用户 HID 始终 taken_over；session 只标记 Agent activation owner |
| Chrome 通过 goal 中关键词选择 surface | `crates/lcu-chrome/src/lib.rs` | 只接受显式 target/surface，不解析任务文字路由 |
| `osascript` 焦点变化被当作根因 | `apps/lcu-desktop` | 审批 UI 可保留；循环必须容忍任何焦点/界面变化 |
| 文档宣称坐标天然 R3 | README、Skill、architecture、contract、reference、guide、status、troubleshooting | 统一引用本文，不保留旧政策措辞 |
| 验收围绕企微流程推动实现 | 报告/临时说明 | 企微仅保留为一个真实应用验收，不进入产品分支 |

现有严格 target、观察绑定、双 Actor、队列、任务状态机、后台能力、Chrome surface、日志
脱敏和清理基础可复用；不要以“重写”为名复制它们。

## 6. 实施任务

只设一个架构门和一个最终验收门，不按小修复反复验收。

### Gate 0：契约冻结

- 本文由 Codex 编写，Claude 与 Grok 独立对抗审查；
- 只处理 P0/P1：是否仍有应用特判、输入原语审批、旧动作重放、双 Actor 分叉、越权授权；
- 两位监工结论收口后再开始代码迁移。

### Wave 1：共享契约

负责人可一次提交：

- 新增闭集 `EffectKind` / `EffectClaim` 和唯一 `ProposedAction` parser；
- Agent 与 VLM proposal/context 使用同一 effect 与 confirmation result；
- 删除 `--intent`、`effect_claim`、`expected_effect`、`model_claimed_risk` 别名；
- 把 approval 拆为 AppPermission、ForegroundGrant 与 ConsequenceGrant；
- 删除坐标 Click/KeyCombo/Exclusive 固定 R3 和旧 pending replay 契约；
- 固化 Runtime 必须执行的语义、文本、URL、origin 和权限风险下限。

最小检查：core/model/contract 定向测试。

### Wave 2：Runtime 与 UI（可在 Wave 1 合入后并行）

Runtime：

- pending confirmation 不保存可执行旧 action；
- 应用授权、后果确认和前台激活后统一进入 `waiting_actor` 并 fresh observe；VLM 自动继续，
  Agent 通过同一 transition result 重连；等待期间释放执行槽但保留目标 reservation；
- grant 按 §3.4 的 Runtime 后果身份或精确画面证据消费；
- `foreground_required` 按 control mode 请求 ForegroundGrant，激活后回 Actor；
- 所有终态统一释放 observation、grant、target 和临时文件。

Desktop UI：

- 应用访问支持 allow once / always allow / deny；
- 前台授权独立、按 task/app 一次，不能持久化；
- 后果确认展示 task、app、effect、对象/目的地摘要；
- UI 用何种原生窗口不是 Runtime 正确性的前提；
- CLI 和 Agent 仍不能自批。

Actor：

- `lcu decide/act` 与 Skill 暴露唯一 `ProposedAction`；
- Qwen 与外部 Agent 使用同一枚举、parser、confirmation result 和等待状态；
- 外部 Agent 超时只暂停，不触发旧动作兜底；
- 删除任何应用名、联系人名或验收字符串提示。

最小检查：每个分支各一组定向测试；不跑 Top100 和长稳。

### Wave 3：文档、清理与一次集成

- 一次性更新 README、Skill、architecture、command-contract、reference、guide、status、
  troubleshooting；
- 删除被新契约替代的旧类型、旧测试和重复说明；
- 不保留兼容开关或第二套审批路径；
- 统一构建后只做 Gate 1 验收。

## 7. Gate 1 最小验收

只有以下全部通过才可称本轮完成：

1. **通用普通任务**：在一个 AX-rich macOS 应用完成打开/搜索/选择/输入；除应用访问外，
   普通点击和按键零逐动作审批。
2. **截图型普通任务**：在一个真实 AX-empty/screenshot-only 应用查找并打开指定对象，
   重新观察确认身份后 Done，不产生外部后果；实验室可选企微作为证据，但契约、代码、
   prompt 和 fixture 不出现应用、联系人或 bundle ID 特判。
3. **确认期间界面变化**：使用已有本地页面或可撤销场景制造瞬态 UI；确认导致画面变化后
   旧坐标绝不执行，Actor 从新观察继续；只有 §3.4 的 Runtime 后果身份/精确画面匹配才可
   消费原 grant，否则明确重确认或接管。
4. **授权边界**：应用访问 deny；always allow 的第二个任务不重复应用提示但外部后果仍
   确认；`background_only` 遇到 foreground_required 不激活；任务级前台授权期间用户输入
   必须 taken_over，resume 从新观察继续。
5. **后果与双 Actor**：在同一无真实副作用的发送/删除边界 fixture 上分别跑 external
   Agent 与 local VLM，断言同一 effect、风险下限、确认/接管停留点和审批次数；再验证
   Actor 把有语义证据的 Send 错报为 navigate 时 Runtime 仍升高风险。
6. **Chrome**：通过显式 Chrome target 跑一个普通真实标签页任务；不从 goal 关键词路由，
   target 绑定 profile + tab lease + origin。
7. **资源回收**：一个表驱动定向检查覆盖 success/fail/cancel/pause/taken_over，断言没有
   pending observation、grant/approval、current target、native session、Chrome lease 和
   Agent/VLM 临时文件；另抽查一个真实终态任务。

证据只保留：命令、task ID、最终状态、审批次数、关键截图路径/哈希和清理结果。不做
30/60/120 分钟长稳、不做 Top100、不新增大批 fixture。

## 8. 存储与垃圾回收

不增加清理 daemon。复用任务终态和 Runtime 启动两个时机：

- 每个任务只保留当前待决观察；新观察替换时删除旧 screenshot；
- success/fail/cancel/pause/taken_over 立即删除或失效任务临时目录、未消费 grant、owner 和
  control session；
- Runtime 启动时只清理能证明无活跃 owner、且超过 1 小时的孤儿临时目录/socket；
- 日志最多 5 个文件、每个 5 MiB；
- SQLite 保留最近 30 天或 1,000 个终态任务中的较小集合，不删除活跃任务；
- 外部 Agent 模式不加载/复制本地 VLM；模型目录不复制进 task、release 或 worktree；
- 安装/升级 staging 在成功、失败或取消时统一删除。

本轮用伪时间/伪终态记录做一个确定性的清理检查，覆盖日志轮转、SQLite 上限和 staging；
不等待 30 天，不增加长稳测试。

## 9. 明确不做

- 不实现业务级搜索恢复器、联系人定位器或企微 adapter；
- 不为每个应用配置焦点、等待或坐标规则；
- 不新增 MCP、Playwright、DOM/CDP Agent 控制面；
- 不引入多租户、远程权限平台、工作流编排器或新的后台服务；
- 不在本轮做 Windows、签名发布、SBOM、Top100 或长时稳定性；
- 不用更多测试掩盖契约未冻结的问题。

## 10. 交付纪律

- 开发只能以本文为目标契约；`docs/computer-use-reference.md` 只保存来源事实；
- 每个实现 PR 必须列出删除的旧规则，不能只列新增代码；
- 发现与本文冲突时先停在契约层，不得为一个验收应用加特判；
- Claude 与 Grok 作为独立监工，只读审查设计和最终 diff，不直接替 AnythingUse 决策；
- Codex 汇总监工意见并向产品负责人报告，不能把“有代码/测试通过”写成产品验收通过。
