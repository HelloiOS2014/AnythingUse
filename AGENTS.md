# AnythingUse — Agent Notes

This machine runs **AnythingUse** (`lcu`), a local-first control plane for
operating real macOS applications and Chrome through one command surface.

## For agents (any CLI: Pi, Grok Build, Claude Code, ...)

- Use the **`local-computer-use`** skill (`skills/local-computer-use/SKILL.md`). (Codex ships its own Computer Use and does not need this skill.)
  It is the only supported way to operate the desktop — never touch the
  private sockets or invent new protocols.
- The skill ships from this repo for each harness:
  - **Pi:** `./scripts/install-pi.sh` (or `pi install /absolute/path/to/AnythingUse`).
    GitHub: `pi install git:github.com/HelloiOS2014/AnythingUse`. This checkout
    also autoloads the skill via `.pi/settings.json` after the project is trusted.
    Use an absolute path for global `pi install`; relative sources resolve against
    `~/.pi/agent/settings.json`, not the repo. `./scripts/install-pi.sh` also runs
    `./scripts/install-cli.sh` so `lcu` and `lcu-desktop` land in `~/.local/bin`.
  - **Claude Code:** `claude plugin marketplace add HelloiOS2014/AnythingUse` +
    `claude plugin install anythinguse`. Update with `claude plugin update anythinguse`.
  - **Grok Build:** `grok plugin install https://github.com/HelloiOS2014/AnythingUse --trust`.
    Update with `grok plugin update`.
  If the skill is missing from your toolset, read the SKILL.md directly.
- Resolve the `lcu` binary as `$LCU_BIN`, then `lcu` on `PATH` (product:
  `./scripts/install-cli.sh` puts sibling `lcu` + `lcu-desktop` in `~/.local/bin`),
  then `./target/release/lcu` from this checkout (**debug fallback only**).
- For machines other than the dev box: copy this file to `~/.grok/AGENTS.md`
  and `~/.claude/CLAUDE.md`. Pi has no global AGENTS.md; install the Pi package
  so the skill is available from any working directory.
- **Decision maker is pluggable, per task**: the agent itself is the default.
  Invoke `lcu` **directly** each step (`lcu run "<goal>" --actor agent` →
  `lcu decide <task-id> --wait --json` → **caller extracts** compact elements +
  reads `image_path` → `lcu act …`). Do not wrap `lcu` in a driver script.
  Do not use `python3 -c` / `.py` to call `lcu` or to parse `decide` JSON;
  redirect to a file and extract with `ctx_execute_file` / Read.
  `decide` is not a full semantic tree: it omits raw platform `actions` but
  includes generic `capabilities` (`invoke` / `set_value` / `focus` / `scroll`).
  Seeing a Finder sidebar label is not `AXPress`: Runtime `invoke` uses `AXSelect`
  (`kAXSelectedRowsAttribute` on the outline). Coordinate click →
  `foreground_required` → `auto` activates Finder (steals iTerm). To forbid
  that, pass `--control-mode background_only`. `lcu run --actor vlm` uses the
  local Qwen3-VL subprocess as an optional fallback. The Runtime default is
  `LCU_VISION_ACTOR` (`auto` = agent); tasks of both kinds coexist in one queue.

## Quick reference

```bash
# product entry: `lcu` on PATH (sibling `lcu-desktop` required)
# lcu starts the Runtime on demand; it exits after 60 idle seconds
lcu doctor --json

# external Agent: submit, then use the returned task id in the decision loop
lcu run "Open Downloads in Finder" --app com.apple.finder --actor agent --control-mode background_only --json
lcu decide <task-id> --wait --json
lcu act <task-id> --observation-id <obs> --action '{"kind":"semantic","type":"invoke","element_id":"e1"}'

# optional local VLM (separate task; model assets required)
lcu run "Open Downloads in Finder" --app com.apple.finder --actor vlm --wait --json
```

Hard rules (never break):

1. Only invoke the `lcu` binary **directly** — no driver scripts, no
   `python3 -c` / `.py` around `lcu` or its JSON, no private sockets, no MCP
   control plane, no Playwright/DOM access.
2. Never approve high-risk actions (R3/R4). If `lcu` returns exit 2 /
   `waiting_user`, stop and ask the human.
3. Claim completion only when `lcu status` / `lcu result` reports
   `succeeded` (explicit `Done` + target re-observation).
4. `lcu decide` returns compact elements + `image_path`. The caller extracts
   key fields. Do not echo the `elements` array, request a full tree, or run
   `lcu` under a rewritten `TMPDIR`.
5. If an element advertises a semantic capability, use it. Runtime rejects a
   coordinate click/type on that element with `semantic_action_required`.

Normative execution contract: `docs/execution-contract.md` (source-aligned;
concentrated live acceptance passed 2026-08-17).
Current implemented surface: `docs/command-contract.md` ·
`docs/architecture.md` · `docs/privacy.md` · `docs/troubleshooting.md`.
