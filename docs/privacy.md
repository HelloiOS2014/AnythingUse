# Privacy

## Data that stays local

| Kind | Location | Notes |
|---|---|---|
| Task metadata | Runtime SQLite | No screenshot blobs |
| Events | SQLite `events` table | Compact messages only |
| Screenshots (if any) | Runtime `screenshots/` with 0700/0600 | TTL cleanup; not agent-visible |
| Model weights | `models/` (user provided) | Offline inference preferred |

## What Agents see

Only `lcu` JSON fields: task id, state, summary/error, doctor flags.  
Agents do **not** receive screenshots, full AX trees, or model chain-of-thought.

## What we do not do by default

- Upload telemetry to the network
- Complete high-risk approvals without GUI user presence

## Model helper isolation

The Python VLM worker is a subprocess. An isolation probe may show that the OS does **not** block network or home-directory writes. Do not claim full sandboxing unless seatbelt/sandbox evidence is recorded.
