# Privacy

Product: **AnythingUse**. Runtime data root on macOS defaults to `~/Library/Application Support/LocalComputerUse` (override with `LCU_RUNTIME_ROOT`).

## Data that stays local

| Kind | Location | Notes |
|---|---|---|
| Task metadata | Runtime SQLite | No screenshot blobs |
| Events | SQLite `events` table | Compact messages only |
| Screenshots (if any) | Runtime `screenshots/` with 0700/0600 | TTL cleanup; not agent-visible |
| Model weights | `models/` (user provided) | Offline inference preferred |

## What Agents see

Only `lcu` JSON fields: task id, goal, state, summary/error, step count, app selector, and doctor flags.  
Agents do **not** receive screenshots, full AX trees, model chain-of-thought, or approval internals.

**Agent decision mode exception** (`LCU_VISION_ACTOR=agent`): when the agent
is the decision maker, `lcu decide` deliberately hands it the same data
surface the local VLM gets — a compact element tree, the goal/step context,
and a scaled screenshot via a 0600 temp file (deleted when the decision is
consumed or times out; never embedded in IPC JSON). This is the explicit
price of agent-driven decisions and only applies while agent mode is
enabled.

## What we do not do by default

- Upload telemetry to the network
- Complete high-risk approvals without GUI user presence

## Model helper isolation

The Python VLM worker is a subprocess. An isolation probe may show that the OS does **not** block network or home-directory writes. Do not claim full sandboxing unless seatbelt/sandbox evidence is recorded.
