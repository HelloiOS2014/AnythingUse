# AnythingUse Computer Use Realignment — Historical Record

Status: **superseded on 2026-08-14**
Successor: [AnythingUse Execution Contract](execution-contract.md)

This file preserves the reason for the August 2026 realignment. It is not an
active design, implementation, or acceptance contract. The full frozen version
remains available in Git history before this replacement.

## What triggered the realignment

- task-specific fixes accumulated around transient UI and foreground behavior;
- foreground authorization, Runtime ownership, and native session state became
  overlapping sources of truth;
- waits and approvals reused `waiting_actor` without a machine-readable reason;
- Agent thinking retained target reservations for too long;
- safety documentation disagreed on whether coordinates or real consequences
  determined risk;
- reference material and current product policy were mixed in one document.

## Decisions that survived review

- public `lcu` only; no MCP or Playwright control plane;
- human and Agent callers, with external Agent and local VLM decision paths;
- one Runtime, global serial action queue, and shared safety/Operator path;
- strict target and observation binding;
- one action followed by a fresh observation;
- no stale-action replay after approval, activation, or user input;
- background-first macOS execution with disclosed foreground fallback;
- real Chrome through the extension and a task-owned tab;
- no application, bundle, contact, or acceptance-specific behavior;
- bounded artifact cleanup;
- one concentrated acceptance instead of Top100 or long soak gates.

## Decisions corrected by the final review

The earlier plan kept a separate `ForegroundGrant`, persistent Runtime
foreground ownership, and Swift `ForegroundSession`. The final review retained
the authorization requirement but rejected those duplicated session
lifecycles. Application permission now discloses necessary foreground fallback;
native execution re-proves the exact target per action.

The earlier review considered using human-authored exact recipient/content as
prior authorization. The final contract rejected that shortcut because the
shared CLI's `source` field is unauthenticated metadata. External communication
therefore keeps one action-time confirmation without re-approving ordinary
search, selection, or typing steps.

See the successor contract for all normative behavior and
[Delivery status](status.md) for implementation progress.
