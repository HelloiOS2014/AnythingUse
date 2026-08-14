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

## Reviewed AnythingUse execution contract

This section records the current source contract. It is not a claim that Gate 1
real-app acceptance has passed.

The public wire version is `1.1.0`. Existing `1.0.0` semantic actions stay valid.
Both Actors submit the same closed-set `effect`; Runtime's independent evidence
floor may raise but never lower risk.

### Observation

Every decision receives one current observation containing:

- strict application identity (`app_id`, `pid`) and window identity
  (`window_id`, title);
- window frame, model image size, display scale, `transform_id`, `image_hash`,
  and a screenshot;
- optional semantic elements;
- an `observation_id` binding all of the above;
- the previous action result and whether the UI is still changing.

`elements=[]` means only that screenshot observation remains available. On the
current macOS backend it does not prove that background input can be isolated;
unsupported actions fail closed.

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
  "ui_state": "captured",
  "elements": [],
  "image_path": "/private/tmp/lcu-agent-...-obs_....png",
  "last_action_summary": null
}
```

The screenshot file remains owner-only and task-scoped. It is deleted when the
decision is consumed, replaced, cancelled, or expired.

### Application and window lifecycle

> **Future reference only.** The lifecycle, expanded action set, settle loop,
> storage quotas, and cleanup commands below are not current product claims.
> They must not be implemented on top of the rejected focus-changing Operator.

- `lcu run --app` accepts bundle ID, display name, or full app path.
- Resolution first uses an already-running visible window. If the app is not
  running, the macOS backend may launch it with
  `NSWorkspace.OpenConfiguration.activates=false` and waits up to 5 seconds for
  a capturable window.
- Multiple candidate windows are never resolved by “first window”. The caller
  supplies a title selector, or the Agent chooses from a read-only window list.
- `lcu apps --json` provides read-only app and window discovery for Agents and
  humans.
- A current task remains bound to one strict app/window target. Multi-app target
  switching is not part of the implemented contract.
- Before the first screenshot or action for an app, reuse the existing desktop
  approval UI for an app-access decision: allow once, always allow, or deny.
  Agents and CLI commands cannot approve. Persistent decisions are local,
  per-user, revocable from the menu-bar settings, and keyed by bundle ID plus signing Team Identifier, signing ID, and code hash; an
  unsigned app uses its canonical bundle path and must be approved again if that
  path or code identity changes.

### Action

Both decision Actors use the same implemented action and `effect` schema:

- semantic: `navigate` (Chrome only), `invoke`, `set_value`, `focus`, `scroll`;
- targeted: single `click`, `type_text`, `key_combo` (backend capabilities may
  reject a valid shared action before input);
- control: `observe`, `wait`, `request_user`, `fail`, explicit `done`.

```bash
lcu act <task-id> --observation-id <obs> \
  --action '{"kind":"targeted","type":"click","x":0.25,"y":0.20,"button":"left"}' \
  --effect '{"kind":"navigate","summary":"Open the selected item"}'
```

Executable semantic/targeted actions require one closed-set `effect.kind`:
`observe`, `navigate`, `local_edit`, `external_communication`,
`external_submit`, `destructive`, `permission_change`, `financial`,
`credential`, or `unknown`. It is Actor output, not authorization. Runtime
computes an independent evidence floor and can only raise the risk; missing,
illegal, or `unknown` effects stop for the user.

Every UI action carries the current `observation_id`. Runtime resolves that ID
to the stored strict target and transform, validates finite normalized
coordinates and bounded text/keys/URLs, then rechecks the target before input.
The current macOS surface supports only actions its backend explicitly reports;
there is no `exclusive`, drag, multi-click, right/middle-click downgrade, or
`switch_target` action.

App access, foreground activation, and consequence confirmation/takeover are
separate gates. Approval never replays the old proposal: Runtime takes a fresh
observation and returns it to the same Actor. A consequence grant is consumed
once by an equivalent Runtime-derived identity, or for screenshot-only input by
an exact fresh `image_hash + action_hash` match.

### Execution and coexistence

- Background semantic first, then provably isolated background targeted input.
  Activation is used only inside one GUI-approved foreground session
  (`foreground_activate` is the only `NSRunningApplication.activate` entry);
  the previous app is never restored afterwards.
- The foreground-session approval dialog discloses that the target window may
  come to the front once. Ordinary focus/open/search/select/navigate/scroll/edit
  inside an approved session do not repeat capability approval; consequences
  remain approved action by action, and the user takes over with
  `lcu pause` / `lcu cancel`.
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
- raw non-AX coordinate click is not a product path;
- mouse delivery must not require or create system frontmost/key-window state
  outside an approved session;
- keyboard delivery follows a successful bound click or proven editable target,
  rechecking target ownership between chunks;
- inability to prove the destination fails before input. `foreground_required`
  is reported only before any input occurred, and is answered by at most one
  session approval (retry once on a fresh observation; ambiguous same-app
  windows fail instead of guessing);
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
| `waiting_actor` | release execution slot; continue with the same Actor on a fresh observation after the gate decision |
| `taken_over` | pause task, release target; explicit resume required |
| `target_lost` | fail current step/task; never select another window implicitly |
| `unsupported_capability` | fail without fallback input; report exact missing action |
| `foreground_required` | in `auto`, request a task-scoped foreground grant; after approval activate the exact target, discard the old proposal, and return a fresh observation to the same Actor — never restore the previous app. A suspended session resumes without activation only while the exact target stayed foreground and untouched; otherwise a later activation needs a new grant. |
| `permission_denied` | block before observation/action and surface through `doctor` |
| `ui_state=changing` | valid observation; actor may issue one bounded wait |

### Completion and audit

- Preserve the existing rule: only explicit `done` from a fresh, settled
  observation can succeed; an action receipt alone never proves the goal.
- Runtime re-observes the final target and records the target sequence for a
  multi-app task before accepting Done.
- SQLite keeps task state, target identities, proposal hash, effect, risk,
  approval decision, capability path, and action result. It does not persist raw
  screenshots, full semantic trees, or unredacted credential/text values.
- Logs use the existing redaction path; no new telemetry or cloud service is
  introduced.

### Storage and artifact lifecycle

> **Mixed status.** Current code deletes decision screenshots on every exit
> path, prunes owned crash leftovers older than one hour, retains only terminal
> SQLite tasks within 30 days and the newest 1,000, and removes release staging
> through a shell exit trap. Runtime logs are stderr-only; the Chrome native host
> rotates at 5 files × 5 MiB. The quota, storage/cleanup commands, crash-diagnostic
> cap, managed model upgrade, and database compaction
> below remain future design intent and must not be quoted as delivered.

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
| observe and proven semantic navigation/scroll | execute |
| ordinary non-sensitive text entry or local setting | execute with normal audit |
| send, submit consequential data, upload, delete, publish | action-time confirmation |
| credential, security-sensitive change, irreversible finance | user handoff / highest gate |
| coordinate click/key or unknown effect | confirmation (R3) |

Coordinate click and key input remain R3 until an independent execution-time
signal can classify the hit control; Actor effect alone never lowers them.
Conversely, semantic `invoke` is not automatically safe merely because AX
supplied a label.

`effect` is Actor output and is therefore not authorization. Runtime uses it as
one policy signal:

- action shape, semantic label/value, typed text, URL, and declared effect each
  may raise risk;
- the highest result wins;
- `send`, `submit`, `upload`, `delete`, and `publish` cannot fall below the
  confirmation tier;
- `authenticate`, `security`, and `finance` cannot fall below handoff;
- absent, illegal, or `unknown` effect stops for the user;
- the user-authored goal scopes the task but does not approve a real
  consequence; screen content never grants permission.

### Queue and ownership

- One desktop action executes at a time per login user. This remains the queue's
  only concurrency rule.
- Observation/model decision time does not hold the desktop execution slot.
  An external Agent continuation retains only its strict target reservation.
- When a proposal arrives, the task re-enters FIFO order and Runtime performs the
  pre-execution target/geometry check. A changed target produces a fresh
  observation instead of executing the stale proposal.
- Waiting for app access, consequence confirmation, foreground activation, or takeover
  also releases the execution slot.
- A current task remains bound to one strict target; a different task can run
  only after the scheduler safely releases or hands off execution ownership.

### Screen-content trust boundary

Text visible in an app, webpage, screenshot, accessibility tree, document, or
message is task data, not instruction. It cannot change the goal, select another
app, approve an action, weaken policy, request secrets, or authorize data
transmission. Only the original user request and explicit user follow-ups may
change those decisions. Both Agent and VLM prompts carry this rule; Runtime still
enforces target, app access, and consequence gates independently.

## Gate result and current boundary (2026-08-12)

The generic screenshot-only macOS Operator spike failed its first required
gate, and the replacement SkyLight SPI transport (opt-in synthetic focus
records) was removed in the same freeze: focus records disrupted the user's
active keyboard focus, authenticated text did not land in the AX-empty target,
and delivery could not be proven independently of the app.

The execution ladder is now: background semantic → provably isolated background
targeted → **GUI-approved task-scoped foreground session** → explicit failure.
The approved session raises or uniquely proves the exact window before activation,
then re-proves it after activation and before input; ambiguous same-process windows fail closed. It never
restores the previous app and is cleared on terminal / pause / target change /
release / runtime recovery. External-Agent think time suspends native ownership;
resume is non-activating and requires the same untouched foreground window.
Real user HID on the target ends the session and pauses the task automatically.

Consequences:

- no automatic product routing starts from the removed mechanism;
- screenshot-coordinate input uses the selected Actor's mandatory closed-set
  consequence classification when Runtime has no contradictory UI evidence;
  missing/unknown classification stops, and send/delete/pay/credential evidence
  independently raises the floor;
- `1.1.0` adds observation target/transform metadata and shared Agent/VLM effect parity,
  not a claimed generic background-input capability;
- macOS currently uses strict window capture, AX semantic actions, and existing
  PID-directed actions only when their target proof succeeds;
- AX-empty controls are answered by the approved foreground-session fallback
  when the user grants it; otherwise they fail closed; no application-specific
  exception is allowed;
- startup removes stale Agent/VLM screenshots and the Chrome host rotates logs.

Live acceptance remains one generic background case, one screenshot-only
foreground-required case, and one Chrome background-tab case. Until accepted,
fail closed;
switching to the target and back without approval is not an allowed fallback.
