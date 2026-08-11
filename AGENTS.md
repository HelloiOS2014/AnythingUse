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
- **Decision maker is pluggable, per task**: the agent itself is the default
  (`lcu run "<goal>" --actor agent` → loop `lcu decide <task-id> --wait --json` → read the
  screenshot path → `lcu act <task-id> --observation-id <obs> --action '<json>'`
  → finish with a `done` action). `lcu run --actor vlm` uses the local
  Qwen3-VL subprocess as an optional fallback. The Runtime default is
  `LCU_VISION_ACTOR` (`auto` = agent); tasks of both kinds coexist in one queue.

## Quick reference

```bash
# runtime must be running first
./target/release/lcu-desktop &

# health
./target/release/lcu doctor --json

# external Agent: submit, then use the returned task id in the decision loop
./target/release/lcu run "Open Downloads in Finder" --app com.apple.finder --actor agent --json
./target/release/lcu decide <task-id> --wait --json
./target/release/lcu act <task-id> --observation-id <obs> --action '{"kind":"semantic","type":"invoke","element_id":"e1"}'

# optional local VLM (separate task; model assets required)
./target/release/lcu run "Open Downloads in Finder" --app com.apple.finder --actor vlm --wait --json
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
