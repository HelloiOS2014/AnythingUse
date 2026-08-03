# AnythingUse

AnythingUse is a local-first control project intended to let humans and Agents operate many kinds of endpoints through one command surface. The current implementation focuses on Computer Use for real macOS applications through the local `lcu` command; additional endpoint types can be added later without changing the current public interface.

Agents use the bundled Skill. There is no MCP or Playwright control surface.

## Current scope

- **macOS windows:** background, window-targeted observation and semantic actions through the native window service.
- **Chrome:** the user's installed Chrome, controlled through an extension and Native Messaging on an inactive task tab.
- **Runtime:** one local serial FIFO queue with pause, resume, cancel, approvals, persistence, and a local VLM actor.
- **Non-interference:** work on another target continues without activating the target app or tab. User takeover of the same target pauses the task.
- **Completion:** an action receipt is not task success. The Runtime succeeds only after the model emits explicit `Done` and the target can be observed again.

Windows and a signed macOS installer are separate follow-up work. MCP, Playwright, an independent automation browser, Top100 gates, and long-duration test programs are not part of the current product.

## Development setup

```bash
cargo build -p lcu-cli -p lcu-desktop --release
(cd native/macos-window-service && swift build -c release)

./target/release/lcu-desktop &
./target/release/lcu doctor --json
```

For Chrome, install the native host, then load the extension once:

```bash
./native/chrome-control/scripts/install-native-host.sh
# chrome://extensions -> Developer mode -> Load unpacked
# select native/chrome-control/extension
```

Submit a task:

```bash
./target/release/lcu run \
  "Open Downloads in Finder" \
  --app com.apple.finder \
  --wait --json
```

Agent-originated tasks add display metadata only:

```bash
./target/release/lcu run \
  "Open Downloads in Finder" \
  --app com.apple.finder \
  --source agent --source-name codex \
  --wait --json
```

## Documentation

- [User guide](docs/user-guide.md)
- [`lcu` command contract](docs/command-contract.md)
- [Privacy](docs/privacy.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Agent Skill](skills/local-computer-use/SKILL.md)
- [Current delivery status](CURRENT_PLAN.md)
