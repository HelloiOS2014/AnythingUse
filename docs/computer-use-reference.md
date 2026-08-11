# Computer Use Reference Notes

Reviewed: 2026-08-11

This document separates external product facts from AnythingUse product
decisions. A referenced capability does not become a requirement, dependency,
or backend choice.

It is a reference and source-evidence review, not a product-readiness or
stability certification.

## Evidence labels

- **Official**: stated in the linked product documentation.
- **Installed contract**: observed in the versioned Codex Computer Use Skill on
  the development machine; not a public implementation guarantee.
- **Implemented**: present in the AnythingUse source tree.
- **Runtime-verified**: demonstrated on the target machine without relying on a
  source-code claim.

Only runtime evidence proves user-visible behavior. Source comments and tests
establish implementation intent, not real-app acceptance.

## AnythingUse decisions

These decisions came from the product owner, not from the products reviewed
below:

- humans and Agents share the public `lcu` CLI; Agents use the Skill;
- external Agent and local VLM decision paths both remain, selected per task;
- both decision paths share one Runtime, queue, action/observation contract,
  safety pipeline, and execution layer;
- desktop execution is serial;
- macOS work must not steal the user's frontmost app, window, pointer, or
  keyboard;
- Chrome uses the existing extension and Native Messaging path against the
  user's real Chrome profile;
- execution backends may change behind the public command contract when the
  existing implementation cannot deliver the required product behavior;
- MCP and Playwright are not AnythingUse control surfaces.

## Product-by-product facts

### Codex / ChatGPT Computer Use

**Layer:** end-user Computer Use product.

**Official:** [OpenAI Computer Use documentation](https://learn.chatgpt.com/docs/computer-use)
states that:

- macOS can run a scoped task in the background while the user works elsewhere;
- one workflow may span multiple applications;
- app access is permissioned and can be saved in an always-allowed list;
- sensitive or disruptive actions may require another permission prompt;
- users can stop or take over a task;
- Chrome can be connected through a browser extension;
- macOS supports an explicitly enabled, narrowly scoped locked-use mode;
- Windows Computer Use is foreground-only.

The same documentation recommends using a different browser if the user wants
to keep browsing while Computer Use operates. It does not promise concurrent,
non-interfering work in the same Chrome instance.

**Installed contract:** Computer Use Skill `1.0.1000633` on the development
machine exposes `list_apps`, app-scoped screenshot plus Accessibility text,
element or coordinate click, drag, secondary Accessibility actions, key input,
scroll, text selection, set-value, and typing. Its workflow prefers semantic
elements, refreshes app state after actions, and tells the Agent not to reuse
stale element indexes. This is useful evidence about the currently installed
tool surface, not a public promise about Codex internals.

### OpenClaw Computer Use

**Layer:** Agent runtime and Computer Use tool, not one end-user desktop
product.

**Official:** [OpenClaw Computer Use documentation](https://docs.openclaw.ai/nodes/computer-use)
describes one action per call, screenshot/frame binding for coordinate input,
fresh screenshots after input, endpoint capability advertisement, and a common
pointer/keyboard action set.

Its macOS path carries display identity and fails closed on relevant geometry
changes. The Windows/Linux CUA path is explicitly experimental, primary-display
only, and lacks equally stable display identity. The built-in tool also does
not provide a lease or per-action confirmation system. Pointer/keyboard control
therefore does not establish AnythingUse's non-interference requirement.

OpenClaw's separate [browser tool](https://docs.openclaw.ai/tools/browser) has
managed-profile and tab-lifecycle features. Those browser-tool features are not
properties of `computer.act`.

### Anthropic Computer Use

**Layer and status:** model tool contract, Beta.

**Official:** [Anthropic Computer Use documentation](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)
describes a client-implemented screenshot/action loop, a standard pointer and
keyboard action set, iteration limits in the reference loop, and sandbox plus
human-oversight guidance. It recommends checking screenshots after actions and
discusses prompt-injection risk. It does not provide AnythingUse's native
macOS background-execution contract.

### Gemini Computer Use

**Layer and status:** model tool contract, Preview.

**Official:** [Gemini Computer Use documentation](https://ai.google.dev/gemini-api/docs/computer-use)
describes screenshot-to-action function calls. Gemini 3.x responses include
action intent and may include an `allowed` / confirmation-required / blocked
safety decision. The client owns execution and returns the next screenshot.
This is a useful safety-contract reference, not an AnythingUse implementation
choice.

### UI-TARS Desktop SDK

**Layer and status:** GUI-agent SDK with pluggable model and operator,
Experimental.

**Official:** [UI-TARS SDK documentation](https://github.com/bytedance/UI-TARS-desktop/blob/main/docs/sdk.md)
describes a screenshot/model/operator loop, `AbortSignal`, and
`maxLoopCount`. It does not establish a common approval or prompt-injection
policy with Anthropic or Gemini.

### Peekaboo and CUA

**Layer:** lower-level execution tools, included only to understand available
desktop-control capabilities.

[Peekaboo](https://github.com/openclaw/Peekaboo) documents screenshot plus
Accessibility inspection, opaque element identifiers, targeted input, and
window/menu/dialog operations on macOS. Some applications still require its
foreground fallback.

[CUA](https://github.com/trycua/cua) claims background native-app control on
macOS, Windows, and Linux, with documented platform limitations. That is a
vendor claim, not AnythingUse runtime evidence. Neither project is an
AnythingUse dependency or backend candidate under this review.

## What the mature products have in common

The references use different transports and platform implementations, but the
stable product loop is the same:

1. **Observe the real target.** A screenshot is always usable; Accessibility or
   DOM-like structure is optional acceleration, not a prerequisite.
2. **Bind an action to that observation.** Coordinates refer to the captured
   target frame, and stale observations are discarded.
3. **Execute one target-scoped action.** Semantic and screenshot-coordinate
   actions are both normal operator capabilities.
4. **Wait for the UI to settle, then observe again.** The next decision never
   assumes that old element indexes or pixels are still valid.
5. **Confirm consequences, not input primitives.** Reading, searching, opening,
   and selecting normally proceed; sending, deleting, paying, changing security
   state, or entering credentials receive the appropriate confirmation or
   handoff.
6. **Keep decision and execution separate.** The model/Agent chooses the next
   action; the operator owns targeting, freshness, input delivery, and failure.

The transport is not the reason these products work. AnythingUse keeps its
public CLI and Agent Skill and does not adopt another product's MCP, browser,
runtime, or dependency graph.

## Target AnythingUse execution contract

This section is the planned contract. It is not a claim that the current source
already implements it.

The public wire version becomes `1.1.0`. Existing `1.0.0` semantic actions stay
valid. Existing coordinate actions without effect intent are accepted as legacy
input but classified as `unknown`, so they require confirmation rather than
silently receiving lower risk.

### Observation

Every decision receives one current observation containing:

- strict application identity (`app_id`, `pid`) and window identity
  (`window_id`, title);
- window frame, model image size, display scale, `transform_id`, `image_hash`,
  and a screenshot;
- optional semantic elements;
- an `observation_id` binding all of the above;
- the previous action result and whether the UI is still changing.

`elements=[]` means “use the screenshot operator”, not “the application is
unsupported”.

The Agent-facing decision envelope is:

```json
{
  "schema_version": "1.1.0",
  "task_id": "task_...",
  "observation_id": "obs_...",
  "target": {"app_id":"...", "pid":123, "window_id":456, "window_title":"..."},
  "transform": {
    "id":"transform_...", "frame":[0, 0, 1200, 800],
    "model_size":[1200, 800], "display_scale":2.0, "image_hash":"..."
  },
  "ui_state": "settled",
  "elements": [],
  "image_path": "~/Library/Application Support/AnythingUse/tmp/task_.../obs_....png",
  "last_action": null
}
```

The screenshot file remains owner-only and task-scoped. It is deleted when the
decision is consumed, replaced, cancelled, or expired.

### Application and window lifecycle

- `lcu run --app` accepts bundle ID, display name, or full app path.
- Resolution first uses an already-running visible window. If the app is not
  running, the macOS backend may launch it with
  `NSWorkspace.OpenConfiguration.activates=false` and waits up to 5 seconds for
  a capturable window.
- Multiple candidate windows are never resolved by “first window”. The caller
  supplies a title selector, or the Agent chooses from a read-only window list.
- `lcu apps --json` provides read-only app and window discovery for Agents and
  humans.
- A task may change apps only through an explicit `switch_target` action. The
  Runtime releases the old target, checks access for the new app, resolves one
  window, and emits a fresh observation before any other action.
- Before the first screenshot or action for an app, reuse the existing desktop
  approval UI for an app-access decision: allow once, always allow, or deny.
  Agents and CLI commands cannot approve. Persistent decisions are local,
  per-user, revocable, and keyed by bundle ID plus signing Team Identifier; an
  unsigned app uses its canonical bundle path and must be approved again if that
  path or code identity changes.

### Action

Both external Agent and local VLM use the same action set:

- semantic invoke, set-value, selection, and secondary action when an element
  exists;
- target-window click, double-click, right-click, drag, scroll, text input, and
  key/key-combination when working from the screenshot;
- wait, request-user, fail, and explicit done;
- Chrome navigation through the existing real-Chrome control surface.

The canonical new actions are intentionally small:

```json
{"kind":"targeted","type":"click","x":0.25,"y":0.20,"button":"left","click_count":1}
{"kind":"targeted","type":"drag","from_x":0.2,"from_y":0.2,"to_x":0.8,"to_y":0.2}
{"kind":"targeted","type":"scroll","x":0.5,"y":0.5,"delta_x":0,"delta_y":-0.5}
{"kind":"targeted","type":"type_text","text":"查询词"}
{"kind":"targeted","type":"key_combo","keys":["RETURN"]}
{"kind":"switch_target","app_id":"com.example.TargetApp","window_title_contains":null}
```

`lcu act` carries policy metadata outside the action hash:

```text
lcu act <task-id> --observation-id <obs> \
  --intent search --evidence "search field in target window" \
  --action '<canonical action json>'
```

`--destination` and `--data-summary` are optional for ordinary actions and
required for consequential transmission/change actions.

Both decision actors populate the same `ProposedAction` fields. `intent` is a
closed value: `observe`, `search`, `open`, `select`, `navigate`, `edit`, `send`,
`submit`, `upload`, `delete`, `publish`, `authenticate`, `security`, `finance`,
or `unknown`. Evidence is a short description of the visible control or expected
screen change; it is audit context, not permission. Consequential proposals also
carry `destination` and `data_summary` so the approval UI can say what will be
sent or changed, and where.

Policy metadata is outside the canonical `Action` JSON but inside the proposal
and its one-time approval binding. Runtime computes a `proposal_hash` over schema
version, observation ID, target/transform identity, action, intent, evidence,
destination, and data summary. Approval of one proposal cannot authorize another
coordinate, recipient, target, observation, or consequence.

Targeted input validation is fixed and shared across callers:

- coordinates and deltas are finite normalized values in `[0,1]` (scroll deltas
  may be `[-1,1]`);
- `click_count` is `1...3`;
- key names use one canonical set: `CMD`, `CTRL`, `ALT`, `SHIFT`, `RETURN`,
  `TAB`, `ESC`, arrows, function keys, or one printable base key;
- text, URL, and key-count limits reuse the existing trust-boundary limits;
- unsupported buttons/keys fail validation before reaching a backend.

Every UI action carries the current `observation_id`. Runtime resolves that ID
to the stored target and transform; callers never submit raw PIDs or window IDs
for an action. Immediately before execution, the backend rechecks app/window
identity, frame, scale, and current frontmost ownership. A geometry mismatch
returns a fresh observation; coordinates are never silently remapped.

### Execution and coexistence

- Never activate, raise, switch to, or later restore the target as an execution
  strategy.
- Never move the user's physical pointer or emit global keyboard shortcuts.
- Deliver input to the selected application/window. A PID-only path must prove
  the selected window at execution time; otherwise it fails without touching
  another window.
- User activity on the same target pauses the task. Activity elsewhere is not a
  reason to pause.
- After an action, wait only until the target becomes stable (bounded), capture a
  fresh observation, and continue.

The macOS operator proof is deliberately narrow:

- screenshot coordinates map only through the bound window transform;
- AX hit-test/press remains the preferred acceleration when available;
- the non-AX path posts only to the target process and first proves the target
  is the topmost window of that process at the requested point;
- mouse delivery must not require or create system frontmost/key-window state;
- keyboard delivery follows a successful bound click or proven editable target,
  rechecking target ownership between chunks;
- inability to prove the destination fails before input. It never activates the
  app, restores another app afterward, or introduces an app-specific shortcut.
- operator, risk, settle, and retry code may branch on platform capability and
  action type only. Bundle ID, process name, application title, vendor, and
  acceptance-case strings are forbidden implementation conditions.

### Settle and retry

After every successful UI action:

1. wait 500 ms and capture once;
2. if image hash and compact element signature are unchanged from the preceding
   capture, mark `settled`;
3. otherwise poll every 250 ms until two consecutive captures agree or 5 seconds
   elapse;
4. at the deadline return the latest observation with `ui_state=changing`; the
   actor may wait once or choose another action;
5. the same action may be retried at most once, and only from a new observation.

There is no unbounded sleep, blind repeated click, or per-application timing
table.

### Recoverable outcomes

Backends return stable outcomes instead of free-form failure guessing:

| Outcome | Runtime behavior |
|---|---|
| `stale_observation` / geometry changed | discard proposal, return fresh decision |
| `waiting_user` | release execution slot; resume from fresh observation after decision |
| `taken_over` | pause task, release target; explicit resume required |
| `target_lost` | fail current step/task; never select another window implicitly |
| `unsupported_capability` | fail without fallback input; report exact missing action |
| `permission_denied` | block before observation/action and surface through `doctor` |
| `ui_state=changing` | valid observation; actor may issue one bounded wait |

### Completion and audit

- Preserve the existing rule: only explicit `done` from a fresh, settled
  observation can succeed; an action receipt alone never proves the goal.
- Runtime re-observes the final target and records the target sequence for a
  multi-app task before accepting Done.
- SQLite keeps task state, target identities, proposal hash, intent, risk,
  approval decision, capability path, and action result. It does not persist raw
  screenshots, full semantic trees, or unredacted credential/text values.
- Logs use the existing redaction path; no new telemetry or cloud service is
  introduced.

### Storage and artifact lifecycle

AnythingUse runtime storage is bounded. Temporary process output is never an
unlimited task history.

| Artifact | Lifetime and limit |
|---|---|
| decision screenshot / compact observation | keep only the currently pending observation; delete when consumed, replaced, expired, cancelled, or terminal |
| per-task temporary directory | delete at task terminal state; startup sweeper removes an orphan older than 1 hour after proving no live task owns it |
| Runtime/native sockets, PID and lock files | remove on clean shutdown; replace only a stale owner-verified entry at startup |
| Chrome task tab/group | close on task terminal state; startup recovery closes only tabs carrying an AnythingUse ownership marker |
| rotating logs | maximum 5 files × 5 MiB; secrets, raw screenshots, and full input text remain redacted |
| SQLite tasks/events | retain 30 days or the newest 1,000 terminal tasks, whichever is smaller; compact after pruning; never prune active tasks |
| crash diagnostics | maximum 20 MiB total and 7 days |
| installer/update staging | use a unique temporary directory and remove it on success, failure, or cancellation |

Ephemeral runtime data excluding the optional model has a 256 MiB hard quota.
When the quota is reached, AnythingUse deletes oldest terminal-task artifacts
inside its own runtime root; it never deletes active-task data and refuses new
work if safe reclamation cannot get below the quota.

The local VLM is an installed asset, not temporary data, but it is the largest
storage risk:

- external-Agent mode does not download or copy a local model;
- VLM mode uses one configured model directory and never copies the model into a
  task, release package, worktree, or runtime cache;
- an upgrade stages one replacement, verifies it, atomically switches the active
  pointer, then removes the superseded managed version;
- before download/staging, AnythingUse verifies enough free space for the new
  model plus a fixed reserve; insufficient space refuses the upgrade, and failed
  or cancelled downloads remove their partial managed file;
- user-supplied model directories are never deleted by AnythingUse;
- `doctor` reports model path, ownership, active version, and byte size.

All automatic deletion is restricted to paths created beneath the resolved
AnythingUse runtime root and recorded in its ownership manifest. Cleanup never
uses a broad home-directory target, unresolved glob, bundle path, user document,
browser profile, or user-supplied model directory.

`lcu storage --json` reports usage by category and quota.
`lcu cleanup --dry-run --json` shows every reclaimable owned path;
`lcu cleanup --json` performs the same scoped cleanup. Ordinary task completion
and startup recovery do not depend on the user running the manual command.

### Safety

Risk is classified from the intended and visible effect:

| Effect | Default handling |
|---|---|
| observe, search, open, select, navigate, scroll | execute |
| ordinary non-sensitive text entry or local setting | execute with normal audit |
| send, submit consequential data, upload, delete, publish | action-time confirmation |
| credential, security-sensitive change, irreversible finance | user handoff / highest gate |
| unknown effect | request clarification or confirmation |

Coordinate click and key input are not automatically high risk. Conversely, a
semantic `invoke` is not automatically safe merely because AX supplied a label.
The effect classifier may raise risk from screen/element evidence; it must not
silently reinterpret an explicitly consequential action as harmless.

`intent` is model/Agent output and is therefore not authorization. Runtime uses
it as one policy signal:

- action shape, semantic label/value, typed text, URL, and declared intent each
  may raise risk;
- the highest result wins;
- `send`, `submit`, `upload`, `delete`, and `publish` cannot fall below the
  confirmation tier;
- `authenticate`, `security`, and `finance` cannot fall below handoff;
- absent or `unknown` intent requires confirmation;
- the user-authored goal may pre-authorize only the specific consequence and
  destination allowed by policy; screen content never grants permission.

### Queue and ownership

- One desktop action executes at a time per login user. This remains the queue's
  only concurrency rule.
- Observation/model decision time does not hold the desktop execution slot or a
  target lease. This applies equally to external Agent and local VLM tasks.
- When a proposal arrives, the task re-enters FIFO order and Runtime performs the
  pre-execution target/geometry check. A changed target produces a fresh
  observation instead of executing the stale proposal.
- Waiting for app access, consequential-action confirmation, or user takeover
  also releases the execution slot.
- A task holds exactly one target lease only during observe/execute/settle. An
  explicit `switch_target` releases the old lease before acquiring the new one.

### Screen-content trust boundary

Text visible in an app, webpage, screenshot, accessibility tree, document, or
message is task data, not instruction. It cannot change the goal, select another
app, approve an action, weaken policy, request secrets, or authorize data
transmission. Only the original user request and explicit user follow-ups may
change those decisions. Both Agent and VLM prompts carry this rule; Runtime still
enforces target, app access, and consequence gates independently.

## Current gap matrix

| Area | Current AnythingUse | Required correction |
|---|---|---|
| Observation | Screenshot exists, but empty AX becomes a practical dead end | Screenshot remains fully actionable when elements are empty |
| Coordinate click | Model can propose it; Guard fixes it at R3 | Treat click as a normal action and gate its business effect |
| Background mouse | AX hit may work; fallback requires the target to be key | Prove and deliver target-window input without frontmost activation |
| Background keyboard | Limited text/Return paths depend on AX/key-window state | Support target-scoped text and general key combinations after a bound click |
| Action coverage | No drag, multi-click, general shortcut, reliable secondary click | Complete the small common operator set |
| Freshness | Observation IDs exist; settling is inconsistent | One bounded settle + re-observe after every action |
| Safety | Primitive-based R3 defaults dominate screenshot operation | Consequence-based policy shared by both decision actors |
| Multi-app | One task resolves one app target | Permit an explicit target switch inside one task; queue remains serial |
| Decision actors | Agent and VLM share most plumbing | Keep both; make their observation/action contract identical |
| App lifecycle | Caller normally supplies a running app; no app-access allowlist | Background resolve/launch, discovery, and one reusable app-access approval |
| Scheduler | Long decision waits can occupy product-loop ownership | Release desktop slot while either actor decides; revalidate on FIFO re-entry |
| Screen content | No explicit prompt-injection contract | Treat all visible content as untrusted data |
| Wire evolution | `1.0.0` has no switch-target or effect-intent contract | Add `1.1.0`; retain safe legacy semantic actions |
| Storage | Screenshots, logs, database history, model/build/staging data have no one documented quota contract | Immediate lifecycle cleanup, startup orphan sweep, rotating limits, ownership manifest, and visible usage |

## Minimal implementation plan

The implementation is deliberately two waves and one final acceptance gate.
No Top100 suite, soak program, backend framework, MCP, or Playwright work is
part of it.

Before Wave 1, review and commit the current dirty candidate as a named
checkpoint. The rework starts from that checkpoint so the existing fixes and
the new operator contract are not mixed into one unreviewable commit.

### Wave 1 — prove the operator and freeze the contract

1. **Generic macOS operator spike** (`DirectedInput.swift`, `Service.swift`,
   `lcu-platform-macos`): accept an arbitrary runtime-resolved `MacWindowTarget`
   and its current screenshot, then perform one background coordinate click
   followed by text/key input with no AX-element dependency. The frontmost app,
   physical pointer, PID, and target window identity must not change. If this
   cannot be proved, stop and replace the shared native input mechanism before
   touching Runtime policy.
2. **Shared contract** (`lcu-core/action.rs`, `observation.rs`, `protocol.rs`,
   `lcu-model/validate.rs`, `lcu-cli/main.rs`): add the actions and `1.1.0`
   envelope above. Bind actions through stored observation/transform identity.
3. **Effect policy** (`lcu-core/effect_guard.rs`, Runtime approval binding,
   Agent/VLM proposal adapters): replace unconditional coordinate-click/key R3
   with the consequence table. Preserve GUI confirmation and R4 handoff.
4. **Actor parity** (`agent_actor.rs`, `subprocess_actor.rs`, Qwen worker, Skill):
   expose identical screenshots, target metadata, action schema, intent, evidence,
   last result, and screen-content trust rule.

Wave 1 gate: code review finds no application-specific branch, and the operator
works from `elements=[]` using only the bound target and screenshot. After app
access has been granted, ordinary search/select/open requires no per-action
approval.

### Wave 2 — integrate the stable loop

1. **Runtime loop and scheduler** (`lcu-runtime/worker.rs`, `lib.rs`): route
   semantic and screenshot actions through one step path; release the desktop
   slot during decisions; re-enqueue and revalidate before execution; apply the
   bounded settle rule once.
2. **Target lifecycle** (`PlatformBackend`, macOS/Chrome product adapters,
   Runtime state): add background resolve/launch, read-only app discovery,
   `switch_target`, and one-target-at-a-time lease transfer.
3. **App access** (existing approval GUI, SQLite state, CLI status): add allow
   once/always/deny before first observation; Agents remain unable to approve.
4. **Artifact lifecycle** (`lcu-runtime/paths.rs`, task finalization/startup,
   Agent/VLM screenshot handling, Chrome task ownership, CLI): centralize owned
   paths, immediate terminal cleanup, bounded startup sweep, quotas, rotating
   logs, database pruning, `storage`, and dry-run cleanup. Packaging uses one
   temporary staging directory with unconditional cleanup. Development
   worktrees/staging are removed after integration; source-build caches are
   reported but deleted only by
   `scripts/clean-dev-artifacts.sh --apply --include-build-cache`; its default
   mode is dry-run and every target must remain repo-owned and explicit.
5. **Cleanup obsolete behavior:** remove unconditional targeted Click/KeyCombo
   R3, macOS Return-only validation, AX-empty “request user” prompt guidance,
   redundant AX retries, and any key-window/restore workaround made obsolete by
   the proven operator. Keep useful AX semantic acceleration and strict target
   checks.
6. **Documentation:** update command contract, architecture, privacy,
   troubleshooting, user guide, README, and Agent Skill from the final schema.

### Compatibility and migration

- Bump wire schema to `1.1.0`; do not rename existing task states or commands.
- Decode existing `1.0.0` semantic actions unchanged.
- Map legacy coordinate/key proposals without intent to `unknown` confirmation.
- On first `1.1.0` start, pause non-terminal `1.0.0` tasks for explicit resume or
  cancel; do not execute a persisted action under new risk rules.
- Reuse the current SQLite, approval UI, queue, Runtime socket, Chrome extension,
  Native Messaging host, and dual Actor selection. No parallel replacement.

### Single final acceptance gate

Run only these three live scenarios:

1. TextEdit background text entry and explicit Done;
2. Enterprise WeChat exact-contact search, with no message sent — a black-box
   AX-incomplete application case, never an implementation dependency;
3. Chrome background read-only navigation through the existing extension.

During every scenario the user continuously works in a different app for at
least 60 seconds, including mouse movement, scrolling, and typing. Record:

- task result and explicit Done evidence;
- target PID/window/transform before every action;
- frontmost app and key-window transitions;
- physical pointer position before/after each agent mouse action;
- the user's typed sentinel text, proving no keystroke was diverted;
- final screenshot and whether any approval appeared;
- runtime storage before submission, after each terminal task, and after process
  restart; no orphan observation/task directory remains and usage stays within
  the declared quota.

Any target activation, pointer movement, diverted user key, wrong-window input,
blind action retry, or post-hoc focus restoration fails the gate. Do not add an
application-specific workaround: fix or replace the shared macOS operator.

Build and unit checks run once after integration. Only one focused unit check is
added for each new trust-boundary branch (schema binding, consequence gate,
queue re-entry, app access); no broad generated test catalog. Windows, signing,
packaging, stress testing, and broad application certification remain separate
future work.
