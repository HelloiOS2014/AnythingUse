# Delivery status

Version: **v3.2** (macOS core)  
Branch: `main`  
Product name: **AnythingUse**  
Command codename: **LCU** (`lcu`, `lcu-*`); data root: `~/Library/Application Support/AnythingUse`

## What is delivered

macOS core Computer Use is implemented and on `main`:

- Humans and Agents share the public `lcu` CLI; Agents use the bundled Skill only.
- One serial FIFO queue per login user, with pause, resume, cancel, GUI approval, and SQLite recovery (leftover tasks from a dead process are recovered to `paused` for the user to decide; recovered tasks do not occupy queue slots).
- macOS: strict window identity (PID + `CGWindowID`); no activate-as-strategy; same-window takeover pauses.
- Chrome: real user Chrome via extension + Native Messaging + inactive task tab (not Playwright).
- Pluggable decision maker: local Qwen3-VL subprocess (default) or the external Agent itself (`LCU_VISION_ACTOR=agent`, `lcu decide`/`lcu act`) — identical data surface and safety pipeline.
- Task `succeeded` only after explicit model `Done` and successful re-observation of the target.

Hard product boundaries (not optional):

- No MCP Computer Use control plane.
- No Playwright / independent automation browser.
- No public TCP control surface (private per-user Unix sockets only).

## What is not a freeze gate

These are explicit non-goals for the current macOS core freeze:

- Top100 multi-app certification suites and long soak / stress programs as release blockers.
- macOS Developer ID signed installer / notarized packaging.
- Windows (or other OS) backends.

## Follow-up (independent tracks)

1. Signed macOS packaging and install/uninstall story.
2. Windows compatibility on the same command-oriented model.

Neither blocks claiming macOS core capability on `main`.

## Naming map

| Surface | Name |
|---|---|
| Product / GitHub | AnythingUse |
| CLI binary | `lcu` |
| Desktop host | `lcu-desktop` |
| Agent Skill directory | `skills/local-computer-use` |
| Runtime data directory | `~/Library/Application Support/AnythingUse` |
| Crates / native prefixes | `lcu-*`, historical “Local Computer Use” phrasing in some docs |

“Local Computer Use” in Skill and older headings is the historical product line name; new prose should prefer **AnythingUse** while keeping path and binary names stable.
