# AnythingUse Execution Contract

Status: **frozen contract; source-aligned, concentrated live acceptance passed 2026-08-17**
Approved: 2026-08-14
Scope: macOS applications and the existing real-Chrome extension surface

> The Android endpoint (`lau`) is governed by the
> [LAU Android plan](lau-android-plan.md). It follows this contract's execution
> loop, gate model and honesty rules on a different platform, adding Android
> specifics (session-scoped observations, the hardware-touch watch, the Android
> evidence layer). Nothing here is overridden by that document.

This is the only normative description of how AnythingUse must execute a
Computer Use task. [Delivery status](status.md) records what the current source
and live product have actually proved. The reference notes and old realignment
plan are evidence and history, not competing contracts.

## 1. Product boundary

- Humans and Agents use the same public `lcu` command surface.
- The caller and the decision maker are independent. A human or Agent may submit
  a task; each task selects either the external Agent or local VLM as its Actor.
- Both Actors share one Runtime, queue, observation/action schema, safety policy,
  platform Operator, audit trail, and cleanup path.
- macOS native applications and the existing Chrome extension are two surfaces
  behind that Runtime. There is no MCP or Playwright control plane.
- Application names, bundle IDs, contacts, and acceptance strings never select
  product behavior.

## 2. Non-negotiable invariants

1. Execute at most one UI action at a time for the current login user.
2. Resolve an exact target before every action. Never guess the first, main, or
   focused window when identity is ambiguous.
3. Bind every proposal to the observation from which it was made.
4. Execute one proposal, then observe again. UI change invalidates stale pixels,
   elements, transforms, and proposals.
5. Approval never replays an old proposal.
6. Background operation is preferred, not promised. A permitted app may need to
   come to the front; AnythingUse never restores the previous app afterwards.
7. Real user input on the controlled target wins immediately.
8. Runtime, not the Actor or Operator, enforces application access and hard
   safety floors.
9. Terminal and abandoned work cannot leave screenshots, leases, grants, or
   staging artifacts growing without bound.

## 3. Identities and provenance

Every task records two independent facts:

- `source`: `human` or `agent`, including a display name for audit;
- `actor`: `agent` or `vlm`, identifying who proposes the next action.

Both fields are audit and routing metadata, not authentication. The public CLI
is shared by humans and Agents, so `--source human` cannot prove that a human
authored the task and must never lower a safety floor.

Native target identity is the signed application identity plus PID and exact
`CGWindowID`. Chrome target identity is the connected profile, task-owned tab,
current origin, and captured frame. An observation also binds its geometry and
image hash or equivalent monotonically current frame token.

## 4. Single-step loop

```text
dequeue one task step
  -> resolve exact target
  -> check application permission
  -> observe target
  -> Actor proposes one action + closed-set effect
  -> Runtime validates schema, target binding, freshness and safety
  -> if waiting: release execution slot and generic target ownership
  -> when resumed: re-resolve and compare against the bound observation
  -> if changed: discard proposal, observe again, return to the same Actor
  -> Operator executes at most one action
  -> bounded settle
  -> fresh observe
  -> continue or finish
```

There is no executable multi-action buffer. The queue schedules task steps, not
a script of clicks. If a proposal returns after an Agent or human wait, Runtime
must recapture enough target state to prove that the bound frame is still
current. Equality may reuse the proposal; any target, geometry, element, image,
origin, or user-input change discards it.

`Done` succeeds only after a final target re-observation. A model saying “done”
without that proof is not task completion.

## 5. Application access and foreground fallback

The first control of an application requires `allow_once`, `always_allow`, or
`deny`. The permission UI must disclose that:

- AnythingUse prefers verified background control;
- applications without a reliable background path may come to the front;
- the permission does not authorize sending, submitting, deleting, paying, or
  any other real-world consequence.

That disclosure is the foreground authorization for the permitted scope. The
target contract has no separate, repeatedly consumed `ForegroundGrant` and no
persistent Runtime or Swift foreground session.

The default control mode is `auto`: background semantic action, then provably
targeted background input, then foreground fallback. `background_only` is the
only restrictive override; it fails instead of activating. A separate
`foreground` task mode is unnecessary.

When foreground is required before any input:

1. verify that the exact app is permitted and the mode is not
   `background_only`;
2. activate or raise only the exact target window;
3. treat activation as a UI change and discard the pre-activation proposal;
4. observe again and let the same Actor propose from the foreground state;
5. immediately before input, re-prove exact-window identity.

Failure to prove the exact destination is a normal capability failure, never a
reason to select another window. The Operator does not restore the previous
application or add an application-specific workaround.

## 6. User coexistence and takeover

The existing listen-only `UserInputMonitor` remains independent of foreground
session state. AnythingUse tags its own input. Untagged click, key, or scroll on
the controlled target invalidates the pending proposal and pauses the task as
`taken_over`.

Input in another application does not disable safe background work. Foreground
fallback may interrupt the user's current foreground application, as disclosed
by application permission; it must never be described as background or
non-interfering execution.

Resume always starts from target resolution and a fresh observation. It never
continues a stored click or key event.

## 7. Queue and ownership

- One global FIFO serializes actual macOS and Chrome actions.
- Actor thinking, application permission, consequence confirmation, and paused
  tasks do not hold the execution slot.
- Those waits also release generic PID/window reservation. Fresh target/frame
  validation handles drift when the task returns.
- A Chrome tab lease is not a generic reservation. It remains task-owned while
  the task is resumable, has a bounded idle lifetime, and closes on terminal or
  abandoned work.
- A user-input epoch, target change, pause, cancel, failure, or runtime recovery
  invalidates the pending proposal before another action can run.

Machine-readable task output keeps the coarse lifecycle state and adds a
`wait_reason` when parked:

- `agent_decision`;
- `app_access`;
- `consequence`.

User pause and takeover use the paused lifecycle state, not `waiting_actor`.
Only a human gate produces the CLI “waiting for user” result.

## 8. Safety contract

The action primitive does not determine risk. A coordinate click is not
automatically dangerous, and a semantic invoke is not automatically safe.
Every executable proposal carries one closed-set effect:

| Effect | Default |
|---|---|
| observe / navigate | execute |
| local edit with non-sensitive data | execute |
| external communication or submit | confirm once at the action boundary |
| destructive or irreversible change | confirm |
| permission, credential, security, or financial action | hand off / highest gate |
| unknown or conflicting evidence | stop and ask |

An exact recipient and exact content in the task goal improve the confirmation
summary and prevent Actor broadening, but do not replace action-time user
confirmation because caller metadata is unauthenticated. Runtime verifies the
same destination and content at execution time. Ambiguity, changed content,
changed destination, sensitive data, or contradictory UI evidence requires a
fresh confirmation or handoff. Runtime may always raise the Actor's claimed
risk; it never lowers a hard floor from evidence.

Application permission, prior task intent, and consequence confirmation remain
separate facts. CLI and Agents cannot approve GUI gates themselves.

## 9. Outcomes and recovery

Operators return generic outcomes only: executed, stale observation,
foreground required, taken over, target lost, unsupported capability, or
permission denied.

- stale: discard proposal and re-observe;
- foreground required: follow section 5 without replaying the action;
- taken over: pause and release generic ownership;
- target lost: fail closed;
- unsupported: report the missing capability, with no hidden fallback;
- permission denied: stop before observation or input.

Retry is bounded and only for a clearly transient observe/settle condition. It
never means repeating an action with an uncertain side effect.

## 10. Artifact lifecycle

Reuse terminal cleanup and Runtime startup; do not add a cleanup daemon.

- Keep only the current decision observation; delete replaced screenshots.
- Terminal, cancel, and takeover remove pending proposals, task-temporary files,
  generic ownership, native ephemeral state, and one-time grants.
- Chrome closes terminal or expired resumable-task leases.
- Runtime startup prunes provably orphaned task artifacts and rotates bounded
  logs; it never copies the local VLM into task, release, or worktree storage.
- Failed or cancelled install/model staging is removed immediately.

## 11. Minimal acceptance

One concentrated acceptance run is sufficient:

1. an AX-rich macOS application using ordinary background semantic actions;
2. an AX-poor/transient macOS application using foreground fallback and fresh
   observations — WeWork may be the sample but cannot appear in product policy;
3. the user's real Chrome through the extension and a task-owned tab;
4. user takeover during a pending task, proving the old proposal is discarded.

No Top100 suite, long soak, per-application fixture matrix, or app-specific
recovery code is part of this migration.

## 12. Source alignment

The source alignment completed by deletion:

- remove `ForegroundGrant`, Runtime `foreground` / `foreground_authorized`,
  persistent Swift `ForegroundSession`, and session suspend/resume APIs;
- keep application permission, consequence confirmation, exact-window proof,
  `UserInputMonitor`, global FIFO, dual Actors, Chrome lease, audit, and cleanup;
- release generic reservations during waits and add `wait_reason`;
- keep external communication at one action-time confirmation until a trusted
  human-intent channel actually exists;
- move the debug embedded Runtime out of the production CLI path;
- delete tests and documentation only when their superseded behavior disappears.

There is no compatibility flag and no second execution path. Section 11 remains
the live product acceptance boundary.
