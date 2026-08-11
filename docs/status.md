# Delivery status

Version: **v3.2** (macOS core)  
Branch: `main`  
Product name: **AnythingUse**  
Command codename: **LCU** (`lcu`, `lcu-*`); data root: `~/Library/Application Support/AnythingUse`

## Source-level delivery

This page records source and interface delivery. **Implemented** means the
behavior exists in the source tree; it does not mean that AnythingUse is already
proven mature, stable, or compatible with every real application.

Current macOS-core implementation includes:

- Humans and Agents share the public `lcu` CLI; Agents use the bundled Skill only.
- One serial FIFO queue per login user, with pause, resume, cancel, GUI approval, and SQLite recovery (leftover tasks from a dead process are recovered to `paused` for the user to decide; recovered tasks do not occupy queue slots).
- macOS: strict window identity (PID + `CGWindowID`); no activate-as-strategy; same-window takeover pauses.
- Chrome: real user Chrome via extension + Native Messaging + inactive task tab (not Playwright).
- Pluggable decision maker: when `LCU_VISION_ACTOR` is unset/`auto`, tasks default to the external Agent (`lcu decide`/`lcu act`); `--actor vlm` selects the optional local Qwen3-VL subprocess per task. Both use the same data surface and safety pipeline.
- Task `succeeded` only after explicit model `Done` and successful re-observation of the target.

## Runtime verification boundary

`Runtime-verified` requires a recorded live run on the target surface. The
source-level items above are not a substitute for verifying a specific app,
Chrome profile, permissions state, model, or user-coexistence scenario. No
broad stability or compatibility claim is made by this status page.

Live evidence recorded on 2026-08-11:

- TextEdit: semantic `set_value` followed by explicit `Done` succeeded without making TextEdit frontmost.
- Enterprise WeChat (`com.tencent.WeWorkMac`): screenshot capture works, but the app returned no AX windows/elements; the contact-search task was cancelled before any action. This surface is not yet runtime-verified.

Hard product boundaries (not optional):

- No MCP Computer Use control plane.
- No Playwright / independent automation browser.
- No public TCP control surface (private per-user Unix sockets only).

## Outside the current source scope

These are not part of the current macOS-core source scope:

- Top100 multi-app certification suites and long soak / stress programs as release blockers.
- macOS Developer ID signed installer / notarized packaging.
- Windows (or other OS) backends.

## Follow-up (independent tracks)

1. Signed macOS packaging and install/uninstall story.
2. Windows compatibility on the same command-oriented model.

Neither changes the current source-level delivery statement.

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
