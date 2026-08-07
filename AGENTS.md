# AnythingUse — Agent Notes

This machine runs **AnythingUse** (`lcu`), a local-first control plane for
operating real macOS applications and Chrome through one command surface.

## For agents (any CLI: Grok Build, Claude Code, ...)

- Use the **`local-computer-use`** skill (`skills/local-computer-use/SKILL.md`). (Codex ships its own Computer Use and does not need this skill.)
  It is the only supported way to operate the desktop — never touch the
  private sockets or invent new protocols.
- The skill ships as a plugin through this repo's marketplace:
  `claude plugin marketplace add HelloiOS2014/AnythingUse` + `claude plugin
  install anythinguse` (Claude Code), or `grok plugin install
  https://github.com/HelloiOS2014/AnythingUse --trust` (Grok Build). Update
  with `claude plugin update` / `grok plugin update`. If it is missing from
  your toolset, read the SKILL.md directly.
- For machines other than the dev box: copy this file to
  `~/.grok/AGENTS.md` and `~/.claude/CLAUDE.md` so the agents know about
  AnythingUse from any working directory.
- **Decision maker is pluggable**: the default is the local Qwen3-VL
  subprocess (`lcu run "<goal>"` decides automatically). An agent can also
  decide itself: start the runtime with `LCU_VISION_ACTOR=agent`, submit a
  goal, then loop `lcu decide <task-id> --wait --json` → read the screenshot
  path → `lcu act <task-id> --observation-id <obs> --action '<json>'` →
  finish with a `done` action.

## Quick reference

```bash
# runtime must be running first
./target/release/lcu-desktop &

# health
./target/release/lcu doctor --json

# submit a goal (VLM decides automatically)
./target/release/lcu run "Open Downloads in Finder" --app com.apple.finder --wait --json

# agent decides (runtime started with LCU_VISION_ACTOR=agent)
lcu decide <task-id> --wait --json
lcu act <task-id> --observation-id <obs> --action '{"kind":"semantic","type":"invoke","element_id":"e1"}'
```

Hard rules (never break):

1. Only invoke the `lcu` binary — no private sockets, no MCP control plane,
   no Playwright/DOM access.
2. Never approve high-risk actions (R3/R4). If `lcu` returns exit 2 /
   `waiting_user`, stop and ask the human.
3. Claim completion only when `lcu status` / `lcu result` reports
   `succeeded` (explicit `Done` + target re-observation).
4. Do not request screenshots or full semantic trees on stdout outside the
   agent decision mode data surface (`lcu decide`).

Full contract: `docs/command-contract.md` · `docs/architecture.md` ·
`docs/privacy.md` · `docs/troubleshooting.md`.
